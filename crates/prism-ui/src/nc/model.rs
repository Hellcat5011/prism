//! Control-centre data: the state the notification-center pages show (ported from QML
//! NcNetwork.qml, SystemMonitor.qml and the Quickshell Bluetooth service). `nc/dbus.rs` fills it
//! from NetworkManager and BlueZ on the status worker thread.

// ── Network ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Net {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub security: String,
    pub rate: String,
    pub active: bool,
    pub known: bool,
}

impl Net {
    /// Section of the network list: 0 connected, 1 saved, 2 available.
    pub fn group(&self) -> u8 {
        if self.active {
            0
        } else if self.known {
            1
        } else {
            2
        }
    }
}

/// One line of the connection-details list: a section header or a label/value pair.
#[derive(Clone, Debug, PartialEq)]
pub enum DetailRow {
    Header(String),
    Item(String, String),
}

/// Parsed `nmcli device show` for one device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DevDetails {
    pub mac: String,
    pub connection: String,
    pub driver: String,
    pub gateway: String,
    pub gateway6: String,
    pub ip4: Vec<String>,
    pub ip6: Vec<String>,
    pub dns: Vec<String>,
    pub dns6: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct NetState {
    pub wifi_device: String,
    pub wifi_enabled: bool,
    pub ethernet_device: String,
    pub ethernet_connected: bool,
    pub networks: Vec<Net>,
    pub eth: DevDetails,
    pub wifi: DevDetails,
    /// Ethernet link speed in Mb/s (0 = unknown).
    pub link_speed: u32,
    pub scanning: bool,
    pub connecting: Option<String>,
    /// (ssid, message) of the last failed connection.
    pub error: Option<(String, String)>,
    /// VPN and WireGuard connections, active first, then by name.
    pub vpns: Vec<Vpn>,
    /// Settings paths of VPNs being switched on or off.
    pub vpn_busy: Vec<String>,
    /// Result of the last VPN action or import, (text, is error).
    pub vpn_msg: Option<(String, bool)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum VpnState {
    #[default]
    Off,
    Connecting,
    On,
    Disconnecting,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vpn {
    pub name: String,
    /// Settings connection path.
    pub path: String,
    /// "openvpn", "wireguard", ...
    pub kind: String,
    pub state: VpnState,
    /// Active connection path while on.
    pub active: Option<String>,
}

impl NetState {
    pub fn wifi_present(&self) -> bool {
        !self.wifi_device.is_empty()
    }

    pub fn active(&self) -> Option<&Net> {
        self.networks.iter().find(|n| n.active)
    }

    fn first_ip(&self) -> String {
        let d = if self.ethernet_connected { &self.eth } else { &self.wifi };
        d.ip4.first().map(|a| a.split('/').next().unwrap_or("").to_string()).unwrap_or_default()
    }

    /// Tile title (QML statusText).
    pub fn status_text(&self) -> String {
        if self.ethernet_connected {
            "Ethernet".into()
        } else if !self.wifi_present() {
            "No network".into()
        } else if !self.wifi_enabled {
            "Wi-Fi off".into()
        } else if let Some(a) = self.active() {
            a.ssid.clone()
        } else {
            "Not connected".into()
        }
    }

    /// Tile subtitle (QML statusDetail).
    pub fn status_detail(&self) -> String {
        let mut parts = Vec::new();
        let ip = self.first_ip();
        if self.ethernet_connected {
            if !ip.is_empty() {
                parts.push(ip);
            }
            if self.link_speed > 0 {
                parts.push(speed_text(self.link_speed));
            }
        } else if let (true, true, Some(a)) = (self.wifi_present(), self.wifi_enabled, self.active()) {
            parts.push(format!("{}%", a.signal));
            if !ip.is_empty() {
                parts.push(ip);
            }
        } else if self.wifi_present() {
            parts.push(if self.wifi_enabled { "Wi-Fi".into() } else { "Turned off".into() });
        }
        parts.join(" · ")
    }

    pub fn eth_rows(&self) -> Vec<DetailRow> {
        if !self.ethernet_connected {
            return Vec::new();
        }
        let speed = if self.link_speed > 0 { format!("{} Mb/s", self.link_speed) } else { String::new() };
        sections(
            &self.eth,
            vec![
                ("Interface", format!("Ethernet ({})", self.ethernet_device)),
                ("Hardware Address", self.eth.mac.clone()),
                ("Driver", self.eth.driver.clone()),
                ("Speed", speed),
                ("Security", "None".into()),
            ],
        )
    }

    pub fn wifi_rows(&self) -> Vec<DetailRow> {
        let Some(a) = self.active() else { return Vec::new() };
        sections(
            &self.wifi,
            vec![
                ("Network", a.ssid.clone()),
                ("Interface", format!("Wi-Fi ({})", self.wifi_device)),
                ("Hardware Address", self.wifi.mac.clone()),
                ("Driver", self.wifi.driver.clone()),
                ("Signal", format!("{}%", a.signal)),
                ("Speed", a.rate.clone()),
                ("Security", if a.security.is_empty() { "None".into() } else { a.security.clone() }),
            ],
        )
    }
}

fn speed_text(n: u32) -> String {
    if n >= 1000 {
        format!("{} Gb/s", n as f64 / 1000.0)
    } else {
        format!("{} Mb/s", n)
    }
}

/// QML NcNetwork.sections: General / IPv4 / IPv6 groups, skipping empty values.
fn sections(d: &DevDetails, general: Vec<(&str, String)>) -> Vec<DetailRow> {
    let mut rows = Vec::new();
    let mut group = |title: &str, pairs: Vec<(&str, String)>| {
        let items: Vec<_> = pairs.into_iter().filter(|(_, v)| !v.is_empty()).collect();
        if items.is_empty() {
            return;
        }
        rows.push(DetailRow::Header(title.into()));
        for (l, v) in items {
            rows.push(DetailRow::Item(l.into(), v));
        }
    };
    group("General", general);
    if let Some((ip, bcast, mask)) = d.ip4.first().and_then(|a| ipv4_info(a)) {
        group(
            "IPv4",
            vec![
                ("IP Address", ip),
                ("Broadcast Address", bcast),
                ("Subnet Mask", mask),
                ("Default Route", d.gateway.clone()),
                ("Primary DNS", d.dns.first().cloned().unwrap_or_default()),
                ("Secondary DNS", d.dns.get(1).cloned().unwrap_or_default()),
            ],
        );
    }
    group(
        "IPv6",
        vec![
            ("IP Address", d.ip6.first().cloned().unwrap_or_default()),
            ("Default Route", d.gateway6.clone()),
            ("Primary DNS", d.dns6.first().cloned().unwrap_or_default()),
        ],
    );
    rows
}

/// "192.168.1.5/24" to (ip, broadcast, mask).
fn ipv4_info(cidr: &str) -> Option<(String, String, String)> {
    let (ip, bits) = cidr.split_once('/').unwrap_or((cidr, "32"));
    let bits: u32 = bits.parse().ok().filter(|b| *b <= 32)?;
    let addr: std::net::Ipv4Addr = ip.parse().ok()?;
    let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
    let bcast = u32::from(addr) | !mask;
    Some((ip.to_string(), std::net::Ipv4Addr::from(bcast).to_string(), std::net::Ipv4Addr::from(mask).to_string()))
}

// ── Bluetooth ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BtDevice {
    pub mac: String,
    pub name: String,
    pub icon: String,
    pub paired: bool,
    pub connected: bool,
    pub battery: Option<u8>,
}

impl BtDevice {
    /// False for devices that only advertise their address.
    pub fn named(&self) -> bool {
        let n = self.name.replace('-', ":");
        !(n.len() == 17 && n.split(':').count() == 6 && n.split(':').all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit())))
    }

    /// NcIcon name for this device (QML NcBluetoothRow.iconFor).
    pub fn glyph(&self) -> &'static str {
        let i = self.icon.to_lowercase();
        if i.contains("headset") || i.contains("headphone") {
            "headset"
        } else if i.contains("audio") {
            "speaker"
        } else if i.contains("keyboard") {
            "keyboard"
        } else if i.contains("phone") {
            "phone"
        } else if i.contains("computer") {
            "monitor"
        } else {
            "bluetooth"
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BtState {
    pub adapter: bool,
    pub name: String,
    pub powered: bool,
    pub discoverable: bool,
    pub discovering: bool,
    pub devices: Vec<BtDevice>,
    /// Devices with an operation running (connect / pair), by MAC.
    pub busy: Vec<(String, BtBusy)>,
    /// MACs whose last pairing attempt failed.
    pub failed: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BtBusy {
    Connecting,
    Pairing,
}

impl BtState {
    pub fn busy(&self, mac: &str) -> Option<BtBusy> {
        self.busy.iter().find(|(m, _)| m == mac).map(|(_, b)| *b)
    }
}

// ── Audio / battery ────────────────────────────────────────────────────────

/// Sink glyph (QML NcFooter iconKind).
pub fn sink_glyph(name: &str, description: &str) -> &'static str {
    let (n, d) = (name.to_lowercase(), description.to_lowercase());
    if n.contains("bluez") || ["blue", "headset", "buds", "airpods"].iter().any(|k| d.contains(k)) {
        "bluetooth"
    } else if n.contains("hdmi") || ["hdmi", "monitor", "display"].iter().any(|k| d.contains(k)) {
        "monitor"
    } else {
        "speaker"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_rows() {
        let d = DevDetails {
            mac: "AA:BB".into(),
            ip4: vec!["192.168.1.5/24".into()],
            gateway: "192.168.1.1".into(),
            dns: vec!["1.1.1.1".into()],
            ip6: vec!["2001:db8::5/64".into()],
            ..Default::default()
        };
        let rows = sections(&d, vec![("Interface", "x".into())]);
        assert!(rows.contains(&DetailRow::Item("Broadcast Address".into(), "192.168.1.255".into())));
        assert!(rows.contains(&DetailRow::Item("Subnet Mask".into(), "255.255.255.0".into())));
        assert!(rows.contains(&DetailRow::Header("IPv6".into())));
    }

    #[test]
    fn device_names_and_glyphs() {
        let d = BtDevice { name: "Buds Pro".into(), icon: "audio-headset".into(), ..Default::default() };
        assert!(d.named());
        assert_eq!(d.glyph(), "headset");
        assert!(!BtDevice { name: "11-22-33-44-55-66".into(), ..Default::default() }.named());
    }

    #[test]
    fn status_text_follows_qml() {
        let mut n = NetState { wifi_device: "wlan0".into(), wifi_enabled: true, ..Default::default() };
        assert_eq!(n.status_text(), "Not connected");
        assert_eq!(n.status_detail(), "Wi-Fi");
        n.networks.push(Net { ssid: "Home".into(), signal: 70, active: true, ..Default::default() });
        n.wifi.ip4.push("10.0.0.2/24".into());
        assert_eq!(n.status_text(), "Home");
        assert_eq!(n.status_detail(), "70% · 10.0.0.2");
        n.wifi_enabled = false;
        assert_eq!(n.status_text(), "Wi-Fi off");
    }
}
