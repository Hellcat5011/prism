use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NetworkStatus {
    pub connected: bool,
    pub kind: String, // "ethernet", "wifi", "disconnected"
    pub ssid: Option<String>,
    pub signal: u8,
    pub ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BluetoothStatus {
    pub connected: bool,
    pub name: Option<String>,
    pub battery: Option<u8>,
    pub icon: Option<String>,
}

/// Retrieve current network connection status via nmcli
pub fn get_network_status() -> NetworkStatus {
    // Check devices via nmcli -t -f TYPE,STATE,CONNECTION device
    let output = Command::new("nmcli")
        .args(["-t", "-f", "TYPE,STATE,CONNECTION", "device"])
        .output();

    let text = match output {
        Ok(ref o) if o.status.success() => String::from_utf8_lossy(&o.stdout),
        _ => return NetworkStatus::default(),
    };

    let mut is_ethernet = false;
    let mut is_wifi = false;
    let mut active_conn = None;

    for line in text.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 2 {
            let dev_type = parts[0];
            let dev_state = parts[1];
            let conn_name = parts.get(2).copied().unwrap_or("");

            if dev_state == "connected" {
                if dev_type == "ethernet" {
                    is_ethernet = true;
                    active_conn = Some(conn_name.to_string());
                    break;
                } else if dev_type == "wifi" {
                    is_wifi = true;
                    active_conn = Some(conn_name.to_string());
                }
            }
        }
    }

    if is_ethernet {
        NetworkStatus {
            connected: true,
            kind: "ethernet".to_string(),
            ssid: active_conn,
            signal: 100,
            ip: get_active_ip(),
        }
    } else if is_wifi {
        NetworkStatus {
            connected: true,
            kind: "wifi".to_string(),
            ssid: active_conn,
            signal: get_wifi_signal().unwrap_or(80),
            ip: get_active_ip(),
        }
    } else {
        NetworkStatus {
            connected: false,
            kind: "disconnected".to_string(),
            ssid: None,
            signal: 0,
            ip: None,
        }
    }
}

fn get_wifi_signal() -> Option<u8> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "ACTIVE,SIGNAL", "dev", "wifi"])
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 2 && parts[0] == "yes" {
            return parts[1].parse().ok();
        }
    }
    None
}

fn get_active_ip() -> Option<String> {
    let output = Command::new("hostname").arg("-I").output().ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        let first = text.split_whitespace().next()?;
        return Some(first.to_string());
    }
    None
}

/// Toggle Wi-Fi radio on/off via nmcli
pub fn toggle_wifi() -> Result<bool, String> {
    let output = Command::new("nmcli")
        .args(["radio", "wifi"])
        .output()
        .map_err(|e| format!("Failed to run nmcli: {}", e))?;

    let current = String::from_utf8_lossy(&output.stdout);
    let enabled = current.trim().eq_ignore_ascii_case("enabled");

    let new_state = if enabled { "off" } else { "on" };
    let status = Command::new("nmcli")
        .args(["radio", "wifi", new_state])
        .status()
        .map_err(|e| format!("Failed to toggle wifi: {}", e))?;

    if status.success() {
        Ok(!enabled)
    } else {
        Err("nmcli radio wifi toggle failed".to_string())
    }
}

/// Retrieve current Bluetooth connection status via bluetoothctl
pub fn get_bluetooth_status() -> BluetoothStatus {
    let output = Command::new("bluetoothctl").arg("info").output();

    let text = match output {
        Ok(ref o) if o.status.success() => String::from_utf8_lossy(&o.stdout),
        _ => return BluetoothStatus::default(),
    };

    if !text.contains("Connected: yes") {
        return BluetoothStatus {
            connected: false,
            name: None,
            battery: None,
            icon: None,
        };
    }

    let mut name = None;
    let mut battery = None;
    let mut icon = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Alias:") {
            name = Some(trimmed.trim_start_matches("Alias:").trim().to_string());
        } else if trimmed.starts_with("Battery Percentage:") {
            if let Some(val) = trimmed.split('(').nth(1).and_then(|s| s.split(')').next()) {
                battery = val.parse().ok();
            }
        } else if trimmed.starts_with("Icon:") {
            icon = Some(trimmed.trim_start_matches("Icon:").trim().to_string());
        }
    }

    BluetoothStatus {
        connected: true,
        name,
        battery,
        icon,
    }
}

/// Toggle Bluetooth power on/off via bluetoothctl
pub fn toggle_bluetooth() -> Result<bool, String> {
    let output = Command::new("bluetoothctl")
        .args(["show"])
        .output()
        .map_err(|e| format!("Failed to query bluetoothctl: {}", e))?;

    let text = String::from_utf8_lossy(&output.stdout);
    let powered_on = text.contains("Powered: yes");

    let new_state = if powered_on { "off" } else { "on" };
    let status = Command::new("bluetoothctl")
        .args(["power", new_state])
        .status()
        .map_err(|e| format!("Failed to toggle bluetooth power: {}", e))?;

    if status.success() {
        Ok(!powered_on)
    } else {
        Err("bluetoothctl power toggle failed".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub security: String,
    pub in_use: bool,
}

/// Scans for nearby Wi-Fi access points via nmcli
pub fn scan_wifi_networks() -> Vec<WifiNetwork> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "SSID,SIGNAL,SECURITY,IN-USE", "device", "wifi", "list"])
        .output();

    let text = match output {
        Ok(ref o) if o.status.success() => String::from_utf8_lossy(&o.stdout),
        _ => return Vec::new(),
    };

    let mut networks: Vec<WifiNetwork> = Vec::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 3 {
            let ssid = parts[0].trim().to_string();
            if ssid.is_empty() || ssid == "--" {
                continue;
            }
            let signal = parts[1].parse::<u8>().unwrap_or(0);
            let security = parts[2].trim().to_string();
            let in_use = parts.get(3).map(|s| s.contains('*')).unwrap_or(false);

            if let Some(existing) = networks.iter_mut().find(|n| n.ssid == ssid) {
                if signal > existing.signal {
                    existing.signal = signal;
                }
                if in_use {
                    existing.in_use = true;
                }
            } else {
                networks.push(WifiNetwork {
                    ssid,
                    signal,
                    security,
                    in_use,
                });
            }
        }
    }

    networks.sort_by(|a, b| {
        b.in_use.cmp(&a.in_use).then_with(|| b.signal.cmp(&a.signal))
    });

    networks
}

/// Connects to a Wi-Fi network via nmcli
pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new("nmcli");
    cmd.args(["dev", "wifi", "connect", ssid]);
    if let Some(pass) = password {
        if !pass.is_empty() {
            cmd.args(["password", pass]);
        }
    }

    let status = cmd.status().map_err(|e| format!("Failed to run nmcli: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Failed to connect to Wi-Fi network '{}'", ssid))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BluetoothDevice {
    pub mac: String,
    pub name: String,
    pub connected: bool,
    pub paired: bool,
}

/// Lists known and nearby Bluetooth devices via bluetoothctl
pub fn scan_bluetooth_devices() -> Vec<BluetoothDevice> {
    let output = Command::new("bluetoothctl").arg("devices").output();
    let text = match output {
        Ok(ref o) if o.status.success() => String::from_utf8_lossy(&o.stdout),
        _ => return Vec::new(),
    };

    let mut devices = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Format: Device <MAC> <Name...>
        if parts.len() >= 3 && parts[0] == "Device" {
            let mac = parts[1].to_string();
            let name = parts[2..].join(" ");

            // Query device info
            let mut connected = false;
            let mut paired = false;
            if let Ok(info_out) = Command::new("bluetoothctl").args(["info", &mac]).output() {
                let info_text = String::from_utf8_lossy(&info_out.stdout);
                connected = info_text.contains("Connected: yes");
                paired = info_text.contains("Paired: yes");
            }

            devices.push(BluetoothDevice {
                mac,
                name,
                connected,
                paired,
            });
        }
    }

    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| b.paired.cmp(&a.paired)));
    devices
}

pub fn connect_bluetooth_device(mac: &str) -> Result<(), String> {
    let status = Command::new("bluetoothctl")
        .args(["connect", mac])
        .status()
        .map_err(|e| format!("Failed to connect bluetooth: {}", e))?;
    if status.success() { Ok(()) } else { Err("Connect failed".to_string()) }
}

pub fn disconnect_bluetooth_device(mac: &str) -> Result<(), String> {
    let status = Command::new("bluetoothctl")
        .args(["disconnect", mac])
        .status()
        .map_err(|e| format!("Failed to disconnect bluetooth: {}", e))?;
    if status.success() { Ok(()) } else { Err("Disconnect failed".to_string()) }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSink {
    pub id: String,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub kind: String, // "bluetooth", "hdmi", "speaker"
}

/// Lists all available audio output sinks via wpctl or pactl
pub fn list_audio_sinks() -> Vec<AudioSink> {
    let output = Command::new("wpctl").arg("status").output();
    if let Ok(ref o) = output {
        if o.status.success() {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut in_sinks = false;
            let mut sinks = Vec::new();

            for line in text.lines() {
                if line.contains("Sinks:") {
                    in_sinks = true;
                    continue;
                }
                if in_sinks && (line.contains("Sources:") || line.contains("Filters:") || line.contains("Streams:") || line.contains("Video")) {
                    break;
                }

                if in_sinks {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with("├") || trimmed.starts_with("└") || trimmed.starts_with("│") {
                        let clean = trimmed.trim_start_matches(['├', '└', '│', '─', ' ']);
                        let is_default = clean.starts_with('*');
                        let clean_item = clean.trim_start_matches('*').trim();

                        if let Some((id_str, rest)) = clean_item.split_once('.') {
                            let id = id_str.trim().to_string();
                            if id.chars().all(|c| c.is_ascii_digit()) {
                                let desc = if let Some((name_part, _vol)) = rest.split_once("[vol:") {
                                    name_part.trim().to_string()
                                } else {
                                    rest.trim().to_string()
                                };

                                let desc_lower = desc.to_lowercase();
                                let kind = if desc_lower.contains("blue") || desc_lower.contains("headset") || desc_lower.contains("airpod") {
                                    "bluetooth"
                                } else if desc_lower.contains("hdmi") || desc_lower.contains("display") || desc_lower.contains("monitor") {
                                    "hdmi"
                                } else {
                                    "speaker"
                                };

                                sinks.push(AudioSink {
                                    id: id.clone(),
                                    name: id,
                                    description: desc,
                                    is_default,
                                    kind: kind.to_string(),
                                });
                            }
                        }
                    }
                }
            }

            if !sinks.is_empty() {
                return sinks;
            }
        }
    }

    // Fallback: pactl list short sinks
    if let Ok(p_out) = Command::new("pactl").args(["list", "short", "sinks"]).output() {
        if p_out.status.success() {
            let text = String::from_utf8_lossy(&p_out.stdout);
            let mut sinks = Vec::new();
            for line in text.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let id = parts[0].to_string();
                    let name = parts[1].to_string();
                    let name_lower = name.to_lowercase();
                    let kind = if name_lower.contains("blue") || name_lower.contains("headset") {
                        "bluetooth"
                    } else if name_lower.contains("hdmi") {
                        "hdmi"
                    } else {
                        "speaker"
                    };

                    sinks.push(AudioSink {
                        id: id.clone(),
                        name: name.clone(),
                        description: name,
                        is_default: false,
                        kind: kind.to_string(),
                    });
                }
            }
            return sinks;
        }
    }

    Vec::new()
}

pub fn set_default_sink(sink_id_or_name: &str) -> Result<(), String> {
    // Try wpctl set-default <id>
    if sink_id_or_name.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(st) = Command::new("wpctl").args(["set-default", sink_id_or_name]).status() {
            if st.success() {
                return Ok(());
            }
        }
    }

    // Try pactl set-default-sink <name>
    let status = Command::new("pactl")
        .args(["set-default-sink", sink_id_or_name])
        .status()
        .map_err(|e| format!("Failed to set default sink: {}", e))?;

    if status.success() {
        Ok(())
    } else {
        Err("Failed to set default sink via pactl".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: bool,
}

/// Retrieves laptop battery level and charging state, or None if on desktop without battery
pub fn get_battery_status() -> Option<BatteryStatus> {
    let power_path = std::path::Path::new("/sys/class/power_supply");
    if let Ok(entries) = std::fs::read_dir(power_path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("BAT") {
                let cap_file = entry.path().join("capacity");
                let status_file = entry.path().join("status");

                if let Ok(cap_str) = std::fs::read_to_string(cap_file) {
                    if let Ok(percent) = cap_str.trim().parse::<u8>() {
                        let status_str = std::fs::read_to_string(status_file).unwrap_or_default();
                        let charging = status_str.trim().eq_ignore_ascii_case("Charging");
                        return Some(BatteryStatus {
                            percent: percent.clamp(0, 100),
                            charging,
                        });
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_status_default() {
        let s = NetworkStatus::default();
        assert!(!s.connected);
        assert_eq!(s.signal, 0);
    }

    #[test]
    fn test_bluetooth_status_default() {
        let b = BluetoothStatus::default();
        assert!(!b.connected);
    }

    #[test]
    fn test_wifi_network_struct() {
        let w = WifiNetwork {
            ssid: "PrismNet".to_string(),
            signal: 95,
            security: "WPA2".to_string(),
            in_use: true,
        };
        assert_eq!(w.ssid, "PrismNet");
        assert_eq!(w.signal, 95);
        assert!(w.in_use);
    }

    #[test]
    fn test_audio_sink_struct() {
        let s = AudioSink {
            id: "34".to_string(),
            name: "analog_out".to_string(),
            description: "Built-in Audio Analog Stereo".to_string(),
            is_default: true,
            kind: "speaker".to_string(),
        };
        assert!(s.is_default);
        assert_eq!(s.kind, "speaker");
    }

    #[test]
    fn test_battery_status_bounds() {
        let b = BatteryStatus {
            percent: 85,
            charging: true,
        };
        assert_eq!(b.percent, 85);
        assert!(b.charging);
    }
}
