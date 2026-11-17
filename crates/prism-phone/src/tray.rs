//! Optional tray icon (config `phone_tray`): a StatusNotifierItem that opens the Phone card on
//! click. Shown by Prism's desktop tray widget and by any other SNI tray. Re-registers when the
//! watcher restarts; dropping the D-Bus connection removes the icon.

use crate::core::Core;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";

/// SNI ToolTip: (icon name, icon pixmaps, title, text).
type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

struct Item {
    core: Weak<Mutex<Core>>,
}

fn open_card() {
    std::thread::spawn(|| {
        use std::io::Write;
        if let Ok(mut s) = std::os::unix::net::UnixStream::connect(prism_common::get_socket_path()) {
            let cmd = prism_common::IpcCommand::PhoneCard("toggle".into());
            let _ = writeln!(s, "{}", serde_json::to_string(&cmd).unwrap_or_default());
        }
    });
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {
        open_card();
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        open_card();
    }
    fn context_menu(&self, _x: i32, _y: i32) {
        open_card();
    }
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(property)]
    fn category(&self) -> &str {
        "Communications"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "prism-phone"
    }
    #[zbus(property)]
    fn title(&self) -> String {
        self.summary().0
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "phone"
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let (title, body) = self.summary();
        ("phone".into(), Vec::new(), title, body)
    }
}

impl Item {
    /// (title, status line) of the primary device.
    fn summary(&self) -> (String, String) {
        let Some(core) = self.core.upgrade() else { return ("Phone".into(), String::new()) };
        let state = core.lock().unwrap().state();
        match state.primary() {
            Some(d) => {
                let mut s = if d.reachable { "Connected".to_string() } else { "Not connected".to_string() };
                if let (true, Some(b)) = (d.reachable, d.battery) {
                    s.push_str(&format!(" · {}%", b.percent));
                }
                (d.name.clone(), s)
            }
            None => ("Phone".into(), "No phone paired".into()),
        }
    }
}

/// Keeps the tray icon matching `phone_tray`; call after every config (re)load.
pub fn sync(core: &Core) {
    static RUNNING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
    let mut running = RUNNING.lock().unwrap();
    match (core.cfg.phone_tray, running.is_some()) {
        (true, false) => {
            let stop = Arc::new(AtomicBool::new(false));
            *running = Some(stop.clone());
            let weak = core.me.clone();
            std::thread::spawn(move || {
                if let Err(e) = serve(weak, stop) {
                    eprintln!("[prism-phoned] tray icon: {e}");
                }
            });
        }
        (false, true) => {
            if let Some(stop) = running.take() {
                stop.store(true, Ordering::SeqCst);
            }
        }
        _ => {}
    }
}

fn serve(core: Weak<Mutex<Core>>, stop: Arc<AtomicBool>) -> zbus::Result<()> {
    let probe = Item { core: core.clone() };
    let conn = zbus::blocking::connection::Builder::session()?.serve_at("/StatusNotifierItem", Item { core })?.build()?;
    let mut last_summary = probe.summary();
    let me = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
    let dbus = zbus::blocking::fdo::DBusProxy::new(&conn)?;
    let mut registered_with = String::new();
    while !stop.load(Ordering::SeqCst) {
        // (Re-)register whenever a (new) watcher owns the name.
        let owner = zbus::names::BusName::try_from(WATCHER).ok().and_then(|n| dbus.get_name_owner(n).ok()).map(|o| o.to_string()).unwrap_or_default();
        if !owner.is_empty() && owner != registered_with {
            let r = conn.call_method(Some(WATCHER), "/StatusNotifierWatcher", Some(WATCHER), "RegisterStatusNotifierItem", &(me.as_str(),));
            if r.is_ok() {
                registered_with = owner;
            }
        }
        // Trays cache the tooltip: tell them when it changes.
        let summary = probe.summary();
        if summary != last_summary {
            last_summary = summary;
            let _ = conn.emit_signal(None::<()>, "/StatusNotifierItem", "org.kde.StatusNotifierItem", "NewToolTip", &());
            let _ = conn.emit_signal(None::<()>, "/StatusNotifierItem", "org.kde.StatusNotifierItem", "NewTitle", &());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(())
}
