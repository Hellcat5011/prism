//! Battery levels of connected devices (headphones, speakers, controllers, mice, ...) for the
//! desktop Batteries widget. UPower already gathers them from BlueZ and the kernel: one thread
//! reads its devices, refreshes on its signals and wakes the main loop when something changed.

use crate::waker::Waker;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, Once};
use std::time::Duration;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::MatchRule;

const SERVICE: &str = "org.freedesktop.UPower";
const ROOT: &str = "/org/freedesktop/UPower";
const DEVICE: &str = "org.freedesktop.UPower.Device";
/// Not every device announces each change.
const REFRESH: Duration = Duration::from_secs(30);
/// At or below this level a device that is not charging shows as low.
pub const LOW_PERCENT: u8 = 20;

const ICON_HEADPHONES: &str = "M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3";
const ICON_SPEAKER: &str = "M6 2h12a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Z M12 6h.01 M16 14a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z M12 14h.01";
const ICON_CONTROLLER: &str = "M6 11h4 M8 9v4 M15 12h.01 M18 10h.01 M17.32 5H6.68a4 4 0 0 0-3.978 3.59c-.006.052-.01.101-.017.152C2.604 9.416 2 14.456 2 16a3 3 0 0 0 3 3c1 0 1.5-.5 2-1l1.414-1.414A2 2 0 0 1 9.828 16h4.344a2 2 0 0 1 1.414.586L17 18c.5.5 1 1 2 1a3 3 0 0 0 3-3c0-1.545-.604-6.584-.685-7.258-.007-.05-.011-.1-.017-.151A4 4 0 0 0 17.32 5Z";
const ICON_MOUSE: &str = "M12 2a6 6 0 0 0-6 6v8a6 6 0 0 0 12 0V8a6 6 0 0 0-6-6Z M12 6v4";
const ICON_KEYBOARD: &str = "M4 5h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2Z M6 9h.01 M10 9h.01 M14 9h.01 M18 9h.01 M6 12h.01 M10 12h.01 M14 12h.01 M18 12h.01 M8 15.5h8";
const ICON_PHONE: &str = "M7 2h10a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Z M12 18h.01";
const ICON_TABLET: &str = "M6 2h12a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Z M12 18h.01";
const ICON_WATCH: &str = "M18 12a6 6 0 1 1-12 0 6 6 0 0 1 12 0Z M12 10v2l1 1 M9 18l.4 3a1 1 0 0 0 1 1h3.2a1 1 0 0 0 1-1l.4-3 M9 6l.4-3a1 1 0 0 1 1-1h3.2a1 1 0 0 1 1 1l.4 3";
const ICON_BATTERY: &str = "M4 7h13a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2Z M22 11v2";
/// Filled, unlike the other icons.
pub const ICON_CHARGING: &str = "M13 2 4 14h7l-1 8 9-12h-7l1-8Z";

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceBattery {
    pub name: String,
    pub icon: &'static str,
    pub percent: u8,
    pub charging: bool,
}

impl DeviceBattery {
    pub fn low(&self) -> bool {
        self.percent <= LOW_PERCENT && !self.charging
    }
}

/// The properties of one UPower device that matter here.
#[derive(Clone, Debug, Default)]
struct Raw {
    kind: u32,
    power_supply: bool,
    present: bool,
    state: u32,
    percentage: f64,
    model: String,
    vendor: String,
    serial: String,
}

/// Icon and fallback name of a UPower device type.
fn describe(kind: u32) -> (&'static str, &'static str) {
    match kind {
        5 => (ICON_MOUSE, "Mouse"),
        6 => (ICON_KEYBOARD, "Keyboard"),
        8 => (ICON_PHONE, "Phone"),
        9 => (ICON_SPEAKER, "Media Player"),
        10 => (ICON_TABLET, "Tablet"),
        12 => (ICON_CONTROLLER, "Controller"),
        14 => (ICON_MOUSE, "Touchpad"),
        17 => (ICON_HEADPHONES, "Headset"),
        18 => (ICON_SPEAKER, "Speaker"),
        19 => (ICON_HEADPHONES, "Headphones"),
        21 => (ICON_SPEAKER, "Audio Device"),
        22 => (ICON_CONTROLLER, "Remote"),
        26 => (ICON_WATCH, "Wearable"),
        _ => (ICON_BATTERY, "Device"),
    }
}

/// None for what is no connected battery-powered device: mains, this computer's own battery
/// or UPS, and devices that report no level.
fn device(raw: &Raw) -> Option<DeviceBattery> {
    let publishes = raw.percentage > 0.0 || raw.state != 0;
    if raw.kind == 1 || raw.power_supply || !raw.present || !publishes {
        return None;
    }
    let (icon, fallback) = describe(raw.kind);
    let name = [&raw.model, &raw.vendor].into_iter().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or(fallback).to_string();
    Some(DeviceBattery { name, icon, percent: raw.percentage.round().clamp(0.0, 100.0) as u8, charging: raw.state == 1 })
}

/// The devices to show, in a stable order. UPower can list one device twice (through BlueZ and
/// through the kernel); both then carry the same serial.
fn collect(raws: &[Raw]) -> Vec<DeviceBattery> {
    let mut serials = HashSet::new();
    let mut out: Vec<DeviceBattery> = raws
        .iter()
        .filter(|r| {
            let serial = r.serial.trim().to_ascii_lowercase();
            device(r).is_some() && (serial.is_empty() || serials.insert(serial))
        })
        .filter_map(device)
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.percent.cmp(&b.percent)));
    out
}

/// The `n` devices to show when not all fit: the emptiest ones, in their usual order.
pub fn fitting(devices: &[DeviceBattery], n: usize) -> Vec<&DeviceBattery> {
    let mut places: Vec<usize> = (0..devices.len()).collect();
    places.sort_by_key(|&i| devices[i].percent);
    places.truncate(n);
    places.sort_unstable();
    places.into_iter().map(|i| &devices[i]).collect()
}

fn read_device(conn: &Connection, path: &OwnedObjectPath) -> zbus::Result<Raw> {
    let reply = conn.call_method(Some(SERVICE), path.as_str(), Some("org.freedesktop.DBus.Properties"), "GetAll", &(DEVICE,))?;
    let props: HashMap<String, OwnedValue> = reply.body().deserialize()?;
    let text = |key: &str| props.get(key).and_then(|v| v.downcast_ref::<&str>().ok()).unwrap_or_default().to_string();
    let number = |key: &str| props.get(key).and_then(|v| v.downcast_ref::<u32>().ok()).unwrap_or(0);
    let flag = |key: &str| props.get(key).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false);
    Ok(Raw {
        kind: number("Type"),
        power_supply: flag("PowerSupply"),
        present: flag("IsPresent"),
        state: number("State"),
        percentage: props.get("Percentage").and_then(|v| v.downcast_ref::<f64>().ok()).unwrap_or(0.0),
        model: text("Model"),
        vendor: text("Vendor"),
        serial: text("Serial"),
    })
}

fn read_devices(conn: &Connection) -> zbus::Result<Vec<DeviceBattery>> {
    let reply = conn.call_method(Some(SERVICE), ROOT, Some(SERVICE), "EnumerateDevices", &())?;
    let paths: Vec<OwnedObjectPath> = reply.body().deserialize()?;
    // A device can go away between the two calls.
    let raws: Vec<Raw> = paths.iter().filter_map(|p| read_device(conn, p).ok()).collect();
    Ok(collect(&raws))
}

static STATE: Mutex<(u64, Option<Vec<DeviceBattery>>)> = Mutex::new((0, None));
static START: Once = Once::new();

fn publish(devices: Option<Vec<DeviceBattery>>, waker: &Waker) {
    let mut st = STATE.lock().unwrap();
    if st.1 != devices {
        st.0 += 1;
        st.1 = devices;
        drop(st);
        waker.wake();
    }
}

/// Runs until the bus connection cannot be set up.
fn watch(waker: &Waker) -> zbus::Result<()> {
    let conn = Connection::system()?;
    let rules = [
        MatchRule::builder().msg_type(MsgType::Signal).interface(SERVICE)?.path(ROOT)?.build(),
        MatchRule::builder().msg_type(MsgType::Signal).interface("org.freedesktop.DBus.Properties")?.member("PropertiesChanged")?.path_namespace(ROOT)?.build(),
        // UPower started or stopped.
        MatchRule::builder().msg_type(MsgType::Signal).interface("org.freedesktop.DBus")?.member("NameOwnerChanged")?.add_arg(SERVICE)?.build(),
    ];
    let (tx, rx) = mpsc::channel();
    for rule in rules {
        let signals = MessageIterator::for_match_rule(rule, &conn, Some(64))?;
        let tx = tx.clone();
        std::thread::spawn(move || {
            for _ in signals {
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    loop {
        publish(read_devices(&conn).ok(), waker);
        match rx.recv_timeout(REFRESH) {
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) => {
                // A device coming or going sends several signals at once.
                std::thread::sleep(Duration::from_millis(200));
                while rx.try_recv().is_ok() {}
            }
        }
    }
}

pub fn start(waker: Waker) {
    START.call_once(|| {
        std::thread::spawn(move || loop {
            if let Err(e) = watch(&waker) {
                eprintln!("[prism-ui] device batteries: {e}");
            }
            publish(None, &waker);
            std::thread::sleep(Duration::from_secs(10));
        });
    });
}

/// The connected devices with a battery; None while UPower cannot be reached.
pub fn snapshot() -> Option<Vec<DeviceBattery>> {
    STATE.lock().unwrap().1.clone()
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: u32, model: &str, percentage: f64) -> Raw {
        Raw { kind, present: true, state: 2, percentage, model: model.into(), ..Raw::default() }
    }

    #[test]
    fn the_computer_itself_is_left_out() {
        assert!(device(&raw(1, "Mains", 100.0)).is_none());
        assert!(device(&Raw { power_supply: true, ..raw(2, "Laptop", 80.0) }).is_none());
        assert!(device(&raw(19, "Buds", 80.0)).is_some());
    }

    #[test]
    fn absent_and_silent_devices_are_left_out() {
        assert!(device(&Raw { present: false, ..raw(12, "Pad", 50.0) }).is_none());
        assert!(device(&Raw { state: 0, ..raw(12, "Pad", 0.0) }).is_none());
        // Really empty, and saying so.
        assert_eq!(device(&raw(12, "Pad", 0.0)).unwrap().percent, 0);
    }

    #[test]
    fn a_device_without_a_model_is_named_after_its_type() {
        assert_eq!(device(&raw(18, " ", 40.0)).unwrap().name, "Speaker");
        assert_eq!(device(&Raw { vendor: "Acme".into(), ..raw(18, "", 40.0) }).unwrap().name, "Acme");
    }

    #[test]
    fn one_device_listed_twice_shows_once() {
        let twin = |s: &str| Raw { serial: s.into(), ..raw(12, "Pad", 50.0) };
        assert_eq!(collect(&[twin("AA:BB"), twin("aa:bb")]).len(), 1);
        // Two of the same model are two devices.
        assert_eq!(collect(&[twin("AA:BB"), twin("CC:DD")]).len(), 2);
        assert_eq!(collect(&[twin(""), twin("")]).len(), 2);
    }

    #[test]
    fn devices_keep_their_order_as_levels_change() {
        let names = |v: Vec<DeviceBattery>| v.into_iter().map(|d| d.name).collect::<Vec<_>>();
        assert_eq!(names(collect(&[raw(19, "zeta", 10.0), raw(12, "Alpha", 90.0)])), ["Alpha", "zeta"]);
    }

    #[test]
    fn the_emptiest_devices_stay_visible_when_not_all_fit() {
        let all = collect(&[raw(19, "A", 90.0), raw(19, "B", 10.0), raw(19, "C", 50.0)]);
        let names: Vec<&str> = fitting(&all, 2).iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["B", "C"]);
        assert_eq!(fitting(&all, 3).len(), 3);
    }

    #[test]
    fn only_a_draining_battery_is_low() {
        let at = |state, p| device(&Raw { state, ..raw(19, "Buds", p) }).unwrap().low();
        assert!(at(2, 20.0));
        assert!(!at(2, 21.0));
        assert!(!at(1, 5.0));
    }
}
