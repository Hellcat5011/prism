use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: u32,

    #[serde(default)]
    pub manage_idle: bool,

    #[serde(default = "default_window_radius")]
    pub window_radius: u32,

    #[serde(default = "default_bg_opacity")]
    pub background_opacity: f32,

    #[serde(default = "default_true")]
    pub blur_enabled: bool,

    #[serde(default = "default_blur_strength")]
    pub blur_strength: f32,

    /// Soft drop shadow under modal cards (drawn on an unblurred surface below the card).
    #[serde(default = "default_true")]
    pub shadows_enabled: bool,

    #[serde(default)]
    pub blue_light_enabled: bool,

    #[serde(default = "default_blue_light_mode")]
    pub blue_light_mode: String,

    #[serde(default = "default_night_start")]
    pub blue_light_night_start: String,

    #[serde(default = "default_night_end")]
    pub blue_light_night_end: String,

    #[serde(default = "default_day_temp")]
    pub blue_light_day_temp: u32,

    #[serde(default = "default_night_temp")]
    pub blue_light_night_temp: u32,

    #[serde(default = "default_true")]
    pub search_enable_apps: bool,

    #[serde(default = "default_true")]
    pub search_enable_files: bool,

    #[serde(default = "default_true")]
    pub search_enable_folders: bool,

    #[serde(default = "default_alignment")]
    pub lockscreen_alignment: String,

    #[serde(default = "default_true")]
    pub remember_last_user: bool,

    #[serde(default = "default_session")]
    pub default_session: String,

    #[serde(default = "default_true")]
    pub show_lockscreen_session_controls: bool,
    /// Lock screen design: classic, stencil, horizon, sentence, aperture or tide.
    #[serde(default = "default_lockscreen_theme")]
    pub lockscreen_theme: String,
    /// Greeter design: one of the lock screen designs, or "mirror" to follow the lock screen.
    #[serde(default = "default_greeter_theme")]
    pub greeter_theme: String,
    /// Launcher design: classic, slices, spectrum or deck.
    #[serde(default = "default_launcher_theme")]
    pub launcher_theme: String,
    /// Power menu design: glide, split, shutters, orbit or cards.
    #[serde(default = "default_power_theme")]
    pub power_theme: String,
    /// Logout, Reboot and Shutdown run only after being held for a second.
    #[serde(default = "default_true")]
    pub power_hold_to_confirm: bool,
    /// Actions the power menu offers.
    #[serde(default = "default_true")]
    pub power_lock: bool,
    #[serde(default = "default_true")]
    pub power_logout: bool,
    #[serde(default = "default_true")]
    pub power_suspend: bool,
    #[serde(default = "default_true")]
    pub power_sleep: bool,
    #[serde(default = "default_true")]
    pub power_hibernate: bool,
    #[serde(default = "default_true")]
    pub power_reboot: bool,
    #[serde(default = "default_true")]
    pub power_shutdown: bool,

    /// "prism" (built-in, prism-ui draws the background), "prism-video" (built-in, playing
    /// `wallpaper_video`), "awww" or "swww".
    #[serde(default = "default_wallpaper_daemon")]
    pub wallpaper_daemon: String,
    /// The video the "prism-video" daemon plays.
    #[serde(default)]
    pub wallpaper_video: String,
    /// The daemon in use before the video one: choosing a picture goes back to it.
    #[serde(default = "default_wallpaper_daemon")]
    pub wallpaper_still_daemon: String,
    /// Built-in daemon transition: random, fade, wipe, grow or none.
    #[serde(default = "default_wallpaper_transition")]
    pub wallpaper_transition: String,

    #[serde(default = "default_wallpaper_dir")]
    pub wallpaper_dir: String,
    /// Applying a wallpaper offers the colour filters first.
    #[serde(default = "default_wallpaper_filters_enabled")]
    pub wallpaper_filters_enabled: bool,

    #[serde(default = "default_matugen_mode")]
    pub matugen_mode: String,

    #[serde(default = "default_matugen_scheme")]
    pub matugen_scheme: String,

    #[serde(default)]
    pub search_include_hidden: bool,

    #[serde(default = "default_max_results")]
    pub search_max_file_results: u32,

    #[serde(default)]
    pub depth_enabled: bool,

    #[serde(default = "default_depth_threshold")]
    pub depth_threshold: u32,

    #[serde(default = "default_depth_feather")]
    pub depth_feather: u32,

    #[serde(default = "default_true")]
    pub widget_media_visible: bool,

    #[serde(default = "default_true")]
    pub widget_tray_visible: bool,

    #[serde(default = "default_true")]
    pub widget_clock_visible: bool,

    #[serde(default = "default_true")]
    pub widget_calendar_visible: bool,

    #[serde(default)]
    pub desktop_layout: DesktopLayoutConfig,
    /// Play a sound when a notification arrives.
    #[serde(default = "default_true")]
    pub notification_sound: bool,
    /// The sound to play; empty for Prism's built-in chime.
    #[serde(default)]
    pub notification_sound_file: String,
    /// Sound files the user added to the list of notification sounds.
    #[serde(default)]
    pub notification_sounds: Vec<String>,
    /// The panel along a screen edge (Settings > Bar).
    #[serde(default)]
    pub bar: BarConfig,
    /// The app dock (Settings > Dock). Pinned apps live in dock.json next to this file.
    #[serde(default)]
    pub dock: DockConfig,

    // ── Global UI (applies to every Prism card) ────────────────────────
    /// Enter/exit and in-card animations. Off = instant.
    #[serde(default = "default_true")]
    pub animations_enabled: bool,
    /// Multiplier on every animation duration (1.0 = QML timings, 2.0 = twice as fast).
    #[serde(default = "default_animation_speed")]
    pub animation_speed: f32,
    /// Dim the screen behind modal cards.
    #[serde(default = "default_true")]
    pub dim_enabled: bool,
    #[serde(default = "default_dim_opacity")]
    pub dim_opacity: f32,
    /// Font family for all UI text ("" or "System Default" = Inter).
    #[serde(default)]
    pub global_font: String,

    // ── Settings that existed in the QML shell ─────────────────────────
    #[serde(default = "default_depth_device")]
    pub depth_device: String,
    #[serde(default = "default_true")]
    pub depth_auto_generate: bool,
    #[serde(default = "default_true")]
    pub depth_pregenerate: bool,
    #[serde(default)]
    pub blue_light_manual_on: bool,
    #[serde(default)]
    pub blue_light_latitude: String,
    #[serde(default)]
    pub blue_light_longitude: String,
    #[serde(default)]
    pub blue_light_use_ip_location: bool,
    #[serde(default = "default_transition_minutes")]
    pub blue_light_transition_minutes: u32,
    #[serde(default = "default_section_order")]
    pub search_section_order: String,
    #[serde(default = "default_max_folder_results")]
    pub search_max_folder_results: u32,
    /// Where notification popups appear: top-left, top-center, top-right, bottom-left,
    /// bottom-center or bottom-right.
    #[serde(default = "default_popup_position")]
    pub notification_popup_position: String,
    /// Where the notification center opens: left, center or right.
    #[serde(default = "default_nc_position")]
    pub notification_center_position: String,
    /// Interface scale: 0 = automatic (screen height / 1440), otherwise a fixed factor.
    #[serde(default)]
    pub ui_scale: f32,

    // ── Phone sync (KDE Connect protocol, run by prism-phoned) ──
    /// prismd runs prism-phoned while this is on.
    #[serde(default)]
    pub phone_enabled: bool,
    /// Name shown on the phone; empty = the host name.
    #[serde(default)]
    pub phone_device_name: String,
    /// The phone may move the pointer and type (KDE Connect remote input, presenter).
    #[serde(default = "default_true")]
    pub phone_allow_input: bool,
    /// Remote input also while the session is locked.
    #[serde(default)]
    pub phone_input_when_locked: bool,
    #[serde(default = "default_true")]
    pub phone_clipboard_sync: bool,
    /// The phone shows and controls the PC's media players.
    #[serde(default = "default_true")]
    pub phone_media_control: bool,
    #[serde(default = "default_true")]
    pub phone_notifications: bool,
    #[serde(default = "default_true")]
    pub phone_notification_popups: bool,
    /// Received files land here (`~` is expanded).
    #[serde(default = "default_phone_download_dir")]
    pub phone_download_dir: String,
    #[serde(default)]
    pub phone_open_received: bool,
    /// Tray icon (StatusNotifierItem) that opens the Phone card.
    #[serde(default)]
    pub phone_tray: bool,

    // ── Screen capture (prism-ui screenshots, prism-rec recordings) ──
    #[serde(default = "default_screenshot_dir")]
    pub capture_screenshot_dir: String,
    #[serde(default = "default_true")]
    pub capture_copy_to_clipboard: bool,
    /// Include the mouse pointer in screenshots and recordings.
    #[serde(default)]
    pub capture_cursor: bool,
    #[serde(default = "default_recording_dir")]
    pub capture_recording_dir: String,
    /// "mkv" or "mp4" (fragmented, crash-safe).
    #[serde(default = "default_container")]
    pub capture_container: String,
    /// "auto", "h264", "hevc" or "av1".
    #[serde(default = "default_codec")]
    pub capture_codec: String,
    /// "low", "medium", "high" or "lossless".
    #[serde(default = "default_quality")]
    pub capture_quality: String,
    #[serde(default = "default_capture_fps")]
    pub capture_fps: u32,
    /// Record desktop audio (default sink monitor).
    #[serde(default)]
    pub capture_audio_desktop: bool,
    /// Record the microphone (default source).
    #[serde(default)]
    pub capture_audio_mic: bool,
    /// Last selection kind in the capture overlay: "region", "window" or "screen".
    #[serde(default = "default_capture_mode")]
    pub capture_last_mode: String,

    // ── Services (built-in replacements for helper daemons) ──
    /// Serve MPD as an MPRIS player (org.mpris.MediaPlayer2.mpd), replacing mpd-mpris.
    #[serde(default)]
    pub mpd_mpris_enabled: bool,
    /// MPD host name or socket path (a path starts with '/').
    #[serde(default = "default_mpd_host")]
    pub mpd_host: String,
    #[serde(default = "default_mpd_port")]
    pub mpd_port: u16,
    #[serde(default)]
    pub mpd_password: String,
    /// Accept files pushed over Bluetooth (OBEX agent for obexd), replacing blueman's.
    #[serde(default)]
    pub bluetooth_receive_files: bool,
    #[serde(default = "default_download_dir")]
    pub bluetooth_download_dir: String,
    /// prism-ui is the polkit authentication agent (replaces polkit-gnome).
    #[serde(default)]
    pub polkit_agent_enabled: bool,
    /// Notify when package updates are available (Arch: checkupdates, yay/paru -Qua).
    #[serde(default = "default_true")]
    pub update_notifications: bool,
    /// Tray icon while updates are available.
    #[serde(default = "default_true")]
    pub updates_tray: bool,
    /// Tray icon while a removable drive is plugged in.
    #[serde(default = "default_true")]
    pub drives_tray: bool,
    /// prism-ui shows gnome-keyring's password prompts (replaces gcr-prompter).
    #[serde(default = "default_true")]
    pub keyring_prompter_enabled: bool,
    /// prismd announces removable drives (replaces udiskie).
    #[serde(default = "default_true")]
    pub drives_enabled: bool,
    /// ... and mounts them as they are plugged in.
    #[serde(default = "default_true")]
    pub drives_automount: bool,
    /// prism-ui is the xdg-desktop-portal backend for screenshots and screen sharing.
    #[serde(default = "default_true")]
    pub portal_enabled: bool,
    /// prism-ui is a NetworkManager secret agent: asks for Wi-Fi / VPN passwords (replaces nm-applet).
    #[serde(default)]
    pub network_agent_enabled: bool,

    // ── Displays (wlr-output-management; prismd applies these at login and on change) ──
    #[serde(default)]
    pub outputs: Vec<crate::outputs::OutputSettings>,
    /// The output Settings > Display edits (empty: the first).
    #[serde(default)]
    pub display_output: String,

    /// Control Centre tiles shown: network, bluetooth, vpn, notifications, caffeine.
    #[serde(default = "default_nc_tiles")]
    pub nc_tiles: Vec<String>,
    /// The secret agent stores what is typed in VPN prompts in the connection.
    #[serde(default)]
    pub vpn_save_username: bool,
    #[serde(default)]
    pub vpn_save_password: bool,
    /// Tray icons served by prism-ui (StatusNotifierItem).
    #[serde(default)]
    pub network_tray: bool,
    #[serde(default)]
    pub bluetooth_tray: bool,
}

pub fn default_nc_tiles() -> Vec<String> {
    ["network", "bluetooth", "vpn", "notifications", "caffeine", "game"].iter().map(|s| s.to_string()).collect()
}

fn default_download_dir() -> String {
    "~/Downloads".to_string()
}

fn default_mpd_host() -> String {
    "127.0.0.1".to_string()
}
fn default_mpd_port() -> u16 {
    6600
}

fn default_screenshot_dir() -> String {
    "~/Pictures/Screenshots".to_string()
}
fn default_recording_dir() -> String {
    "~/Videos/Recordings".to_string()
}
fn default_container() -> String {
    "mkv".to_string()
}
fn default_codec() -> String {
    "auto".to_string()
}
fn default_quality() -> String {
    "medium".to_string()
}
fn default_capture_fps() -> u32 {
    60
}
fn default_capture_mode() -> String {
    "region".to_string()
}

/// `~/x` to `$HOME/x`.
pub fn expand_home(p: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(home).join(rest),
        None if p == "~" => std::path::PathBuf::from(home),
        None => std::path::PathBuf::from(p),
    }
}

fn default_phone_download_dir() -> String {
    "~/Downloads".to_string()
}

fn default_popup_position() -> String {
    "top-left".to_string()
}

fn default_nc_position() -> String {
    "left".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortcutsConfig {
    #[serde(default = "default_true")]
    pub launcher: bool,
    #[serde(default = "default_true")]
    pub notifications: bool,
    #[serde(default = "default_true")]
    pub settings: bool,
    #[serde(default = "default_true")]
    pub power: bool,
    #[serde(default = "default_true")]
    pub phone: bool,
}

impl Default for ShortcutsConfig {
    fn default() -> Self {
        Self {
            launcher: true,
            notifications: true,
            settings: true,
            power: true,
            phone: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetPosition {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub transparent_bg: bool,
    #[serde(default)]
    pub send_to_background: bool,
    #[serde(default)]
    pub vertical: bool,
    #[serde(default)]
    pub hide_date: bool,
    #[serde(default)]
    pub font_family: String,
    #[serde(default)]
    pub shortcuts: ShortcutsConfig,
}

impl WidgetPosition {
    pub fn resolve(&self, screen_w: f64, screen_h: f64) -> (f64, f64, f64, f64) {
        let x = if self.x > 0.0 && self.x <= 1.0 {
            self.x * screen_w
        } else if self.x < 0.0 {
            screen_w + self.x
        } else {
            self.x
        };
        let y = if self.y > 0.0 && self.y <= 1.0 {
            self.y * screen_h
        } else if self.y < 0.0 {
            screen_h + self.y
        } else {
            self.y
        };
        let w = if self.w > 0.0 && self.w <= 1.0 { self.w * screen_w } else { self.w };
        let h = if self.h > 0.0 && self.h <= 1.0 { self.h * screen_h } else { self.h };
        (x, y, w, h)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopLayoutConfig {
    #[serde(default = "default_clock_widget")]
    pub clock: WidgetPosition,
    #[serde(default = "default_analogclock_widget")]
    pub analogclock: WidgetPosition,
    #[serde(default = "default_calendar_widget")]
    pub calendar: WidgetPosition,
    #[serde(default = "default_mpris_widget")]
    pub mpris: WidgetPosition,
    #[serde(default = "default_tray_widget")]
    pub tray: WidgetPosition,
    #[serde(default = "default_shortcuts_widget")]
    pub shortcuts: WidgetPosition,
    #[serde(default = "default_phone_widget")]
    pub phone: WidgetPosition,
    #[serde(default = "default_batteries_widget")]
    pub batteries: WidgetPosition,
    #[serde(default = "default_cat_widget")]
    pub cat: WidgetPosition,
}

/// The Cat widget at 100%: the size its drawing is designed for, in pixels.
pub const CAT_SIZE: (f64, f64) = (320.0, 290.0);

impl DesktopLayoutConfig {
    /// Show or hide the Cat widget. It always arrives at 100%, whatever size it had before.
    pub fn set_cat_visible(&mut self, visible: bool) {
        if visible && !self.cat.visible {
            (self.cat.w, self.cat.h) = CAT_SIZE;
        }
        self.cat.visible = visible;
    }
}

/// Cat widget (a black cat asleep on the wallpaper until hovered): hidden until enabled.
pub fn default_cat_widget() -> WidgetPosition {
    WidgetPosition { x: 30.0, y: -640.0, w: CAT_SIZE.0, h: CAT_SIZE.1, visible: false, transparent_bg: true, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

/// Batteries widget (connected headphones, speakers, controllers, ...): hidden until enabled.
pub fn default_batteries_widget() -> WidgetPosition {
    WidgetPosition { x: 30.0, y: -390.0, w: 320.0, h: 180.0, visible: false, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

/// Phone widget (battery, status, media, find my phone): hidden until enabled.
pub fn default_phone_widget() -> WidgetPosition {
    WidgetPosition { x: 30.0, y: -190.0, w: 360.0, h: 140.0, visible: false, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

// Defaults mirror the QML DesktopLayout.getDefaultGeometry (negative = from the right/bottom
// edge; values in (0, 1] are fractions of the screen, which is what edit mode saves).
pub fn default_clock_widget() -> WidgetPosition {
    WidgetPosition { x: -390.0, y: -560.0, w: 360.0, h: 180.0, visible: true, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

pub fn default_analogclock_widget() -> WidgetPosition {
    WidgetPosition { x: 30.0, y: 30.0, w: 200.0, h: 200.0, visible: false, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

pub fn default_calendar_widget() -> WidgetPosition {
    WidgetPosition { x: -390.0, y: -350.0, w: 360.0, h: 320.0, visible: true, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

pub fn default_mpris_widget() -> WidgetPosition {
    WidgetPosition { x: -430.0, y: 30.0, w: 400.0, h: 170.0, visible: true, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

pub fn default_tray_widget() -> WidgetPosition {
    WidgetPosition { x: -80.0, y: 210.0, w: 50.0, h: 50.0, visible: true, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

pub fn default_shortcuts_widget() -> WidgetPosition {
    WidgetPosition { x: -270.0, y: 270.0, w: 240.0, h: 64.0, visible: false, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() }
}

impl DesktopLayoutConfig {
    pub fn default_for_screen(sw: f64, sh: f64) -> Self {
        Self {
            mpris: WidgetPosition {
                x: (sw - 430.0).max(0.0),
                y: 30.0,
                w: 400.0,
                h: 160.0,
                visible: true,
                transparent_bg: false,
                send_to_background: false,
                vertical: false,
                hide_date: false,
                font_family: String::new(),
                shortcuts: ShortcutsConfig::default(),
            },
            tray: WidgetPosition {
                x: (sw - 30.0 - 140.0).max(0.0),
                y: 206.0,
                w: 140.0,
                h: 44.0,
                visible: true,
                transparent_bg: false,
                send_to_background: false,
                vertical: false,
                hide_date: false,
                font_family: String::new(),
                shortcuts: ShortcutsConfig::default(),
            },
            shortcuts: WidgetPosition {
                x: (sw - 30.0 - 240.0).max(0.0),
                y: 266.0,
                w: 240.0,
                h: 60.0,
                visible: true,
                transparent_bg: false,
                send_to_background: false,
                vertical: false,
                hide_date: false,
                font_family: String::new(),
                shortcuts: ShortcutsConfig::default(),
            },
            clock: WidgetPosition {
                x: (sw - 390.0).max(0.0),
                y: (sh - 520.0).max(0.0),
                w: 360.0,
                h: 160.0,
                visible: true,
                transparent_bg: false,
                send_to_background: false,
                vertical: false,
                hide_date: false,
                font_family: String::new(),
                shortcuts: ShortcutsConfig::default(),
            },
            calendar: WidgetPosition {
                x: (sw - 390.0).max(0.0),
                y: (sh - 330.0).max(0.0),
                w: 360.0,
                h: 300.0,
                visible: true,
                transparent_bg: false,
                send_to_background: false,
                vertical: false,
                hide_date: false,
                font_family: String::new(),
                shortcuts: ShortcutsConfig::default(),
            },
            analogclock: WidgetPosition {
                x: 30.0,
                y: 30.0,
                w: 200.0,
                h: 200.0,
                visible: false,
                transparent_bg: false,
                send_to_background: false,
                vertical: false,
                hide_date: false,
                font_family: String::new(),
                shortcuts: ShortcutsConfig::default(),
            },
            phone: default_phone_widget(),
            batteries: default_batteries_widget(),
            cat: default_cat_widget(),
        }
    }
}

impl Default for DesktopLayoutConfig {
    fn default() -> Self {
        Self {
            clock: default_clock_widget(),
            analogclock: default_analogclock_widget(),
            calendar: default_calendar_widget(),
            mpris: default_mpris_widget(),
            tray: default_tray_widget(),
            shortcuts: default_shortcuts_widget(),
            phone: default_phone_widget(),
            batteries: default_batteries_widget(),
            cat: default_cat_widget(),
        }
    }
}

fn default_idle_timeout() -> u32 { 5 }
fn default_animation_speed() -> f32 { 1.0 }
fn default_depth_device() -> String { "cpu".to_string() }
fn default_transition_minutes() -> u32 { 30 }
fn default_section_order() -> String { "apps,folders,files".to_string() }
fn default_max_folder_results() -> u32 { 10 }
fn default_dim_opacity() -> f32 { 0.38 }
fn default_true() -> bool { true }
fn default_alignment() -> String { "left".to_string() }
fn default_session() -> String { "mango".to_string() }
fn default_window_radius() -> u32 { 16 }
fn default_bg_opacity() -> f32 { 0.85 }
fn default_blur_strength() -> f32 { crate::blur::DEFAULT_LEVEL as f32 }
fn default_blue_light_mode() -> String { "fixed_time".to_string() }
fn default_night_start() -> String { "20:00".to_string() }
fn default_night_end() -> String { "06:00".to_string() }
fn default_day_temp() -> u32 { 6500 }
fn default_night_temp() -> u32 { 4500 }
fn default_lockscreen_theme() -> String { "classic".to_string() }
fn default_greeter_theme() -> String { "mirror".to_string() }
fn default_launcher_theme() -> String { "classic".to_string() }
fn default_power_theme() -> String { "glide".to_string() }
fn default_wallpaper_daemon() -> String { "prism".to_string() }
fn default_wallpaper_transition() -> String { "random".to_string() }
fn default_wallpaper_dir() -> String { "~/Pictures/Wallpapers".to_string() }
fn default_wallpaper_filters_enabled() -> bool { true }
fn default_matugen_mode() -> String { "smart".to_string() }
fn default_matugen_scheme() -> String { "scheme-smart".to_string() }
fn default_max_results() -> u32 { 20 }
fn default_depth_threshold() -> u32 { 30 }
fn default_depth_feather() -> u32 { 8 }

/// Every module the bar can show: config id and the name shown in Settings.
pub const BAR_MODULES: [(&str, &str); 9] = [
    ("launcher", "Launcher"),
    ("workspaces", "Workspaces"),
    ("mpris", "Media Player"),
    ("clock", "Clock"),
    ("sysmon", "System Monitor"),
    ("updates", "Updates"),
    ("tray", "System Tray"),
    ("notifications", "Notifications"),
    ("power", "Power"),
];

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct BarConfig {
    pub enabled: bool,
    /// top, bottom, left or right.
    pub position: String,
    /// full (edge to edge), floating (one rounded panel) or islands (one pill per section).
    pub style: String,
    pub thickness: u32,
    /// Gap to the screen edges in the floating and islands styles.
    pub gap: u32,
    pub radius: u32,
    pub opacity: f32,
    /// Windows stay clear of the bar.
    pub reserve_space: bool,
    pub left: Vec<String>,
    pub center: Vec<String>,
    pub right: Vec<String>,
    /// numbers, dots or pills.
    pub workspace_style: String,
    pub workspace_hide_empty: bool,
    /// strftime format.
    pub clock_format: String,
    pub clock_date: bool,
    pub sysmon_cpu: bool,
    pub sysmon_memory: bool,
    pub sysmon_temperature: bool,
    pub media_title: bool,
    /// Updates module: what to run in a terminal when it is clicked (empty: yay, paru or
    /// pacman, whichever is installed).
    pub updates_command: String,
    /// Hours between update checks.
    pub updates_interval_hours: u32,
}

impl Default for BarConfig {
    fn default() -> Self {
        let list = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect();
        BarConfig {
            enabled: false,
            position: "top".into(),
            style: "floating".into(),
            thickness: 40,
            gap: 8,
            radius: 14,
            opacity: 0.8,
            reserve_space: true,
            left: list(&["launcher", "workspaces"]),
            center: list(&["clock"]),
            right: list(&["mpris", "sysmon", "updates", "tray", "notifications", "power"]),
            workspace_style: "numbers".into(),
            workspace_hide_empty: false,
            clock_format: "%H:%M".into(),
            clock_date: true,
            sysmon_cpu: true,
            sysmon_memory: true,
            sysmon_temperature: false,
            media_title: true,
            updates_command: String::new(),
            updates_interval_hours: 3,
        }
    }
}

impl BarConfig {
    /// The modules of a section: 0 left, 1 center, 2 right.
    pub fn section(&self, section: usize) -> &[String] {
        match section {
            0 => &self.left,
            1 => &self.center,
            _ => &self.right,
        }
    }

    fn section_mut(&mut self, section: usize) -> &mut Vec<String> {
        match section {
            0 => &mut self.left,
            1 => &mut self.center,
            _ => &mut self.right,
        }
    }

    /// Move `id` to place `index` of `section`; None turns the module off.
    pub fn place(&mut self, id: &str, target: Option<(usize, usize)>) {
        for s in 0..3 {
            self.section_mut(s).retain(|m| m != id);
        }
        if let Some((section, index)) = target {
            let list = self.section_mut(section);
            let index = index.min(list.len());
            list.insert(index, id.to_string());
        }
    }

    /// The section a module is in; None while it is off.
    pub fn section_of(&self, id: &str) -> Option<usize> {
        (0..3).find(|&s| self.section(s).iter().any(|m| m == id))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct DockConfig {
    pub enabled: bool,
    /// bottom, left or right.
    pub position: String,
    pub icon_size: u32,
    /// off, always, or smart (hidden only while windows are on screen).
    pub autohide: String,
    /// Windows may go behind the dock instead of staying clear of it.
    pub windows_behind: bool,
    pub gap: u32,
    pub opacity: f32,
}

impl Default for DockConfig {
    fn default() -> Self {
        DockConfig {
            enabled: false,
            position: "bottom".into(),
            icon_size: 48,
            autohide: "off".into(),
            windows_behind: false,
            gap: 8,
            opacity: 0.8,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout: default_idle_timeout(),
            manage_idle: false,
            window_radius: default_window_radius(),
            background_opacity: default_bg_opacity(),
            blur_enabled: default_true(),
            blur_strength: default_blur_strength(),
            shadows_enabled: default_true(),
            blue_light_enabled: false,
            blue_light_mode: default_blue_light_mode(),
            blue_light_night_start: default_night_start(),
            blue_light_night_end: default_night_end(),
            blue_light_day_temp: default_day_temp(),
            blue_light_night_temp: default_night_temp(),
            search_enable_apps: default_true(),
            search_enable_files: default_true(),
            search_enable_folders: default_true(),
            lockscreen_alignment: default_alignment(),
            remember_last_user: default_true(),
            default_session: default_session(),
            show_lockscreen_session_controls: default_true(),
            lockscreen_theme: default_lockscreen_theme(),
            greeter_theme: default_greeter_theme(),
            launcher_theme: default_launcher_theme(),
            power_theme: default_power_theme(),
            power_hold_to_confirm: default_true(),
            power_lock: default_true(),
            power_logout: default_true(),
            power_suspend: default_true(),
            power_sleep: default_true(),
            power_hibernate: default_true(),
            power_reboot: default_true(),
            power_shutdown: default_true(),
            wallpaper_daemon: default_wallpaper_daemon(),
            wallpaper_video: String::new(),
            wallpaper_still_daemon: default_wallpaper_daemon(),
            wallpaper_transition: default_wallpaper_transition(),
            wallpaper_dir: default_wallpaper_dir(),
            wallpaper_filters_enabled: default_wallpaper_filters_enabled(),
            matugen_mode: default_matugen_mode(),
            matugen_scheme: default_matugen_scheme(),
            search_include_hidden: false,
            search_max_file_results: default_max_results(),
            depth_enabled: false,
            depth_threshold: default_depth_threshold(),
            depth_feather: default_depth_feather(),
            widget_media_visible: true,
            widget_tray_visible: true,
            widget_clock_visible: true,
            widget_calendar_visible: true,
            desktop_layout: DesktopLayoutConfig::default(),
            notification_sound: true,
            notification_sound_file: String::new(),
            notification_sounds: Vec::new(),
            bar: BarConfig::default(),
            dock: DockConfig::default(),
            animations_enabled: true,
            animation_speed: default_animation_speed(),
            dim_enabled: true,
            dim_opacity: default_dim_opacity(),
            global_font: String::new(),
            depth_device: default_depth_device(),
            depth_auto_generate: true,
            depth_pregenerate: true,
            blue_light_manual_on: false,
            blue_light_latitude: String::new(),
            blue_light_longitude: String::new(),
            blue_light_use_ip_location: false,
            blue_light_transition_minutes: default_transition_minutes(),
            search_section_order: default_section_order(),
            search_max_folder_results: default_max_folder_results(),
            notification_popup_position: default_popup_position(),
            notification_center_position: default_nc_position(),
            ui_scale: 0.0,
            phone_enabled: false,
            phone_device_name: String::new(),
            phone_allow_input: true,
            phone_input_when_locked: false,
            phone_clipboard_sync: true,
            phone_media_control: true,
            phone_notifications: true,
            phone_notification_popups: true,
            phone_download_dir: default_phone_download_dir(),
            phone_open_received: false,
            phone_tray: false,
            capture_screenshot_dir: default_screenshot_dir(),
            capture_copy_to_clipboard: true,
            capture_cursor: false,
            capture_recording_dir: default_recording_dir(),
            capture_container: default_container(),
            capture_codec: default_codec(),
            capture_quality: default_quality(),
            capture_fps: default_capture_fps(),
            capture_audio_desktop: false,
            capture_audio_mic: false,
            capture_last_mode: default_capture_mode(),
            mpd_mpris_enabled: false,
            mpd_host: default_mpd_host(),
            mpd_port: default_mpd_port(),
            mpd_password: String::new(),
            bluetooth_receive_files: false,
            bluetooth_download_dir: default_download_dir(),
            polkit_agent_enabled: false,
            portal_enabled: true,
            keyring_prompter_enabled: true,
            update_notifications: true,
            updates_tray: true,
            drives_tray: true,
            drives_enabled: true,
            drives_automount: true,
            network_agent_enabled: false,
            outputs: Vec::new(),
            display_output: String::new(),
            nc_tiles: default_nc_tiles(),
            vpn_save_username: false,
            vpn_save_password: false,
            network_tray: false,
            bluetooth_tray: false,
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
            PathBuf::from(config_home).join("prism").join("config.json")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".config").join("prism").join("config.json")
        } else {
            PathBuf::from("config.json")
        }
    }

    pub fn load() -> Self {
        Self::try_load().unwrap_or_default()
    }

    /// Like `load`, but reports a file that exists and fails to parse instead of silently
    /// returning defaults. A missing file is `Ok(default)`. Live reloaders use this to keep
    /// the previous config when the file is momentarily invalid.
    pub fn try_load() -> Result<Self, String> {
        let path = Self::config_path();
        match fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The daemon that shows pictures: the current one, or the last one used before a video.
    pub fn still_daemon(&self) -> &str {
        match self.wallpaper_daemon.as_str() {
            "prism-video" if self.wallpaper_still_daemon != "prism-video" && !self.wallpaper_still_daemon.is_empty() => &self.wallpaper_still_daemon,
            "prism-video" => "prism",
            daemon => daemon,
        }
    }

    /// The video wallpaper in use, if the built-in video daemon is selected and has one.
    pub fn video_wallpaper(&self) -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").unwrap_or_default();
        (self.wallpaper_daemon == "prism-video" && !self.wallpaper_video.trim().is_empty())
            .then(|| crate::sound::expand(self.wallpaper_video.trim(), &home))
            .filter(|p| p.is_file())
    }

    /// What the greeter (another user, no access to this config) reads from its state directory.
    fn greeter_snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "lockscreenAlignment": self.lockscreen_alignment,
            "theme": if self.greeter_theme == "mirror" { &self.lockscreen_theme } else { &self.greeter_theme },
            "rememberLastUser": self.remember_last_user,
            "defaultSession": self.default_session,
            "wallpaperVideo": self.video_wallpaper().is_some(),
        })
    }

    pub fn sync_greeter_snapshot(&self) {
        let state_dirs = [
            "/var/lib/greetd/prism",
            "/var/lib/greetd/quickshell-greeter",
        ];
        let snapshot = self.greeter_snapshot();
        for dir_str in &state_dirs {
            let dir = Path::new(dir_str);
            if dir.is_dir() {
                // The greeter runs as another user: give it the matugen palette too.
                let colors = crate::theme::Theme::source_path();
                if colors.exists() {
                    let dst = dir.join("colors.json");
                    if fs::copy(&colors, &dst).is_ok() {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o644));
                        }
                    }
                }
                let target = dir.join("config-snapshot.json");
                if let Ok(content) = serde_json::to_string_pretty(&snapshot) {
                    let _ = fs::write(&target, content);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o644));
                    }
                }
            }
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_string_pretty(self)?;
        // Write a sibling temp file and rename it over the target, so watchers (prismd,
        // the desktop) never read a half-written file.
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(format!(".{}.tmp", std::process::id()));
        let tmp = path.with_file_name(tmp_name);
        fs::write(&tmp, serialized)?;
        if let Err(e) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.sync_greeter_snapshot();
        Ok(())
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::config_path())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn greeter_theme_mirrors_the_lock_screen_by_default() {
        let mut c = Config { lockscreen_theme: "tide".into(), ..Config::default() };
        assert_eq!(c.greeter_theme, "mirror");
        assert_eq!(c.greeter_snapshot()["theme"], "tide");
        c.greeter_theme = "horizon".into();
        assert_eq!(c.greeter_snapshot()["theme"], "horizon");
    }

    use super::*;

    #[test]
    fn the_cat_is_added_at_full_size() {
        let mut layout = DesktopLayoutConfig::default();
        // Resized earlier, as a fraction of the screen, then removed.
        (layout.cat.w, layout.cat.h) = (0.1, 0.15);
        layout.set_cat_visible(true);
        assert!(layout.cat.visible);
        assert_eq!((layout.cat.w, layout.cat.h), CAT_SIZE);
        // Resizing it afterwards sticks, also when it is shown again without being removed.
        (layout.cat.w, layout.cat.h) = (0.2, 0.2);
        layout.set_cat_visible(true);
        assert_eq!((layout.cat.w, layout.cat.h), (0.2, 0.2));
        layout.set_cat_visible(false);
        assert!(!layout.cat.visible);
        assert_eq!((layout.cat.w, layout.cat.h), (0.2, 0.2));
    }

    #[test]
    fn a_picture_goes_back_to_the_daemon_used_before_the_video() {
        let mut c = Config::default();
        assert_eq!(c.still_daemon(), "prism");
        c.wallpaper_daemon = "awww".into();
        assert_eq!(c.still_daemon(), "awww");
        c.wallpaper_daemon = "prism-video".into();
        c.wallpaper_still_daemon = "swww".into();
        assert_eq!(c.still_daemon(), "swww");
        c.wallpaper_still_daemon = "prism-video".into();
        assert_eq!(c.still_daemon(), "prism");
    }

    #[test]
    fn a_bar_module_is_in_one_place_at_a_time() {
        let mut bar = BarConfig::default();
        bar.place("clock", Some((0, 1)));
        assert_eq!(bar.left, ["launcher", "clock", "workspaces"]);
        assert!(bar.center.is_empty());
        assert_eq!(bar.section_of("clock"), Some(0));
        bar.place("clock", None);
        assert_eq!(bar.section_of("clock"), None);
        // Past the end means last.
        bar.place("clock", Some((1, 9)));
        assert_eq!(bar.center, ["clock"]);
    }

    #[test]
    fn configs_from_before_the_bar_and_dock_load_with_both_off() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert!(!cfg.bar.enabled && !cfg.dock.enabled);
        assert_eq!(cfg.bar.thickness, 40);
        let cfg: Config = serde_json::from_str(r#"{"bar":{"enabled":true}}"#).unwrap();
        assert!(cfg.bar.enabled);
        assert_eq!(cfg.bar.position, "top");
    }

    #[test]
    fn test_desktop_layout_defaults() {
        let layout = DesktopLayoutConfig::default();
        assert!(layout.clock.visible);
        assert!(layout.calendar.visible);
        assert!(layout.mpris.visible);
        assert!(layout.tray.visible);
        // Optional widgets start removed, as in QML DesktopLayout.getDefaultGeometry.
        assert!(!layout.analogclock.visible);
        assert!(!layout.shortcuts.visible);
    }

    #[test]
    fn test_widget_position_resolve() {
        let norm = WidgetPosition { x: 0.5, y: 0.25, w: 0.2, h: 0.1, visible: true, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() };
        let (x, y, w, h) = norm.resolve(1920.0, 1080.0);
        assert_eq!(x, 960.0);
        assert_eq!(y, 270.0);
        assert_eq!(w, 384.0);
        assert_eq!(h, 108.0);

        let abs = WidgetPosition { x: 100.0, y: 200.0, w: 300.0, h: 400.0, visible: true, transparent_bg: false, send_to_background: false, vertical: false, hide_date: false, font_family: String::new(), shortcuts: ShortcutsConfig::default() };
        let (x2, y2, w2, h2) = abs.resolve(1920.0, 1080.0);
        assert_eq!(x2, 100.0);
        assert_eq!(y2, 200.0);
        assert_eq!(w2, 300.0);
        assert_eq!(h2, 400.0);
    }

    #[test]
    fn test_config_serde_with_desktop_layout() {
        let json_str = r#"{
            "desktop_layout": {
                "shortcuts": { "x": 100.0, "y": 200.0, "w": 240.0, "h": 60.0, "visible": true }
            }
        }"#;
        let cfg: Config = serde_json::from_str(json_str).expect("deserialize config");
        assert_eq!(cfg.desktop_layout.shortcuts.x, 100.0);
        assert_eq!(cfg.desktop_layout.shortcuts.y, 200.0);
        assert!(cfg.desktop_layout.clock.visible);
    }
}
