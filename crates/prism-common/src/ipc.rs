pub use crate::power::PowerAction;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Standard socket path resolution for Prism IPC.
///
/// `<runtime dir>/prism.sock` (see `runtime::dir`).
pub fn get_socket_path() -> PathBuf {
    crate::runtime::dir().join("prism.sock")
}

/// Socket path resolution for active notification popup overlay.
pub fn get_notif_socket_path() -> PathBuf {
    crate::runtime::dir().join("prism-notif-overlay.sock")
}

/// Socket path for the running OSD overlay. prismd writes `"<kind> <value>"` lines to it so a
/// visible OSD updates in place instead of a new process being spawned per key press.
pub fn get_osd_socket_path() -> PathBuf {
    crate::runtime::dir().join("prism-osd-overlay.sock")
}

/// Socket of the resident `prism-ui daemon` (all overlays, toasts, OSD, desktop widgets).
pub fn get_ui_socket_path() -> PathBuf {
    crate::runtime::dir().join("prism-ui.sock")
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
    /// Hold (true) or release an idle inhibitor (Caffeinate).
    Caffeinate(bool),
    /// Game mode: no animations and no blur while on.
    GameMode(bool),
    /// Nobody is watching: the screen dimmed for idle or the session is locked. Idle desktop
    /// animations (the sleeping cat) stop until it is false again.
    Away(bool),
    /// Screen colour picker: "toggle", "open" or "close".
    ColorPicker(String),
    /// Window switcher: "next", "prev" or "close".
    Switcher(String),
    /// Screen capture overlay and instant screenshots (prism-ui).
    Capture(CaptureAction),
    /// Recording state for the indicator: Some while prism-rec runs, None when it stopped.
    Recording(Option<crate::capture::RecordStatus>),
    /// Open the Trim card for this recording.
    Trim(String),
    /// Downloads in progress (the downloads pill); empty hides it.
    Downloads(Vec<DownloadProgress>),
    /// Show this wallpaper on the built-in background.
    Wallpaper(std::path::PathBuf),
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
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        image: Option<String>,
        #[serde(default = "normal_urgency")]
        urgency: u8,
        #[serde(default)]
        actions: Vec<(String, String)>,
        #[serde(default)]
        desktop_entry: String,
        /// Show `image` as a large preview (see Notification::preview).
        #[serde(default)]
        preview: bool,
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
    /// Send files to a Bluetooth device over OBEX Object Push (prismd, obexd).
    BluetoothSend { address: String, files: Vec<PathBuf> },

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

    /// Keep the session awake (idle inhibitor): on, off, or toggle (None).
    #[serde(rename = "caffeinate")]
    Caffeinate(Option<bool>),

    /// Game mode (silent, performance power profile, no animations or blur): on, off, toggle.
    #[serde(rename = "game_mode")]
    GameMode(Option<bool>),

    /// Window switcher (Alt+Tab): "next", "prev" or "close".
    #[serde(rename = "switcher")]
    Switcher(String),

    /// The Notes card: "toggle", "open" or "close".
    #[serde(rename = "notes")]
    Notes(String),
    /// Open the first-time setup guide.
    Setup,

    /// Timer and stopwatch.
    #[serde(rename = "timer")]
    Timer(TimerAction),

    /// Phone sync: forwarded to prism-phoned.
    #[serde(rename = "phone")]
    Phone(crate::phone::PhoneCommand),

    /// The Phone card: "toggle", "open" or "close".
    #[serde(rename = "phone_card")]
    PhoneCard(String),

    /// Screenshots and screen recording (SCREEN_CAPTURE_PLAN.md).
    #[serde(rename = "screenshot")]
    Capture(CaptureAction),
}

/// `prism-ctl screenshot ...`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureAction {
    /// Overlay in the last used mode (closes it when open).
    Toggle,
    Close,
    /// Overlay starting in "region", "window" or "screen".
    Open(String),
    /// Whole output at once, no overlay.
    Screen { save: bool, clipboard: bool },
    /// Overlay in recording mode, or stop the running recording.
    Record,
    Stop,
    Status,
    /// Start recording (sent by prism-ui's overlay to prismd).
    Start(crate::capture::RecordJob),
    /// Replay buffer: "save" the last seconds, or turn it "on", "off" or "toggle".
    Replay(String),
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
    /// Put history entry `n` back on the clipboard (prismd owns it, so prism-ctl may exit).
    Copy(usize),
    Clear,
    /// Pin (true) or unpin the entry with this id.
    Pin(String, bool),
    /// Delete the entry with this id.
    Remove(String),
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
    /// Silent mode (no popups except critical ones): on, off, or toggle (None).
    Silent(Option<bool>),
    /// Remove every notification of one app (case-insensitive app name).
    ClearApp(String),
    /// JSON `DaemonFlags` for the control centre.
    State,
    /// The user picked an action of a notification (sent to the app as ActionInvoked).
    Invoke { id: u32, action: String },
    /// The user typed an inline reply (sent to the app as NotificationReplied).
    Reply { id: u32, text: String },
    /// Hide a notification for `secs` seconds, then show it again.
    Snooze { id: u32, secs: u64 },
    /// prism-ui: silent for a reason of its own (screen sharing, a fullscreen window); kept
    /// apart from the user's silent switch.
    AutoSilent(bool),
}

fn normal_urgency() -> u8 {
    1
}

/// Daemon-side switches shown in the control centre.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct DaemonFlags {
    pub silent: bool,
    pub caffeinate: bool,
    #[serde(default)]
    pub game_mode: bool,
    /// Silent because of screen sharing or a fullscreen window (Focus).
    #[serde(default)]
    pub auto_silent: bool,
    #[serde(default)]
    pub timer: TimerState,
}

/// The timer and stopwatch (kept by prismd; times are Unix milliseconds, so every reader
/// counts the same way).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
pub struct TimerState {
    /// A running countdown ends at this time.
    pub end_ms: Option<i64>,
    /// A paused countdown has this much left.
    pub paused_ms: Option<i64>,
    /// The countdown's length (for progress).
    pub total_ms: i64,
    /// The stopwatch runs since this time ...
    pub sw_start_ms: Option<i64>,
    /// ... on top of what it counted before (pauses).
    pub sw_acc_ms: i64,
}

impl TimerState {
    /// Countdown time left, if one is set (running or paused).
    pub fn remaining(&self, now_ms: i64) -> Option<i64> {
        self.end_ms.map(|e| (e - now_ms).max(0)).or(self.paused_ms)
    }

    pub fn stopwatch(&self, now_ms: i64) -> i64 {
        self.sw_acc_ms + self.sw_start_ms.map(|s| (now_ms - s).max(0)).unwrap_or(0)
    }

    /// Something is counting (the view redraws every second).
    pub fn ticking(&self) -> bool {
        self.end_ms.is_some() || self.sw_start_ms.is_some()
    }
}

/// "mm:ss", or "h:mm:ss" from an hour.
pub fn clock_text(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

/// "5m", "90s", "1h30m", "2:30" or a bare number of minutes, in seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    if let Some((m, sec)) = s.split_once(':') {
        return Some(m.parse::<u64>().ok()? * 60 + sec.parse::<u64>().ok()?);
    }
    if let Ok(m) = s.parse::<u64>() {
        return Some(m * 60);
    }
    let (mut total, mut num) = (0u64, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            total += n * match c {
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => return None,
            };
        }
    }
    num.is_empty().then_some(total).filter(|t| *t > 0)
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimerAction {
    /// Count down this many seconds.
    Start(u64),
    /// Add (or take off) seconds.
    Add(i64),
    Pause,
    Resume,
    Cancel,
    /// Start or pause the stopwatch.
    StopwatchToggle,
    StopwatchReset,
    /// JSON `TimerState`.
    State,
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
    /// The Shortcuts editor, or (true) its Window Rules tab.
    Edit(bool),
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

/// Send one command to prismd and wait for its reply (blocking; call off the render path).
pub fn request(cmd: &IpcCommand) -> Result<Option<String>, String> {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(get_socket_path()).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
    let mut payload = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    payload.push('\n');
    stream.write_all(payload.as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
    match serde_json::from_str::<IpcResponse>(&line).map_err(|e| e.to_string())? {
        IpcResponse::Ok(v) => Ok(v),
        IpcResponse::Error(e) => Err(e),
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m"), Some(300));
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("2:30"), Some(150));
        assert_eq!(parse_duration("10"), Some(600));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(clock_text(61_000), "01:01");
        assert_eq!(clock_text(3_600_000), "1:00:00");
    }
}

/// A download being written in the Downloads folder (prismd's downloads watcher).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DownloadProgress {
    pub name: String,
    /// Bytes on disk so far.
    pub bytes: u64,
    /// Bytes per second over the last few seconds.
    pub rate: u64,
}
