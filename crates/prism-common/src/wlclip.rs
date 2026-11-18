//! Wayland clipboard without wl-clipboard: one `ext_data_control_v1` client per process on its
//! own thread and Wayland connection. It owns selections we set (serving paste requests until
//! another client takes over), reads the current selection, and tells watchers about each new one.
//!
//! Copies that must stay out of the clipboard history carry [`SELF_MIME`]; selections offering
//! `x-kde-passwordManagerHint` (password managers) are never read for watchers.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as device, ext_data_control_manager_v1 as manager, ext_data_control_offer_v1 as offer, ext_data_control_source_v1 as source,
};

/// Marker MIME type on Prism's own copies that must not enter the clipboard history.
pub const SELF_MIME: &str = "application/x-prism-self";
/// Offered by password managers for secrets.
pub const SECRET_MIME: &str = "x-kde-passwordManagerHint";
/// Text MIME types, most preferred first.
pub const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"];
/// Largest selection read for watchers and paste.
const MAX_READ: u64 = 64 << 20;

/// A new selection, as watchers see it.
#[derive(Debug, Clone)]
pub struct Selection {
    pub mimes: Vec<String>,
    pub content: Content,
}

#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    /// `image/png` bytes (offered only when the selection has no text).
    Png(Vec<u8>),
}

impl Selection {
    /// Copied by Prism itself and marked to stay out of the history.
    pub fn is_self(&self) -> bool {
        self.mimes.iter().any(|m| m == SELF_MIME)
    }
}

type Watcher = Arc<dyn Fn(&Selection) + Send + Sync>;

enum Cmd {
    Set { mimes: Vec<String>, data: Arc<Vec<u8>> },
    Get { prefs: Vec<String>, reply: mpsc::Sender<Option<Vec<u8>>> },
    Types(mpsc::Sender<Vec<String>>),
}

struct Service {
    tx: Mutex<mpsc::Sender<Cmd>>,
    wake: OwnedFd,
}

static SERVICE: OnceLock<Option<Service>> = OnceLock::new();
static WATCHERS: Mutex<Vec<Watcher>> = Mutex::new(Vec::new());

fn service() -> Option<&'static Service> {
    SERVICE
        .get_or_init(|| match start() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[prism/clipboard] {e}");
                None
            }
        })
        .as_ref()
}

fn send(cmd: Cmd) -> bool {
    let Some(s) = service() else { return false };
    if s.tx.lock().unwrap().send(cmd).is_err() {
        return false;
    }
    unsafe { libc::write(s.wake.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
    true
}

/// Own the clipboard with `data` under each of `mimes`. `history: false` adds [`SELF_MIME`] so
/// Prism's history watcher skips it. The data is served until another client takes the selection.
pub fn copy(mimes: &[&str], data: Vec<u8>, history: bool) -> bool {
    let mut mimes: Vec<String> = mimes.iter().map(|m| m.to_string()).collect();
    if !history {
        mimes.push(SELF_MIME.to_string());
    }
    send(Cmd::Set { mimes, data: Arc::new(data) })
}

/// Copy text under the usual text MIME types.
pub fn copy_text(text: &str, history: bool) -> bool {
    copy(&TEXT_MIMES, text.as_bytes().to_vec(), history)
}

/// Read the current selection in the first of `prefs` it offers.
pub fn paste(prefs: &[&str]) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if !send(Cmd::Get { prefs: prefs.iter().map(|m| m.to_string()).collect(), reply: tx }) {
        return None;
    }
    rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

pub fn paste_text() -> Option<String> {
    paste(&TEXT_MIMES).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// MIME types the current selection offers (empty when there is none).
pub fn types() -> Vec<String> {
    let (tx, rx) = mpsc::channel();
    if !send(Cmd::Types(tx)) {
        return Vec::new();
    }
    rx.recv_timeout(Duration::from_secs(3)).unwrap_or_default()
}

/// Call `f` (on a worker thread) for every new selection with text or a PNG image, secrets excluded.
/// Starts the clipboard service; returns false when the compositor has no data-control support.
pub fn watch(f: impl Fn(&Selection) + Send + Sync + 'static) -> bool {
    WATCHERS.lock().unwrap().push(Arc::new(f));
    // Watching starts with the current selection: make sure the service is running.
    service().is_some()
}

fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Ask the selection owner for `mime`; returns the read end. The caller reads it off this thread
/// (the owner may be us, answering on this thread's queue).
fn receive(conn: &Connection, o: &offer::ExtDataControlOfferV1, mime: &str) -> Option<std::fs::File> {
    let (r, w) = pipe()?;
    o.receive(mime.to_string(), w.as_fd());
    drop(w);
    let _ = conn.flush();
    Some(std::fs::File::from(r))
}

fn read_all(f: std::fs::File) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    f.take(MAX_READ).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn start() -> Result<Service, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("WAYLAND_SOCKET").is_none() {
        return Err("no Wayland display".into());
    }
    let conn = Connection::connect_to_env().map_err(|e| format!("Wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| format!("Wayland registry: {e}"))?;
    let qh = queue.handle();
    let manager: manager::ExtDataControlManagerV1 = globals.bind(&qh, 1..=1, ()).map_err(|_| "compositor lacks ext_data_control_manager_v1".to_string())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|_| "no wl_seat".to_string())?;
    let device = manager.get_data_device(&seat, &qh, ());
    let (tx, rx) = mpsc::channel();
    let (wake_r, wake_w) = pipe().ok_or("pipe failed")?;
    unsafe { libc::fcntl(wake_r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
    let mut st = State { conn: conn.clone(), manager, device, current: None, pending: Vec::new(), selection: None };
    std::thread::Builder::new()
        .name("prism-clipboard".into())
        .spawn(move || {
            loop {
                if queue.dispatch_pending(&mut st).is_err() {
                    break;
                }
                while let Ok(cmd) = rx.try_recv() {
                    st.command(cmd, &qh);
                }
                if conn.flush().is_err() {
                    break;
                }
                let Some(guard) = conn.prepare_read() else { continue };
                let mut fds = [
                    libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
                    libc::pollfd { fd: wake_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                ];
                if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
                    drop(guard);
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if fds[0].revents != 0 {
                    if let Err(e) = guard.read() {
                        if !matches!(e, wayland_client::backend::WaylandError::Io(ref io) if io.kind() == std::io::ErrorKind::WouldBlock) {
                            eprintln!("[prism/clipboard] Wayland connection lost: {e}");
                            break;
                        }
                    }
                } else {
                    drop(guard);
                }
                if fds[1].revents != 0 {
                    let mut b = [0u8; 64];
                    while unsafe { libc::read(wake_r.as_raw_fd(), b.as_mut_ptr().cast(), b.len()) } > 0 {}
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Service { tx: Mutex::new(tx), wake: wake_w })
}

struct State {
    conn: Connection,
    manager: manager::ExtDataControlManagerV1,
    device: device::ExtDataControlDeviceV1,
    /// Our source and what it serves.
    current: Option<(source::ExtDataControlSourceV1, Vec<String>, Arc<Vec<u8>>)>,
    /// Offers announced but not yet named as a selection, with their MIME types.
    pending: Vec<(offer::ExtDataControlOfferV1, Vec<String>)>,
    /// The current regular selection.
    selection: Option<(offer::ExtDataControlOfferV1, Vec<String>)>,
}

impl State {
    fn command(&mut self, cmd: Cmd, qh: &QueueHandle<State>) {
        match cmd {
            Cmd::Set { mimes, data } => {
                let src = self.manager.create_data_source(qh, ());
                for m in &mimes {
                    src.offer(m.clone());
                }
                self.device.set_selection(Some(&src));
                if let Some((old, _, _)) = self.current.replace((src, mimes, data)) {
                    old.destroy();
                }
            }
            Cmd::Get { prefs, reply } => {
                let Some((o, mimes)) = &self.selection else {
                    let _ = reply.send(None);
                    return;
                };
                let Some(mime) = prefs.iter().find(|p| mimes.contains(p)) else {
                    let _ = reply.send(None);
                    return;
                };
                match receive(&self.conn, o, mime) {
                    Some(f) => {
                        std::thread::spawn(move || {
                            let _ = reply.send(read_all(f));
                        });
                    }
                    None => {
                        let _ = reply.send(None);
                    }
                }
            }
            Cmd::Types(reply) => {
                let _ = reply.send(self.selection.as_ref().map(|(_, m)| m.clone()).unwrap_or_default());
            }
        }
    }

    fn take_pending(&mut self, id: &offer::ExtDataControlOfferV1) -> Vec<String> {
        match self.pending.iter().position(|(o, _)| o == id) {
            Some(i) => self.pending.swap_remove(i).1,
            None => Vec::new(),
        }
    }

    fn new_selection(&mut self, id: Option<offer::ExtDataControlOfferV1>) {
        if let Some((old, _)) = self.selection.take() {
            old.destroy();
        }
        let Some(o) = id else { return };
        let mimes = self.take_pending(&o);
        let watchers: Vec<Watcher> = WATCHERS.lock().unwrap().clone();
        if !watchers.is_empty() && !mimes.iter().any(|m| m == SECRET_MIME) {
            let text = TEXT_MIMES.iter().find(|t| mimes.iter().any(|m| m == *t));
            let (mime, is_text) = match text {
                Some(t) => (Some(*t), true),
                None => (mimes.iter().any(|m| m == "image/png").then_some("image/png"), false),
            };
            if let Some(f) = mime.and_then(|m| receive(&self.conn, &o, m)) {
                let mimes = mimes.clone();
                std::thread::spawn(move || {
                    let Some(bytes) = read_all(f) else { return };
                    if bytes.is_empty() {
                        return;
                    }
                    let content = if is_text { Content::Text(String::from_utf8_lossy(&bytes).into_owned()) } else { Content::Png(bytes) };
                    let sel = Selection { mimes, content };
                    for w in &watchers {
                        w(&sel);
                    }
                });
            }
        }
        self.selection = Some((o, mimes));
    }
}

impl Dispatch<source::ExtDataControlSourceV1, ()> for State {
    fn event(st: &mut Self, src: &source::ExtDataControlSourceV1, ev: source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            source::Event::Send { mime_type, fd } => {
                if let Some((cur, mimes, data)) = &st.current {
                    if cur == src && mimes.contains(&mime_type) {
                        let data = data.clone();
                        // Off the Wayland thread: the reader may be slow (or be us).
                        std::thread::spawn(move || {
                            let mut f = std::fs::File::from(fd);
                            let _ = f.write_all(&data);
                        });
                    }
                }
            }
            source::Event::Cancelled => {
                if st.current.as_ref().is_some_and(|(c, _, _)| c == src) {
                    st.current = None;
                }
                src.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<device::ExtDataControlDeviceV1, ()> for State {
    fn event(st: &mut Self, _: &device::ExtDataControlDeviceV1, ev: device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            device::Event::DataOffer { id } => st.pending.push((id, Vec::new())),
            device::Event::Selection { id } => st.new_selection(id),
            device::Event::PrimarySelection { id: Some(o) } => {
                st.take_pending(&o);
                o.destroy();
            }
            device::Event::Finished => {
                st.current = None;
                eprintln!("[prism/clipboard] data-control device finished");
            }
            _ => {}
        }
    }
    event_created_child!(State, device::ExtDataControlDeviceV1, [device::EVT_DATA_OFFER_OPCODE => (offer::ExtDataControlOfferV1, ())]);
}

impl Dispatch<offer::ExtDataControlOfferV1, ()> for State {
    fn event(st: &mut Self, o: &offer::ExtDataControlOfferV1, ev: offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let offer::Event::Offer { mime_type } = ev {
            if let Some((_, m)) = st.pending.iter_mut().find(|(p, _)| p == o) {
                m.push(mime_type);
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<manager::ExtDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &manager::ExtDataControlManagerV1, _: manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
