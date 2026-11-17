// ─────────────────────────────────────────────────────────────────────────
// desktop/dbus.rs — session-bus services for the desktop widgets
//
// One worker thread owns all D-Bus work so the widget renderer never blocks:
//
//   • MPRIS: tracks org.mpris.MediaPlayer2.* players from PropertiesChanged /
//     Seeked / NameOwnerChanged signals (no polling). Player choice matches
//     the QML get-mpris.sh: the first Playing player, else the last one that
//     played, else the first paused one. Album art URLs are resolved to local
//     files (http(s) art is downloaded once into ~/.cache/prism/mpris-art).
//
//   • System tray (StatusNotifierItem): becomes org.kde.StatusNotifierWatcher
//     when nobody else is (nothing else runs one in this session), otherwise
//     registers as a host with the existing watcher. Item properties are read
//     with GetAll and refreshed on NewIcon/NewStatus/NewTitle; icon names are
//     resolved to files here (theme scan happens on this thread, not the UI).
//     Context menus use com.canonical.dbusmenu (GetLayout / Event).
//
// The UI sends `DbusCmd`s and receives `DbusEvent`s; every event also wakes
// the UI event loop through `Waker`.
// ─────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use zbus::blocking::{fdo::DBusProxy, Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedValue, Value};
use zbus::MatchRule;

use crate::waker::Waker;

// ── Public data model ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct MediaState {
    /// Bus name, e.g. org.mpris.MediaPlayer2.spotify
    pub bus_name: String,
    pub title: String,
    pub artist: String,
    /// Local file for the album art, once available.
    pub art_path: Option<PathBuf>,
    pub playing: bool,
    pub position_s: f64,
    pub length_s: f64,
    /// When `position_s` was sampled; the UI interpolates while playing.
    pub sampled_at: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrayItem {
    /// "<bus name><object path>", stable id of the item.
    pub key: String,
    pub title: String,
    pub status: String,
    pub icon_path: Option<PathBuf>,
    /// IconPixmap fallback: (width, height, premultiplied native-endian ARGB32 bytes).
    pub pixmap: Option<(i32, i32, Vec<u8>)>,
    pub item_is_menu: bool,
    pub has_menu: bool,
    /// Hover text: (title, body) from ToolTip, else (Title, "").
    pub tooltip: (String, String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// None = plain item; Some(checked) = checkbox/radio.
    pub toggle: Option<bool>,
    /// Nesting depth (submenu entries are shown inline, indented).
    pub depth: u32,
    pub is_submenu: bool,
}

pub enum DbusEvent {
    Media(Option<MediaState>),
    Tray(Vec<TrayItem>),
    Menu { key: String, entries: Vec<MenuEntry> },
}

pub enum DbusCmd {
    /// MPRIS Player method: "PlayPause", "Next", "Previous".
    Media(&'static str),
    TrayActivate { key: String, x: i32, y: i32 },
    TraySecondary { key: String, x: i32, y: i32 },
    TrayMenu { key: String },
    TrayMenuClick { key: String, id: i32 },
    /// Re-resolve tray icons (icon theme changed).
    RefreshTray,
}

pub struct DbusHandle {
    pub events: Receiver<DbusEvent>,
    pub cmds: Sender<DbusCmd>,
}

/// Start the D-Bus worker. `want_tray` / `want_media` limit work to visible widgets.
pub fn spawn(waker: Waker) -> DbusHandle {
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (sig_tx, sig_rx) = std::sync::mpsc::channel::<Internal>();
    let fwd = sig_tx.clone();
    std::thread::Builder::new()
        .name("prism-dbus".into())
        .spawn(move || {
            // Commands are forwarded into the same internal queue as bus signals.
            std::thread::spawn(move || {
                for c in cmd_rx {
                    if fwd.send(Internal::Cmd(c)).is_err() {
                        break;
                    }
                }
            });
            if let Err(e) = Worker::run(ev_tx, sig_tx, sig_rx, waker) {
                eprintln!("[prism-desktop/dbus] worker stopped: {}", e);
            }
        })
        .expect("spawn dbus worker");
    DbusHandle { events: ev_rx, cmds: cmd_tx }
}

// ── Internals ─────────────────────────────────────────────────────────────

enum Internal {
    Cmd(DbusCmd),
    Signal(zbus::Message),
    /// From the watcher interface: (item string, sender unique name)
    Register(String, String),
    ArtReady(String, PathBuf),
}

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";

/// org.kde.StatusNotifierWatcher implementation, served when we own the name.
struct Watcher {
    items: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    tx: Sender<Internal>,
    waker_tx: Waker,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] hdr: zbus::message::Header<'_>) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        let _ = self.tx.send(Internal::Register(service.to_string(), sender));
        self.waker_tx.wake();
    }

    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().map(|v| v.clone()).unwrap_or_default()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// dbusmenu layout node: (id, properties, children as variants of the same structure).
#[derive(serde::Deserialize, zbus::zvariant::Type)]
#[allow(dead_code)]
struct MenuLayout(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

struct ItemRef {
    service: String,
    path: String,
    owner: String,
}

struct Worker {
    conn: Connection,
    dbus: DBusProxy<'static>,
    out: Sender<DbusEvent>,
    internal_tx: Sender<Internal>,
    waker: Waker,
    // MPRIS
    last_playing: Option<String>,
    media: Option<MediaState>,
    art_cache: HashMap<String, PathBuf>,
    art_pending: Vec<String>,
    // Tray
    is_watcher: bool,
    watcher_items: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    items: Vec<ItemRef>,
    tray: Vec<TrayItem>,
}

impl Worker {
    fn run(out: Sender<DbusEvent>, tx: Sender<Internal>, rx: Receiver<Internal>, waker: Waker) -> zbus::Result<()> {
        let conn = Connection::session()?;
        let dbus = DBusProxy::new(&conn)?;

        // Signal subscriptions, each pumped into the internal queue by its own thread.
        let rules = [
            MatchRule::builder()
                .msg_type(MsgType::Signal)
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .build(),
            MatchRule::builder()
                .msg_type(MsgType::Signal)
                .interface("org.freedesktop.DBus.Properties")?
                .member("PropertiesChanged")?
                .path(MPRIS_PATH)?
                .build(),
            MatchRule::builder()
                .msg_type(MsgType::Signal)
                .interface("org.mpris.MediaPlayer2.Player")?
                .member("Seeked")?
                .build(),
            MatchRule::builder().msg_type(MsgType::Signal).interface("org.kde.StatusNotifierItem")?.build(),
            MatchRule::builder().msg_type(MsgType::Signal).interface(WATCHER_NAME)?.build(),
        ];
        for rule in rules {
            let iter = MessageIterator::for_match_rule(rule.to_owned(), &conn, Some(256))?;
            let tx = tx.clone();
            let w = waker.clone();
            std::thread::spawn(move || {
                for msg in iter.flatten() {
                    if tx.send(Internal::Signal(msg)).is_err() {
                        break;
                    }
                    w.wake();
                }
            });
        }

        let watcher_items = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut worker = Worker {
            conn,
            dbus,
            out,
            internal_tx: tx.clone(),
            waker: waker.clone(),
            last_playing: None,
            media: None,
            art_cache: HashMap::new(),
            art_pending: Vec::new(),
            is_watcher: false,
            watcher_items: watcher_items.clone(),
            items: Vec::new(),
            tray: Vec::new(),
        };

        worker.setup_tray(tx.clone(), waker.clone());
        worker.refresh_media();

        // Main loop: handle signals/commands; refresh media position periodically while
        // playing so interpolation does not drift on players that never emit Seeked.
        loop {
            let timeout = if worker.media.as_ref().map(|m| m.playing).unwrap_or(false) {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(3600)
            };
            let first = match rx.recv_timeout(timeout) {
                Ok(m) => Some(m),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return Ok(()),
            };
            let mut media_dirty = first.is_none();
            let mut tray_dirty_keys: Vec<String> = Vec::new();
            let mut tray_list_dirty = false;
            for msg in first.into_iter().chain(rx.try_iter()) {
                match msg {
                    Internal::Cmd(c) => worker.command(c),
                    Internal::Register(service, sender) => {
                        worker.register_item(&service, &sender);
                        tray_list_dirty = true;
                    }
                    Internal::ArtReady(url, path) => {
                        worker.art_cache.insert(url, path);
                        media_dirty = true;
                    }
                    Internal::Signal(m) => {
                        let hdr = m.header();
                        let member = hdr.member().map(|s| s.to_string()).unwrap_or_default();
                        let iface = hdr.interface().map(|s| s.to_string()).unwrap_or_default();
                        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
                        match (iface.as_str(), member.as_str()) {
                            ("org.freedesktop.DBus", "NameOwnerChanged") => {
                                if let Ok((name, old, new)) = m.body().deserialize::<(String, String, String)>() {
                                    if name.starts_with(MPRIS_PREFIX) {
                                        media_dirty = true;
                                    }
                                    if new.is_empty() && !old.is_empty() {
                                        let before = worker.items.len();
                                        worker.items.retain(|i| i.owner != old && i.service != name);
                                        if worker.items.len() != before {
                                            tray_list_dirty = true;
                                        }
                                    }
                                    if name == WATCHER_NAME && new.is_empty() && !worker.is_watcher {
                                        // The external watcher went away: take over.
                                        worker.setup_tray(worker.internal_tx.clone(), worker.waker.clone());
                                        tray_list_dirty = true;
                                    }
                                }
                            }
                            ("org.freedesktop.DBus.Properties", _) | ("org.mpris.MediaPlayer2.Player", _) => media_dirty = true,
                            ("org.kde.StatusNotifierItem", _) => {
                                for it in worker.items.iter().filter(|i| i.owner == sender) {
                                    tray_dirty_keys.push(format!("{}{}", it.service, it.path));
                                }
                            }
                            (WATCHER_NAME, "StatusNotifierItemRegistered") if !worker.is_watcher => {
                                if let Ok(s) = m.body().deserialize::<String>() {
                                    worker.register_item(&s, "");
                                    tray_list_dirty = true;
                                }
                            }
                            (WATCHER_NAME, "StatusNotifierItemUnregistered") if !worker.is_watcher => {
                                if let Ok(s) = m.body().deserialize::<String>() {
                                    let (svc, path) = split_item(&s, "");
                                    worker.items.retain(|i| !(i.service == svc && i.path == path));
                                    tray_list_dirty = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if media_dirty {
                worker.refresh_media();
            }
            if tray_list_dirty {
                worker.refresh_tray_all();
            } else if !tray_dirty_keys.is_empty() {
                tray_dirty_keys.dedup();
                worker.refresh_tray_keys(&tray_dirty_keys);
            }
        }
    }

    fn emit(&self, ev: DbusEvent) {
        let _ = self.out.send(ev);
        self.waker.wake();
    }

    // ── MPRIS ──────────────────────────────────────────────────────────

    fn refresh_media(&mut self) {
        let names: Vec<String> = self
            .dbus
            .list_names()
            .map(|v| v.into_iter().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        // playerctld only mirrors other players; skip it to avoid duplicates.
        let players: Vec<String> = names
            .into_iter()
            .filter(|n| n.starts_with(MPRIS_PREFIX) && !n.ends_with(".playerctld"))
            .collect();

        let mut states: Vec<(String, String, HashMap<String, OwnedValue>)> = Vec::new();
        for p in &players {
            if let Some(props) = self.get_all(p, MPRIS_PATH, "org.mpris.MediaPlayer2.Player") {
                let status = props.get("PlaybackStatus").and_then(|v| val_str(v)).unwrap_or_default();
                states.push((p.clone(), status, props));
            }
        }

        let chosen = states
            .iter()
            .find(|(_, s, _)| s == "Playing")
            .or_else(|| {
                self.last_playing
                    .as_ref()
                    .and_then(|lp| states.iter().find(|(n, s, _)| n == lp && s != "Stopped"))
            })
            .or_else(|| states.iter().find(|(_, s, _)| s == "Paused"));

        let media = chosen.map(|(name, status, props)| {
            if status == "Playing" {
                self.last_playing = Some(name.clone());
            }
            let meta = props.get("Metadata");
            let title = meta.and_then(|m| dict_get(m, "xesam:title")).and_then(|v| val_str(&v)).unwrap_or_default();
            let artist = meta
                .and_then(|m| dict_get(m, "xesam:artist"))
                .map(|v| val_strs(&v).join(", "))
                .unwrap_or_default();
            let art_url = meta.and_then(|m| dict_get(m, "mpris:artUrl")).and_then(|v| val_str(&v)).unwrap_or_default();
            let length_us = meta.and_then(|m| dict_get(m, "mpris:length")).and_then(|v| val_i64(&v)).unwrap_or(0);
            let position_us = self
                .get_prop(name, MPRIS_PATH, "org.mpris.MediaPlayer2.Player", "Position")
                .and_then(|v| val_i64(&v))
                .unwrap_or(0);
            let art_path = self.resolve_art(&art_url);
            MediaState {
                bus_name: name.clone(),
                title,
                artist,
                art_path,
                playing: status == "Playing",
                position_s: position_us as f64 / 1e6,
                length_s: length_us as f64 / 1e6,
                sampled_at: Instant::now(),
            }
        });

        let changed = match (&self.media, &media) {
            (Some(a), Some(b)) => {
                a.bus_name != b.bus_name
                    || a.title != b.title
                    || a.artist != b.artist
                    || a.art_path != b.art_path
                    || a.playing != b.playing
                    || (a.length_s - b.length_s).abs() > 0.5
                    || {
                        // Report position jumps (seeks), not normal progress.
                        let expected = a.position_s + if a.playing { a.sampled_at.elapsed().as_secs_f64() } else { 0.0 };
                        (expected - b.position_s).abs() > 1.5
                    }
            }
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.media = media.clone();
            self.emit(DbusEvent::Media(media));
        }
    }

    fn resolve_art(&mut self, url: &str) -> Option<PathBuf> {
        if url.is_empty() {
            return None;
        }
        if let Some(p) = url.strip_prefix("file://") {
            let decoded = percent_decode(p);
            return Path::new(&decoded).exists().then(|| PathBuf::from(decoded));
        }
        if let Some(p) = self.art_cache.get(url) {
            return Some(p.clone());
        }
        if (url.starts_with("http://") || url.starts_with("https://")) && !self.art_pending.iter().any(|u| u == url) {
            self.art_pending.push(url.to_string());
            let dir = cache_dir().join("mpris-art");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{:016x}", fnv1a(url)));
            if path.exists() {
                self.art_cache.insert(url.to_string(), path.clone());
                return Some(path);
            }
            let (url_s, tx, w) = (url.to_string(), self.internal_tx.clone(), self.waker.clone());
            std::thread::spawn(move || {
                let tmp = path.with_extension("part");
                let ok = std::process::Command::new("curl")
                    .args(["-sfL", "--max-time", "15", "-o"])
                    .arg(&tmp)
                    .arg(&url_s)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok && std::fs::rename(&tmp, &path).is_ok() {
                    let _ = tx.send(Internal::ArtReady(url_s, path));
                    w.wake();
                }
            });
        }
        None
    }

    // ── System tray ───────────────────────────────────────────────────

    fn setup_tray(&mut self, tx: Sender<Internal>, waker: Waker) {
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        let watcher = Watcher { items: self.watcher_items.clone(), tx, waker_tx: waker };
        let served = self.conn.object_server().at(WATCHER_PATH, watcher).unwrap_or(false);
        let reply = self
            .dbus
            .request_name(WATCHER_NAME.try_into().unwrap(), RequestNameFlags::DoNotQueue.into());
        match reply {
            Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {
                self.is_watcher = true;
                // Ask already-running items to (re)register with the new watcher.
                let _ = self.conn.emit_signal(
                    None::<&str>,
                    WATCHER_PATH,
                    WATCHER_NAME,
                    "StatusNotifierHostRegistered",
                    &(),
                );
                eprintln!("[prism-desktop/dbus] acting as StatusNotifierWatcher");
            }
            _ => {
                if served {
                    let _ = self.conn.object_server().remove::<Watcher, _>(WATCHER_PATH);
                }
                self.is_watcher = false;
                // Host mode: register with the existing watcher and import its items.
                let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
                if let Ok(name) = zbus::names::WellKnownName::try_from(host.as_str()) {
                    let _ = self.dbus.request_name(name, RequestNameFlags::DoNotQueue.into());
                }
                let _ = self.conn.call_method(
                    Some(WATCHER_NAME),
                    WATCHER_PATH,
                    Some(WATCHER_NAME),
                    "RegisterStatusNotifierHost",
                    &(host.as_str(),),
                );
                if let Some(v) = self.get_prop(WATCHER_NAME, WATCHER_PATH, WATCHER_NAME, "RegisteredStatusNotifierItems") {
                    for s in val_strs(&v) {
                        self.register_item(&s, "");
                    }
                }
                self.refresh_tray_all();
            }
        }
    }

    fn register_item(&mut self, service: &str, sender: &str) {
        let (svc, path) = split_item(service, sender);
        if svc.is_empty() || self.items.iter().any(|i| i.service == svc && i.path == path) {
            return;
        }
        let owner = if svc.starts_with(':') {
            svc.clone()
        } else {
            zbus::names::BusName::try_from(svc.as_str())
                .ok()
                .and_then(|n| self.dbus.get_name_owner(n).ok())
                .map(|o| o.to_string())
                .unwrap_or_default()
        };
        self.items.push(ItemRef { service: svc, path, owner });
    }

    fn refresh_tray_all(&mut self) {
        let keys: Vec<String> = self.items.iter().map(|i| format!("{}{}", i.service, i.path)).collect();
        if let Ok(mut w) = self.watcher_items.lock() {
            *w = self.items.iter().map(|i| format!("{}{}", i.service, i.path)).collect();
        }
        self.tray.retain(|t| keys.contains(&t.key));
        self.refresh_tray_keys(&keys);
    }

    fn refresh_tray_keys(&mut self, keys: &[String]) {
        for key in keys {
            let Some(item) = self.items.iter().find(|i| &format!("{}{}", i.service, i.path) == key) else { continue };
            let (svc, path) = (item.service.clone(), item.path.clone());
            let Some(props) = self.get_all(&svc, &path, "org.kde.StatusNotifierItem") else { continue };
            let s = |k: &str| props.get(k).and_then(|v| val_str(v)).unwrap_or_default();
            let icon_name = {
                let status = s("Status");
                let attention = s("AttentionIconName");
                if status == "NeedsAttention" && !attention.is_empty() { attention } else { s("IconName") }
            };
            let theme_path = s("IconThemePath");
            let icon_path = if icon_name.is_empty() {
                None
            } else if icon_name.starts_with('/') && Path::new(&icon_name).exists() {
                Some(PathBuf::from(&icon_name))
            } else {
                // The app's own IconThemePath first, then the gsettings icon theme.
                find_in_dir(&icon_name, &theme_path).or_else(|| crate::icons::lookup(&icon_name, 32))
            };
            let pixmap = if icon_path.is_none() { props.get("IconPixmap").and_then(|v| best_pixmap(v)) } else { None };
            let entry = TrayItem {
                key: key.clone(),
                title: s("Title"),
                status: s("Status"),
                icon_path,
                pixmap,
                item_is_menu: props.get("ItemIsMenu").and_then(|v| val_bool(v)).unwrap_or(false),
                has_menu: !s("Menu").is_empty() && s("Menu") != "/",
                tooltip: props.get("ToolTip").and_then(|v| tooltip_of(v)).filter(|t| !t.0.is_empty() || !t.1.is_empty()).unwrap_or_else(|| (s("Title"), String::new())),
            };
            match self.tray.iter_mut().find(|t| &t.key == key) {
                Some(t) => *t = entry,
                None => self.tray.push(entry),
            }
        }
        // Keep registration order; hide "Passive" items like other trays do.
        let order: Vec<String> = self.items.iter().map(|i| format!("{}{}", i.service, i.path)).collect();
        self.tray.sort_by_key(|t| order.iter().position(|k| k == &t.key).unwrap_or(usize::MAX));
        let visible: Vec<TrayItem> = self.tray.iter().filter(|t| t.status != "Passive").cloned().collect();
        self.emit(DbusEvent::Tray(visible));
    }

    fn command(&mut self, cmd: DbusCmd) {
        match cmd {
            DbusCmd::Media(method) => {
                if let Some(m) = &self.media {
                    let _ = self.conn.call_method(
                        Some(m.bus_name.as_str()),
                        MPRIS_PATH,
                        Some("org.mpris.MediaPlayer2.Player"),
                        method,
                        &(),
                    );
                }
            }
            DbusCmd::TrayActivate { key, x, y } => self.item_call(&key, "Activate", x, y),
            DbusCmd::TraySecondary { key, x, y } => self.item_call(&key, "SecondaryActivate", x, y),
            DbusCmd::TrayMenu { key } => {
                let entries = self.menu_layout(&key);
                self.emit(DbusEvent::Menu { key, entries });
            }
            DbusCmd::RefreshTray => self.refresh_tray_all(),
            DbusCmd::TrayMenuClick { key, id } => {
                if let Some((svc, menu)) = self.menu_target(&key) {
                    let ts = 0u32;
                    let _ = self.conn.call_method(
                        Some(svc.as_str()),
                        menu.as_str(),
                        Some("com.canonical.dbusmenu"),
                        "Event",
                        &(id, "clicked", Value::I32(0), ts),
                    );
                }
            }
        }
    }

    fn item_call(&self, key: &str, method: &str, x: i32, y: i32) {
        if let Some(i) = self.items.iter().find(|i| format!("{}{}", i.service, i.path) == key) {
            let _ = self.conn.call_method(
                Some(i.service.as_str()),
                i.path.as_str(),
                Some("org.kde.StatusNotifierItem"),
                method,
                &(x, y),
            );
        }
    }

    fn menu_target(&self, key: &str) -> Option<(String, String)> {
        let i = self.items.iter().find(|i| format!("{}{}", i.service, i.path) == key)?;
        let menu = self.get_prop(&i.service, &i.path, "org.kde.StatusNotifierItem", "Menu").and_then(|v| val_str(&v))?;
        (!menu.is_empty() && menu != "/").then(|| (i.service.clone(), menu))
    }

    fn menu_layout(&self, key: &str) -> Vec<MenuEntry> {
        let Some((svc, menu)) = self.menu_target(key) else { return Vec::new() };
        let _ = self.conn.call_method(
            Some(svc.as_str()),
            menu.as_str(),
            Some("com.canonical.dbusmenu"),
            "AboutToShow",
            &(0i32,),
        );
        let props: Vec<&str> = Vec::new();
        let reply = self.conn.call_method(
            Some(svc.as_str()),
            menu.as_str(),
            Some("com.canonical.dbusmenu"),
            "GetLayout",
            &(0i32, -1i32, props),
        );
        let mut out = Vec::new();
        if let Ok(msg) = reply {
            if let Ok((_rev, root)) = msg.body().deserialize::<(u32, MenuLayout)>() {
                for child in &root.2 {
                    walk_menu(child, 0, &mut out, false);
                }
            }
        }
        out
    }

    // ── helpers ───────────────────────────────────────────────────────

    fn get_all(&self, dest: &str, path: &str, iface: &str) -> Option<HashMap<String, OwnedValue>> {
        let reply = self
            .conn
            .call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "GetAll", &(iface,))
            .ok()?;
        reply.body().deserialize::<HashMap<String, OwnedValue>>().ok()
    }

    fn get_prop(&self, dest: &str, path: &str, iface: &str, prop: &str) -> Option<OwnedValue> {
        let reply = self
            .conn
            .call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "Get", &(iface, prop))
            .ok()?;
        reply.body().deserialize::<OwnedValue>().ok()
    }
}

/// Item registration strings are either a bus name or an object path (then the sender
/// is the bus name), per the StatusNotifierItem spec. Some send "name/path".
fn split_item(service: &str, sender: &str) -> (String, String) {
    if service.starts_with('/') {
        (sender.to_string(), service.to_string())
    } else if let Some(idx) = service.find('/') {
        (service[..idx].to_string(), service[idx..].to_string())
    } else {
        (service.to_string(), "/StatusNotifierItem".to_string())
    }
}

fn walk_menu(node: &Value, depth: u32, out: &mut Vec<MenuEntry>, is_root: bool) {
    let node = unwrap_variant(node);
    let Value::Structure(st) = node else { return };
    let f = st.fields();
    if f.len() < 3 {
        return;
    }
    let id = val_i64(&f[0]).unwrap_or(0) as i32;
    let prop = |k: &str| -> Option<Value> {
        match unwrap_variant(&f[1]) {
            Value::Dict(d) => d.iter().find(|(kk, _)| val_str(kk).as_deref() == Some(k)).and_then(|(_, v)| unwrap_variant(v).try_clone().ok()),
            _ => None,
        }
    };
    let children: Vec<&Value> = match unwrap_variant(&f[2]) {
        Value::Array(a) => a.inner().iter().collect(),
        _ => Vec::new(),
    };
    if !is_root {
        let visible = prop("visible").and_then(|v| val_bool(&v)).unwrap_or(true);
        if !visible {
            return;
        }
        let separator = prop("type").and_then(|v| val_str(&v)).map(|t| t == "separator").unwrap_or(false);
        let label = prop("label").and_then(|v| val_str(&v)).unwrap_or_default().replace("__", "\u{0}").replace('_', "").replace('\u{0}', "_");
        let toggle = match prop("toggle-type").and_then(|v| val_str(&v)).as_deref() {
            Some("checkmark") | Some("radio") => Some(prop("toggle-state").and_then(|v| val_i64(&v)).unwrap_or(0) == 1),
            _ => None,
        };
        let is_submenu = !children.is_empty() || prop("children-display").and_then(|v| val_str(&v)).as_deref() == Some("submenu");
        out.push(MenuEntry {
            id,
            label,
            enabled: prop("enabled").and_then(|v| val_bool(&v)).unwrap_or(true),
            separator,
            toggle,
            depth,
            is_submenu,
        });
    }
    let child_depth = if is_root { depth } else { depth + 1 };
    for c in children {
        walk_menu(c, child_depth, out, false);
    }
}

fn unwrap_variant<'a, 'b>(v: &'b Value<'a>) -> &'b Value<'a> {
    match v {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

/// SNI ToolTip `(sa(iiay)ss)`: (title, body), markup stripped.
fn tooltip_of(v: &Value) -> Option<(String, String)> {
    let Value::Structure(st) = unwrap_variant(v) else { return None };
    let f = st.fields();
    let title = f.get(2).and_then(val_str).unwrap_or_default();
    let body = f.get(3).and_then(val_str).unwrap_or_default();
    Some((strip_markup(&title), strip_markup(&body)))
}

fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").trim().to_string()
}

fn dict_get(v: &Value, key: &str) -> Option<Value<'static>> {
    match unwrap_variant(v) {
        Value::Dict(d) => d
            .iter()
            .find(|(k, _)| val_str(k).as_deref() == Some(key))
            .and_then(|(_, v)| unwrap_variant(v).try_to_owned().ok().map(|o| o.into())),
        _ => None,
    }
}

fn val_str(v: &Value) -> Option<String> {
    match unwrap_variant(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn val_strs(v: &Value) -> Vec<String> {
    match unwrap_variant(v) {
        Value::Array(a) => a.inner().iter().filter_map(val_str).collect(),
        other => val_str(other).into_iter().collect(),
    }
}

fn val_bool(v: &Value) -> Option<bool> {
    match unwrap_variant(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn val_i64(v: &Value) -> Option<i64> {
    match unwrap_variant(v) {
        Value::I64(n) => Some(*n),
        Value::U64(n) => Some(*n as i64),
        Value::I32(n) => Some(*n as i64),
        Value::U32(n) => Some(*n as i64),
        Value::I16(n) => Some(*n as i64),
        Value::U16(n) => Some(*n as i64),
        Value::U8(n) => Some(*n as i64),
        Value::F64(f) => Some(*f as i64),
        _ => None,
    }
}

/// IconPixmap is a(iiay) in network byte order ARGB; pick the largest and convert to
/// premultiplied native-endian ARGB32 for cairo.
fn best_pixmap(v: &Value) -> Option<(i32, i32, Vec<u8>)> {
    let Value::Array(arr) = unwrap_variant(v) else { return None };
    let mut best: Option<(i32, i32, Vec<u8>)> = None;
    for e in arr.inner() {
        let Value::Structure(s) = unwrap_variant(e) else { continue };
        let f = s.fields();
        if f.len() < 3 {
            continue;
        }
        let (w, h) = (val_i64(&f[0]).unwrap_or(0) as i32, val_i64(&f[1]).unwrap_or(0) as i32);
        let Value::Array(bytes) = unwrap_variant(&f[2]) else { continue };
        let raw: Vec<u8> = bytes.inner().iter().filter_map(|b| if let Value::U8(x) = b { Some(*x) } else { None }).collect();
        if w <= 0 || h <= 0 || raw.len() < (w * h * 4) as usize {
            continue;
        }
        if best.as_ref().map(|b| b.0 * b.1 < w * h).unwrap_or(true) {
            let mut px = Vec::with_capacity(raw.len());
            for c in raw.as_chunks::<4>().0 {
                let (a, r, g, b) = (c[0] as u32, c[1] as u32, c[2] as u32, c[3] as u32);
                let pr = r * a / 255;
                let pg = g * a / 255;
                let pb = b * a / 255;
                let word = (a << 24) | (pr << 16) | (pg << 8) | pb;
                px.extend_from_slice(&word.to_ne_bytes());
            }
            best = Some((w, h, px));
        }
    }
    best
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"))
        .join("prism")
}

/// Look for `name` under an item-provided IconThemePath (any depth up to 4).
fn find_in_dir(name: &str, dir: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = walk(&p, name, depth + 1) {
                    return Some(f);
                }
            } else if p.file_stem().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
        None
    }
    if dir.is_empty() {
        return None;
    }
    walk(Path::new(dir), name, 0)
}
