//! NetworkManager and BlueZ over the system D-Bus for the control centre (replaces the nmcli /
//! bluetoothctl scraping). Fills the same `NetState` / `BtState` the pages draw. Runs on the
//! status worker thread (blocking zbus calls); a watcher thread turns PropertiesChanged /
//! InterfacesAdded signals into refresh requests so the pages update live.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

use super::model::{BtDevice, BtState, DevDetails, Net, NetState};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const BLUEZ: &str = "org.bluez";
const AGENT_PATH: &str = "/org/prism/BluetoothAgent";

type Props = HashMap<String, OwnedValue>;

pub struct Bus {
    conn: Connection,
}

// ── Small value helpers ──────────────────────────────────────────────────

fn get_all(conn: &Connection, dest: &str, path: &str, iface: &str) -> Props {
    conn.call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "GetAll", &(iface,))
        .ok()
        .and_then(|m| m.body().deserialize::<Props>().ok())
        .unwrap_or_default()
}

fn set_prop(conn: &Connection, dest: &str, path: &str, iface: &str, name: &str, value: Value<'_>) -> Result<(), String> {
    conn.call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "Set", &(iface, name, value))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn u32_of(p: &Props, k: &str) -> u32 {
    p.get(k).and_then(|v| u32::try_from(&**v).ok()).unwrap_or(0)
}

fn bool_of(p: &Props, k: &str) -> bool {
    p.get(k).and_then(|v| bool::try_from(&**v).ok()).unwrap_or(false)
}

fn str_of(p: &Props, k: &str) -> String {
    p.get(k).and_then(|v| <&str>::try_from(&**v).ok()).map(String::from).unwrap_or_default()
}

fn path_of(p: &Props, k: &str) -> Option<String> {
    match p.get(k).map(|v| &**v) {
        Some(Value::ObjectPath(o)) if o.as_str() != "/" => Some(o.to_string()),
        _ => None,
    }
}

fn paths_of(p: &Props, k: &str) -> Vec<String> {
    match p.get(k).map(|v| &**v) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| match x {
                Value::ObjectPath(o) => Some(o.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn bytes_of(v: &Value) -> Vec<u8> {
    match v {
        Value::Array(a) => a.iter().filter_map(|x| u8::try_from(x).ok()).collect(),
        _ => Vec::new(),
    }
}

/// aa{sv} property as a list of dicts.
fn dicts_of(p: &Props, k: &str) -> Vec<HashMap<String, String>> {
    let Some(Value::Array(a)) = p.get(k).map(|v| &**v) else { return Vec::new() };
    a.iter()
        .filter_map(|d| match d {
            Value::Dict(dict) => {
                let map: HashMap<String, Value> = HashMap::try_from(dict.try_clone().ok()?).ok()?;
                Some(
                    map.into_iter()
                        .map(|(k, v)| {
                            let s = match &v {
                                Value::Str(s) => s.to_string(),
                                Value::U32(n) => n.to_string(),
                                Value::U8(n) => n.to_string(),
                                other => format!("{other}"),
                            };
                            (k, s)
                        })
                        .collect(),
                )
            }
            _ => None,
        })
        .collect()
}

/// IPv6 address bytes as text (RFC 5952 compression via std).
fn ipv6_text(b: &[u8]) -> Option<String> {
    let arr: [u8; 16] = b.try_into().ok()?;
    Some(std::net::Ipv6Addr::from(arr).to_string())
}

impl Bus {
    pub fn system() -> Option<Bus> {
        Connection::system().ok().map(|conn| Bus { conn })
    }

    // ── Network ─────────────────────────────────────────────────────

    fn nm_devices(&self) -> Vec<(String, Props)> {
        let nm = get_all(&self.conn, NM, NM_PATH, NM);
        paths_of(&nm, "Devices")
            .into_iter()
            .map(|p| {
                let props = get_all(&self.conn, NM, &p, "org.freedesktop.NetworkManager.Device");
                (p, props)
            })
            .collect()
    }

    /// Wi-Fi device object path, if any.
    fn wifi_device(&self) -> Option<String> {
        self.nm_devices().into_iter().find(|(_, p)| u32_of(p, "DeviceType") == 2).map(|(path, _)| path)
    }

    /// Saved Wi-Fi profiles: SSID text to settings path.
    fn known_wifi(&self) -> Vec<(String, String)> {
        let Ok(msg) = self.conn.call_method(Some(NM), NM_SETTINGS, Some("org.freedesktop.NetworkManager.Settings"), "ListConnections", &())
        else {
            return Vec::new();
        };
        let paths: Vec<OwnedObjectPath> = msg.body().deserialize().unwrap_or_default();
        let mut out = Vec::new();
        for p in paths {
            let Ok(m) = self.conn.call_method(Some(NM), p.as_str(), Some("org.freedesktop.NetworkManager.Settings.Connection"), "GetSettings", &())
            else {
                continue;
            };
            let Ok(settings) = m.body().deserialize::<HashMap<String, Props>>() else { continue };
            let is_wifi = settings.get("connection").map(|c| str_of(c, "type") == "802-11-wireless").unwrap_or(false);
            if !is_wifi {
                continue;
            }
            let ssid = settings
                .get("802-11-wireless")
                .and_then(|w| w.get("ssid"))
                .map(|v| String::from_utf8_lossy(&bytes_of(v)).into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| settings.get("connection").map(|c| str_of(c, "id")).unwrap_or_default());
            out.push((ssid, p.to_string()));
        }
        out
    }

    fn details(&self, dev: &Props) -> DevDetails {
        let mut d = DevDetails { mac: str_of(dev, "HwAddress"), driver: str_of(dev, "Driver"), ..Default::default() };
        if let Some(ac) = path_of(dev, "ActiveConnection") {
            d.connection = str_of(&get_all(&self.conn, NM, &ac, "org.freedesktop.NetworkManager.Connection.Active"), "Id");
        }
        if let Some(p4) = path_of(dev, "Ip4Config") {
            let c = get_all(&self.conn, NM, &p4, "org.freedesktop.NetworkManager.IP4Config");
            d.ip4 = dicts_of(&c, "AddressData").iter().filter_map(|a| Some(format!("{}/{}", a.get("address")?, a.get("prefix")?))).collect();
            d.gateway = str_of(&c, "Gateway");
            d.dns = dicts_of(&c, "NameserverData").iter().filter_map(|a| a.get("address").cloned()).collect();
        }
        if let Some(p6) = path_of(dev, "Ip6Config") {
            let c = get_all(&self.conn, NM, &p6, "org.freedesktop.NetworkManager.IP6Config");
            d.ip6 = dicts_of(&c, "AddressData").iter().filter_map(|a| Some(format!("{}/{}", a.get("address")?, a.get("prefix")?))).collect();
            d.ip6.sort_by_key(|a| a.to_lowercase().starts_with("fe80"));
            d.gateway6 = str_of(&c, "Gateway");
            if let Some(Value::Array(a)) = c.get("Nameservers").map(|v| &**v) {
                d.dns6 = a.iter().filter_map(|x| ipv6_text(&bytes_of(x))).collect();
            }
        }
        d
    }

    /// Fill radio, devices, access points, saved profiles and connection details.
    pub fn refresh_network(&self, n: &mut NetState) {
        let nm = get_all(&self.conn, NM, NM_PATH, NM);
        n.wifi_enabled = bool_of(&nm, "WirelessEnabled");
        n.wifi_device.clear();
        n.ethernet_device.clear();
        n.ethernet_connected = false;
        n.networks.clear();
        n.eth = DevDetails::default();
        n.wifi = DevDetails::default();
        n.link_speed = 0;
        let known = self.known_wifi();
        for (path, dev) in self.nm_devices() {
            let iface = str_of(&dev, "Interface");
            let activated = u32_of(&dev, "State") == 100;
            match u32_of(&dev, "DeviceType") {
                1 if activated && n.ethernet_device.is_empty() => {
                    n.ethernet_device = iface;
                    n.ethernet_connected = true;
                    n.link_speed = u32_of(&get_all(&self.conn, NM, &path, "org.freedesktop.NetworkManager.Device.Wired"), "Speed");
                    n.eth = self.details(&dev);
                }
                2 if n.wifi_device.is_empty() => {
                    n.wifi_device = iface;
                    let w = get_all(&self.conn, NM, &path, "org.freedesktop.NetworkManager.Device.Wireless");
                    let active_ap = path_of(&w, "ActiveAccessPoint");
                    let mut nets: Vec<Net> = Vec::new();
                    for ap in paths_of(&w, "AccessPoints") {
                        let a = get_all(&self.conn, NM, &ap, "org.freedesktop.NetworkManager.AccessPoint");
                        let ssid = a.get("Ssid").map(|v| String::from_utf8_lossy(&bytes_of(v)).into_owned()).unwrap_or_default();
                        if ssid.is_empty() {
                            continue;
                        }
                        let net = Net {
                            security: security_label(u32_of(&a, "Flags"), u32_of(&a, "WpaFlags"), u32_of(&a, "RsnFlags")),
                            secure: u32_of(&a, "Flags") & 1 != 0 || u32_of(&a, "WpaFlags") != 0 || u32_of(&a, "RsnFlags") != 0,
                            signal: a.get("Strength").and_then(|v| u8::try_from(&**v).ok()).unwrap_or(0),
                            rate: format!("{} Mbit/s", u32_of(&a, "MaxBitrate") / 1000),
                            active: active_ap.as_deref() == Some(ap.as_str()),
                            known: known.iter().any(|(s, _)| *s == ssid),
                            ssid,
                        };
                        match nets.iter_mut().find(|b| b.ssid == net.ssid) {
                            Some(b) => {
                                if net.signal > b.signal {
                                    b.signal = net.signal;
                                    b.rate = net.rate.clone();
                                }
                                b.active |= net.active;
                            }
                            None => nets.push(net),
                        }
                    }
                    nets.sort_by(|a, b| a.group().cmp(&b.group()).then(b.signal.cmp(&a.signal)));
                    n.networks = nets;
                    if activated {
                        n.wifi = self.details(&dev);
                    }
                }
                _ => {}
            }
        }
        if !n.wifi_enabled {
            n.networks.clear();
        }
    }

    pub fn set_wifi(&self, on: bool) -> Result<(), String> {
        set_prop(&self.conn, NM, NM_PATH, NM, "WirelessEnabled", Value::from(on))
    }

    pub fn scan_wifi(&self) {
        if let Some(dev) = self.wifi_device() {
            let opts: HashMap<&str, Value> = HashMap::new();
            let _ = self.conn.call_method(Some(NM), dev.as_str(), Some("org.freedesktop.NetworkManager.Device.Wireless"), "RequestScan", &(opts,));
        }
    }

    pub fn disconnect_wifi(&self) {
        if let Some(dev) = self.wifi_device() {
            let _ = self.conn.call_method(Some(NM), dev.as_str(), Some("org.freedesktop.NetworkManager.Device"), "Disconnect", &());
        }
    }

    pub fn forget_wifi(&self, ssid: &str) {
        for (s, path) in self.known_wifi() {
            if s == ssid {
                let _ = self.conn.call_method(Some(NM), path.as_str(), Some("org.freedesktop.NetworkManager.Settings.Connection"), "Delete", &());
            }
        }
    }

    /// Connect to `ssid` (a saved profile, or a new one with `password`) and wait up to 25 s for
    /// the result. Err carries the message the row shows. A new profile that fails is removed.
    pub fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        let dev = self.wifi_device().ok_or("No Wi-Fi device")?;
        let devp = zbus::zvariant::ObjectPath::try_from(dev.as_str()).map_err(|e| e.to_string())?;
        let root = zbus::zvariant::ObjectPath::from_static_str_unchecked("/");
        let mut created: Option<String> = None;
        if let Some((_, conn_path)) = self.known_wifi().into_iter().find(|(s, _)| s == ssid) {
            let cp = zbus::zvariant::ObjectPath::try_from(conn_path.as_str()).map_err(|e| e.to_string())?;
            self.conn.call_method(Some(NM), NM_PATH, Some(NM), "ActivateConnection", &(cp, &devp, &root)).map_err(|e| short_error(&e))?;
        } else {
            let w = get_all(&self.conn, NM, &dev, "org.freedesktop.NetworkManager.Device.Wireless");
            let ap = paths_of(&w, "AccessPoints")
                .into_iter()
                .find(|ap| {
                    let a = get_all(&self.conn, NM, ap, "org.freedesktop.NetworkManager.AccessPoint");
                    a.get("Ssid").map(|v| bytes_of(v) == ssid.as_bytes()).unwrap_or(false)
                })
                .ok_or("Network not found")?;
            let app = zbus::zvariant::ObjectPath::try_from(ap.as_str()).map_err(|e| e.to_string())?;
            let mut settings: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
            if let Some(pw) = password {
                let mut sec: HashMap<&str, Value> = HashMap::new();
                sec.insert("key-mgmt", Value::from("wpa-psk"));
                sec.insert("psk", Value::from(pw));
                settings.insert("802-11-wireless-security", sec);
            }
            let reply = self
                .conn
                .call_method(Some(NM), NM_PATH, Some(NM), "AddAndActivateConnection", &(settings, &devp, app))
                .map_err(|e| short_error(&e))?;
            let (conn_path, _active): (OwnedObjectPath, OwnedObjectPath) = reply.body().deserialize().map_err(|e| e.to_string())?;
            created = Some(conn_path.to_string());
        }
        let result = self.wait_activated(&dev);
        if let (Err(_), Some(path)) = (&result, created) {
            let _ = self.conn.call_method(Some(NM), path.as_str(), Some("org.freedesktop.NetworkManager.Settings.Connection"), "Delete", &());
        }
        result
    }

    fn wait_activated(&self, dev: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut seen_activating = false;
        loop {
            let d = get_all(&self.conn, NM, dev, "org.freedesktop.NetworkManager.Device");
            let state = u32_of(&d, "State");
            match state {
                100 => return Ok(()),
                40..=90 => seen_activating = true,
                120 => return Err(reason_text(state_reason(&d))),
                30 if seen_activating => return Err(reason_text(state_reason(&d))),
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err("Connection timed out".into());
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    // ── Bluetooth ───────────────────────────────────────────────────

    fn objects(&self) -> HashMap<OwnedObjectPath, HashMap<String, Props>> {
        self.conn
            .call_method(Some(BLUEZ), "/", Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &())
            .ok()
            .and_then(|m| m.body().deserialize().ok())
            .unwrap_or_default()
    }

    fn adapter_path(&self) -> Option<String> {
        let mut paths: Vec<String> = self.objects().into_iter().filter(|(_, i)| i.contains_key("org.bluez.Adapter1")).map(|(p, _)| p.to_string()).collect();
        paths.sort();
        paths.into_iter().next()
    }

    fn device_path(&self, mac: &str) -> Option<String> {
        self.objects()
            .into_iter()
            .find(|(_, i)| i.get("org.bluez.Device1").map(|d| str_of(d, "Address").eq_ignore_ascii_case(mac)).unwrap_or(false))
            .map(|(p, _)| p.to_string())
    }

    pub fn refresh_bluetooth(&self, bt: &mut BtState) {
        let objs = self.objects();
        let mut adapters: Vec<(&OwnedObjectPath, &Props)> = objs.iter().filter_map(|(p, i)| i.get("org.bluez.Adapter1").map(|a| (p, a))).collect();
        adapters.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let Some((apath, a)) = adapters.first().copied() else {
            bt.adapter = false;
            bt.devices.clear();
            return;
        };
        bt.adapter = true;
        bt.powered = bool_of(a, "Powered");
        bt.discoverable = bool_of(a, "Discoverable");
        bt.discovering = bool_of(a, "Discovering");
        bt.name = Some(str_of(a, "Alias")).filter(|s| !s.is_empty()).unwrap_or_else(|| str_of(a, "Name"));
        let prefix = format!("{}/", apath.as_str());
        let mut devices: Vec<BtDevice> = objs
            .iter()
            .filter(|(p, _)| p.as_str().starts_with(&prefix))
            .filter_map(|(_, i)| {
                let d = i.get("org.bluez.Device1")?;
                let mac = str_of(d, "Address");
                let name = Some(str_of(d, "Alias")).filter(|s| !s.is_empty()).or_else(|| Some(str_of(d, "Name")).filter(|s| !s.is_empty())).unwrap_or_else(|| mac.replace(':', "-"));
                let battery = i.get("org.bluez.Battery1").and_then(|b| b.get("Percentage")).and_then(|v| u8::try_from(&**v).ok());
                Some(BtDevice { mac, name, icon: str_of(d, "Icon"), paired: bool_of(d, "Paired"), connected: bool_of(d, "Connected"), battery })
            })
            .collect();
        devices.sort_by_key(|d| d.name.to_lowercase());
        bt.devices = if bt.powered { devices } else { Vec::new() };
    }

    pub fn set_bluetooth(&self, on: bool) -> Result<(), String> {
        let a = self.adapter_path().ok_or("No Bluetooth adapter")?;
        set_prop(&self.conn, BLUEZ, &a, "org.bluez.Adapter1", "Powered", Value::from(on))
    }

    pub fn set_discoverable(&self, on: bool) -> Result<(), String> {
        let a = self.adapter_path().ok_or("No Bluetooth adapter")?;
        set_prop(&self.conn, BLUEZ, &a, "org.bluez.Adapter1", "Discoverable", Value::from(on))
    }

    /// Discover for `secs` seconds (restarting a running discovery so devices re-announce).
    pub fn scan_bluetooth(&self, secs: u64) {
        let Some(a) = self.adapter_path() else { return };
        let _ = self.conn.call_method(Some(BLUEZ), a.as_str(), Some("org.bluez.Adapter1"), "StopDiscovery", &());
        if self.conn.call_method(Some(BLUEZ), a.as_str(), Some("org.bluez.Adapter1"), "StartDiscovery", &()).is_ok() {
            // Discovery belongs to this connection; stop it after the window.
            let conn = self.conn.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                let _ = conn.call_method(Some(BLUEZ), a.as_str(), Some("org.bluez.Adapter1"), "StopDiscovery", &());
            });
        }
    }

    fn device_call(&self, mac: &str, method: &str) -> Result<(), String> {
        let p = self.device_path(mac).ok_or("Device not found")?;
        self.conn.call_method(Some(BLUEZ), p.as_str(), Some("org.bluez.Device1"), method, &()).map(|_| ()).map_err(|e| short_error(&e))
    }

    pub fn connect_bt(&self, mac: &str) -> Result<(), String> {
        self.device_call(mac, "Connect")
    }

    pub fn disconnect_bt(&self, mac: &str) -> Result<(), String> {
        self.device_call(mac, "Disconnect")
    }

    pub fn cancel_pair(&self, mac: &str) {
        let _ = self.device_call(mac, "CancelPairing");
    }

    /// Pair, trust and connect (QML NcBluetoothRow.startPair). Blocking (up to ~30 s).
    pub fn pair(&self, mac: &str) -> Result<(), String> {
        let a = self.adapter_path();
        if let Some(a) = &a {
            let _ = self.conn.call_method(Some(BLUEZ), a.as_str(), Some("org.bluez.Adapter1"), "StopDiscovery", &());
        }
        self.device_call(mac, "Pair")?;
        if let Some(p) = self.device_path(mac) {
            let _ = set_prop(&self.conn, BLUEZ, &p, "org.bluez.Device1", "Trusted", Value::from(true));
        }
        self.device_call(mac, "Connect")
    }

    pub fn forget_bt(&self, mac: &str) -> Result<(), String> {
        let a = self.adapter_path().ok_or("No Bluetooth adapter")?;
        let p = self.device_path(mac).ok_or("Device not found")?;
        let dp = zbus::zvariant::ObjectPath::try_from(p.as_str()).map_err(|e| e.to_string())?;
        self.conn.call_method(Some(BLUEZ), a.as_str(), Some("org.bluez.Adapter1"), "RemoveDevice", &(dp,)).map(|_| ()).map_err(|e| short_error(&e))
    }

    /// Register a pairing agent so "just works" pairing succeeds without bluetoothctl running.
    /// It lives as long as this connection (the control centre being open).
    pub fn register_agent(&self) {
        if self.conn.object_server().at(AGENT_PATH, Agent).is_err() {
            return;
        }
        let path = zbus::zvariant::ObjectPath::from_static_str_unchecked(AGENT_PATH);
        let _ = self.conn.call_method(Some(BLUEZ), "/org/bluez", Some("org.bluez.AgentManager1"), "RegisterAgent", &(path, "NoInputNoOutput"));
    }

    /// Watch NetworkManager / BlueZ signals; `on_change` runs (debounced to 300 ms) on changes.
    pub fn watch(on_change: impl Fn() -> bool + Send + 'static) {
        std::thread::Builder::new()
            .name("prism-nc-signals".into())
            .spawn(move || {
                let Ok(conn) = Connection::system() else { return };
                let rules = [
                    MatchRule::builder().msg_type(MsgType::Signal).sender(NM).map(|b| b.build()),
                    MatchRule::builder().msg_type(MsgType::Signal).sender(BLUEZ).map(|b| b.build()),
                ];
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                for rule in rules.into_iter().flatten() {
                    let Ok(mut it) = MessageIterator::for_match_rule(rule, &conn, Some(64)) else { continue };
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        while it.next().is_some() {
                            if tx.send(()).is_err() {
                                return;
                            }
                        }
                    });
                }
                drop(tx);
                while rx.recv().is_ok() {
                    std::thread::sleep(Duration::from_millis(300));
                    while rx.try_recv().is_ok() {}
                    if !on_change() {
                        return; // the control centre closed
                    }
                }
            })
            .ok();
    }
}

/// Auto-accepting agent for NoInputNoOutput ("just works") pairing.
struct Agent;

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}
    fn request_pin_code(&self, _device: OwnedObjectPath) -> zbus::fdo::Result<String> {
        Err(zbus::fdo::Error::NotSupported("PIN entry is not supported".into()))
    }
    fn display_pin_code(&self, _device: OwnedObjectPath, _pincode: String) {}
    fn request_passkey(&self, _device: OwnedObjectPath) -> zbus::fdo::Result<u32> {
        Err(zbus::fdo::Error::NotSupported("passkey entry is not supported".into()))
    }
    fn display_passkey(&self, _device: OwnedObjectPath, _passkey: u32, _entered: u16) {}
    fn request_confirmation(&self, _device: OwnedObjectPath, _passkey: u32) {}
    fn request_authorization(&self, _device: OwnedObjectPath) {}
    fn authorize_service(&self, _device: OwnedObjectPath, _uuid: String) {}
    fn cancel(&self) {}
}

fn state_reason(d: &Props) -> u32 {
    match d.get("StateReason").map(|v| &**v) {
        Some(Value::Structure(s)) => s.fields().get(1).and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        _ => 0,
    }
}

/// NM device state reasons to the messages QML showed.
fn reason_text(reason: u32) -> String {
    match reason {
        7..=10 => "Wrong password or authentication failed".into(),
        11 => "Connection timed out".into(),
        53 => "Network not found".into(),
        _ => "Could not connect".into(),
    }
}

/// Short user-facing text for a D-Bus error.
fn short_error(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(msg), _) => msg.clone(),
        other => other.to_string(),
    }
}

/// Access point security as nmcli prints it (WPA2, WPA3, WPA1, WEP, 802.1X).
fn security_label(flags: u32, wpa: u32, rsn: u32) -> String {
    const EAP: u32 = 0x200;
    const SAE: u32 = 0x400;
    let mut parts = Vec::new();
    if wpa != 0 {
        parts.push("WPA1");
    }
    if rsn & SAE != 0 {
        parts.push("WPA3");
    } else if rsn != 0 {
        parts.push("WPA2");
    }
    if (wpa | rsn) & EAP != 0 {
        parts.push("802.1X");
    }
    if parts.is_empty() && flags & 1 != 0 {
        parts.push("WEP");
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_labels() {
        assert_eq!(security_label(1, 0, 0x188), "WPA2");
        assert_eq!(security_label(1, 0, 0x400), "WPA3");
        assert_eq!(security_label(1, 0x188, 0x188), "WPA1 WPA2");
        assert_eq!(security_label(1, 0, 0x288), "WPA2 802.1X");
        assert_eq!(security_label(1, 0, 0), "WEP");
        assert_eq!(security_label(0, 0, 0), "");
        assert_eq!(reason_text(7), "Wrong password or authentication failed");
        assert_eq!(ipv6_text(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).as_deref(), Some("fe80::1"));
    }
}
