//! Drives and Updates tray icons: StatusNotifierItems that exist only while there is something
//! to show (a removable drive plugged in, package updates available). Each registers with the
//! tray watcher when it becomes relevant and drops its D-Bus connection when it no longer is,
//! which removes it from every tray. Config `drives_tray`, `updates_tray`; the notifications
//! (prismd's drives.rs, desktop/updates.rs) are separate.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, StructureBuilder, Type, Value};

use prism_common::drives::{self, Volume};

use crate::desktop::updates;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Drives,
    Updates,
}

#[derive(Clone)]
enum Act {
    None,
    Open(Volume),
    Eject(Volume),
    Update,
    Recheck,
}

/// What the icon shows right now.
#[derive(Clone, Default, PartialEq)]
struct Snapshot {
    volumes: Vec<Volume>,
    updates: Option<updates::Updates>,
}

impl Snapshot {
    fn visible(&self, kind: Kind) -> bool {
        match kind {
            Kind::Drives => !self.volumes.is_empty(),
            Kind::Updates => self.updates.is_some_and(|u| u.total() > 0),
        }
    }
}

struct Shared {
    kind: Kind,
    snap: Mutex<Snapshot>,
    acts: Mutex<HashMap<i32, Act>>,
}

impl Shared {
    fn icon(&self) -> &'static str {
        match self.kind {
            Kind::Drives => "drive-removable-media",
            Kind::Updates => "system-software-update",
        }
    }

    fn tooltip(&self) -> (String, String) {
        let s = self.snap.lock().unwrap();
        match self.kind {
            Kind::Drives => ("Removable Drives".into(), s.volumes.iter().map(|v| v.title()).collect::<Vec<_>>().join("\n")),
            Kind::Updates => ("Updates".into(), s.updates.map(summary).unwrap_or_default()),
        }
    }

    fn menu(&self) -> Vec<(String, bool, Act, Vec<(String, Act)>)> {
        let s = self.snap.lock().unwrap();
        match self.kind {
            Kind::Drives => s
                .volumes
                .iter()
                .map(|v| {
                    let sub = vec![(if v.mount.is_some() { "Open".to_string() } else { "Mount and Open".to_string() }, Act::Open(v.clone())), ("Eject".to_string(), Act::Eject(v.clone()))];
                    (v.title(), true, Act::None, sub)
                })
                .collect(),
            Kind::Updates => vec![
                (s.updates.map(summary).unwrap_or_default(), false, Act::None, Vec::new()),
                ("Update Now".into(), true, Act::Update, Vec::new()),
                ("Check Again".into(), true, Act::Recheck, Vec::new()),
            ],
        }
    }

    fn run(&self, act: Act) {
        std::thread::spawn(move || {
            let r = match act {
                Act::None => Ok(()),
                Act::Open(v) => Connection::system().map_err(|e| e.to_string()).and_then(|c| drives::mount(&c, &v)).map(|p| {
                    let _ = prism_common::apps::app_command("xdg-open", &[p]).spawn();
                }),
                Act::Eject(v) => Connection::system().map_err(|e| e.to_string()).and_then(|c| drives::eject(&c, &v.drive)).map(|_| notify(&format!("{} can be removed", v.label), "It was unmounted and switched off.")),
                Act::Update => {
                    updates::run_update(&prism_common::Config::load().bar.updates_command);
                    Ok(())
                }
                Act::Recheck => {
                    updates::recheck();
                    Ok(())
                }
            };
            if let Err(e) = r {
                notify("Drive action failed", &e);
            }
        });
    }
}

fn summary(u: updates::Updates) -> String {
    match (u.repo, u.aur) {
        (r, 0) => format!("{r} update{}", if r == 1 { "" } else { "s" }),
        (0, a) => format!("{a} AUR update{}", if a == 1 { "" } else { "s" }),
        (r, a) => format!("{r} updates, {a} from the AUR"),
    }
}

fn notify(summary: &str, body: &str) {
    if let Ok(c) = Connection::session() {
        const D: &str = "org.freedesktop.Notifications";
        let hints: HashMap<&str, Value> = HashMap::new();
        let _ = c.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Drives", 0u32, "drive-removable-media", summary, body, Vec::<&str>::new(), hints, -1i32));
    }
}

// ── StatusNotifierItem + dbusmenu ───────────────────────────────────────

type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

struct Item(Arc<Shared>);

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {
        // Click: open the first drive / run the update.
        let first = self.0.snap.lock().unwrap().volumes.first().cloned();
        match self.0.kind {
            Kind::Drives => {
                if let Some(v) = first {
                    self.0.run(Act::Open(v));
                }
            }
            Kind::Updates => self.0.run(Act::Update),
        }
    }
    fn secondary_activate(&self, x: i32, y: i32) {
        self.activate(x, y);
    }
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(property)]
    fn category(&self) -> &str {
        if self.0.kind == Kind::Drives { "Hardware" } else { "SystemServices" }
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        if self.0.kind == Kind::Drives { "prism-drives" } else { "prism-updates" }
    }
    #[zbus(property)]
    fn title(&self) -> String {
        self.0.tooltip().0
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        self.0.icon()
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::OwnedObjectPath::try_from(MENU_PATH).unwrap()
    }
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let (t, b) = self.0.tooltip();
        (self.0.icon().into(), Vec::new(), t, b)
    }
}

struct Menu(Arc<Shared>);

#[derive(serde::Serialize, Type)]
struct Layout(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn props(label: &str, enabled: bool, submenu: bool) -> HashMap<String, OwnedValue> {
    let mut p: HashMap<String, Value<'static>> = HashMap::new();
    p.insert("label".into(), Value::from(label.replace('_', "__")));
    if !enabled {
        p.insert("enabled".into(), Value::from(false));
    }
    if submenu {
        p.insert("children-display".into(), Value::from("submenu"));
    }
    p.into_iter().filter_map(|(k, v)| OwnedValue::try_from(v).ok().map(|v| (k, v))).collect()
}

fn node(id: i32, p: HashMap<String, OwnedValue>, children: Vec<OwnedValue>) -> OwnedValue {
    let st = StructureBuilder::new().add_field(id).add_field(p).add_field(children).build().expect("menu node");
    OwnedValue::try_from(Value::Structure(st)).expect("menu node value")
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, _parent_id: i32, _depth: i32, _names: Vec<String>) -> (u32, Layout) {
        let mut acts = HashMap::new();
        let mut id = 1;
        let mut top = Vec::new();
        for (label, enabled, act, sub) in self.0.menu() {
            let my = id;
            id += 1;
            acts.insert(my, act);
            let children: Vec<OwnedValue> = sub
                .into_iter()
                .map(|(l, a)| {
                    let c = id;
                    id += 1;
                    acts.insert(c, a);
                    node(c, props(&l, true, false), Vec::new())
                })
                .collect();
            let has_sub = !children.is_empty();
            top.push(node(my, props(&label, enabled, has_sub), children));
        }
        *self.0.acts.lock().unwrap() = acts;
        static REV: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        (REV.fetch_add(1, Ordering::Relaxed), Layout(0, props("", true, true), top))
    }

    fn get_group_properties(&self, _ids: Vec<i32>, _names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        Vec::new()
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _ts: u32) {
        if event_id == "clicked" {
            if let Some(a) = self.0.acts.lock().unwrap().get(&id).cloned() {
                self.0.run(a);
            }
        }
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
}

// ── Lifecycle ───────────────────────────────────────────────────────────

static WANT_DRIVES: AtomicBool = AtomicBool::new(false);
static WANT_UPDATES: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();

pub fn apply(cfg: &prism_common::Config) {
    WANT_DRIVES.store(cfg.drives_tray, Ordering::SeqCst);
    WANT_UPDATES.store(cfg.updates_tray, Ordering::SeqCst);
    if cfg.drives_tray || cfg.updates_tray {
        START.call_once(|| {
            for kind in [Kind::Drives, Kind::Updates] {
                std::thread::Builder::new().name("prism-status-tray".into()).spawn(move || run(kind)).ok();
            }
        });
    }
}

fn wanted(kind: Kind) -> bool {
    match kind {
        Kind::Drives => WANT_DRIVES.load(Ordering::SeqCst),
        Kind::Updates => WANT_UPDATES.load(Ordering::SeqCst),
    }
}

/// Current state. Drives: UDisks2 on the system bus; updates: the updates worker.
fn snapshot(kind: Kind, system: Option<&Connection>) -> Snapshot {
    match kind {
        Kind::Drives => Snapshot { volumes: system.and_then(|c| drives::list(c).ok()).unwrap_or_default(), updates: None },
        Kind::Updates => Snapshot { volumes: Vec::new(), updates: updates::snapshot() },
    }
}

/// UDisks2 changes (drives plugged, mounted, removed) wake the drives icon; no polling there.
fn watch_udisks(tx: std::sync::mpsc::Sender<()>) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::system() else { return };
        let Ok(rule) = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).sender(drives::UDISKS).map(|b| b.build()) else { return };
        let Ok(iter) = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64)) else { return };
        for _ in iter.flatten() {
            if tx.send(()).is_err() {
                return;
            }
        }
    });
}

fn run(kind: Kind) {
    let shared = Arc::new(Shared { kind, snap: Mutex::new(Snapshot::default()), acts: Mutex::new(HashMap::new()) });
    let system = Connection::system().ok();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    if kind == Kind::Drives {
        watch_udisks(tx.clone());
    }
    let mut shown: Option<Connection> = None;
    let mut seen_gen = u64::MAX;
    let mut registered_with = String::new();
    loop {
        // Drives wake on UDisks signals (coalesced); updates check the worker's generation.
        let wait = if kind == Kind::Drives { Duration::from_secs(30) } else { Duration::from_secs(5) };
        if rx.recv_timeout(wait).is_ok() {
            std::thread::sleep(Duration::from_millis(400));
            while rx.try_recv().is_ok() {}
        }
        if kind == Kind::Updates {
            let g = updates::generation();
            if g == seen_gen && shown.is_some() == shared.snap.lock().unwrap().visible(kind) {
                continue;
            }
            seen_gen = g;
        }
        let snap = snapshot(kind, system.as_ref());
        let visible = wanted(kind) && snap.visible(kind);
        let changed = *shared.snap.lock().unwrap() != snap;
        *shared.snap.lock().unwrap() = snap;
        match (visible, shown.is_some()) {
            (true, false) => match show(&shared) {
                Ok(c) => {
                    registered_with = register(&c);
                    shown = Some(c);
                }
                Err(e) => eprintln!("[prism-ui/tray] {e}"),
            },
            // Dropping the connection takes the icon off every tray.
            (false, true) => {
                shown = None;
                registered_with.clear();
            }
            (true, true) => {
                let c = shown.as_ref().unwrap();
                // The tray watcher may have restarted: register again.
                let owner = watcher_owner(c);
                if !owner.is_empty() && owner != registered_with {
                    registered_with = register(c);
                }
                if changed {
                    for sig in ["NewIcon", "NewToolTip", "NewTitle"] {
                        let _ = c.emit_signal(None::<()>, ITEM_PATH, "org.kde.StatusNotifierItem", sig, &());
                    }
                }
            }
            _ => {}
        }
    }
}

fn show(shared: &Arc<Shared>) -> Result<Connection, String> {
    zbus::blocking::connection::Builder::session()
        .and_then(|b| b.serve_at(ITEM_PATH, Item(Arc::clone(shared))))
        .and_then(|b| b.serve_at(MENU_PATH, Menu(Arc::clone(shared))))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())
}

fn watcher_owner(c: &Connection) -> String {
    zbus::blocking::fdo::DBusProxy::new(c)
        .ok()
        .and_then(|d| zbus::names::BusName::try_from(WATCHER).ok().and_then(|n| d.get_name_owner(n).ok()))
        .map(|o| o.to_string())
        .unwrap_or_default()
}

/// Register with the tray watcher; returns its owner when that worked.
fn register(c: &Connection) -> String {
    let me = c.unique_name().map(|n| n.to_string()).unwrap_or_default();
    match c.call_method(Some(WATCHER), "/StatusNotifierWatcher", Some(WATCHER), "RegisterStatusNotifierItem", &(me.as_str(),)) {
        Ok(_) => watcher_owner(c),
        Err(_) => String::new(),
    }
}
