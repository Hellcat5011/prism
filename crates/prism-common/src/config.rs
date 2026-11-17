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

    #[serde(default = "default_wallpaper_daemon")]
    pub wallpaper_daemon: String,

    #[serde(default = "default_wallpaper_dir")]
    pub wallpaper_dir: String,

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
}

impl Default for ShortcutsConfig {
    fn default() -> Self {
        Self {
            launcher: true,
            notifications: true,
            settings: true,
            power: true,
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
fn default_wallpaper_daemon() -> String { "awww".to_string() }
fn default_wallpaper_dir() -> String { "~/Pictures/Wallpapers".to_string() }
fn default_matugen_mode() -> String { "smart".to_string() }
fn default_matugen_scheme() -> String { "scheme-smart".to_string() }
fn default_max_results() -> u32 { 20 }
fn default_depth_threshold() -> u32 { 30 }
fn default_depth_feather() -> u32 { 8 }

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout: default_idle_timeout(),
            manage_idle: false,
            window_radius: default_window_radius(),
            background_opacity: default_bg_opacity(),
            blur_enabled: default_true(),
            blur_strength: default_blur_strength(),
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
            wallpaper_daemon: default_wallpaper_daemon(),
            wallpaper_dir: default_wallpaper_dir(),
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

    pub fn sync_greeter_snapshot(&self) {
        let state_dirs = [
            "/var/lib/greetd/prism",
            "/var/lib/greetd/quickshell-greeter",
        ];
        let snapshot = serde_json::json!({
            "lockscreenAlignment": self.lockscreen_alignment,
            "rememberLastUser": self.remember_last_user,
            "defaultSession": self.default_session,
        });
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
    use super::*;

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
