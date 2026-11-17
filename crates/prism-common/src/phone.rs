//! Phone sync: types shared by prism-phoned (the KDE Connect protocol daemon), prismd,
//! prism-ctl and prism-ui. prism-phoned serves `$XDG_RUNTIME_DIR/prism-phone.sock`: one JSON
//! `PhoneCommand` per line in, one JSON `PhoneReply` per line out. After `Subscribe` the
//! connection also receives a `PhoneReply::State` line whenever the state changes.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Control socket of prism-phoned.
pub fn get_phone_socket_path() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if std::path::Path::new(&dir).is_dir() => PathBuf::from(dir).join("prism-phone.sock"),
        _ => PathBuf::from(format!("/tmp/prism-phone-{}.sock", unsafe { libc::getuid() })),
    }
}

/// Directory for runtime caches of prism-phoned (album art, notification icons).
pub fn phone_runtime_dir() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if std::path::Path::new(&dir).is_dir() => PathBuf::from(dir).join("prism-phone"),
        _ => PathBuf::from(format!("/tmp/prism-phone-{}", unsafe { libc::getuid() })),
    }
}

/// Everything the UI shows about phones. Sent whole on every change (it is small).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PhoneState {
    /// A problem that stops phone sync from working (e.g. the ports are taken).
    pub error: Option<String>,
    /// This computer's name as phones see it.
    pub name: String,
    pub devices: Vec<PhoneDevice>,
}

impl PhoneState {
    /// The device the UI is about: the first paired and connected one, else the first paired.
    pub fn primary(&self) -> Option<&PhoneDevice> {
        self.devices.iter().find(|d| d.paired && d.reachable).or_else(|| self.devices.iter().find(|d| d.paired))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairStatus {
    #[default]
    NotPaired,
    /// We asked; waiting for the phone.
    Requested,
    /// The phone asked; waiting for the user here.
    RequestedByPeer,
    Paired,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PhoneDevice {
    pub id: String,
    pub name: String,
    /// "phone", "tablet", "desktop", "laptop", "tv".
    pub device_type: String,
    pub paired: bool,
    /// A link is up right now.
    pub reachable: bool,
    pub pair: PairStatus,
    /// Shown on both devices while pairing; compare before accepting.
    pub verification_key: Option<String>,
    /// Unix seconds of the last packet or discovery.
    pub last_seen: u64,
    pub battery: Option<PhoneBattery>,
    pub network: Option<PhoneNetwork>,
    pub media: Option<PhoneMedia>,
    pub notifications: Vec<PhoneNotification>,
    pub transfers: Vec<PhoneTransfer>,
    /// Packet types the phone can receive (what we may ask of it).
    pub can_receive: Vec<String>,
}

impl PhoneDevice {
    pub fn supports(&self, packet_type: &str) -> bool {
        self.can_receive.iter().any(|t| t == packet_type)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct PhoneBattery {
    pub percent: i32,
    pub charging: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PhoneNetwork {
    /// "5G", "LTE", "HSPA", "UMTS", "EDGE", "GPRS", "GSM", "CDMA", ... as the phone reports it.
    pub kind: String,
    /// 0..=4 bars.
    pub strength: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PhoneMedia {
    pub player: String,
    pub players: Vec<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub position_ms: i64,
    pub length_ms: i64,
    /// Unix milliseconds when `position_ms` was read (to advance it while playing).
    pub position_at_ms: i64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    /// Album art cached under `phone_runtime_dir()`.
    pub art_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PhoneNotification {
    pub id: String,
    pub app: String,
    pub title: String,
    pub text: String,
    /// Unix milliseconds.
    pub time_ms: i64,
    pub dismissable: bool,
    /// Non-empty when the app accepts a reply.
    pub reply_id: String,
    pub actions: Vec<String>,
    pub icon_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    #[default]
    Incoming,
    Outgoing,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct PhoneTransfer {
    pub id: u64,
    pub direction: TransferDirection,
    pub name: String,
    pub bytes: u64,
    pub total: u64,
    /// Bytes per second, smoothed.
    pub speed: u64,
    pub done: bool,
    pub error: Option<String>,
    /// Final path of a received file.
    pub path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaCommand {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
}

/// Commands to prism-phoned. `device: None` means the primary device (`PhoneState::primary`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum PhoneCommand {
    /// Reply with the state now and after every change on this connection.
    Subscribe,
    Status,
    /// Re-read config.json (prismd sends this when it changes).
    ReloadConfig,
    /// Broadcast our identity again (find phones now).
    Refresh,
    /// Connect to a phone by address ("192.168.0.42" or "192.168.0.42:1716").
    Connect { addr: String },
    Pair { device: Option<String> },
    Unpair { device: Option<String> },
    /// Accept the phone's pair request.
    Accept { device: Option<String> },
    /// Reject the phone's pair request (or cancel ours).
    Reject { device: Option<String> },
    Ping { device: Option<String> },
    /// Find my phone: make it ring.
    Ring { device: Option<String> },
    /// Send this text to the phone's clipboard.
    SendClipboard { device: Option<String>, text: String },
    Media { device: Option<String>, action: MediaCommand },
    /// Seek the phone's player to this position.
    Seek { device: Option<String>, position_ms: i64 },
    NotificationAction { device: Option<String>, id: String, action: String },
    NotificationReply { device: Option<String>, reply_id: String, text: String },
    NotificationDismiss { device: Option<String>, id: String },
    SendFiles { device: Option<String>, paths: Vec<String> },
    SendText { device: Option<String>, text: String },
    SendUrl { device: Option<String>, url: String },
    CancelTransfer { id: u64 },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum PhoneReply {
    Ok { message: Option<String> },
    Error { message: String },
    State { state: PhoneState },
}

/// Send one command to prism-phoned and return its reply.
pub fn phone_request(cmd: &PhoneCommand) -> Result<PhoneReply, String> {
    use std::io::{BufRead, BufReader, Write};
    let path = get_phone_socket_path();
    let mut stream = std::os::unix::net::UnixStream::connect(&path).map_err(|_| "phone sync is not running (enable it in Settings > Phone)".to_string())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
    let line = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    writeln!(stream, "{line}").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    serde_json::from_str(reply.trim()).map_err(|e| format!("bad reply from prism-phoned: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        let c = PhoneCommand::SendFiles { device: None, paths: vec!["/tmp/a b".into()] };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, r#"{"cmd":"send_files","device":null,"paths":["/tmp/a b"]}"#);
        assert_eq!(serde_json::from_str::<PhoneCommand>(&s).unwrap(), c);
        let r: PhoneReply = serde_json::from_str(r#"{"reply":"ok","message":null}"#).unwrap();
        assert_eq!(r, PhoneReply::Ok { message: None });
    }
}
