//! Low battery warnings (laptops): follows UPower's display device (the combined laptop
//! battery) on the system bus and notifies at 20 %, 10 % (critical) and 5 % (critical) while
//! discharging, each once until the charger goes in. With `battery_auto_saver`, the first
//! warning also switches to the power-saver profile; plugging in puts the old one back.
//! Machines without a battery see nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type as MsgType;
use zbus::MatchRule;

const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE: &str = "org.freedesktop.UPower.Device";
/// UPower device states.
const DISCHARGING: u32 = 2;
const EMPTY: u32 = 3;
const LEVELS: [f64; 3] = [20.0, 10.0, 5.0];

static ENABLED: AtomicBool = AtomicBool::new(false);
static AUTO_SAVER: AtomicBool = AtomicBool::new(false);
static START: std::sync::Once = std::sync::Once::new();

pub fn apply(cfg: &prism_common::Config) {
    apply_charge_limit(cfg.battery_charge_limit);
    ENABLED.store(cfg.battery_warnings, Ordering::SeqCst);
    AUTO_SAVER.store(cfg.battery_auto_saver, Ordering::SeqCst);
    if cfg.battery_warnings {
        START.call_once(|| {
            std::thread::Builder::new().name("prism-battery".into()).spawn(run).ok();
        });
    }
}

/// Charge limit: written at start (quietly: without the udev rule it stays as the firmware
/// keeps it) and again when Settings changes it (then asking for a password if needed).
fn apply_charge_limit(limit: u8) {
    static LAST: std::sync::Mutex<Option<u8>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if *last == Some(limit) {
        return;
    }
    let first = last.is_none();
    *last = Some(limit);
    if prism_common::hardware::charge_limit_files().is_empty() || (first && limit == 0) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = prism_common::hardware::set_charge_limit(limit, !first) {
            eprintln!("[prismd/battery] charge limit: {e}");
        }
    });
}

fn run() {
    loop {
        if let Err(e) = watch() {
            eprintln!("[prismd/battery] {e}");
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// (percent, state) of the laptop battery; None without one.
fn read(conn: &Connection) -> Option<(f64, u32)> {
    let p = Proxy::new(conn, UPOWER, DISPLAY, DEVICE).ok()?;
    // Type 2 = battery; a desktop's display device is not one (or not present).
    if p.get_property::<u32>("Type").ok()? != 2 || !p.get_property::<bool>("IsPresent").unwrap_or(false) {
        return None;
    }
    Some((p.get_property::<f64>("Percentage").ok()?, p.get_property::<u32>("State").ok()?))
}

/// The warning for this many percent while discharging (index into LEVELS), if any.
pub fn level_for(percent: f64) -> Option<usize> {
    LEVELS.iter().rposition(|l| percent <= *l)
}

fn watch() -> Result<(), String> {
    let e = |e: zbus::Error| e.to_string();
    let conn = Connection::system().map_err(e)?;
    if read(&conn).is_none() {
        // No battery: nothing to do on this machine.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .map_err(e)?
        .member("PropertiesChanged")
        .map_err(e)?
        .path(DISPLAY)
        .map_err(e)?
        .build();
    let signals = MessageIterator::for_match_rule(rule, &conn, Some(16)).map_err(e)?;
    // Highest warning given since the charger was last in (None: none yet).
    let mut warned: Option<usize> = None;
    let mut saved_profile: Option<String> = None;
    let mut check = |conn: &Connection| {
        let Some((pct, state)) = read(conn) else { return };
        if state != DISCHARGING && state != EMPTY {
            // Charging or full: reset, and put the profile back.
            warned = None;
            if let Some(p) = saved_profile.take() {
                crate::set_power_profile(&p);
            }
            return;
        }
        if !ENABLED.load(Ordering::SeqCst) {
            return;
        }
        let Some(level) = level_for(pct) else { return };
        if warned.is_some_and(|w| w >= level) {
            return;
        }
        warned = Some(level);
        let critical = level > 0;
        let (summary, body) = match level {
            0 => ("Battery low".to_string(), format!("{pct:.0}% left. Plug in soon.")),
            1 => ("Battery very low".to_string(), format!("{pct:.0}% left. Plug in the charger.")),
            _ => ("Battery almost empty".to_string(), format!("{pct:.0}% left. Plug in now or save your work.")),
        };
        let mut body = body;
        if AUTO_SAVER.load(Ordering::SeqCst) && saved_profile.is_none() {
            if let Some(cur) = crate::power_profile().filter(|p| p != "power-saver") {
                crate::set_power_profile("power-saver");
                saved_profile = Some(cur);
                body.push_str(" Power saver is on.");
            }
        }
        notify(&summary, &body, critical);
    };
    check(&conn);
    for msg in signals {
        msg.map_err(e)?;
        check(&conn);
    }
    Err("UPower signals ended".into())
}

fn notify(summary: &str, body: &str, critical: bool) {
    let (s, b) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Ok(c) = Connection::session() {
            let mut hints: std::collections::HashMap<&str, zbus::zvariant::Value> = std::collections::HashMap::new();
            hints.insert("urgency", zbus::zvariant::Value::U8(if critical { 2 } else { 1 }));
            hints.insert("desktop-entry", zbus::zvariant::Value::from("prism-battery"));
            const D: &str = "org.freedesktop.Notifications";
            let icon = if critical { "battery-caution" } else { "battery-low" };
            let _ = c.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Battery", 0u32, icon, s.as_str(), b.as_str(), Vec::<&str>::new(), hints, -1i32));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(level_for(50.0), None);
        assert_eq!(level_for(20.0), Some(0));
        assert_eq!(level_for(15.0), Some(0));
        assert_eq!(level_for(9.0), Some(1));
        assert_eq!(level_for(4.0), Some(2));
    }
}
