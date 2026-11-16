pub use crate::power::PowerAction;
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

/// Standard socket path resolution for Prism IPC.
///
/// Evaluates $XDG_RUNTIME_DIR/prism.sock or falls back to /tmp/prism-<UID>.sock
pub fn get_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(&runtime_dir);
        if p.exists() && p.is_dir() {
            return p.join("prism.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/prism-{}.sock", uid))
}

/// Socket path resolution for active notification popup overlay.
pub fn get_notif_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(&runtime_dir);
        if p.exists() && p.is_dir() {
            return p.join("prism-notif-overlay.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/prism-notif-overlay-{}.sock", uid))
}

/// Socket path for the running OSD overlay. prismd writes `"<kind> <value>"` lines to it so a
/// visible OSD updates in place instead of a new process being spawned per key press.
pub fn get_osd_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(&runtime_dir);
        if p.exists() && p.is_dir() {
            return p.join("prism-osd-overlay.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/prism-osd-overlay-{}.sock", uid))
}

/// Socket of the resident `prism-ui daemon` (all overlays, toasts, OSD, desktop widgets).
pub fn get_ui_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(&runtime_dir);
        if p.exists() && p.is_dir() {
            return p.join("prism-ui.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/prism-ui-{}.sock", uid))
}

/// Commands from prismd to the resident UI process (one JSON object per line).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum UiCommand {
    /// Open the modal overlay `mode` (launcher, search, wallpaper, clipboard, emoji, settings,
    /// power, keybinds, notif-center), or close it if it is already the open one.
    Toggle(String),
    /// Open `mode`, replacing any other modal overlay.
    Open(String),
    /// Close the open modal overlay (with its exit transition).
    Close,
    /// Show / update the volume or brightness OSD (value in percent).
    Osd { kind: String, value: f32 },
    /// Notification toasts.
    Notify(NotifOverlayMessage),
    /// Desktop edit mode: "toggle", "open" or "close".
    EditMode(String),
}

/// Messages sent to the running notification popup overlay.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NotifOverlayMessage {
    Add {
        id: u32,
        app_name: String,
        summary: String,
        body: String,
        timeout_ms: u64,
    },
    Dismiss {
        id: u32,
    },
}

/// Commands accepted by the Prism IPC server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "target", content = "action")]
pub enum IpcCommand {
    #[serde(rename = "launcher")]
    Launcher(LauncherAction),

    #[serde(rename = "search")]
    Search(SearchAction),

    #[serde(rename = "wallpaper")]
    Wallpaper(WallpaperAction),

    #[serde(rename = "media")]
    Media(MediaAction),

    #[serde(rename = "theme")]
    Theme(ThemeAction),

    #[serde(rename = "notif")]
    Notif(NotifAction),

    #[serde(rename = "osd")]
    Osd(OsdAction),

    #[serde(rename = "clipboard")]
    Clipboard(ClipboardAction),

    #[serde(rename = "emoji")]
    Emoji(EmojiAction),

    #[serde(rename = "colorpicker")]
    ColorPicker(ColorPickerAction),

    #[serde(rename = "power")]
    Power(PowerAction),

    #[serde(rename = "settings")]
    Settings(SettingsAction),

    #[serde(rename = "desktopedit")]
    DesktopEdit(DesktopEditAction),

    #[serde(rename = "keybinds")]
    Keybinds(KeybindsAction),

    #[serde(rename = "network")]
    Network(NetworkAction),

    #[serde(rename = "bluetooth")]
    Bluetooth(BluetoothAction),

    #[serde(rename = "lock")]
    Lock(LockAction),

    #[serde(rename = "bluelight")]
    BlueLight(BlueLightAction),

    #[serde(rename = "depth")]
    Depth(DepthAction),

    #[serde(rename = "index")]
    Index(IndexAction),

    #[serde(rename = "help")]
    Help,

    #[serde(rename = "ping")]
    Ping,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LauncherAction {
    Toggle,
    Open,
    Close,
    Query(String),
    Run(String),
    List,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardAction {
    Toggle,
    Open,
    Close,
    List,
    Get(usize),
    Add(String),
    AddImage(Vec<u8>),
    Clear,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperAction {
    Set {
        path: PathBuf,
        scheme: Option<String>,
        mode: Option<String>,
    },
    Get,
    Toggle,
    Open,
    Close,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MediaAction {
    Status,
    PlayPause,
    Next,
    Previous,
    Stop,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TargetAction {
    Toggle,
    Open,
    Close,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SearchAction {
    Toggle,
    Open,
    Close,
    SetQuery(String),
    GetQuery,
    GetResults,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAction {
    Reload,
    GetPalette,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotifAction {
    Toggle,
    Open,
    Close,
    List,
    Clear,
    Send {
        app_name: String,
        summary: String,
        body: String,
    },
    Dismiss(u32),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OsdAction {
    Volume(String),
    VolumeDelta(i32),
    VolumeMute,
    Brightness(String),
    BrightnessDelta(i32),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EmojiAction {
    Toggle,
    Open,
    Close,
    List,
    Search(String),
    Copy(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ColorPickerAction {
    Toggle,
    Open,
    Close,
    Pick,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum KeybindsAction {
    Toggle,
    Open,
    Close,
    List,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAction {
    Status,
    ToggleWifi,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothAction {
    Status,
    Toggle,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SettingsAction {
    Toggle,
    Open,
    Close,
    OpenTab(u32),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEditAction {
    Toggle,
    Open,
    Close,
    Save,
    Discard,
    Status,
    Reset,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LockAction {
    Lock,
    Unlock,
    Status,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BlueLightAction {
    Toggle,
    ManualToggle,
    SetFixedTime { start: String, end: String },
    SetSunsetSunriseManual { lat: String, lon: String },
    SetSunsetSunriseIp,
    SetTransitionMinutes(u32),
    Dump,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DepthAction {
    Status,
    Generate,
    Clear,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IndexAction {
    Rebuild,
    Status,
    Message,
    LastError,
    LastIndexed,
}

/// Structured IPC response returned to clients.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "status", content = "data")]
pub enum IpcResponse {
    #[serde(rename = "ok")]
    Ok(Option<String>),

    #[serde(rename = "error")]
    Error(String),
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_command_serialization() {
        let cmd = IpcCommand::Launcher(LauncherAction::Toggle);
        let serialized = serde_json::to_string(&cmd).unwrap();
        let deserialized: IpcCommand = serde_json::from_str(&serialized).unwrap();
        assert_eq!(cmd, deserialized);

        let lock_cmd = IpcCommand::Lock(LockAction::Lock);
        let lock_str = serde_json::to_string(&lock_cmd).unwrap();
        let lock_deser: IpcCommand = serde_json::from_str(&lock_str).unwrap();
        assert_eq!(lock_cmd, lock_deser);

        let ping_cmd = IpcCommand::Ping;
        let ping_str = serde_json::to_string(&ping_cmd).unwrap();
        let ping_deser: IpcCommand = serde_json::from_str(&ping_str).unwrap();
        assert_eq!(ping_cmd, ping_deser);

        let media_cmd = IpcCommand::Media(MediaAction::PlayPause);
        let media_str = serde_json::to_string(&media_cmd).unwrap();
        let media_deser: IpcCommand = serde_json::from_str(&media_str).unwrap();
        assert_eq!(media_cmd, media_deser);

        let wp_cmd = IpcCommand::Wallpaper(WallpaperAction::Get);
        let wp_str = serde_json::to_string(&wp_cmd).unwrap();
        let wp_deser: IpcCommand = serde_json::from_str(&wp_str).unwrap();
        assert_eq!(wp_cmd, wp_deser);

        let notif_cmd = IpcCommand::Notif(NotifAction::List);
        let notif_str = serde_json::to_string(&notif_cmd).unwrap();
        let notif_deser: IpcCommand = serde_json::from_str(&notif_str).unwrap();
        assert_eq!(notif_cmd, notif_deser);

        let osd_cmd = IpcCommand::Osd(OsdAction::VolumeDelta(5));
        let osd_str = serde_json::to_string(&osd_cmd).unwrap();
        let osd_deser: IpcCommand = serde_json::from_str(&osd_str).unwrap();
        assert_eq!(osd_cmd, osd_deser);

        let power_cmd = IpcCommand::Power(PowerAction::Reboot);
        let power_str = serde_json::to_string(&power_cmd).unwrap();
        let power_deser: IpcCommand = serde_json::from_str(&power_str).unwrap();
        assert_eq!(power_cmd, power_deser);

        let emoji_cmd = IpcCommand::Emoji(EmojiAction::Search("smile".to_string()));
        let emoji_str = serde_json::to_string(&emoji_cmd).unwrap();
        let emoji_deser: IpcCommand = serde_json::from_str(&emoji_str).unwrap();
        assert_eq!(emoji_cmd, emoji_deser);

        let net_cmd = IpcCommand::Network(NetworkAction::Status);
        let net_str = serde_json::to_string(&net_cmd).unwrap();
        let net_deser: IpcCommand = serde_json::from_str(&net_str).unwrap();
        assert_eq!(net_cmd, net_deser);

        let kb_cmd = IpcCommand::Keybinds(KeybindsAction::List);
        let kb_str = serde_json::to_string(&kb_cmd).unwrap();
        let kb_deser: IpcCommand = serde_json::from_str(&kb_str).unwrap();
        assert_eq!(kb_cmd, kb_deser);
    }

    #[test]
    fn test_ipc_response_serialization() {
        let resp = IpcResponse::Ok(Some("pong".to_string()));
        let s = serde_json::to_string(&resp).unwrap();
        let deser: IpcResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(resp, deser);
    }
}
