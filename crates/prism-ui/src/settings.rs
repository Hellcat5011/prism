// ─────────────────────────────────────────────────────────────────────────
// settings.rs — the Settings window (port of modules/SettingsWindow.qml)
//
// Built from a table of rows, each bound to a Config field through a getter
// and setter, so every visible control actually reads and writes the config.
// Changes apply at once: each one is written to ~/.config/prism/config.json
// (prismd and the desktop reload it via inotify, cards read it on open) and
// can be undone. Only the fields a change touched are merged into the file,
// so values changed elsewhere meanwhile (edit mode, prism-ctl) are kept.
// Slider drags and text edits count as one change each, when they finish.
//
// Look follows the QML window: sidebar with search and tabs (selected tab
// filled with onPrimaryContainer @0.85), a darker content pane (black @0.25,
// outlineVariant @0.45 border), header with close button, scrolling rows
// (title 15 px medium, subtitle 12 px @0.6), and a persistent footer.
// ─────────────────────────────────────────────────────────────────────────

use std::sync::mpsc::{Receiver, Sender};

use prism_common::{Config, Theme};

use crate::cairo_backend::*;
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, wrapped, Align, TextStyle};
use crate::ui::Style;
use crate::uikit::{self, ButtonKind, Dropdown, Palette};
use crate::waker::Waker;

pub const TABS: [&str; 11] = ["Wallpaper", "Lock Screen", "Greeter", "Display", "Desktop", "UI", "Search", "Notifications", "Phone", "Capture", "Services"];
/// Undo steps kept per Settings session.
const UNDO_LIMIT: usize = 100;

pub const CARD_W: f64 = 860.0;
pub const CARD_H: f64 = 620.0;

const MARGIN: f64 = 12.0;
const SIDEBAR_W: f64 = 220.0;
const PANE_PAD: f64 = 24.0;
const CONTROL_W: f64 = 230.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    EditMode,
    ResetLayout,
    RebuildIndex,
    DepthGenerate,
    DepthClear,
    DepthInstallCpu,
    DepthInstallGpu,
    OpenPhone,
}

/// Title of the row whose subtitle is the live depth-runtime status.
const DEPTH_STATUS_ROW: &str = "Status & Device";

/// Last `wallpaper-depth.py status`, shown as that row's subtitle (QML DepthService).
static DEPTH_STATUS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Set when DEPTH_STATUS or INDEX_LAST changed and the view has not redrawn since.
static DEPTH_STATUS_NEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Title of the file-index row; its subtitle ends with the last index time (QML IndexService).
const INDEX_ROW: &str = "Rebuild File Index";
static INDEX_LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Read the plocate database time in the background (no I/O on the render path).
fn refresh_index_time(waker: Waker) {
    std::thread::spawn(move || {
        *INDEX_LAST.lock().unwrap() = prism_common::files::last_indexed();
        DEPTH_STATUS_NEW.store(true, std::sync::atomic::Ordering::SeqCst);
        waker.wake();
    });
}

/// Query the depth runtime in the background; `waker` redraws when it is known.
fn refresh_depth_status(waker: Waker) {
    std::thread::spawn(move || {
        let text = match prism_common::depth::script()
            .and_then(|s| std::process::Command::new("python3").arg(s).arg("status").output().ok())
            .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        {
            Some(v) => {
                let installed = v["installed"].as_bool().unwrap_or(false);
                let device = if v["gpuInstalled"].as_bool().unwrap_or(false) { "GPU (CUDA)" } else { "CPU" };
                let cache = v["cacheSizeFormatted"].as_str().unwrap_or("0 B");
                if installed {
                    format!("Status: Ready \u{2022} Device: {device} \u{2022} Cache: {cache}")
                } else {
                    format!("Status: Not installed \u{2022} Cache: {cache}")
                }
            }
            None => "Status: wallpaper-depth.py unavailable".into(),
        };
        *DEPTH_STATUS.lock().unwrap() = text;
        DEPTH_STATUS_NEW.store(true, std::sync::atomic::Ordering::SeqCst);
        waker.wake();
    });
}

enum Control {
    Switch(fn(&Config) -> bool, fn(&mut Config, bool)),
    Slider { min: f64, max: f64, step: f64, get: fn(&Config) -> f64, set: fn(&mut Config, f64), fmt: fn(f64) -> String },
    /// Text input; `set` returns false when the value is invalid (it is then not applied).
    /// `browse` adds a button that picks a folder with the system file dialog.
    Text { get: fn(&Config) -> String, set: fn(&mut Config, &str) -> bool, placeholder: &'static str, browse: bool },
    /// (label, value) options; `fonts` renders each option in its own font.
    Combo { options: fn() -> Vec<(String, String)>, get: fn(&Config) -> String, set: fn(&mut Config, &str), fonts: bool },
    Buttons(&'static [(&'static str, Action, ButtonKind)]),
    /// Toggle pills: (label, value) options, any number on.
    Pills { options: &'static [(&'static str, &'static str)], get: fn(&Config) -> Vec<String>, set: fn(&mut Config, Vec<String>) },
    /// Small print at the end of a tab (the row subtitle); not focusable, hidden in search results.
    Note,
    /// Section heading (the row title) grouping the rows after it; not focusable, hidden
    /// in search results and when its section has no visible rows.
    Heading,
}

const LOCK_THEMES: &[(&str, &str)] = &[("Classic", "classic"), ("Stencil", "stencil"), ("Horizon", "horizon"), ("Sentence", "sentence"), ("Aperture", "aperture"), ("Tide", "tide")];
const THEME_FILTER_NOTE: &str = "Every theme shows your current wallpaper, including any wallpaper filter applied to it.";
struct Row {
    tab: usize,
    title: &'static str,
    subtitle: &'static str,
    control: Control,
    visible: fn(&Config) -> bool,
}

/// A row's subtitle; the depth status row shows the last runtime status.
fn row_subtitle(r: &Row) -> std::borrow::Cow<'static, str> {
    if r.title == DEPTH_STATUS_ROW {
        let s = DEPTH_STATUS.lock().unwrap();
        if !s.is_empty() {
            return s.clone().into();
        }
    }
    if r.title == INDEX_ROW {
        if let Some(t) = INDEX_LAST.lock().unwrap().as_ref() {
            return format!("{} \u{2022} Last indexed: {t}", r.subtitle).into();
        }
    }
    r.subtitle.into()
}

impl Control {
    /// The control's value as a comparable string; None for buttons.
    fn value(&self, c: &Config) -> Option<String> {
        match self {
            Control::Switch(get, _) => Some(get(c).to_string()),
            Control::Slider { get, .. } => Some(get(c).to_string()),
            Control::Text { get, .. } | Control::Combo { get, .. } => Some(get(c)),
            Control::Pills { get, .. } => Some(get(c).join(",")),
            Control::Buttons(_) | Control::Heading | Control::Note => None,
        }
    }

    /// Copy this control's value from `from` into `to`.
    fn copy_value(&self, from: &Config, to: &mut Config) {
        match self {
            Control::Switch(get, set) => set(to, get(from)),
            Control::Slider { get, set, .. } => set(to, get(from)),
            Control::Text { get, set, .. } => {
                set(to, &get(from));
            }
            Control::Combo { get, set, .. } => set(to, &get(from)),
            Control::Pills { get, set, .. } => set(to, get(from)),
            Control::Buttons(_) | Control::Heading | Control::Note => {}
        }
    }
}

fn always(_: &Config) -> bool {
    true
}

fn note(tab: usize, subtitle: &'static str) -> Row {
    Row { tab, title: "", subtitle, control: Control::Note, visible: always }
}

fn heading(tab: usize, title: &'static str) -> Row {
    Row { tab, title, subtitle: "", control: Control::Heading, visible: always }
}

fn opts(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(l, v)| (l.to_string(), v.to_string())).collect()
}

fn expand_home(p: &str) -> std::path::PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => std::path::PathBuf::from(p),
    }
}

fn valid_time(s: &str) -> bool {
    let mut it = s.split(':');
    match (it.next().and_then(|h| h.parse::<u32>().ok()), it.next().and_then(|m| m.parse::<u32>().ok()), it.next()) {
        (Some(h), Some(m), None) => h < 24 && m < 60 && s.len() == 5,
        _ => false,
    }
}

fn rows() -> Vec<Row> {
    use Control::*;
    let pct = |v: f64| format!("{:.0}%", v * 100.0);
    let _ = pct;
    vec![
        // ── Wallpaper ──
        heading(0, "Wallpaper"),
        Row { tab: 0, title: "Wallpaper Directory", subtitle: "The directory containing your wallpaper images (~ is expanded).", visible: always,
            control: Text { get: |c| c.wallpaper_dir.clone(), set: |c, v| { let v = v.trim(); if v.is_empty() { return false; } c.wallpaper_dir = v.to_string(); true }, placeholder: "~/Pictures/Wallpapers", browse: true } },
        Row { tab: 0, title: "Wallpaper Daemon", subtitle: "The backend used to set and render your desktop wallpapers.", visible: always,
            control: Combo { options: || opts(&[("Built-in", "prism"), ("awww", "awww"), ("swww", "swww")]), get: |c| c.wallpaper_daemon.clone(), set: |c, v| c.wallpaper_daemon = v.to_string(), fonts: false } },
        Row { tab: 0, title: "Transition", subtitle: "How a new wallpaper replaces the old one.", visible: |c| c.wallpaper_daemon == "prism",
            control: Combo { options: || opts(&[("Random", "random"), ("Fade", "fade"), ("Wipe", "wipe"), ("Grow", "grow"), ("None", "none")]), get: |c| c.wallpaper_transition.clone(), set: |c, v| c.wallpaper_transition = v.to_string(), fonts: false } },
        Row { tab: 0, title: "Use Wallpaper Filters", subtitle: "Choose a colour filter when applying a wallpaper.", visible: always,
            control: Switch(|c| c.wallpaper_filters_enabled, |c, v| c.wallpaper_filters_enabled = v) },
        heading(0, "Depth Effect"),
        Row { tab: 0, title: "Wallpaper Depth", subtitle: "Makes desktop widgets pass behind wallpaper foregrounds.", visible: always,
            control: Switch(|c| c.depth_enabled, |c, v| c.depth_enabled = v) },
        Row { tab: 0, title: DEPTH_STATUS_ROW, subtitle: "Status: checking\u{2026}", visible: |c| c.depth_enabled,
            control: Buttons(&[("Install CPU (~120 MB)", Action::DepthInstallCpu, ButtonKind::Primary), ("Install GPU support (~1.5 GB)", Action::DepthInstallGpu, ButtonKind::Primary)]) },
        Row { tab: 0, title: "Compute Device", subtitle: "Execution provider for neural depth estimation.", visible: |c| c.depth_enabled,
            control: Combo { options: || opts(&[("CPU", "cpu"), ("GPU (CUDA)", "cuda")]), get: |c| c.depth_device.clone(), set: |c, v| c.depth_device = v.to_string(), fonts: false } },
        Row { tab: 0, title: "Generate automatically", subtitle: "Generate the mask when the wallpaper or parameters change.", visible: |c| c.depth_enabled,
            control: Switch(|c| c.depth_auto_generate, |c, v| c.depth_auto_generate = v) },
        Row { tab: 0, title: "Pre-generate wallpapers", subtitle: "Generate masks for every wallpaper in the background.", visible: |c| c.depth_enabled,
            control: Switch(|c| c.depth_pregenerate, |c, v| c.depth_pregenerate = v) },
        heading(0, "Depth Mask"),
        Row { tab: 0, title: "Foreground Threshold", subtitle: "Lower threshold = more of the scene in front of widgets.", visible: |c| c.depth_enabled,
            control: Slider { min: 0.0, max: 100.0, step: 1.0, get: |c| c.depth_threshold as f64, set: |c, v| c.depth_threshold = v as u32, fmt: |v| format!("{:.0}%", v) } },
        Row { tab: 0, title: "Edge Feather", subtitle: "Smoothness between foreground and background (0-50).", visible: |c| c.depth_enabled,
            control: Slider { min: 0.0, max: 50.0, step: 1.0, get: |c| c.depth_feather as f64, set: |c, v| c.depth_feather = v as u32, fmt: |v| format!("{:.0}", v) } },
        Row { tab: 0, title: "Depth Actions", subtitle: "Generate the mask for the current wallpaper or clear cached files.", visible: |c| c.depth_enabled,
            control: Buttons(&[("Generate now", Action::DepthGenerate, ButtonKind::Primary), ("Clear cache", Action::DepthClear, ButtonKind::Danger)]) },
        // ── Lock Screen ──
        Row { tab: 1, title: "Lockscreen Theme", subtitle: "The design of the lock screen.", visible: always,
            control: Combo { options: || opts(LOCK_THEMES), get: |c| c.lockscreen_theme.clone(), set: |c, v| c.lockscreen_theme = v.to_string(), fonts: false } },
        Row { tab: 1, title: "Lockscreen Power Menu", subtitle: "Allow Suspend, Reboot and Shutdown directly from the lockscreen.", visible: always,
            control: Switch(|c| c.show_lockscreen_session_controls, |c, v| c.show_lockscreen_session_controls = v) },
        Row { tab: 1, title: "Lockscreen Alignment", subtitle: "Align the lockscreen elements to the left or right edge (Classic theme).", visible: |c| c.lockscreen_theme == "classic" || c.greeter_theme == "classic",
            control: Combo { options: || opts(&[("Left", "left"), ("Right", "right")]), get: |c| c.lockscreen_alignment.clone(), set: |c, v| c.lockscreen_alignment = v.to_string(), fonts: false } },
        note(1, THEME_FILTER_NOTE),
        // ── Greeter ──
        Row { tab: 2, title: "Greeter Theme", subtitle: "The design of the login screen.", visible: always,
            control: Combo { options: || { let mut o = opts(&[("Mirror the Lock Screen", "mirror")]); o.extend(opts(LOCK_THEMES)); o }, get: |c| c.greeter_theme.clone(), set: |c, v| c.greeter_theme = v.to_string(), fonts: false } },
        Row { tab: 2, title: "Remember Last User", subtitle: "Pre-select the last user and session on the next boot (applies after logout).", visible: always,
            control: Switch(|c| c.remember_last_user, |c, v| c.remember_last_user = v) },
        note(2, THEME_FILTER_NOTE),
        // ── Display ──
        heading(3, "Monitor"),
        Row { tab: 3, title: "Monitor", subtitle: "Which monitor the settings below change.", visible: |_| crate::displays::count() > 1,
            control: Combo { options: crate::displays::monitor_options, get: |c| crate::displays::selected(c).map(|o| o.name).unwrap_or_default(), set: |c, v| c.display_output = v.to_string(), fonts: false } },
        Row { tab: 3, title: "Enabled", subtitle: "Turn this monitor off or on.", visible: |_| crate::displays::count() > 1,
            control: Switch(|c| crate::displays::entry(c).map(|e| e.enabled).unwrap_or(true), |c, v| crate::displays::update(c, |e| e.enabled = v)) },
        Row { tab: 3, title: "Resolution", subtitle: "Size and refresh rate. A change is undone after 15 seconds unless you keep it.", visible: |_| crate::displays::count() > 0,
            control: Combo { options: crate::displays::mode_options, get: |c| crate::displays::entry(c).map(|e| e.mode).unwrap_or_default(), set: |c, v| crate::displays::update(c, |e| e.mode = v.to_string()), fonts: false } },
        Row { tab: 3, title: "Display Scale", subtitle: "How large everything is drawn on this monitor (compositor scale).", visible: |_| crate::displays::count() > 0,
            control: Combo { options: crate::displays::scale_options, get: |c| crate::displays::entry(c).map(|e| format!("{:.2}", e.scale)).unwrap_or_default(), set: |c, v| { if let Ok(s) = v.parse::<f64>() { crate::displays::update(c, |e| e.scale = s) } }, fonts: false } },
        Row { tab: 3, title: "Rotation", subtitle: "", visible: |_| crate::displays::count() > 0,
            control: Combo { options: || opts(&[("Normal", "normal"), ("90\u{00B0}", "90"), ("180\u{00B0}", "180"), ("270\u{00B0}", "270"), ("Flipped", "flipped")]), get: |c| crate::displays::entry(c).map(|e| e.transform).unwrap_or_default(), set: |c, v| crate::displays::update(c, |e| e.transform = v.to_string()), fonts: false } },
        Row { tab: 3, title: "Adaptive Sync", subtitle: "Variable refresh rate (FreeSync / G-Sync Compatible).", visible: |c| crate::displays::selected(c).is_some_and(|o| o.adaptive_sync.is_some()),
            control: Switch(|c| crate::displays::entry(c).and_then(|e| e.adaptive_sync).unwrap_or(false), |c, v| crate::displays::update(c, |e| e.adaptive_sync = Some(v))) },
        Row { tab: 3, title: "Position", subtitle: "Top-left corner in the layout, as X,Y in logical pixels.", visible: |_| crate::displays::count() > 1,
            control: Text { get: |c| crate::displays::entry(c).and_then(|e| e.position).map(|(x, y)| format!("{x},{y}")).unwrap_or_default(), set: |c, v| match v.split_once(',').and_then(|(x, y)| Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?))) { Some(p) => { crate::displays::update(c, |e| e.position = Some(p)); true } None => false }, placeholder: "0,0", browse: false } },
        heading(3, "Appearance"),
        Row { tab: 3, title: "Global font", subtitle: "Font for the shell and desktop widgets.", visible: always,
            control: Combo {
                options: || {
                    let mut v = vec![("System Default".to_string(), String::new())];
                    v.extend(uikit::font_families().into_iter().map(|f| (f.clone(), f)));
                    v
                },
                get: |c| if c.global_font == "System Default" { String::new() } else { c.global_font.clone() },
                set: |c, v| c.global_font = v.to_string(),
                fonts: true,
            } },
        Row { tab: 3, title: "Interface Scale", subtitle: "Size of cards and popups. Auto follows the screen resolution (100% at 1440p).", visible: always,
            control: Combo {
                options: || opts(&[("Auto", "0"), ("75%", "0.75"), ("90%", "0.9"), ("100%", "1"), ("110%", "1.1"), ("125%", "1.25"), ("150%", "1.5"), ("175%", "1.75"), ("200%", "2")]),
                get: |c| if c.ui_scale > 0.0 { format!("{}", c.ui_scale) } else { "0".into() },
                set: |c, v| c.ui_scale = v.parse().unwrap_or(0.0),
                fonts: false,
            } },
        // ── Notifications ──
        heading(3, "Idle & Lock"),
        Row { tab: 3, title: "Manage idle behavior", subtitle: "Lock the screen automatically when the system is idle.", visible: always,
            control: Switch(|c| c.manage_idle, |c, v| c.manage_idle = v) },
        Row { tab: 3, title: "Lock timeout (minutes)", subtitle: "Minutes of inactivity before the screen locks.", visible: |c| c.manage_idle,
            control: Text { get: |c| c.idle_timeout.to_string(), set: |c, v| match v.trim().parse::<u32>() { Ok(n) if n >= 1 => { c.idle_timeout = n; true } _ => false }, placeholder: "5", browse: false } },
        heading(3, "Blue Light Filter"),
        Row { tab: 3, title: "Blue Light Filter", subtitle: "Adjust display colour temperature based on time or location.", visible: always,
            control: Switch(|c| c.blue_light_enabled, |c, v| c.blue_light_enabled = v) },
        Row { tab: 3, title: "Turn on now", subtitle: "Force the blue light filter on.", visible: |c| c.blue_light_enabled,
            control: Switch(|c| c.blue_light_manual_on, |c, v| c.blue_light_manual_on = v) },
        Row { tab: 3, title: "Mode", subtitle: "How day and night are determined.", visible: |c| c.blue_light_enabled,
            control: Combo { options: || opts(&[("Fixed Time", "fixed_time"), ("Sunset/Sunrise", "sunset_sunrise")]),
                get: |c| if c.blue_light_mode == "time" { "fixed_time".into() } else { c.blue_light_mode.clone() }, set: |c, v| c.blue_light_mode = v.to_string(), fonts: false } },
        heading(3, "Schedule"),
        Row { tab: 3, title: "Night starts (HH:MM)", subtitle: "Overnight windows like 22:00 to 06:00 are supported.", visible: |c| c.blue_light_enabled && c.blue_light_mode != "sunset_sunrise",
            control: Text { get: |c| c.blue_light_night_start.clone(), set: |c, v| { let v = v.trim(); if !valid_time(v) { return false; } c.blue_light_night_start = v.to_string(); true }, placeholder: "22:00", browse: false } },
        Row { tab: 3, title: "Night ends (HH:MM)", subtitle: "", visible: |c| c.blue_light_enabled && c.blue_light_mode != "sunset_sunrise",
            control: Text { get: |c| c.blue_light_night_end.clone(), set: |c, v| { let v = v.trim(); if !valid_time(v) { return false; } c.blue_light_night_end = v.to_string(); true }, placeholder: "06:00", browse: false } },
        Row { tab: 3, title: "Use location from IP", subtitle: "Look up coordinates automatically instead of entering them.", visible: |c| c.blue_light_enabled && c.blue_light_mode == "sunset_sunrise",
            control: Switch(|c| c.blue_light_use_ip_location, |c, v| c.blue_light_use_ip_location = v) },
        Row { tab: 3, title: "Latitude", subtitle: "Used to calculate sunrise and sunset.", visible: |c| c.blue_light_enabled && c.blue_light_mode == "sunset_sunrise" && !c.blue_light_use_ip_location,
            control: Text { get: |c| c.blue_light_latitude.clone(), set: |c, v| match v.trim().parse::<f64>() { Ok(x) if (-90.0..=90.0).contains(&x) => { c.blue_light_latitude = v.trim().to_string(); true } _ => v.trim().is_empty() && { c.blue_light_latitude.clear(); true } }, placeholder: "e.g. 12.97", browse: false } },
        Row { tab: 3, title: "Longitude", subtitle: "", visible: |c| c.blue_light_enabled && c.blue_light_mode == "sunset_sunrise" && !c.blue_light_use_ip_location,
            control: Text { get: |c| c.blue_light_longitude.clone(), set: |c, v| match v.trim().parse::<f64>() { Ok(x) if (-180.0..=180.0).contains(&x) => { c.blue_light_longitude = v.trim().to_string(); true } _ => v.trim().is_empty() && { c.blue_light_longitude.clear(); true } }, placeholder: "e.g. 77.59", browse: false } },
        heading(3, "Colour Temperature"),
        Row { tab: 3, title: "Transition Duration (minutes)", subtitle: "The transition completes by the scheduled time.", visible: |c| c.blue_light_enabled,
            control: Text { get: |c| c.blue_light_transition_minutes.to_string(), set: |c, v| match v.trim().parse::<u32>() { Ok(n) if n <= 240 => { c.blue_light_transition_minutes = n; true } _ => false }, placeholder: "30", browse: false } },
        Row { tab: 3, title: "Day Temperature", subtitle: "Colour temperature during the day.", visible: |c| c.blue_light_enabled,
            control: Slider { min: 4500.0, max: 6500.0, step: 100.0, get: |c| c.blue_light_day_temp as f64, set: |c, v| c.blue_light_day_temp = v as u32, fmt: |v| format!("{:.0} K", v) } },
        Row { tab: 3, title: "Night Temperature", subtitle: "Colour temperature at night.", visible: |c| c.blue_light_enabled,
            control: Slider { min: 1500.0, max: 6500.0, step: 100.0, get: |c| c.blue_light_night_temp as f64, set: |c, v| c.blue_light_night_temp = v as u32, fmt: |v| format!("{:.0} K", v) } },
        // ── Desktop ──
        heading(4, "Layout"),
        Row { tab: 4, title: "Desktop Edit Mode", subtitle: "Reposition, resize, remove and re-add desktop widgets.", visible: always,
            control: Buttons(&[("Enter desktop edit mode", Action::EditMode, ButtonKind::Primary), ("Reset layout", Action::ResetLayout, ButtonKind::Danger)]) },
        heading(4, "Widgets"),
        Row { tab: 4, title: "Media Player", subtitle: "Album art, track info and controls.", visible: always,
            control: Switch(|c| c.desktop_layout.mpris.visible, |c, v| c.desktop_layout.mpris.visible = v) },
        Row { tab: 4, title: "System Tray", subtitle: "Status icons of running applications.", visible: always,
            control: Switch(|c| c.desktop_layout.tray.visible, |c, v| c.desktop_layout.tray.visible = v) },
        Row { tab: 4, title: "Digital Clock", subtitle: "Time and date.", visible: always,
            control: Switch(|c| c.desktop_layout.clock.visible, |c, v| c.desktop_layout.clock.visible = v) },
        Row { tab: 4, title: "Calendar", subtitle: "Month view with today highlighted.", visible: always,
            control: Switch(|c| c.desktop_layout.calendar.visible, |c, v| c.desktop_layout.calendar.visible = v) },
        Row { tab: 4, title: "Analog Clock", subtitle: "Scalloped clock face with a ticking second dot.", visible: always,
            control: Switch(|c| c.desktop_layout.analogclock.visible, |c, v| c.desktop_layout.analogclock.visible = v) },
        Row { tab: 4, title: "Shortcuts", subtitle: "Launcher, notifications, settings and power buttons.", visible: always,
            control: Switch(|c| c.desktop_layout.shortcuts.visible, |c, v| c.desktop_layout.shortcuts.visible = v) },
        Row { tab: 4, title: "Phone", subtitle: "Battery, status and media of the paired phone, with find my phone.", visible: always,
            control: Switch(|c| c.desktop_layout.phone.visible, |c, v| c.desktop_layout.phone.visible = v) },
        // ── UI ──
        heading(5, "Windows"),
        Row { tab: 5, title: "Window Background Blur", subtitle: "Compositor blur behind Prism cards.", visible: always,
            control: Switch(|c| c.blur_enabled, |c, v| c.blur_enabled = v) },
        // Niri and Hyprland only: Mango has no runtime blur setting (prismd applies it).
        Row { tab: 5, title: "Blur Strength", subtitle: "Blur passes and offset used by the compositor (level 1 to 5).",
            visible: |c| c.blur_enabled && prism_common::blur::compositor().is_some(),
            control: Slider { min: 1.0, max: 5.0, step: 1.0, get: |c| prism_common::blur::level(c.blur_strength) as f64, set: |c, v| c.blur_strength = v.round() as f32, fmt: |v| format!("Level {:.0}", v) } },
        Row { tab: 5, title: "Window Shadows", subtitle: "Soft drop shadow under cards.", visible: always,
            control: Switch(|c| c.shadows_enabled, |c, v| c.shadows_enabled = v) },
        Row { tab: 5, title: "Window Background Opacity", subtitle: "Opacity of every card's background.", visible: always,
            control: Slider { min: 0.2, max: 1.0, step: 0.01, get: |c| c.background_opacity as f64, set: |c, v| c.background_opacity = v as f32, fmt: |v| format!("{:.0}%", v * 100.0) } },
        Row { tab: 5, title: "Window Corner Radius", subtitle: "Corner rounding of every card.", visible: always,
            control: Slider { min: 0.0, max: 36.0, step: 1.0, get: |c| c.window_radius as f64, set: |c, v| c.window_radius = v as u32, fmt: |v| format!("{:.0} px", v) } },
        heading(5, "Motion"),
        Row { tab: 5, title: "Animations", subtitle: "Card open/close and in-card motion, synced to the display refresh.", visible: always,
            control: Switch(|c| c.animations_enabled, |c, v| c.animations_enabled = v) },
        Row { tab: 5, title: "Animation Speed", subtitle: "Multiplier for every animation (1.0 = original timings).", visible: |c| c.animations_enabled,
            control: Slider { min: 0.5, max: 2.0, step: 0.05, get: |c| c.animation_speed as f64, set: |c, v| c.animation_speed = v as f32, fmt: |v| format!("{:.2}x", v) } },
        heading(5, "Background Dim"),
        Row { tab: 5, title: "Dim Background", subtitle: "Darken the screen behind open cards.", visible: always,
            control: Switch(|c| c.dim_enabled, |c, v| c.dim_enabled = v) },
        Row { tab: 5, title: "Dim Strength", subtitle: "", visible: |c| c.dim_enabled,
            control: Slider { min: 0.0, max: 0.8, step: 0.01, get: |c| c.dim_opacity as f64, set: |c, v| c.dim_opacity = v as f32, fmt: |v| format!("{:.0}%", v * 100.0) } },
        heading(5, "Colours"),
        Row { tab: 5, title: "Matugen Colour Mode", subtitle: "Light or dark palette generated from the wallpaper.", visible: always,
            control: Combo { options: || opts(&[("Dark", "dark"), ("Light", "light"), ("Smart", "smart")]), get: |c| c.matugen_mode.clone(), set: |c, v| c.matugen_mode = v.to_string(), fonts: false } },
        Row { tab: 5, title: "Palette Scheme", subtitle: "Material You scheme used by matugen.", visible: always,
            control: Combo {
                options: || opts(&[("Smart", "scheme-smart"), ("Tonal Spot", "scheme-tonal-spot"), ("Expressive", "scheme-expressive"),
                    ("Fruit Salad", "scheme-fruit-salad"), ("Vibrant", "scheme-vibrant"), ("Rainbow", "scheme-rainbow"),
                    ("Neutral", "scheme-neutral"), ("Fidelity", "scheme-fidelity"), ("Content", "scheme-content"), ("Monochrome", "scheme-monochrome")]),
                get: |c| c.matugen_scheme.clone(), set: |c, v| c.matugen_scheme = v.to_string(), fonts: false } },
        // ── Search ──
        heading(6, "Sources"),
        Row { tab: 6, title: "Search Section Order", subtitle: "Order of the result sections in Spotlight.", visible: always,
            control: Combo {
                options: || {
                    let perms = ["apps,folders,files", "apps,files,folders", "folders,apps,files", "folders,files,apps", "files,apps,folders", "files,folders,apps"];
                    perms.iter().map(|p| (p.split(',').map(|s| { let mut c = s.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(", "), p.to_string())).collect()
                },
                get: |c| c.search_section_order.clone(), set: |c, v| c.search_section_order = v.to_string(), fonts: false } },
        Row { tab: 6, title: "Search Applications", subtitle: "Include installed applications.", visible: always,
            control: Switch(|c| c.search_enable_apps, |c, v| c.search_enable_apps = v) },
        Row { tab: 6, title: "Search Folders", subtitle: "Include folders from the file index.", visible: always,
            control: Switch(|c| c.search_enable_folders, |c, v| c.search_enable_folders = v) },
        Row { tab: 6, title: "Search Files", subtitle: "Include files from the file index.", visible: always,
            control: Switch(|c| c.search_enable_files, |c, v| c.search_enable_files = v) },
        heading(6, "Results"),
        Row { tab: 6, title: "Max Folder Results", subtitle: "", visible: |c| c.search_enable_folders,
            control: Combo { options: || ["5", "10", "15", "20", "30"].iter().map(|s| (s.to_string(), s.to_string())).collect(),
                get: |c| c.search_max_folder_results.to_string(), set: |c, v| c.search_max_folder_results = v.parse().unwrap_or(10), fonts: false } },
        Row { tab: 6, title: "Max File Results", subtitle: "", visible: |c| c.search_enable_files,
            control: Combo { options: || ["10", "15", "20", "30", "50"].iter().map(|s| (s.to_string(), s.to_string())).collect(),
                get: |c| c.search_max_file_results.to_string(), set: |c, v| c.search_max_file_results = v.parse().unwrap_or(20), fonts: false } },
        Row { tab: 6, title: "Include Hidden Files & Folders", subtitle: "Show dot-files and dot-folders in results.", visible: always,
            control: Switch(|c| c.search_include_hidden, |c, v| c.search_include_hidden = v) },
        heading(6, "File Index"),
        Row { tab: 6, title: "Rebuild File Index", subtitle: "Indexes files system-wide for search (pkexec updatedb). Can take a few minutes.", visible: always,
            control: Buttons(&[("Rebuild index", Action::RebuildIndex, ButtonKind::Primary)]) },
        Row { tab: 7, title: "Popup Position", subtitle: "Where notification popups appear on screen.", visible: always,
            control: Combo {
                options: || opts(&[("Top Left", "top-left"), ("Top Center", "top-center"), ("Top Right", "top-right"),
                    ("Bottom Left", "bottom-left"), ("Bottom Center", "bottom-center"), ("Bottom Right", "bottom-right")]),
                get: |c| c.notification_popup_position.clone(),
                set: |c, v| c.notification_popup_position = v.to_string(),
                fonts: false,
            } },
        Row { tab: 7, title: "Control Centre Tiles", subtitle: "Tiles shown in the Control Centre; the others close up the gap.", visible: always,
            control: Pills { options: &[("Network", "network"), ("Bluetooth", "bluetooth"), ("VPN", "vpn"), ("Notifications", "notifications"), ("Caffeine", "caffeine")], get: |c| c.nc_tiles.clone(), set: |c, v| c.nc_tiles = v } },
        Row { tab: 7, title: "Notification Center Position", subtitle: "Which side of the screen the notification center opens on.", visible: always,
            control: Combo {
                options: || opts(&[("Left", "left"), ("Center", "center"), ("Right", "right")]),
                get: |c| c.notification_center_position.clone(),
                set: |c, v| c.notification_center_position = v.to_string(),
                fonts: false,
            } },
        // ── Phone ──
        heading(8, "Phone Sync"),
        Row { tab: 8, title: "Phone Sync", subtitle: "Connect an Android phone running KDE Connect (same Wi-Fi). Needs the ports open: sudo ufw allow KDEConnect.", visible: always,
            control: Switch(|c| c.phone_enabled, |c, v| c.phone_enabled = v) },
        Row { tab: 8, title: "Device Name", subtitle: "How this computer appears on the phone. Empty = the host name.", visible: |c| c.phone_enabled,
            control: Text { get: |c| c.phone_device_name.clone(), set: |c, v| { c.phone_device_name = v.trim().chars().take(32).collect(); true }, placeholder: "host name", browse: false } },
        Row { tab: 8, title: "Pairing & Status", subtitle: "Pair a phone, see battery, media, notifications and transfers.", visible: |c| c.phone_enabled,
            control: Buttons(&[("Open the Phone card", Action::OpenPhone, ButtonKind::Primary)]) },
        heading(8, "Phone Card Access"),
        Row { tab: 8, title: "Shortcuts Button", subtitle: "A phone button in the desktop Shortcuts widget (when that widget is shown).", visible: |c| c.phone_enabled,
            control: Switch(|c| c.desktop_layout.shortcuts.shortcuts.phone, |c, v| c.desktop_layout.shortcuts.shortcuts.phone = v) },
        Row { tab: 8, title: "Tray Icon", subtitle: "A phone icon in the system tray (Prism's tray widget or any other tray).", visible: |c| c.phone_enabled,
            control: Switch(|c| c.phone_tray, |c, v| c.phone_tray = v) },
        Row { tab: 8, title: "Desktop Widget", subtitle: "The Phone desktop widget, with a button that opens the card.", visible: |c| c.phone_enabled,
            control: Switch(|c| c.desktop_layout.phone.visible, |c, v| c.desktop_layout.phone.visible = v) },
        heading(8, "Remote Input"),
        Row { tab: 8, title: "Allow Remote Input", subtitle: "The phone can move the pointer and type (KDE Connect \u{201c}Remote input\u{201d} and presenter).", visible: |c| c.phone_enabled,
            control: Switch(|c| c.phone_allow_input, |c, v| c.phone_allow_input = v) },
        Row { tab: 8, title: "Also While Locked", subtitle: "Accept remote input while the screen is locked.", visible: |c| c.phone_enabled && c.phone_allow_input,
            control: Switch(|c| c.phone_input_when_locked, |c, v| c.phone_input_when_locked = v) },
        heading(8, "Sync"),
        Row { tab: 8, title: "Clipboard", subtitle: "Copies go to the phone. From the phone, use Send clipboard in KDE Connect (Android blocks automatic reading).", visible: |c| c.phone_enabled,
            control: Switch(|c| c.phone_clipboard_sync, |c, v| c.phone_clipboard_sync = v) },
        Row { tab: 8, title: "Phone Notifications", subtitle: "Show the phone's notifications here, with reply and actions. Allow notification access for KDE Connect on the phone.", visible: |c| c.phone_enabled,
            control: Switch(|c| c.phone_notifications, |c, v| c.phone_notifications = v) },
        Row { tab: 8, title: "Notification Popups", subtitle: "Pop up phone notifications (they are always listed in the Phone card).", visible: |c| c.phone_enabled && c.phone_notifications,
            control: Switch(|c| c.phone_notification_popups, |c, v| c.phone_notification_popups = v) },
        heading(8, "Files"),
        Row { tab: 8, title: "Download Folder", subtitle: "Where files from the phone are saved (~ is expanded).", visible: |c| c.phone_enabled,
            control: Text { get: |c| c.phone_download_dir.clone(), set: |c, v| { let v = v.trim(); if v.is_empty() { return false; } c.phone_download_dir = v.to_string(); true }, placeholder: "~/Downloads", browse: true } },
        Row { tab: 8, title: "Open Received Files", subtitle: "Open each file with its default app when it arrives.", visible: |c| c.phone_enabled,
            control: Switch(|c| c.phone_open_received, |c, v| c.phone_open_received = v) },
        // ── Capture ──
        heading(9, "Screenshots"),
        Row { tab: 9, title: "Screenshot Folder", subtitle: "Where screenshots are saved (~ is expanded).", visible: always,
            control: Text { get: |c| c.capture_screenshot_dir.clone(), set: |c, v| { let v = v.trim(); if v.is_empty() { return false; } c.capture_screenshot_dir = v.to_string(); true }, placeholder: "~/Pictures/Screenshots", browse: true } },
        Row { tab: 9, title: "Copy to Clipboard", subtitle: "Put every screenshot on the clipboard as an image.", visible: always,
            control: Switch(|c| c.capture_copy_to_clipboard, |c, v| c.capture_copy_to_clipboard = v) },
        Row { tab: 9, title: "Show the Pointer", subtitle: "Include the mouse pointer in screenshots and recordings.", visible: always,
            control: Switch(|c| c.capture_cursor, |c, v| c.capture_cursor = v) },
        heading(9, "Recording"),
        Row { tab: 9, title: "Recording Folder", subtitle: "Where recordings are saved (~ is expanded).", visible: always,
            control: Text { get: |c| c.capture_recording_dir.clone(), set: |c, v| { let v = v.trim(); if v.is_empty() { return false; } c.capture_recording_dir = v.to_string(); true }, placeholder: "~/Videos/Recordings", browse: true } },
        Row { tab: 9, title: "File Format", subtitle: "MKV and MP4 are both written as they record, so a crash still leaves a playable file.", visible: always,
            control: Combo { options: || opts(&[("MKV", "mkv"), ("MP4", "mp4")]), get: |c| c.capture_container.clone(), set: |c, v| c.capture_container = v.to_string(), fonts: false } },
        Row { tab: 9, title: "Video Codec", subtitle: "Auto uses H.264 on the GPU (NVENC) when available, else x264.", visible: always,
            control: Combo { options: || opts(&[("Auto (H.264)", "auto"), ("HEVC", "hevc"), ("AV1", "av1")]), get: |c| c.capture_codec.clone(), set: |c, v| c.capture_codec = v.to_string(), fonts: false } },
        Row { tab: 9, title: "Quality", subtitle: "Higher quality means larger files.", visible: always,
            control: Combo { options: || opts(&[("Low", "low"), ("Medium", "medium"), ("High", "high"), ("Near lossless", "lossless")]), get: |c| c.capture_quality.clone(), set: |c, v| c.capture_quality = v.to_string(), fonts: false } },
        Row { tab: 9, title: "Frame Rate", subtitle: "Most frames per second; still screens use none.", visible: always,
            control: Combo { options: || opts(&[("30 fps", "30"), ("60 fps", "60")]), get: |c| c.capture_fps.to_string(), set: |c, v| c.capture_fps = v.parse().unwrap_or(60), fonts: false } },
        Row { tab: 9, title: "Desktop Audio", subtitle: "Record what the speakers play. Also switchable in the capture overlay.", visible: always,
            control: Switch(|c| c.capture_audio_desktop, |c, v| c.capture_audio_desktop = v) },
        Row { tab: 9, title: "Microphone", subtitle: "Record the default microphone. Also switchable in the capture overlay.", visible: always,
            control: Switch(|c| c.capture_audio_mic, |c, v| c.capture_audio_mic = v) },
        // ── Services ──
        heading(10, "Music Player Daemon"),
        Row { tab: 10, title: "MPD Media Controls", subtitle: "Show MPD as a media player (MPRIS) for media keys, the media widget and the phone. Replaces mpd-mpris.", visible: always,
            control: Switch(|c| c.mpd_mpris_enabled, |c, v| c.mpd_mpris_enabled = v) },
        Row { tab: 10, title: "MPD Host", subtitle: "Host name, or the path of MPD's socket.", visible: |c| c.mpd_mpris_enabled,
            control: Text { get: |c| c.mpd_host.clone(), set: |c, v| { let v = v.trim(); if v.is_empty() { return false; } c.mpd_host = v.to_string(); true }, placeholder: "127.0.0.1", browse: false } },
        Row { tab: 10, title: "MPD Port", subtitle: "TCP port (not used with a socket path).", visible: |c| c.mpd_mpris_enabled && !c.mpd_host.starts_with('/'),
            control: Text { get: |c| c.mpd_port.to_string(), set: |c, v| match v.trim().parse::<u16>() { Ok(n) if n > 0 => { c.mpd_port = n; true } _ => false }, placeholder: "6600", browse: false } },
        heading(10, "Authentication"),
        Row { tab: 10, title: "Polkit Agent", subtitle: "Ask for your password when an app needs administrator rights. Replaces polkit-gnome (stop it first: only one agent per session).", visible: always,
            control: Switch(|c| c.polkit_agent_enabled, |c, v| c.polkit_agent_enabled = v) },
        Row { tab: 10, title: "Network Passwords", subtitle: "Ask for Wi-Fi, VPN and 802.1X secrets when NetworkManager needs them. Replaces nm-applet.", visible: always,
            control: Switch(|c| c.network_agent_enabled, |c, v| c.network_agent_enabled = v) },
        heading(10, "VPN"),
        Row { tab: 10, title: "Save VPN User Names", subtitle: "Store the user name typed in the VPN prompt in the connection.", visible: |c| c.network_agent_enabled,
            control: Switch(|c| c.vpn_save_username, |c, v| c.vpn_save_username = v) },
        Row { tab: 10, title: "Save VPN Passwords", subtitle: "Store VPN passwords with NetworkManager (system-owned secrets, like nm-applet's \u{201C}store for all users\u{201D}).", visible: |c| c.network_agent_enabled,
            control: Switch(|c| c.vpn_save_password, |c, v| c.vpn_save_password = v) },
        heading(10, "Tray Icons"),
        Row { tab: 10, title: "Network Tray Icon", subtitle: "Network status in the tray; its menu connects VPNs. Replaces nm-applet's icon.", visible: always,
            control: Switch(|c| c.network_tray, |c, v| c.network_tray = v) },
        Row { tab: 10, title: "Bluetooth Tray Icon", subtitle: "Bluetooth status in the tray; its menu connects devices and sends files. Replaces blueman's icon.", visible: always,
            control: Switch(|c| c.bluetooth_tray, |c, v| c.bluetooth_tray = v) },
        heading(10, "Bluetooth"),
        Row { tab: 10, title: "Receive Files", subtitle: "Accept files sent from phones and computers over Bluetooth (asks each time; needs bluez-obex). Replaces blueman's file receiving.", visible: always,
            control: Switch(|c| c.bluetooth_receive_files, |c, v| c.bluetooth_receive_files = v) },
        Row { tab: 10, title: "Bluetooth Download Folder", subtitle: "Where received files are saved (~ is expanded).", visible: |c| c.bluetooth_receive_files,
            control: Text { get: |c| c.bluetooth_download_dir.clone(), set: |c, v| { let v = v.trim(); if v.is_empty() { return false; } c.bluetooth_download_dir = v.to_string(); true }, placeholder: "~/Downloads", browse: true } },
    ]
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Hit {
    Search,
    Tab(usize),
    Close,
    Undo,
    Redo,
    Control(usize),
    Button(usize, usize),
    Reset(usize),
    Browse(usize),
}

pub enum Effect {
    None,
    Redraw,
    Close,
}

pub struct SettingsView {
    rows: Vec<Row>,
    /// The config as it was when the window opened: rows that differ from it are marked
    /// as changed and can be reverted to it.
    baseline: Config,
    /// The live config, as last written (or being dragged/edited).
    pub config: Config,
    undo: Vec<Config>,
    redo: Vec<Config>,
    /// Config before the slider drag in progress.
    drag_start: Option<Config>,
    tab: usize,
    scroll: crate::ui::Scroller,
    /// Hover fades of every hit target, switch knob positions (per row), the gliding tab pill,
    /// the gliding keyboard focus ring (x/w and content y/h) and the tab-change entrance.
    hover_fade: crate::ui::Fades<Hit>,
    knobs: crate::ui::Fades<usize>,
    tab_glide: crate::ui::Glide,
    focus_glide: (crate::ui::Glide, crate::ui::Glide),
    tab_in: crate::ui::SpringValue,
    time_scale: Option<f64>,
    /// Where the selected tab was drawn last frame (target of `tab_glide`).
    last_tab_rect: Option<Rect>,
    content_h: f64,
    search: String,
    search_focus: bool,
    editing: Option<(usize, String)>,
    invalid: Option<usize>,
    combo: Option<(usize, Dropdown)>,
    drag: Option<usize>,
    hover: Option<Hit>,
    /// Keyboard focus: a Control or Button hit. Drawn only after keyboard navigation.
    focus: Option<Hit>,
    focus_visible: bool,
    /// Scroll the focused row into view on the next draw.
    scroll_to_focus: bool,
    /// Focusable controls in visual order, from the last draw.
    focus_order: Vec<Hit>,
    hits: Vec<(Hit, Rect)>,
    control_rects: Vec<(usize, Rect)>,
    pane: Rect,
    card: Rect,
    message: Option<(String, bool)>,
    bg_tx: Sender<(String, bool)>,
    bg_rx: Receiver<(String, bool)>,
    waker: Waker,
    pending_edit_mode: bool,
    /// Row whose folder picker is open, and where its answer arrives.
    picking: Option<usize>,
    pick_tx: Sender<Result<Option<std::path::PathBuf>, String>>,
    pick_rx: Receiver<Result<Option<std::path::PathBuf>, String>>,
    /// Pending request to lower (true) or restore (false) the window around the dialog.
    layer_request: Option<bool>,
    /// True while a polkit dialog is up: the window is lowered and drawn empty so the dialog
    /// is reachable (it comes back when the dialog closes).
    hidden: bool,
    hide_rx: Receiver<bool>,
    hide_tx: std::sync::mpsc::Sender<bool>,
}

impl SettingsView {
    pub fn new(config: &Config, waker: Waker) -> Self {
        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        let (pick_tx, pick_rx) = std::sync::mpsc::channel();
        let (hide_tx, hide_rx) = std::sync::mpsc::channel();
        refresh_depth_status(waker.clone());
        refresh_index_time(waker.clone());
        SettingsView {
            rows: rows(),
            baseline: config.clone(),
            config: config.clone(),
            undo: Vec::new(),
            redo: Vec::new(),
            drag_start: None,
            tab: 0,
            scroll: crate::ui::Scroller::default(),
            hover_fade: crate::ui::Fades::default(),
            knobs: crate::ui::Fades::default(),
            tab_glide: crate::ui::Glide::default(),
            focus_glide: Default::default(),
            tab_in: crate::ui::SpringValue::new(1.0),
            time_scale: Some(1.0),
            last_tab_rect: None,
            content_h: 0.0,
            search: String::new(),
            search_focus: false,
            editing: None,
            invalid: None,
            combo: None,
            drag: None,
            hover: None,
            focus: None,
            focus_visible: false,
            scroll_to_focus: false,
            focus_order: Vec::new(),
            hits: Vec::new(),
            control_rects: Vec::new(),
            pane: Rect::default(),
            card: Rect::default(),
            message: None,
            bg_tx,
            bg_rx,
            waker,
            pending_edit_mode: false,
            picking: None,
            pick_tx,
            pick_rx,
            layer_request: None,
            hidden: false,
            hide_rx,
            hide_tx,
        }
    }

    /// Results of background jobs; true if a redraw is needed.
    pub fn poll(&mut self) -> bool {
        let mut changed = DEPTH_STATUS_NEW.swap(false, std::sync::atomic::Ordering::SeqCst);
        for hide in self.hide_rx.try_iter() {
            if hide != self.hidden {
                self.hidden = hide;
                self.layer_request = Some(hide);
                changed = true;
            }
        }
        for m in self.bg_rx.try_iter() {
            self.message = Some(m);
            changed = true;
        }
        if let Ok(result) = self.pick_rx.try_recv() {
            let row = self.picking.take();
            self.layer_request = Some(false);
            changed = true;
            match (result, row) {
                (Ok(Some(path)), Some(i)) => {
                    if let Control::Text { set, .. } = &self.rows[i].control {
                        let home = std::env::var("HOME").unwrap_or_default();
                        let text = path.to_string_lossy().to_string();
                        // Keep paths under $HOME portable, as "~/…".
                        let text = match text.strip_prefix(&format!("{home}/")) {
                            Some(rest) if !home.is_empty() => format!("~/{rest}"),
                            _ => text,
                        };
                        let mut after = self.config.clone();
                        if set(&mut after, &text) {
                            self.editing = None;
                            self.invalid = None;
                            let before = self.config.clone();
                            self.change(before, after);
                        }
                    }
                }
                (Ok(_), _) => {}
                (Err(e), _) => self.message = Some((format!("File dialog unavailable: {e}"), false)),
            }
        }
        changed
    }

    fn palette(style: &Style, theme: &Theme) -> Palette {
        Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: hex_to_rgba(&theme.on_primary, 1.0),
            outline: hex_to_rgba(&theme.outline_variant, 1.0),
            error: hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        }
    }

    fn query(&self) -> String {
        self.search.trim().to_lowercase()
    }

    fn row_matches(&self, i: usize, q: &str) -> bool {
        let r = &self.rows[i];
        r.title.to_lowercase().contains(q) || r.subtitle.to_lowercase().contains(q) || TABS[r.tab].to_lowercase().contains(q)
    }

    /// Rows shown in the pane: the current tab, or every match across tabs while searching.
    fn shown_rows(&self) -> Vec<usize> {
        let q = self.query();
        let mut v: Vec<usize> = (0..self.rows.len())
            .filter(|&i| (self.rows[i].visible)(&self.config))
            .filter(|&i| if q.is_empty() { self.rows[i].tab == self.tab } else { self.row_matches(i, &q) })
            .collect();
        v.sort_by_key(|&i| (self.rows[i].tab, i));
        // Headings: not in search results, and only with at least one row of their section.
        let heading = |i: usize| matches!(self.rows[i].control, Control::Heading);
        let small_print = |i: usize| matches!(self.rows[i].control, Control::Note);
        let v: Vec<usize> = v.into_iter().filter(|&i| q.is_empty() || !(heading(i) || small_print(i))).collect();
        (0..v.len()).filter(|&k| !heading(v[k]) || v.get(k + 1).is_some_and(|&n| !heading(n))).map(|k| v[k]).collect()
    }

    fn tab_matches(&self, tab: usize) -> bool {
        let q = self.search.trim().to_lowercase();
        q.is_empty()
            || TABS[tab].to_lowercase().contains(&q)
            || self.rows.iter().any(|r| r.tab == tab && (r.title.to_lowercase().contains(&q) || r.subtitle.to_lowercase().contains(&q)))
    }

    // ── Drawing ───────────────────────────────────────────────────────

    pub fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let p = Self::palette(style, theme);
        self.time_scale = style.time_scale();
        self.card = card;
        self.hits.clear();
        self.control_rects.clear();

        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);

        // ── Sidebar: search + tabs ──
        let sx = card.x + MARGIN;
        let sy = card.y + MARGIN;
        let search = Rect { x: sx, y: sy, w: SIDEBAR_W, h: 36.0 };
        fill_rounded_rect(cr, search.x, search.y, search.w, search.h, 18.0, (0.0, 0.0, 0.0, 0.0));
        let ring = if self.search_focus || self.hover == Some(Hit::Search) { (p.accent.0, p.accent.1, p.accent.2, 1.0) } else { (p.fg.0, p.fg.1, p.fg.2, 0.15) };
        stroke_rounded_rect(cr, search.x + 0.5, search.y + 0.5, search.w - 1.0, search.h - 1.0, 18.0, ring, 1.0);
        draw_in_box(cr, "\u{2315}", &TextStyle { family: &p.family, px: 15.0, bold: false, color: (p.fg.0, p.fg.1, p.fg.2, 0.5) }, search.x + 12.0, search.y, 16.0, search.h, Align::Start);
        let stext = if self.search.is_empty() && !self.search_focus { "Search" } else { &self.search };
        let scol = if self.search.is_empty() { (p.fg.0, p.fg.1, p.fg.2, 0.5) } else { p.fg };
        let (bx, _, bw, _) = draw_in_box(cr, stext, &TextStyle { family: &p.family, px: 14.0, bold: false, color: scol }, search.x + 34.0, search.y, search.w - 46.0, search.h, Align::Start);
        if self.search_focus {
            let cx = if self.search.is_empty() { search.x + 34.0 } else { bx + bw + 1.0 };
            fill_rounded_rect(cr, cx, search.y + 9.0, 1.5, search.h - 18.0, 0.0, p.accent);
        }
        self.hits.push((Hit::Search, search));

        let mut ty = sy + 36.0 + 16.0;
        // The selected tab's pill glides between tabs (drawn first, under the labels). Its
        // target is where the selected tab was last frame (the tab list rarely changes).
        let mut tab_rect = None;
        let tab_pill = if self.query().is_empty() {
            let (ry, rh) = self.last_tab_rect.map_or((ty + 38.0 * self.tab as f64, 38.0), |r| (r.y, r.h));
            let (py, ph) = self.tab_glide.to(ry, rh, style);
            fill_rounded_rect(cr, sx + 4.0, py, SIDEBAR_W - 8.0, ph, 8.0, (p.fg.0, p.fg.1, p.fg.2, 0.85));
            Some((py, ph))
        } else {
            self.tab_glide.reset();
            None
        };
        for (i, name) in TABS.iter().enumerate() {
            if !self.tab_matches(i) {
                continue;
            }
            let r = Rect { x: sx + 4.0, y: ty, w: SIDEBAR_W - 8.0, h: 38.0 };
            let sel = i == self.tab && self.query().is_empty();
            if sel {
                tab_rect = Some(r);
            }
            let hv = self.hover_fade.level(Hit::Tab(i), self.hover == Some(Hit::Tab(i)) && !sel, style);
            if hv > 0.0 {
                fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 8.0, (p.fg.0, p.fg.1, p.fg.2, 0.15 * hv));
            }
            let on = tab_pill.map_or(0.0, |(py, ph)| crate::ui::coverage(py, ph, r.y, r.h));
            let color = crate::ui::mix(p.fg, hex_to_rgba(&theme.inverse_primary, 1.0), on);
            let fam = if sel { format!("{} Semi-Bold", p.family) } else { p.family.clone() };
            draw_in_box(cr, name, &TextStyle { family: &fam, px: 14.0, bold: false, color }, r.x + 12.0, r.y, r.w - 24.0, r.h, Align::Start);
            self.hits.push((Hit::Tab(i), r));
            ty += 38.0;
        }
        if tab_rect != self.last_tab_rect {
            self.last_tab_rect = tab_rect;
            crate::ui::keep_animating();
        }

        // ── Content pane ──
        let pane = Rect { x: sx + SIDEBAR_W + MARGIN, y: sy, w: card.w - 3.0 * MARGIN - SIDEBAR_W, h: card.h - 2.0 * MARGIN };
        self.pane = pane;
        fill_rounded_rect(cr, pane.x, pane.y, pane.w, pane.h, style.radius, (0.0, 0.0, 0.0, 0.25));
        let ov = hex_to_rgba(&theme.outline_variant, 0.45);
        stroke_rounded_rect(cr, pane.x + 0.5, pane.y + 0.5, pane.w - 1.0, pane.h - 1.0, style.radius, ov, 1.0);

        let ix = pane.x + PANE_PAD;
        let iw = pane.w - 2.0 * PANE_PAD;
        let hy = pane.y + PANE_PAD;
        let title = if self.query().is_empty() { format!("\u{2699}  {} Settings", TABS[self.tab]) } else { format!("\u{2315}  Results for \u{201C}{}\u{201D}", self.search.trim()) };
        draw_in_box(cr, &title, &TextStyle { family: &format!("{} Semi-Bold", p.family), px: 20.0, bold: false, color: p.fg }, ix, hy, iw - 40.0, 32.0, Align::Start);
        let close = Rect { x: ix + iw - 32.0, y: hy, w: 32.0, h: 32.0 };
        let ch = self.hover_fade.level(Hit::Close, self.hover == Some(Hit::Close), style);
        fill_rounded_rect(cr, close.x, close.y, 32.0, 32.0, 16.0, (p.error.0, p.error.1, p.error.2, 0.8 * ch));
        if ch < 1.0 {
            stroke_rounded_rect(cr, close.x + 0.5, close.y + 0.5, 31.0, 31.0, 16.0, (p.outline.0, p.outline.1, p.outline.2, p.outline.3 * (1.0 - ch)), 1.0);
        }
        draw_in_box(cr, "\u{2715}", &TextStyle { family: &p.family, px: 14.0, bold: false, color: crate::ui::mix(p.fg, (1.0, 1.0, 1.0, 1.0), ch) }, close.x, close.y, 32.0, 32.0, Align::Center);
        self.hits.push((Hit::Close, close));
        let sep_y = hy + 32.0 + 16.0;
        fill_rounded_rect(cr, ix, sep_y, iw, 1.0, 0.0, hex_to_rgba(&theme.outline_variant, 0.3));

        // Footer
        let footer_y = pane.y + pane.h - PANE_PAD - 36.0;
        fill_rounded_rect(cr, ix, footer_y - 17.0, iw, 1.0, 0.0, hex_to_rgba(&theme.outline_variant, 0.3));
        let redo = Rect { x: ix + iw - 110.0, y: footer_y, w: 110.0, h: 36.0 };
        let undo = Rect { x: redo.x - 120.0, y: footer_y, w: 110.0, h: 36.0 };
        let (hu, hr) = (self.hover_fade.level(Hit::Undo, self.hover == Some(Hit::Undo), style), self.hover_fade.level(Hit::Redo, self.hover == Some(Hit::Redo), style));
        uikit::button(cr, undo, "Undo", ButtonKind::Outline, hu, !self.undo.is_empty(), &p);
        uikit::button(cr, redo, "Redo", ButtonKind::Outline, hr, !self.redo.is_empty(), &p);
        self.hits.push((Hit::Undo, undo));
        self.hits.push((Hit::Redo, redo));
        let msg = match &self.message {
            Some((m, ok)) => Some((m.clone(), *ok)),
            None => Some(("Changes apply instantly".to_string(), true)),
        };
        if let Some((m, ok)) = msg {
            let c = if ok { (p.fg.0, p.fg.1, p.fg.2, 0.6) } else { p.error };
            draw_in_box(cr, &m, &TextStyle { family: &p.family, px: 13.0, bold: false, color: c }, ix, footer_y, undo.x - ix - 12.0, 36.0, Align::Start);
        }

        // ── Rows (scrolling) ──
        let list = Rect { x: ix, y: sep_y + 17.0, w: iw, h: footer_y - 17.0 - 16.0 - (sep_y + 17.0) };
        // Room between text and control for the reset button.
        let text_w = list.w - CONTROL_W - 44.0;
        let title_fam = format!("{} Medium", p.family);
        let draft = self.config.clone();
        let visible = self.shown_rows();
        let searching = !self.query().is_empty();
        // While searching, a tab heading precedes each tab's group of results.
        const GROUP_H: f64 = 30.0;
        // Section headings inside a tab, and the extra space above all but the first.
        const HEADING_H: f64 = 46.0;
        const HEADING_GAP: f64 = 14.0;
        let group_start: Vec<bool> = visible.iter().enumerate().map(|(k, &i)| searching && (k == 0 || self.rows[visible[k - 1]].tab != self.rows[i].tab)).collect();
        // Measure
        let mut heights = Vec::with_capacity(visible.len());
        for (k, &i) in visible.iter().enumerate() {
            let r = &self.rows[i];
            if matches!(r.control, Control::Heading) {
                heights.push(if k == 0 { HEADING_H - HEADING_GAP } else { HEADING_H });
                continue;
            }
            if matches!(r.control, Control::Note) {
                heights.push(wrapped(cr, r.subtitle, &TextStyle { family: &p.family, px: 11.5, bold: false, color: p.fg }, list.w, None) + 28.0);
                continue;
            }
            let sub = row_subtitle(r);
            let th = 20.0 + if sub.is_empty() { 0.0 } else { 4.0 + wrapped(cr, &sub, &TextStyle { family: &p.family, px: 12.0, bold: false, color: p.fg }, if matches!(r.control, Control::Buttons(_) | Control::Pills { .. }) { list.w } else { text_w }, None) };
            let ctl_h = match r.control {
                Control::Buttons(_) | Control::Pills { .. } => th + 12.0 + 34.0,
                _ => th.max(36.0),
            };
            heights.push(ctl_h + 28.0 + if group_start[k] { GROUP_H } else { 0.0 });
        }
        self.content_h = heights.iter().sum();
        if std::mem::take(&mut self.scroll_to_focus) {
            let focused_row = match self.focus {
                Some(Hit::Control(i)) | Some(Hit::Button(i, _)) | Some(Hit::Browse(i)) => Some(i),
                _ => None,
            };
            if let Some(k) = focused_row.and_then(|f| visible.iter().position(|&i| i == f)) {
                let top: f64 = heights[..k].iter().sum();
                self.scroll.set_max(self.content_h - list.h, style);
                self.scroll.ensure_visible(top, heights[k], list.h);
            }
        }
        self.scroll.set_max(self.content_h - list.h, style);
        let scroll = self.scroll.pos();
        unsafe {
            cairo_save(cr);
            cairo_rectangle(cr, list.x - 4.0, list.y, list.w + 8.0, list.h);
            cairo_clip(cr);
        }
        // Tab change: the rows fade in and rise into place.
        let tab_in = self.tab_in.animated(0.002);
        let entering = tab_in < 0.999;
        if entering {
            unsafe {
                cairo_push_group(cr);
                cairo_translate(cr, 0.0, 14.0 * (1.0 - tab_in));
            }
        }
        let mut y = list.y - scroll;
        let mut open_combo_anchor = None;
        self.focus_order.clear();
        if visible.is_empty() {
            let msg = if searching { format!("No settings match \u{201C}{}\u{201D}", self.search.trim()) } else { "Nothing to configure here".into() };
            draw_in_box(cr, &msg, &TextStyle { family: &p.family, px: 14.0, bold: false, color: (p.fg.0, p.fg.1, p.fg.2, 0.6) }, list.x, list.y + 20.0, list.w, 24.0, Align::Center);
        }
        for (k, &i) in visible.iter().enumerate() {
            match self.rows[i].control {
                Control::Buttons(btns) => self.focus_order.extend((0..btns.len()).map(|b| Hit::Button(i, b))),
                Control::Pills { options, .. } => self.focus_order.extend((0..options.len()).map(|b| Hit::Button(i, b))),
                Control::Text { browse: true, .. } => self.focus_order.extend([Hit::Control(i), Hit::Browse(i)]),
                Control::Heading | Control::Note => {}
                _ => self.focus_order.push(Hit::Control(i)),
            }
            let mut h = heights[k];
            if y + h < list.y || y > list.y + list.h {
                y += h;
                continue;
            }
            if matches!(self.rows[i].control, Control::Heading) {
                // Accent label with a hairline running to the right edge.
                let top = if k == 0 { y } else { y + HEADING_GAP };
                let st = TextStyle { family: &format!("{} Semi-Bold", p.family), px: 12.0, bold: false, color: (p.accent.0, p.accent.1, p.accent.2, 0.95) };
                let (_, _, tw, _) = draw_in_box(cr, &self.rows[i].title.to_uppercase(), &st, list.x, top + 8.0, list.w, 20.0, Align::Start);
                let lx = list.x + tw + 12.0;
                if lx < list.x + list.w {
                    fill_rounded_rect(cr, lx, top + 18.0, list.x + list.w - lx, 1.0, 0.0, hex_to_rgba(&theme.outline_variant, 0.35));
                }
                y += h;
                continue;
            }
            if matches!(self.rows[i].control, Control::Note) {
                wrapped(cr, self.rows[i].subtitle, &TextStyle { family: &p.family, px: 11.5, bold: false, color: (p.fg.0, p.fg.1, p.fg.2, 0.55) }, list.w, Some((list.x, y + 14.0)));
                y += h;
                continue;
            }
            if group_start[k] {
                let ty = if k == 0 { y } else { y + 8.0 };
                draw_in_box(cr, &TABS[self.rows[i].tab].to_uppercase(), &TextStyle { family: &format!("{} Semi-Bold", p.family), px: 11.0, bold: false, color: (p.accent.0, p.accent.1, p.accent.2, 0.9) }, list.x, ty, list.w, GROUP_H - 8.0, Align::Start);
                y += GROUP_H;
                h -= GROUP_H;
            }
            let r = &self.rows[i];
            let ry = y + 14.0;
            let (_, _, tw, _) = draw_in_box(cr, r.title, &TextStyle { family: &title_fam, px: 15.0, bold: false, color: p.fg }, list.x, ry, text_w, 20.0, Align::Start);
            let modified = r.control.value(&draft) != r.control.value(&self.baseline);
            if modified {
                // Changed-this-session marker after the title, and a revert button.
                fill_rounded_rect(cr, list.x + tw.min(text_w - 6.0) + 8.0, ry + 7.0, 6.0, 6.0, 3.0, p.accent);
                let rr = Rect { x: list.x + list.w - CONTROL_W - 36.0, y: y + h / 2.0 - 14.0, w: 28.0, h: 28.0 };
                let rh = self.hover_fade.level(Hit::Reset(i), self.hover == Some(Hit::Reset(i)), style);
                if rh > 0.0 {
                    fill_rounded_rect(cr, rr.x, rr.y, rr.w, rr.h, 14.0, (p.fg.0, p.fg.1, p.fg.2, 0.15 * rh));
                }
                let a = 0.55 + 0.45 * rh;
                draw_in_box(cr, "\u{21BA}", &TextStyle { family: &p.family, px: 16.0, bold: false, color: (p.fg.0, p.fg.1, p.fg.2, a) }, rr.x, rr.y, rr.w, rr.h, Align::Center);
                self.hits.push((Hit::Reset(i), rr));
            }
            let sub = row_subtitle(r);
            if !sub.is_empty() {
                let sw = if matches!(r.control, Control::Buttons(_) | Control::Pills { .. }) { list.w } else { text_w };
                wrapped(cr, &sub, &TextStyle { family: &p.family, px: 12.0, bold: false, color: (p.fg.0, p.fg.1, p.fg.2, 0.6) }, sw, Some((list.x, ry + 24.0)));
            }
            let cx = list.x + list.w - CONTROL_W;
            let mid = y + h / 2.0;
            let hov = self.hover_fade.level(Hit::Control(i), self.hover == Some(Hit::Control(i)) || self.drag == Some(i), style);
            match &r.control {
                Control::Switch(get, _) => {
                    let sr = Rect { x: list.x + list.w - 50.0, y: mid - 14.0, w: 50.0, h: 28.0 };
                    let knob = self.knobs.settled_level(i, get(&draft), crate::ui::SPRING_KNOB, style);
                    uikit::switch(cr, sr, knob, hov, &p);
                    self.control_rects.push((i, sr));
                }
                Control::Slider { min, max, get, fmt, .. } => {
                    let v = get(&draft);
                    let label = fmt(v);
                    draw_in_box(cr, &label, &TextStyle { family: &p.family, px: 12.0, bold: true, color: p.fg }, cx, mid - 9.0, 60.0, 18.0, Align::Start);
                    let sr = Rect { x: cx + 66.0, y: mid - 12.0, w: CONTROL_W - 76.0, h: 24.0 };
                    uikit::slider(cr, sr, (v - min) / (max - min), hov, &p);
                    self.control_rects.push((i, sr));
                }
                Control::Text { get, placeholder, browse, .. } => {
                    let fw = if *browse { CONTROL_W - 46.0 } else { CONTROL_W };
                    let tr = Rect { x: cx, y: mid - 18.0, w: fw, h: 36.0 };
                    if *browse {
                        let br = Rect { x: cx + fw + 8.0, y: mid - 18.0, w: 38.0, h: 36.0 };
                        let busy = self.picking.is_some();
                        let bh = self.hover_fade.level(Hit::Browse(i), self.hover == Some(Hit::Browse(i)), style);
                        uikit::button(cr, br, "\u{2026}", ButtonKind::Outline, bh, !busy, &p);
                        self.hits.push((Hit::Browse(i), br));
                    }
                    let (text, editing) = match &self.editing {
                        Some((ei, buf)) if *ei == i => (buf.clone(), true),
                        _ => (get(&draft), false),
                    };
                    uikit::text_field(cr, tr, &text, placeholder, editing, hov, self.invalid == Some(i), &p);
                    self.control_rects.push((i, tr));
                }
                Control::Combo { options, get, fonts, .. } => {
                    let tr = Rect { x: cx, y: mid - 18.0, w: CONTROL_W, h: 36.0 };
                    let cur = get(&draft);
                    let opts = options();
                    let label = opts.iter().find(|(_, v)| *v == cur).map(|(l, _)| l.clone()).unwrap_or_else(|| if cur.is_empty() { "—".into() } else { cur.clone() });
                    let open = self.combo.as_ref().map(|(ci, _)| *ci == i).unwrap_or(false);
                    let font = if *fonts && !cur.is_empty() { Some(cur.as_str()) } else { None };
                    uikit::combo(cr, tr, &label, open, hov, &p, font);
                    if open {
                        open_combo_anchor = Some(tr);
                    }
                    self.control_rects.push((i, tr));
                }
                Control::Buttons(btns) => {
                    let mut bx = list.x;
                    let by = y + h - 14.0 - 34.0;
                    for (b, (label, _, kind)) in btns.iter().enumerate() {
                        let w = (crate::desktop::text::measure(cr, label, &TextStyle { family: &p.family, px: 13.0, bold: true, color: p.fg }).0 + 40.0).max(120.0);
                        let br = Rect { x: bx, y: by, w, h: 34.0 };
                        let bh = self.hover_fade.level(Hit::Button(i, b), self.hover == Some(Hit::Button(i, b)), style);
                        uikit::button(cr, br, label, *kind, bh, true, &p);
                        self.hits.push((Hit::Button(i, b), br));
                        bx += w + 10.0;
                    }
                }
                Control::Pills { options, get, .. } => {
                    let on = get(&self.config);
                    let mut bx = list.x;
                    let by = y + h - 14.0 - 34.0;
                    for (b, (label, value)) in options.iter().enumerate() {
                        let w = (crate::desktop::text::measure(cr, label, &TextStyle { family: &p.family, px: 13.0, bold: true, color: p.fg }).0 + 40.0).max(100.0);
                        let br = Rect { x: bx, y: by, w, h: 34.0 };
                        let bh = self.hover_fade.level(Hit::Button(i, b), self.hover == Some(Hit::Button(i, b)), style);
                        let kind = if on.iter().any(|v| v == value) { ButtonKind::Primary } else { ButtonKind::Outline };
                        uikit::button(cr, br, label, kind, bh, true, &p);
                        self.hits.push((Hit::Button(i, b), br));
                        bx += w + 10.0;
                    }
                }
                Control::Heading | Control::Note => {}
            }
            let next_heading = visible.get(k + 1).is_some_and(|&n| matches!(self.rows[n].control, Control::Heading));
            if k + 1 < visible.len() && !next_heading && !group_start.get(k + 1).copied().unwrap_or(false) {
                fill_rounded_rect(cr, list.x, y + h - 0.5, list.w, 1.0, 0.0, hex_to_rgba(&theme.outline_variant, 0.15));
            }
            y += h;
        }
        if let (true, Some(f)) = (self.focus_visible && !self.search_focus, self.focus) {
            let rect = match f {
                Hit::Control(i) => self.control_rects.iter().find(|(k, _)| *k == i).map(|(_, r)| *r),
                h => self.hits.iter().find(|(k, _)| *k == h).map(|(_, r)| *r),
            };
            if let Some(r) = rect {
                // The ring glides between controls (in content coordinates, so it scrolls along).
                let (gx, gw) = self.focus_glide.0.to(r.x, r.w, style);
                let (gy, gh) = self.focus_glide.1.to(r.y + scroll, r.h, style);
                let r = Rect { x: gx, y: gy - scroll, w: gw, h: gh };
                let rad = if matches!(f, Hit::Control(i) if matches!(self.rows[i].control, Control::Switch(..) | Control::Slider { .. })) { (r.h + 6.0) / 2.0 } else { 20.0 };
                stroke_rounded_rect(cr, r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0, rad.min((r.h + 6.0) / 2.0), p.accent, 2.0);
            }
        } else {
            self.focus_glide.0.reset();
            self.focus_glide.1.reset();
        }
        unsafe {
            if entering {
                cairo_pop_group_to_source(cr);
                cairo_paint_with_alpha(cr, tab_in.clamp(0.0, 1.0));
            }
            cairo_restore(cr);
        }
        if self.content_h > list.h {
            let thumb_h = (list.h * list.h / self.content_h).max(30.0);
            let tyy = list.y + (list.h - thumb_h) * (scroll / (self.content_h - list.h)).clamp(0.0, 1.0);
            fill_rounded_rect(cr, pane.x + pane.w - 9.0, tyy, 3.0, thumb_h, 1.5, (p.fg.0, p.fg.1, p.fg.2, 0.3));
        }
        // Clip control hit rects to the visible list.
        self.control_rects.retain(|(_, r)| r.y + r.h > list.y && r.y < list.y + list.h);
        self.hits.retain(|(h, r)| !matches!(h, Hit::Button(..) | Hit::Reset(_) | Hit::Browse(_)) || (r.y + r.h > list.y && r.y < list.y + list.h));

        // Open dropdown on top of everything else in the card.
        if let (Some(anchor), Some((ci, dd))) = (open_combo_anchor, self.combo.as_mut()) {
            if let Control::Combo { options, get, fonts, .. } = &self.rows[*ci].control {
                let opts = options();
                let labels: Vec<String> = opts.iter().map(|(l, _)| l.clone()).collect();
                let cur = get(&draft);
                let sel = opts.iter().position(|(_, v)| *v == cur);
                dd.draw(cr, anchor, card, &labels, sel, *fonts, &p);
            }
        }

    }

    /// Start the rows' entrance after a tab change.
    fn tab_entrance(&mut self) {
        self.tab_in.snap(0.0);
        self.tab_in.set_scaled(1.0, (0.32, 0.9), self.time_scale);
    }

    // ── Input ─────────────────────────────────────────────────────────

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        if let Some(h) = self.hits.iter().rev().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h) {
            return Some(h);
        }
        self.control_rects.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| Hit::Control(*i))
    }

    /// Pointer motion; returns true if a redraw is needed.
    pub fn motion(&mut self, x: f64, y: f64, pressed: bool) -> bool {
        if let (Some(i), true) = (self.drag, pressed) {
            self.set_slider(i, x);
            return true;
        }
        if let Some((_, dd)) = self.combo.as_mut() {
            if dd.contains(x, y) {
                let h = dd.pick(x, y);
                if h != dd.hover {
                    dd.hover = h;
                    return true;
                }
                return false;
            }
        }
        let h = self.hit_at(x, y);
        if h != self.hover {
            self.hover = h;
            return true;
        }
        false
    }

    pub fn release(&mut self) -> bool {
        if self.drag.take().is_none() {
            return false;
        }
        if let Some(before) = self.drag_start.take() {
            let after = self.config.clone();
            self.change(before, after);
        }
        true
    }

    pub fn scroll(&mut self, x: f64, y: f64, delta: f64) -> bool {
        if let Some((_, dd)) = self.combo.as_mut() {
            if dd.contains(x, y) {
                dd.scroll_by(delta);
                return true;
            }
        }
        if self.pane.contains(x, y) {
            self.combo = None;
            return self.scroll.by(delta);
        }
        false
    }

    fn set_slider(&mut self, i: usize, x: f64) {
        let Some((_, r)) = self.control_rects.iter().find(|(k, _)| *k == i).copied() else { return };
        if let Control::Slider { min, max, step, set, .. } = &self.rows[i].control {
            let t = uikit::slider_value(r, x);
            let v = ((min + (max - min) * t) / step).round() * step;
            set(&mut self.config, v.clamp(*min, *max));
        }
    }

    fn commit_edit(&mut self) {
        if let Some((i, buf)) = self.editing.take() {
            if let Control::Text { set, .. } = &self.rows[i].control {
                let mut after = self.config.clone();
                if set(&mut after, &buf) {
                    self.invalid = None;
                    let before = self.config.clone();
                    self.change(before, after);
                    if self.rows[i].title == "Wallpaper Directory" {
                        let dir = expand_home(&self.config.wallpaper_dir);
                        if !dir.is_dir() {
                            self.message = Some((format!("{} does not exist yet", dir.display()), false));
                        }
                    }
                } else {
                    self.invalid = Some(i);
                    self.message = Some((format!("Invalid value for \"{}\"", self.rows[i].title), false));
                }
            }
        }
    }

    pub fn press(&mut self, x: f64, y: f64) -> Effect {
        // An open dropdown takes the click first.
        if let Some((ci, dd)) = self.combo.as_ref() {
            let ci = *ci;
            if dd.contains(x, y) {
                if let Some(idx) = dd.pick(x, y) {
                    self.pick_option(ci, idx);
                    self.combo = None;
                }
                return Effect::Redraw;
            }
            self.combo = None;
            if self.hit_at(x, y) == Some(Hit::Control(ci)) {
                return Effect::Redraw; // clicking the combo again closes it
            }
        }
        if self.editing.as_ref().map(|(i, _)| self.hit_at(x, y) != Some(Hit::Control(*i))).unwrap_or(false) {
            self.commit_edit();
        }
        self.search_focus = false;
        self.focus_visible = false;
        let Some(hit) = self.hit_at(x, y) else { return Effect::Redraw };
        if matches!(hit, Hit::Control(_) | Hit::Button(..) | Hit::Browse(_)) {
            self.focus = Some(hit);
        }
        match hit {
            Hit::Search => self.search_focus = true,
            Hit::Tab(t) => {
                if t != self.tab {
                    self.tab_entrance();
                }
                self.tab = t;
                self.search.clear();
                self.scroll.reset(0.0);
                self.focus = None;
            }
            Hit::Close => return self.request_close(),
            Hit::Reset(i) => {
                self.editing = None;
                self.invalid = None;
                let before = self.config.clone();
                let mut after = before.clone();
                self.rows[i].control.copy_value(&self.baseline, &mut after);
                self.change(before, after);
                self.message = Some((format!("\u{201C}{}\u{201D} reverted", self.rows[i].title), true));
            }
            Hit::Browse(i) => self.browse(i),
            Hit::Undo => self.undo(),
            Hit::Redo => self.redo(),
            Hit::Button(i, b) => {
                if let Control::Buttons(btns) = &self.rows[i].control {
                    let action = btns[b].1;
                    return self.run_action(action);
                }
                self.toggle_pill(i, b);
            }
            Hit::Control(i) => match &self.rows[i].control {
                Control::Switch(get, set) => {
                    let mut after = self.config.clone();
                    set(&mut after, !get(&self.config));
                    let before = self.config.clone();
                    self.change(before, after);
                }
                Control::Slider { .. } => {
                    self.drag = Some(i);
                    self.drag_start = Some(self.config.clone());
                    self.set_slider(i, x);
                }
                Control::Text { get, .. } => {
                    if self.editing.as_ref().map(|(e, _)| *e != i).unwrap_or(true) {
                        self.editing = Some((i, get(&self.config)));
                    }
                }
                Control::Combo { .. } => self.combo = Some((i, Dropdown::default())),
                Control::Buttons(_) | Control::Pills { .. } | Control::Heading | Control::Note => {}
            },
        }
        Effect::Redraw
    }

    /// Esc / close button: close an open dropdown or edit first, else the window.
    /// Every change is already saved, so closing never asks.
    pub fn request_close(&mut self) -> Effect {
        if self.combo.take().is_some() {
            return Effect::Redraw;
        }
        if self.editing.take().is_some() {
            self.invalid = None;
            return Effect::Redraw;
        }
        self.release();
        Effect::Close
    }

    /// Keyboard input. `special`: evdev-style code of a non-text key (Esc 1, Backspace 14,
    /// Tab 15, Enter 28, arrows, paging) or 0; `typed`: the character for text keys
    /// (with `ctrl`, the plain letter of the shortcut).
    pub fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        const ESC: u32 = 1;
        const BACKSPACE: u32 = 14;
        const TAB: u32 = 15;
        const ENTER: u32 = 28;
        const HOME: u32 = 102;
        const UP: u32 = 103;
        const PGUP: u32 = 104;
        const LEFT: u32 = 105;
        const RIGHT: u32 = 106;
        const END: u32 = 107;
        const DOWN: u32 = 108;
        const PGDN: u32 = 109;

        if special == ESC {
            if self.combo.is_none() && self.editing.is_none() && !self.search.is_empty() {
                self.search.clear();
                self.search_focus = false;
                self.scroll.reset(0.0);
                return Effect::Redraw;
            }
            return self.request_close();
        }
        // Shortcuts: Ctrl+Z undo, Ctrl+Y / Ctrl+Shift+Z redo, Ctrl+F search, Ctrl+S save
        // (every change is already saved; this commits a field being edited),
        // Ctrl+PageUp/PageDown switch tabs.
        if ctrl {
            match typed {
                Some('z') if self.editing.is_none() => {
                    self.combo = None;
                    if shift { self.redo() } else { self.undo() }
                }
                Some('y') if self.editing.is_none() => {
                    self.combo = None;
                    self.redo();
                }
                Some('f') => {
                    self.commit_edit();
                    self.combo = None;
                    self.search_focus = true;
                    self.focus_visible = false;
                }
                Some('s') => {
                    self.commit_edit();
                    if self.invalid.is_none() {
                        self.message = Some(("All changes saved".into(), true));
                    }
                }
                Some(_) => return Effect::None,
                None => {}
            }
            if typed.is_some() {
                return Effect::Redraw;
            }
        }
        if typed.is_some_and(|c| c.is_control()) {
            return Effect::None;
        }
        if ctrl && (special == PGUP || special == PGDN) {
            self.commit_edit();
            self.combo = None;
            self.search.clear();
            self.tab = if special == PGDN { (self.tab + 1) % TABS.len() } else { (self.tab + TABS.len() - 1) % TABS.len() };
            self.tab_entrance();
            self.scroll.reset(0.0);
            self.focus = None;
            return Effect::Redraw;
        }

        // Open dropdown: arrows move, Enter picks, typing filters.
        if let Some((ci, dd)) = self.combo.as_mut() {
            let ci = *ci;
            let Control::Combo { options, .. } = &self.rows[ci].control else { return Effect::None };
            let labels: Vec<String> = options().into_iter().map(|(l, _)| l).collect();
            match special {
                UP => dd.step(&labels, -1),
                DOWN => dd.step(&labels, 1),
                PGUP => dd.step(&labels, -8),
                PGDN => dd.step(&labels, 8),
                ENTER => {
                    if let Some(idx) = dd.current(&labels) {
                        self.pick_option(ci, idx);
                    }
                    self.combo = None;
                }
                TAB => self.combo = None,
                _ => return if dd.key(typed, special == BACKSPACE) { Effect::Redraw } else { Effect::None },
            }
            return Effect::Redraw;
        }

        if let Some((_, buf)) = self.editing.as_mut() {
            match (special, typed) {
                (ENTER, _) => self.commit_edit(),
                (TAB, _) => {
                    self.commit_edit();
                    self.move_focus(if shift { -1 } else { 1 });
                }
                (BACKSPACE, _) => {
                    buf.pop();
                }
                (_, Some(c)) => buf.push(c),
                _ => return Effect::None,
            }
            return Effect::Redraw;
        }

        if self.search_focus {
            match (special, typed) {
                (DOWN | TAB | ENTER, _) => {
                    self.search_focus = false;
                    self.focus = None;
                    self.move_focus(1);
                }
                (BACKSPACE, _) => {
                    self.search.pop();
                    self.scroll.reset(0.0);
                }
                (_, Some(c)) => {
                    self.search.push(c);
                    self.scroll.reset(0.0);
                }
                _ => return Effect::None,
            }
            return Effect::Redraw;
        }

        // Navigating the rows.
        match (special, typed) {
            (TAB, _) => self.move_focus(if shift { -1 } else { 1 }),
            (DOWN, _) => self.move_focus(1),
            (UP, _) => {
                if self.focus_order.first() == self.focus.as_ref() {
                    self.focus = None;
                    self.search_focus = true;
                } else {
                    self.move_focus(-1);
                }
            }
            (LEFT | RIGHT, _) => {
                let dir = if special == RIGHT { 1 } else { -1 };
                match self.focus {
                    Some(Hit::Control(i)) => {
                        if let Control::Slider { min, max, step, get, set, .. } = self.rows[i].control {
                            let mut after = self.config.clone();
                            let v = (get(&self.config) + step * dir as f64).clamp(min, max);
                            set(&mut after, v);
                            let before = self.config.clone();
                            self.change(before, after);
                        } else if let Control::Switch(get, set) = self.rows[i].control {
                            if get(&self.config) != (dir > 0) {
                                let mut after = self.config.clone();
                                set(&mut after, dir > 0);
                                let before = self.config.clone();
                                self.change(before, after);
                            }
                        }
                    }
                    Some(Hit::Button(..)) => self.move_focus(dir),
                    _ => self.move_focus(dir),
                }
                self.focus_visible = true;
            }
            (ENTER, _) | (_, Some(' ')) if self.focus.is_some() => return self.activate(),
            (PGUP | PGDN, _) => {
                let page = (self.pane.h - 160.0).max(100.0);
                self.scroll.to(self.scroll.target() + if special == PGDN { page } else { -page });
            }
            (HOME, _) => {
                self.focus = None;
                self.move_focus(1);
            }
            (END, _) => {
                self.focus = None;
                self.move_focus(-1);
            }
            (BACKSPACE, _) if !self.search.is_empty() => {
                self.search.pop();
                self.search_focus = true;
            }
            // Typing anywhere else goes to the search box (like the QML window).
            (_, Some(c)) if c != ' ' => {
                self.search_focus = true;
                self.focus_visible = false;
                self.search.push(c);
                self.scroll.reset(0.0);
            }
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// Move keyboard focus `delta` steps through the focusable controls, wrapping.
    fn move_focus(&mut self, delta: i32) {
        let n = self.focus_order.len() as i32;
        if n == 0 {
            self.focus = None;
            return;
        }
        let pos = self.focus.and_then(|f| self.focus_order.iter().position(|&h| h == f));
        let k = match pos {
            Some(p) => (p as i32 + delta).rem_euclid(n),
            None if delta > 0 => 0,
            None => n - 1,
        };
        self.focus = Some(self.focus_order[k as usize]);
        self.focus_visible = true;
        self.scroll_to_focus = true;
    }

    /// Turn pill `b` of row `i` on or off (options keep their listed order).
    fn toggle_pill(&mut self, i: usize, b: usize) {
        let Control::Pills { options, get, set } = self.rows[i].control else { return };
        let mut on = get(&self.config);
        let v = options[b].1.to_string();
        match on.iter().position(|x| *x == v) {
            Some(k) => {
                on.remove(k);
            }
            None => on.push(v),
        }
        on.sort_by_key(|x| options.iter().position(|(_, o)| o == x));
        let before = self.config.clone();
        let mut after = before.clone();
        set(&mut after, on);
        self.change(before, after);
    }

    /// Enter / Space on the focused control.
    fn activate(&mut self) -> Effect {
        match self.focus {
            Some(Hit::Browse(i)) => self.browse(i),
            Some(Hit::Button(i, b)) => {
                if let Control::Buttons(btns) = self.rows[i].control {
                    return self.run_action(btns[b].1);
                }
                self.toggle_pill(i, b);
            }
            Some(Hit::Control(i)) => match &self.rows[i].control {
                Control::Switch(get, set) => {
                    let mut after = self.config.clone();
                    set(&mut after, !get(&self.config));
                    let before = self.config.clone();
                    self.change(before, after);
                }
                Control::Text { get, .. } => self.editing = Some((i, get(&self.config))),
                Control::Combo { options, get, .. } => {
                    let cur = get(&self.config);
                    let dd = Dropdown { hover: options().iter().position(|(_, v)| *v == cur), ..Dropdown::default() };
                    self.combo = Some((i, dd));
                }
                Control::Slider { .. } | Control::Buttons(_) | Control::Pills { .. } | Control::Heading | Control::Note => {}
            },
            _ => {}
        }
        Effect::Redraw
    }

    /// Open the system folder picker for a path row. The window is lowered below normal
    /// windows while the dialog is up (it would otherwise open underneath the overlay).
    fn browse(&mut self, i: usize) {
        if self.picking.is_some() {
            return;
        }
        let Control::Text { get, .. } = self.rows[i].control else { return };
        self.commit_edit();
        self.combo = None;
        let start = expand_home(&get(&self.config));
        let title = format!("Choose {}", self.rows[i].title);
        let tx = self.pick_tx.clone();
        let w = self.waker.clone();
        self.picking = Some(i);
        self.layer_request = Some(true);
        std::thread::spawn(move || {
            let _ = tx.send(crate::portal::pick_folder(&title, Some(&start)));
            w.wake();
        });
    }

    /// True while the window is hidden for a polkit dialog.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Some(true): lower the window for a dialog; Some(false): restore it.
    pub fn take_layer_request(&mut self) -> Option<bool> {
        self.layer_request.take()
    }

    fn pick_option(&mut self, row: usize, idx: usize) {
        if let Control::Combo { options, set, .. } = &self.rows[row].control {
            if let Some((_, v)) = options().get(idx) {
                let mut after = self.config.clone();
                set(&mut after, v);
                let before = self.config.clone();
                self.change(before, after);
            }
        }
    }

    // ── Changes, undo & actions ───────────────────────────────────────

    /// Apply one user change: persist it, record it for undo, run follow-up work.
    fn change(&mut self, before: Config, after: Config) {
        if self.commit(&before, &after) {
            self.undo.push(before);
            self.undo.truncate_front(UNDO_LIMIT);
            self.redo.clear();
        }
    }

    fn undo(&mut self) {
        self.commit_edit();
        if let Some(prev) = self.undo.pop() {
            let cur = self.config.clone();
            if self.commit(&cur, &prev) {
                self.redo.push(cur);
                self.message = Some(("Undone".into(), true));
            } else {
                self.undo.push(prev);
            }
        }
    }

    fn redo(&mut self) {
        self.commit_edit();
        if let Some(next) = self.redo.pop() {
            let cur = self.config.clone();
            if self.commit(&cur, &next) {
                self.undo.push(cur);
                self.message = Some(("Redone".into(), true));
            } else {
                self.redo.push(next);
            }
        }
    }

    /// Write the fields that differ between `before` and `after` into config.json, keeping
    /// everything else as it is on disk. Updates `self.config` to the merged result.
    /// False if nothing changed or the write failed.
    fn commit(&mut self, before: &Config, after: &Config) -> bool {
        let (Ok(b), Ok(a)) = (serde_json::to_value(before), serde_json::to_value(after)) else { return false };
        if a == b {
            self.config = after.clone();
            return false;
        }
        let mut disk = serde_json::to_value(Config::load()).unwrap_or_else(|_| a.clone());
        merge_changes(&mut disk, &b, &a);
        let merged: Config = match serde_json::from_value(disk) {
            Ok(c) => c,
            Err(_) => after.clone(),
        };
        if let Err(e) = merged.save() {
            self.message = Some((format!("Save failed: {}", e), false));
            return false;
        }
        let palette_changed = before.matugen_mode != after.matugen_mode || before.matugen_scheme != after.matugen_scheme;
        self.config = merged;
        self.message = None;
        if palette_changed {
            self.regenerate_palette();
        }
        true
    }

    fn spawn_job(&self, f: impl FnOnce() -> (String, bool) + Send + 'static) {
        let tx = self.bg_tx.clone();
        let w = self.waker.clone();
        std::thread::spawn(move || {
            let r = f();
            let _ = tx.send(r);
            w.wake();
        });
    }

    /// Re-run matugen on the current wallpaper with the saved mode/scheme (regenerate-theme.sh).
    fn regenerate_palette(&mut self) {
        let home = std::env::var("HOME").unwrap_or_default();
        let wallpaper = format!("{}/.wa.jpg", home);
        let (mode, scheme) = (self.config.matugen_mode.clone(), self.config.matugen_scheme.clone());
        self.message = Some(("Regenerating colours…".into(), true));
        self.spawn_job(move || {
            let out = std::process::Command::new("matugen")
                .args(["image", &wallpaper, "-m", &mode, "-t", &scheme, "--source-color-index", "0"])
                .output();
            match out {
                Ok(o) if o.status.success() => ("Colours regenerated".into(), true),
                Ok(o) => (format!("matugen failed: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")), false),
                Err(e) => (format!("matugen not available: {}", e), false),
            }
        });
    }

    fn run_action(&mut self, action: Action) -> Effect {
        match action {
            Action::EditMode => {
                self.commit_edit();
                self.pending_edit_mode = true;
                return Effect::Close;
            }
            Action::OpenPhone => {
                self.commit_edit();
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = prism_common::ipc::request(&prism_common::IpcCommand::PhoneCard("open".into()));
                });
                return Effect::Close;
            }
            Action::ResetLayout => {
                let before = self.config.clone();
                let mut after = before.clone();
                after.desktop_layout = prism_common::config::DesktopLayoutConfig::default();
                self.change(before, after);
                self.message = Some(("Desktop layout reset".into(), true));
            }
            Action::RebuildIndex => {
                self.message = Some(("Rebuilding file index…".into(), true));
                let waker = self.waker.clone();
                // Hide the window while the polkit dialog is up; show it again once it closes.
                self.hidden = true;
                self.layer_request = Some(true);
                let (hide_tx, hide_waker) = (self.hide_tx.clone(), self.waker.clone());
                self.spawn_job(move || {
                    let restore = move || {
                        let _ = hide_tx.send(false);
                        hide_waker.wake();
                    };
                    let r = match prism_common::files::rebuild_index(restore) {
                        Ok(()) => ("Index updated".into(), true),
                        Err(e) => (e, false),
                    };
                    refresh_index_time(waker);
                    r
                });
            }
            Action::DepthGenerate => {
                self.message = Some(("Generating depth mask…".into(), true));
                // prismd's depth worker generates and publishes the mask for the desktop.
                let cmd = prism_common::IpcCommand::Depth(prism_common::ipc::DepthAction::Generate);
                self.spawn_job(move || match prism_common::ipc::request(&cmd) {
                    Ok(_) => ("Generating depth mask; the widgets update when it is ready".into(), true),
                    Err(e) => (format!("Depth generation failed: {}", e), false),
                });
            }
            Action::DepthInstallCpu | Action::DepthInstallGpu => {
                let gpu = action == Action::DepthInstallGpu;
                self.message = Some((if gpu { "Installing GPU depth runtime (~1.5 GB)\u{2026}" } else { "Installing CPU depth runtime (~120 MB)\u{2026}" }.into(), true));
                let waker = self.waker.clone();
                self.spawn_job(move || {
                    let r = match prism_common::depth::script() {
                        None => ("wallpaper-depth.py not found".into(), false),
                        Some(script) => {
                            let mut c = std::process::Command::new("python3");
                            c.arg(script).arg("install");
                            if gpu {
                                c.arg("--gpu");
                            }
                            match c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status() {
                                Ok(s) if s.success() => ("Depth runtime installed".into(), true),
                                Ok(_) => ("Depth runtime install failed".into(), false),
                                Err(e) => (format!("Depth runtime install failed: {e}"), false),
                            }
                        }
                    };
                    refresh_depth_status(waker);
                    r
                });
            }
            Action::DepthClear => {
                self.message = Some(match prism_common::depth::clear_depth() {
                    Ok(_) => ("Depth cache cleared".into(), true),
                    Err(e) => (format!("Clear failed: {}", e), false),
                });
            }
        }
        Effect::Redraw
    }

    /// True once if the window was closed to enter desktop edit mode.
    pub fn take_edit_mode(&mut self) -> bool {
        std::mem::take(&mut self.pending_edit_mode)
    }
}

trait TruncateFront {
    fn truncate_front(&mut self, max: usize);
}

impl<T> TruncateFront for Vec<T> {
    fn truncate_front(&mut self, max: usize) {
        if self.len() > max {
            self.drain(..self.len() - max);
        }
    }
}

/// Copy into `disk` every value that differs between `before` and `after`, recursing into
/// objects so a change to one widget's visibility leaves its position on disk alone.
fn merge_changes(disk: &mut serde_json::Value, before: &serde_json::Value, after: &serde_json::Value) {
    use serde_json::Value;
    match (disk, before, after) {
        (Value::Object(d), Value::Object(b), Value::Object(a)) => {
            for (k, av) in a {
                let bv = b.get(k);
                if bv == Some(av) {
                    continue;
                }
                match (d.get_mut(k), bv) {
                    (Some(dv), Some(bv)) if dv.is_object() && bv.is_object() && av.is_object() => merge_changes(dv, bv, av),
                    _ => {
                        d.insert(k.clone(), av.clone());
                    }
                }
            }
        }
        (d, _, a) => *d = a.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_keeps_untouched_fields_from_disk() {
        let before = json!({"a": 1, "b": 2, "layout": {"clock": {"x": 0.1, "visible": true}}});
        let after = json!({"a": 5, "b": 2, "layout": {"clock": {"x": 0.1, "visible": false}}});
        // Meanwhile the clock moved and `b` changed on disk.
        let mut disk = json!({"a": 1, "b": 9, "layout": {"clock": {"x": 0.7, "visible": true}}});
        merge_changes(&mut disk, &before, &after);
        assert_eq!(disk, json!({"a": 5, "b": 9, "layout": {"clock": {"x": 0.7, "visible": false}}}));
    }

    #[test]
    fn undo_history_is_bounded() {
        let mut v: Vec<u32> = (0..10).collect();
        v.truncate_front(3);
        assert_eq!(v, vec![7, 8, 9]);
    }
}
