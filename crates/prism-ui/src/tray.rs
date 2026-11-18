//! Network and Bluetooth tray icons (replace nm-applet's and blueman's). Each is a
//! StatusNotifierItem with a com.canonical.dbusmenu menu built from live NetworkManager / BlueZ
//! state when it opens: the network menu connects and disconnects VPNs and imports profiles,
//! the Bluetooth one switches the adapter, connects devices and sends files. A click opens the
//! Control Centre on the matching page. Config `network_tray`, `bluetooth_tray`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{OwnedValue, StructureBuilder, Type, Value};

use crate::nc::dbus::Bus;
use crate::nc::model::{BtState, NetState, VpnState};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Network,
    Bluetooth,
}

/// What a menu entry does.
#[derive(Clone)]
enum Act {
    None,
    OpenCentre,
    OpenVpnPage,
    VpnDown(String),
    ImportVpn,
    BtPower(bool),
    BtConnect(String),
    BtDisconnect(String),
    BtSend(String, String),
}

struct Entry {
    label: String,
    enabled: bool,
    separator: bool,
    toggle: Option<bool>,
    children: Vec<Entry>,
    act: Act,
}

fn item(label: impl Into<String>, act: Act) -> Entry {
    Entry { label: label.into(), enabled: true, separator: false, toggle: None, children: Vec::new(), act }
}

fn label(text: impl Into<String>) -> Entry {
    Entry { enabled: false, ..item(text, Act::None) }
}

fn sep() -> Entry {
    Entry { separator: true, ..item("", Act::None) }
}

fn check(text: impl Into<String>, on: bool, act: Act) -> Entry {
    Entry { toggle: Some(on), ..item(text, act) }
}

fn submenu(text: impl Into<String>, children: Vec<Entry>) -> Entry {
    Entry { children, ..item(text, Act::None) }
}

#[derive(Default)]
struct State {
    net: NetState,
    bt: BtState,
}

struct Shared {
    kind: Kind,
    bus: Bus,
    state: Mutex<State>,
    /// Actions of the last layout, by menu id.
    acts: Mutex<HashMap<i32, Act>>,
}

impl Shared {
    fn refresh(&self) {
        let mut s = State::default();
        match self.kind {
            Kind::Network => {
                self.bus.refresh_network(&mut s.net);
                self.bus.refresh_vpns(&mut s.net);
            }
            Kind::Bluetooth => self.bus.refresh_bluetooth(&mut s.bt),
        }
        *self.state.lock().unwrap() = s;
    }

    fn icon(&self) -> &'static str {
        let s = self.state.lock().unwrap();
        match self.kind {
            Kind::Network if s.net.vpns.iter().any(|v| v.state == VpnState::On) => "network-vpn",
            Kind::Network if s.net.ethernet_connected => "network-wired",
            Kind::Network if s.net.active().is_some() => "network-wireless",
            Kind::Network => "network-offline",
            Kind::Bluetooth if !s.bt.powered => "bluetooth-disabled",
            Kind::Bluetooth if s.bt.devices.iter().any(|d| d.connected) => "bluetooth-active",
            Kind::Bluetooth => "bluetooth",
        }
    }

    fn tooltip(&self) -> (String, String) {
        let s = self.state.lock().unwrap();
        match self.kind {
            Kind::Network => {
                let vpn = s.net.vpns.iter().filter(|v| v.state == VpnState::On).map(|v| v.name.clone()).collect::<Vec<_>>();
                let mut body = s.net.status_text();
                if !vpn.is_empty() {
                    body.push_str(&format!("\nVPN: {}", vpn.join(", ")));
                }
                ("Network".into(), body)
            }
            Kind::Bluetooth => {
                let con: Vec<String> = s.bt.devices.iter().filter(|d| d.connected).map(|d| d.name.clone()).collect();
                let body = if !s.bt.powered { "Off".to_string() } else if con.is_empty() { "On".to_string() } else { format!("Connected: {}", con.join(", ")) };
                ("Bluetooth".into(), body)
            }
        }
    }

    fn menu(&self) -> Vec<Entry> {
        let s = self.state.lock().unwrap();
        match self.kind {
            Kind::Network => {
                let mut m = vec![label(s.net.status_text())];
                let on: Vec<_> = s.net.vpns.iter().filter(|v| v.state != VpnState::Off).collect();
                for v in &on {
                    let a = v.active.clone().map(Act::VpnDown).unwrap_or(Act::None);
                    let suffix = if v.state == VpnState::Connecting { "  (connecting\u{2026})" } else { "" };
                    m.push(check(format!("{}{suffix}", v.name), true, a));
                }
                if on.is_empty() {
                    m.push(label("No VPN connected"));
                }
                m.push(sep());
                // The full list lives on the Control Centre's VPN page.
                m.push(item("Open VPN Page", Act::OpenVpnPage));
                m.push(item("Import VPN Profiles\u{2026}", Act::ImportVpn));
                m.push(sep());
                m.push(item("Open Control Centre", Act::OpenCentre));
                m
            }
            Kind::Bluetooth => {
                let bt = &s.bt;
                if !bt.adapter {
                    return vec![label("No Bluetooth adapter"), sep(), item("Open Control Centre", Act::OpenCentre)];
                }
                let mut m = vec![check("Bluetooth", bt.powered, Act::BtPower(!bt.powered))];
                let paired: Vec<_> = bt.devices.iter().filter(|d| d.paired).collect();
                if bt.powered && !paired.is_empty() {
                    m.push(sep());
                    for d in &paired {
                        let a = if d.connected { Act::BtDisconnect(d.mac.clone()) } else { Act::BtConnect(d.mac.clone()) };
                        let name = match (d.connected, d.battery) {
                            (true, Some(b)) => format!("{}  ({b}%)", d.name),
                            _ => d.name.clone(),
                        };
                        m.push(check(name, d.connected, a));
                    }
                    m.push(submenu("Send Files To", paired.iter().map(|d| item(format!("{}\u{2026}", d.name), Act::BtSend(d.mac.clone(), d.name.clone()))).collect()));
                }
                m.push(sep());
                m.push(item("Open Control Centre", Act::OpenCentre));
                m
            }
        }
    }

    fn run(self: &Arc<Self>, act: Act) {
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let r = match act {
                Act::None => Ok(()),
                Act::OpenCentre => {
                    open_centre(me.kind);
                    Ok(())
                }
                Act::OpenVpnPage => {
                    crate::nc::open_on_tab(crate::nc::TAB_VPN);
                    prism_common::ipc::request(&prism_common::IpcCommand::Notif(prism_common::ipc::NotifAction::Open)).map(|_| ())
                }
                Act::VpnDown(a) => me.bus.vpn_down(&a),
                Act::ImportVpn => {
                    match crate::portal::pick_files("Import VPN profiles (.ovpn, WireGuard .conf)") {
                        Ok(files) if !files.is_empty() => {
                            let (msg, err) = crate::status::import_vpns(&files);
                            notify(if err { "VPN import failed" } else { "VPN import" }, &msg);
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("[prism-ui/tray] file dialog: {e}"),
                    }
                    Ok(())
                }
                Act::BtPower(on) => me.bus.set_bluetooth(on),
                Act::BtConnect(mac) => me.bus.connect_bt(&mac),
                Act::BtDisconnect(mac) => me.bus.disconnect_bt(&mac),
                Act::BtSend(mac, name) => {
                    crate::nc::send_files_bluetooth(mac, name);
                    Ok(())
                }
            };
            if let Err(e) = r {
                eprintln!("[prism-ui/tray] {e}");
                notify("Action failed", &e);
            }
            me.refresh();
        });
    }
}

fn notify(summary: &str, body: &str) {
    if let Ok(c) = zbus::blocking::Connection::session() {
        const D: &str = "org.freedesktop.Notifications";
        let hints: HashMap<&str, Value> = HashMap::new();
        let _ = c.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Prism", 0u32, "network-vpn", summary, body, Vec::<&str>::new(), hints, -1i32));
    }
}

fn open_centre(kind: Kind) {
    crate::nc::open_on_tab(if kind == Kind::Network { crate::nc::TAB_NET } else { crate::nc::TAB_BT });
    if let Err(e) = prism_common::ipc::request(&prism_common::IpcCommand::Notif(prism_common::ipc::NotifAction::Open)) {
        eprintln!("[prism-ui/tray] {e}");
    }
}

// ── StatusNotifierItem ───────────────────────────────────────────────────

type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

struct Item(Arc<Shared>);

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {
        self.0.run(Act::OpenCentre);
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.0.run(Act::OpenCentre);
    }
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(property)]
    fn category(&self) -> &str {
        "Hardware"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        if self.0.kind == Kind::Network { "prism-network" } else { "prism-bluetooth" }
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

// ── com.canonical.dbusmenu ───────────────────────────────────────────────

struct Menu(Arc<Shared>);

/// dbusmenu layout node: (id, properties, children as variants of nodes).
#[derive(serde::Serialize, Type)]
struct Layout(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn layout(e: &Entry, id: &mut i32, acts: &mut HashMap<i32, Act>) -> Layout {
    let my = *id;
    *id += 1;
    acts.insert(my, e.act.clone());
    let mut props: HashMap<String, Value<'static>> = HashMap::new();
    if e.separator {
        props.insert("type".into(), Value::from("separator"));
    } else {
        props.insert("label".into(), Value::from(e.label.replace('_', "__")));
    }
    if !e.enabled {
        props.insert("enabled".into(), Value::from(false));
    }
    if let Some(on) = e.toggle {
        props.insert("toggle-type".into(), Value::from("checkmark"));
        props.insert("toggle-state".into(), Value::from(on as i32));
    }
    if !e.children.is_empty() {
        props.insert("children-display".into(), Value::from("submenu"));
    }
    let children: Vec<OwnedValue> = e
        .children
        .iter()
        .map(|c| {
            let Layout(cid, cprops, cchildren) = layout(c, id, acts);
            let st = StructureBuilder::new().add_field(cid).add_field(cprops).add_field(cchildren).build().expect("menu node");
            OwnedValue::try_from(Value::Structure(st)).expect("menu node value")
        })
        .collect();
    let props = props.into_iter().map(|(k, v)| (k, OwnedValue::try_from(v).expect("menu property"))).collect();
    Layout(my, props, children)
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, _parent_id: i32, _depth: i32, _names: Vec<String>) -> (u32, Layout) {
        self.0.refresh();
        let root = item("", Act::None);
        let root = Entry { children: self.0.menu(), ..root };
        let mut acts = HashMap::new();
        let mut id = 0;
        let l = layout(&root, &mut id, &mut acts);
        *self.0.acts.lock().unwrap() = acts;
        static REV: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        (REV.fetch_add(1, Ordering::Relaxed), l)
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

// ── Lifecycle ────────────────────────────────────────────────────────────

static RUNNING: Mutex<[Option<Arc<AtomicBool>>; 2]> = Mutex::new([None, None]);

/// Show or remove the tray icons to match the config.
pub fn apply(cfg: &prism_common::Config) {
    let mut running = RUNNING.lock().unwrap();
    for (slot, kind, want) in [(0, Kind::Network, cfg.network_tray), (1, Kind::Bluetooth, cfg.bluetooth_tray)] {
        match (want, running[slot].is_some()) {
            (true, false) => {
                let stop = Arc::new(AtomicBool::new(false));
                running[slot] = Some(stop.clone());
                std::thread::spawn(move || {
                    if let Err(e) = serve(kind, stop) {
                        eprintln!("[prism-ui/tray] {e}");
                    }
                });
            }
            (false, true) => {
                if let Some(s) = running[slot].take() {
                    s.store(true, Ordering::SeqCst);
                }
            }
            _ => {}
        }
    }
}

fn serve(kind: Kind, stop: Arc<AtomicBool>) -> Result<(), String> {
    let bus = Bus::system().ok_or("no system bus")?;
    let shared = Arc::new(Shared { kind, bus, state: Mutex::new(State::default()), acts: Mutex::new(HashMap::new()) });
    shared.refresh();
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.serve_at(ITEM_PATH, Item(Arc::clone(&shared))))
        .and_then(|b| b.serve_at(MENU_PATH, Menu(Arc::clone(&shared))))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    let me = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
    let dbus = zbus::blocking::fdo::DBusProxy::new(&conn).map_err(|e| e.to_string())?;

    // Wake-ups: NetworkManager / BlueZ signals (debounced) and tray watcher (re)starts. No polling.
    enum Wake {
        State,
        Watcher,
    }
    let (tx, rx) = std::sync::mpsc::channel::<Wake>();
    let (t, st) = (tx.clone(), Arc::clone(&stop));
    let on_change = move || !st.load(Ordering::SeqCst) && t.send(Wake::State).is_ok();
    match kind {
        Kind::Network => Bus::watch_network(on_change),
        Kind::Bluetooth => Bus::watch_bluetooth(on_change),
    }
    {
        let (t, st) = (tx.clone(), Arc::clone(&stop));
        let watcher_changes = zbus::blocking::fdo::DBusProxy::new(&conn)
            .and_then(|d| d.receive_name_owner_changed_with_args(&[(0, WATCHER)]))
            .map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            for _ in watcher_changes {
                if st.load(Ordering::SeqCst) || t.send(Wake::Watcher).is_err() {
                    return;
                }
            }
        });
    }
    let _ = tx.send(Wake::Watcher);
    drop(tx);

    let mut registered_with = String::new();
    let mut last = (shared.icon().to_string(), shared.tooltip());
    while !stop.load(Ordering::SeqCst) {
        // The timeout only notices `stop` (the icon was turned off); it does no work.
        let wake = match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(w) => w,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        match wake {
            Wake::Watcher => {
                let owner = zbus::names::BusName::try_from(WATCHER).ok().and_then(|n| dbus.get_name_owner(n).ok()).map(|o| o.to_string()).unwrap_or_default();
                if !owner.is_empty() && owner != registered_with {
                    let r = conn.call_method(Some(WATCHER), "/StatusNotifierWatcher", Some(WATCHER), "RegisterStatusNotifierItem", &(me.as_str(),));
                    if r.is_ok() {
                        registered_with = owner;
                    }
                }
            }
            Wake::State => {
                shared.refresh();
                let now = (shared.icon().to_string(), shared.tooltip());
                if now != last {
                    last = now;
                    for sig in ["NewIcon", "NewToolTip", "NewTitle"] {
                        let _ = conn.emit_signal(None::<()>, ITEM_PATH, "org.kde.StatusNotifierItem", sig, &());
                    }
                }
            }
        }
    }
    Ok(())
}
