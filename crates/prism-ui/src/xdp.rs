//! xdg-desktop-portal backend (`org.freedesktop.impl.portal.desktop.prism`): Screenshot
//! (screenshot, pick colour) and ScreenCast (screen sharing in browsers, OBS, Discord...).
//! While `portal_enabled` is on, prism-ui owns the name; `deploy/portal/prism.portal` tells
//! xdg-desktop-portal which interfaces it implements and `mango-portals.conf` picks it.
//!
//! Screenshots and colour picking need the run loop (overlays): requests are queued for it
//! ([`take`]) and answered over a channel. Screen sharing asks which screen or window to share
//! with the prompt card's choice list (prompt.rs), then runs `prism-rec --cast`, which
//! publishes the source as a PipeWire node; the portal hands that node to the app.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use prism_capture::Image;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::prompt::{self, Choice, Request, Verdict};
use crate::waker::Waker;

const NAME: &str = "org.freedesktop.impl.portal.desktop.prism";
const PATH: &str = "/org/freedesktop/portal/desktop";
/// Portal response codes.
const OK: u32 = 0;
const CANCELLED: u32 = 1;
const FAILED: u32 = 2;
/// ScreenCast source types and cursor modes.
const MONITOR: u32 = 1;
const WINDOW: u32 = 2;
const CURSOR_HIDDEN: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;

type Results = HashMap<String, OwnedValue>;

/// Work for the run loop.
pub enum UiReq {
    Screenshot { interactive: bool, reply: Sender<Option<Image>> },
    PickColor { reply: Sender<Option<(u8, u8, u8)>> },
}

static QUEUE: Mutex<Vec<UiReq>> = Mutex::new(Vec::new());
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// The run loop's waker (set by `init`).
pub fn waker() -> Option<Waker> {
    WAKER.lock().unwrap().clone()
}

pub fn init(waker: Waker) {
    *WAKER.lock().unwrap() = Some(waker);
}

/// Requests waiting for the run loop.
pub fn take() -> Vec<UiReq> {
    std::mem::take(&mut *QUEUE.lock().unwrap())
}

fn queue(req: UiReq) -> bool {
    let Some(w) = WAKER.lock().unwrap().clone() else { return false };
    QUEUE.lock().unwrap().push(req);
    w.wake();
    true
}

// ── Screenshot ──────────────────────────────────────────────────────────

struct Screenshot;

#[zbus::interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    async fn screenshot(&self, _handle: OwnedObjectPath, _app_id: String, _parent_window: String, options: HashMap<String, OwnedValue>) -> (u32, Results) {
        let interactive = options.get("interactive").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        let (tx, rx) = channel();
        if !queue(UiReq::Screenshot { interactive, reply: tx }) {
            return (FAILED, Results::new());
        }
        blocking::unblock(move || match rx.recv().ok().flatten() {
            Some(img) => match save(img) {
                Ok(uri) => (OK, results([("uri", Value::from(uri))])),
                Err(e) => {
                    eprintln!("[prism-ui/portal] screenshot: {e}");
                    (FAILED, Results::new())
                }
            },
            None => (CANCELLED, Results::new()),
        })
        .await
    }

    async fn pick_color(&self, _handle: OwnedObjectPath, _app_id: String, _parent_window: String, _options: HashMap<String, OwnedValue>) -> (u32, Results) {
        let (tx, rx) = channel();
        if !queue(UiReq::PickColor { reply: tx }) {
            return (FAILED, Results::new());
        }
        blocking::unblock(move || match rx.recv().ok().flatten() {
            Some((r, g, b)) => (OK, results([("color", Value::from((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)))])),
            None => (CANCELLED, Results::new()),
        })
        .await
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

fn results<const N: usize>(items: [(&str, Value<'_>); N]) -> Results {
    items.into_iter().filter_map(|(k, v)| v.try_to_owned().ok().map(|v| (k.to_string(), v))).collect()
}

/// Screenshots for apps go to the cache (the portal exports them to sandboxed apps); files
/// older than a day are removed.
fn save(mut img: Image) -> Result<String, String> {
    let dir = prism_common::config::expand_home("~/.cache/prism/portal");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let old = std::time::SystemTime::now() - Duration::from_secs(24 * 3600);
        for e in rd.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = dir.join(format!("Screenshot_{}.png", crate::capture::timestamp()));
    crate::capture::write_png(&mut img, &path)?;
    Ok(format!("file://{}", path.display()))
}

// ── ScreenCast ──────────────────────────────────────────────────────────

#[derive(Default)]
struct Session {
    /// The app sharing (for the privacy indicator).
    app: String,
    types: u32,
    cursor_mode: u32,
    child: Option<Child>,
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

struct ScreenCast {
    sessions: Sessions,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> (u32, Results) {
        let path = session_handle.to_string();
        let app = app_name(&app_id);
        self.sessions.lock().unwrap().insert(path.clone(), Session { app, types: MONITOR, cursor_mode: CURSOR_EMBEDDED, child: None });
        let obj = SessionObj { path: path.clone(), sessions: self.sessions.clone() };
        if let Err(e) = server.at(session_handle.as_ref(), obj).await {
            eprintln!("[prism-ui/portal] session {path}: {e}");
            self.sessions.lock().unwrap().remove(&path);
            return (FAILED, Results::new());
        }
        (OK, results([("session_id", Value::from(path))]))
    }

    async fn select_sources(&self, _handle: OwnedObjectPath, session_handle: OwnedObjectPath, _app_id: String, options: HashMap<String, OwnedValue>) -> (u32, Results) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(session_handle.as_str()) else { return (FAILED, Results::new()) };
        let num = |k: &str| options.get(k).and_then(|v| u32::try_from(v).ok());
        s.types = num("types").filter(|t| t & (MONITOR | WINDOW) != 0).unwrap_or(MONITOR);
        s.cursor_mode = num("cursor_mode").unwrap_or(CURSOR_EMBEDDED);
        (OK, Results::new())
    }

    async fn start(&self, _handle: OwnedObjectPath, session_handle: OwnedObjectPath, app_id: String, _parent_window: String, _options: HashMap<String, OwnedValue>) -> (u32, Results) {
        let path = session_handle.to_string();
        let (types, cursor) = match self.sessions.lock().unwrap().get(&path) {
            Some(s) => (s.types, s.cursor_mode == CURSOR_EMBEDDED),
            None => return (FAILED, Results::new()),
        };
        let sessions = self.sessions.clone();
        blocking::unblock(move || match start_cast(&app_id, types, cursor) {
            Ok(None) => (CANCELLED, Results::new()),
            Ok(Some((child, node, source_type, pos, size))) => {
                let mut map = sessions.lock().unwrap();
                let Some(s) = map.get_mut(&path) else {
                    // Closed while the user was choosing.
                    stop(child);
                    return (CANCELLED, Results::new());
                };
                s.child = Some(child);
                drop(map);
                changed();
                watch(sessions.clone(), path.clone());
                let mut props: HashMap<String, OwnedValue> = HashMap::new();
                let mut put = |k: &str, v: Value<'_>| {
                    if let Ok(v) = v.try_to_owned() {
                        props.insert(k.to_string(), v);
                    }
                };
                put("source_type", Value::from(source_type));
                put("size", Value::from(size));
                if let Some(p) = pos {
                    put("position", Value::from(p));
                }
                let streams: Vec<(u32, HashMap<String, OwnedValue>)> = vec![(node, props)];
                match Value::from(streams).try_to_owned() {
                    Ok(v) => (OK, HashMap::from([("streams".to_string(), v)])),
                    Err(_) => (FAILED, Results::new()),
                }
            }
            Err(e) => {
                eprintln!("[prism-ui/portal] screen cast: {e}");
                (FAILED, Results::new())
            }
        })
        .await
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        MONITOR | WINDOW
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        CURSOR_HIDDEN | CURSOR_EMBEDDED
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        4
    }
}

struct SessionObj {
    path: String,
    sessions: Sessions,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionObj {
    async fn close(&self, #[zbus(object_server)] server: &zbus::ObjectServer) {
        end(&self.sessions, &self.path);
        if let Ok(p) = ObjectPath::try_from(self.path.as_str()) {
            let _ = server.remove::<SessionObj, _>(p).await;
        }
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

fn stop(mut child: Child) {
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Forget a session and stop its stream.
fn end(sessions: &Sessions, path: &str) {
    if let Some(child) = sessions.lock().unwrap().remove(path).and_then(|s| s.child) {
        stop(child);
    }
    changed();
}

/// The sessions of the running service (for the privacy indicator).
static SESSIONS: OnceLock<Sessions> = OnceLock::new();

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Counts share starts and ends (the privacy indicator follows it).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// A share started or ended: the run loop updates the privacy indicator.
fn changed() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Some(w) = WAKER.lock().unwrap().clone() {
        w.wake();
    }
}

/// Apps sharing the screen right now.
pub fn casts() -> Vec<String> {
    let Some(s) = SESSIONS.get() else { return Vec::new() };
    let mut apps: Vec<String> = s.lock().unwrap().values().filter(|s| s.child.is_some()).map(|s| s.app.clone()).collect();
    apps.dedup();
    apps
}

/// Stop every screen share (the privacy indicator's Stop button). The apps are told through
/// the sessions' Closed signal.
pub fn stop_casts() {
    let Some(s) = SESSIONS.get() else { return };
    let paths: Vec<String> = s.lock().unwrap().iter().filter(|(_, v)| v.child.is_some()).map(|(k, _)| k.clone()).collect();
    for p in paths {
        if let Some(child) = s.lock().unwrap().get_mut(&p).and_then(|v| v.child.take()) {
            stop(child);
        }
        // `watch` sees the stream gone and closes the session.
    }
    changed();
}

/// When the stream ends by itself (the shared window closed), tell the app.
fn watch(sessions: Sessions, path: String) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(500));
        let mut map = sessions.lock().unwrap();
        let Some(s) = map.get_mut(&path) else { return };
        let exited = s.child.as_mut().is_none_or(|c| c.try_wait().ok().flatten().is_some());
        if exited {
            changed();
        }
        if !exited {
            continue;
        }
        map.remove(&path);
        drop(map);
        if let Some(conn) = CONN.get() {
            let _ = conn.emit_signal(None::<&str>, path.as_str(), "org.freedesktop.impl.portal.Session", "Closed", &());
            if let Ok(p) = ObjectPath::try_from(path.as_str()) {
                let _ = conn.object_server().remove::<SessionObj, _>(p);
            }
        }
        return;
    });
}

type Started = (Child, u32, u32, Option<(i32, i32)>, (i32, i32));

/// Ask what to share, then start the stream. Ok(None) when the user cancelled.
fn start_cast(app_id: &str, types: u32, cursor: bool) -> Result<Option<Started>, String> {
    let conn = wayland_client::Connection::connect_to_env().map_err(|e| e.to_string())?;
    let mut cap = prism_capture::Capturer::new(&conn)?;
    cap.refresh();
    let outputs = if types & MONITOR != 0 { cap.outputs() } else { Vec::new() };
    let windows = if types & WINDOW != 0 && cap.can_capture_windows() { cap.windows() } else { Vec::new() };
    let mut choices: Vec<Choice> = outputs
        .iter()
        .map(|o| Choice { title: format!("Screen {}", o.name), detail: format!("{} · {}×{}", o.description, o.width, o.height), icon: "monitor" })
        .collect();
    choices.extend(windows.iter().map(|w| Choice { title: if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() }, detail: w.app_id.clone(), icon: "window" }));
    if choices.is_empty() {
        return Err("nothing to share".into());
    }
    let app = app_name(app_id);
    let session = prompt::ask(Request {
        title: "Share Your Screen".into(),
        message: format!("{app} wants to see your screen. Choose what to share."),
        detail: String::new(),
        icon: "screen-share",
        fields: Vec::new(),
        ok_label: "Share".into(),
        choices,
    })
    .ok_or("no prompt")?;
    let Some(answer) = session.answers.recv().ok().flatten() else { return Ok(None) };
    session.verdict(Verdict::Done);
    let i: usize = answer.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let (job, source_type, pos) = if i < outputs.len() {
        let o = &outputs[i];
        (prism_common::capture::CastJob { output: Some(o.name.clone()), window: None, cursor }, MONITOR, Some((o.x, o.y)))
    } else {
        let w = windows.get(i - outputs.len()).ok_or("bad choice")?;
        (prism_common::capture::CastJob { output: None, window: Some(w.id.clone()), cursor }, WINDOW, None)
    };
    let logical = if i < outputs.len() { Some((outputs[i].width, outputs[i].height)) } else { None };
    let json = serde_json::to_string(&job).map_err(|e| e.to_string())?;
    let mut child = Command::new(rec_binary()).arg("--cast").arg(json).stdout(Stdio::piped()).stdin(Stdio::null()).spawn().map_err(|e| format!("prism-rec: {e}"))?;
    let out = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    let Some(node) = v.get("node").and_then(|n| n.as_u64()) else {
        stop(child);
        return Err("the stream did not start".into());
    };
    let px = |k: &str| v.get(k).and_then(|n| n.as_i64()).unwrap_or(0) as i32;
    let size = logical.unwrap_or((px("width"), px("height")));
    Ok(Some((child, node as u32, source_type, pos, size)))
}

/// prism-rec next to this binary, else from PATH.
fn rec_binary() -> std::path::PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("prism-rec"))).filter(|p| p.exists()).unwrap_or_else(|| "prism-rec".into())
}

/// A readable name for the app asking (its desktop file's Name when it has an app id).
fn app_name(app_id: &str) -> String {
    if app_id.is_empty() {
        return "An app".into();
    }
    let file = format!("{app_id}.desktop");
    let dirs = [prism_common::config::expand_home("~/.local/share/applications"), "/var/lib/flatpak/exports/share/applications".into(), "/usr/share/applications".into()];
    for d in dirs {
        if let Ok(text) = std::fs::read_to_string(d.join(&file)) {
            if let Some(n) = text.lines().find_map(|l| l.strip_prefix("Name=")) {
                return n.to_string();
            }
        }
    }
    app_id.rsplit('.').next().unwrap_or(app_id).to_string()
}

// ── Service ─────────────────────────────────────────────────────────────

static CONN: OnceLock<zbus::blocking::Connection> = OnceLock::new();
static WANTED: AtomicBool = AtomicBool::new(false);
static STARTED: std::sync::Once = std::sync::Once::new();

/// Own the portal name while `portal_enabled` is on (it is kept until prism-ui restarts).
pub fn apply(cfg: &prism_common::Config) {
    WANTED.store(cfg.portal_enabled, Ordering::SeqCst);
    if cfg.portal_enabled {
        STARTED.call_once(|| {
            std::thread::Builder::new().name("prism-portal".into()).spawn(serve).ok();
        });
    }
}

fn serve() {
    let sessions: Sessions = Arc::default();
    let _ = SESSIONS.set(sessions.clone());
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(NAME))
        .and_then(|b| b.serve_at(PATH, Screenshot))
        .and_then(|b| b.serve_at(PATH, ScreenCast { sessions: sessions.clone() }))
        .and_then(|b| b.build());
    match conn {
        Ok(c) => {
            eprintln!("[prism-ui/portal] serving {NAME}");
            let _ = CONN.set(c);
        }
        Err(e) => eprintln!("[prism-ui/portal] cannot own {NAME}: {e}"),
    }
}
