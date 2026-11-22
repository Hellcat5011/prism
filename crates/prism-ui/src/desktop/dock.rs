// ─────────────────────────────────────────────────────────────────────────
// desktop/dock.rs — the app dock
//
// Pinned apps first, then every other app with open windows. Pins live in
// ~/.config/prism/dock.json, apart from config.json, so a pin made while
// Settings is open is never overwritten by Settings saving its copy.
// ─────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, Once};
use std::time::{Duration, Instant};

use prism_common::apps::DesktopApp;
use prism_common::config::DockConfig;
use prism_common::Config;
use serde::{Deserialize, Serialize};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1;

use crate::cairo_backend::*;
use crate::ui::{Fades, SpringValue, Style};
use crate::waker::Waker;

use super::layout::Rect;
use super::panel::PanelHit;
use super::text::{draw_in_box, Align, TextStyle};
use super::widgets::Look;
use super::windows::Toplevel;

/// Padding around the row of icons, and the space between neighbours.
const PAD: f64 = 8.0;
const SPACING: f64 = 12.0;
/// Room on the screen edge's side for the running indicator.
const INDICATOR: f64 = 8.0;
/// How long the dock stays after the pointer has left.
const HIDE_DELAY: Duration = Duration::from_millis(400);
/// Slide in and out.
const SPRING_SLIDE: (f64, f64) = (0.36, 0.9);

// ── Pins ──────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct PinFile {
    pinned: Vec<String>,
}

fn pin_path() -> PathBuf {
    Config::config_path().with_file_name("dock.json")
}

fn read_pins() -> Option<Vec<String>> {
    let text = std::fs::read_to_string(pin_path()).ok()?;
    serde_json::from_str::<PinFile>(&text).ok().map(|f| f.pinned)
}

fn write_pins(pinned: &[String]) {
    let path = pin_path();
    let Ok(text) = serde_json::to_string_pretty(&PinFile { pinned: pinned.to_vec() }) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The desktop entry a command like `xdg-mime query default` names, without its extension.
fn query_default(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let id = name.strip_suffix(".desktop").unwrap_or(&name).to_string();
    (!id.is_empty()).then_some(id)
}

/// Terminals in the order they are preferred when nothing names one.
const TERMINALS: [&str; 10] = ["ghostty", "kitty", "alacritty", "foot", "wezterm", "konsole", "ptyxis", "gnome-terminal", "gnome-console", "xterm"];

/// The user's terminal: the one $TERMINAL names, else a well-known installed one, else any.
fn pick_terminal(apps: &[DesktopApp], env: Option<&str>) -> Option<String> {
    let terminals: Vec<&DesktopApp> = apps.iter().filter(|a| a.categories.iter().any(|c| c == "TerminalEmulator")).collect();
    let named = |name: &str| {
        let name = name.to_lowercase();
        terminals.iter().find(|a| {
            let id = a.id.to_lowercase();
            id == name || id.rsplit('.').next() == Some(name.as_str()) || a.clean_exec_args().first().is_some_and(|e| e.rsplit('/').next() == Some(name.as_str()))
        })
    };
    let from_env = env.map(|e| e.split_whitespace().next().unwrap_or(e).rsplit('/').next().unwrap_or(e).to_string()).and_then(|e| named(&e));
    from_env.or_else(|| TERMINALS.iter().find_map(|t| named(t))).or(terminals.first()).map(|a| a.id.clone())
}

/// The first app of a category, for when the system names no default.
fn first_in_category(apps: &[DesktopApp], category: &str) -> Option<String> {
    apps.iter().find(|a| !a.no_display && a.categories.iter().any(|c| c == category)).map(|a| a.id.clone())
}

/// What a new dock starts with: terminal, files, browser, skipping what is not installed.
fn default_pins(apps: &[DesktopApp]) -> Vec<String> {
    let known = |id: Option<String>| id.filter(|id| apps.iter().any(|a| &a.id == id));
    let files = known(query_default("xdg-mime", &["query", "default", "inode/directory"])).or_else(|| first_in_category(apps, "FileManager"));
    let browser = known(query_default("xdg-settings", &["get", "default-web-browser"])).or_else(|| first_in_category(apps, "WebBrowser"));
    let mut pins = Vec::new();
    for id in [pick_terminal(apps, std::env::var("TERMINAL").ok().as_deref()), files, browser].into_iter().flatten() {
        if !pins.contains(&id) {
            pins.push(id);
        }
    }
    pins
}

// ── Installed apps (worker thread) ────────────────────────────────────────

#[derive(Default)]
struct Catalog {
    generation: u64,
    apps: Vec<DesktopApp>,
    icons: HashMap<String, PathBuf>,
    pins: Option<Vec<String>>,
}

static CATALOG: LazyLock<Mutex<Catalog>> = LazyLock::new(Default::default);
static START: Once = Once::new();

/// Scan apps and icons, again whenever apps are installed or removed. A dock turned on for the
/// first time gets its first pins here.
pub fn start(waker: Waker) {
    crate::apps_watch::start(waker.clone());
    START.call_once(|| {
        std::thread::spawn(move || loop {
            let seen = crate::apps_watch::generation();
            let apps = prism_common::apps::load_all_desktop_apps();
            let icons: HashMap<String, PathBuf> = apps
                .iter()
                .filter_map(|a| Some((a.id.clone(), crate::icons::lookup(a.icon.as_deref()?, 96)?)))
                .collect();
            let pins = read_pins().unwrap_or_else(|| {
                let pins = default_pins(&apps);
                write_pins(&pins);
                pins
            });
            let mut c = CATALOG.lock().unwrap();
            c.apps = apps;
            c.icons = icons;
            c.pins = Some(pins);
            c.generation += 1;
            drop(c);
            waker.wake();
            crate::apps_watch::wait_for_change(seen);
        });
    });
}

pub fn catalog_generation() -> u64 {
    CATALOG.lock().unwrap().generation
}

// ── Items ─────────────────────────────────────────────────────────────────

/// The installed app whose windows report `app_id`.
fn app_for<'a>(app_id: &str, apps: &'a [DesktopApp]) -> Option<&'a DesktopApp> {
    let want = app_id.to_lowercase();
    if want.is_empty() {
        return None;
    }
    let by = |test: &dyn Fn(&DesktopApp) -> bool| apps.iter().find(|a| test(a));
    by(&|a| a.id.to_lowercase() == want)
        .or_else(|| by(&|a| a.wm_class.as_ref().is_some_and(|c| c.to_lowercase() == want)))
        .or_else(|| by(&|a| a.id.to_lowercase().rsplit('.').next() == Some(want.as_str())))
        .or_else(|| by(&|a| a.clean_exec_args().first().is_some_and(|e| e.rsplit('/').next().is_some_and(|b| b.to_lowercase() == want))))
        .or_else(|| by(&|a| a.name.to_lowercase() == want))
}

pub struct DockItem {
    /// Desktop entry id, or the window's app id for an app without one.
    pub key: String,
    pub name: String,
    pub app: Option<DesktopApp>,
    pub icon: Option<PathBuf>,
    pub pinned: bool,
    pub windows: Vec<ZwlrForeignToplevelHandleV1>,
    pub focused: bool,
}

/// Pinned apps in their order, then running apps in the order they opened.
fn build_items(pins: &[String], toplevels: &[Toplevel], apps: &[DesktopApp], icons: &HashMap<String, PathBuf>) -> Vec<DockItem> {
    let entry = |key: &str, pinned: bool| {
        let app = apps.iter().find(|a| a.id == key).cloned();
        DockItem {
            key: key.to_string(),
            name: app.as_ref().map_or_else(|| key.to_string(), |a| a.name.clone()),
            icon: icons.get(key).cloned(),
            app,
            pinned,
            windows: Vec::new(),
            focused: false,
        }
    };
    // A pin of an app that has been uninstalled is kept in the file but not shown.
    let mut items: Vec<DockItem> = pins.iter().filter(|p| apps.iter().any(|a| &a.id == *p)).map(|p| entry(p, true)).collect();
    for t in toplevels {
        if t.app_id.is_empty() {
            continue;
        }
        let key = app_for(&t.app_id, apps).map_or_else(|| t.app_id.clone(), |a| a.id.clone());
        let i = match items.iter().position(|it| it.key == key) {
            Some(i) => i,
            None => {
                items.push(entry(&key, false));
                items.len() - 1
            }
        };
        items[i].windows.push(t.handle.clone());
        items[i].focused |= t.focused;
    }
    items
}

// ── Hiding ────────────────────────────────────────────────────────────────

/// Whether the dock should be on screen right now.
fn wanted(autohide: &str, hovered: bool, windows_on_screen: bool, menu_open: bool) -> bool {
    match autohide {
        "always" => hovered || menu_open,
        "smart" => hovered || menu_open || !windows_on_screen,
        _ => true,
    }
}

pub struct DockState {
    pub items: Vec<DockItem>,
    pub pins: Vec<String>,
    catalog_seen: u64,
    /// 0 on screen, 1 hidden.
    pub offset: SpringValue,
    /// The pointer is on the dock or on the strip along the screen edge.
    pub hovered: bool,
    hide_at: Option<Instant>,
    pub fades: Fades<String>,
}

impl Default for DockState {
    fn default() -> Self {
        DockState {
            items: Vec::new(),
            pins: Vec::new(),
            catalog_seen: 0,
            offset: SpringValue::new(0.0),
            hovered: false,
            hide_at: None,
            fades: Fades::default(),
        }
    }
}

impl DockState {
    /// Rebuild the items from the windows and installed apps.
    pub fn refresh(&mut self, toplevels: &[Toplevel]) {
        let c = CATALOG.lock().unwrap();
        if c.generation != self.catalog_seen {
            self.catalog_seen = c.generation;
            if let Some(pins) = &c.pins {
                self.pins = pins.clone();
            }
        }
        self.items = build_items(&self.pins, toplevels, &c.apps, &c.icons);
    }

    pub fn pin(&mut self, key: &str) {
        if !self.pins.iter().any(|p| p == key) {
            self.pins.push(key.to_string());
            write_pins(&self.pins);
        }
    }

    pub fn unpin(&mut self, key: &str) {
        self.pins.retain(|p| p != key);
        write_pins(&self.pins);
    }

    /// Head for shown or hidden; returns how long until the next decision is due.
    pub fn update_visibility(&mut self, cfg: &DockConfig, windows_on_screen: bool, menu_open: bool, style: &Style) -> Option<Duration> {
        let show = wanted(&cfg.autohide, self.hovered, windows_on_screen, menu_open);
        if show {
            self.hide_at = None;
            if self.offset.target() != 0.0 {
                self.offset.set(0.0, SPRING_SLIDE, style);
            }
            return None;
        }
        if self.offset.target() == 1.0 {
            return None;
        }
        let now = Instant::now();
        let at = *self.hide_at.get_or_insert(now + HIDE_DELAY);
        if now >= at {
            self.hide_at = None;
            self.offset.set(1.0, SPRING_SLIDE, style);
            return None;
        }
        Some(at - now)
    }
}

// ── Geometry ──────────────────────────────────────────────────────────────

pub fn is_vertical(cfg: &DockConfig) -> bool {
    cfg.position != "bottom"
}

fn pitch(cfg: &DockConfig) -> f64 {
    cfg.icon_size as f64 + SPACING
}

pub fn thickness(cfg: &DockConfig) -> f64 {
    cfg.icon_size as f64 + 2.0 * PAD + INDICATOR
}

/// Length of the dock along its edge; a dock that would not fit is squeezed.
pub fn length(cfg: &DockConfig, items: usize, available: f64) -> f64 {
    (items as f64 * pitch(cfg) + 2.0 * PAD).min(available.max(1.0)).round()
}

// ── Drawing ───────────────────────────────────────────────────────────────

/// A letter in a circle for apps without an icon.
fn draw_monogram(cr: *mut Cairo, cx: f64, cy: f64, size: f64, name: &str, look: &Look) {
    fill_circle(cr, cx, cy, size / 2.0, (look.primary.0, look.primary.1, look.primary.2, 0.3));
    let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    let st = TextStyle { family: &look.family, px: size * 0.45, bold: true, color: look.fg };
    draw_in_box(cr, &letter, &st, cx - size / 2.0, cy - size / 2.0, size, size, Align::Center);
}

/// Draw the dock; returns the rectangle of every item.
pub fn draw(cr: *mut Cairo, w: f64, h: f64, cfg: &DockConfig, state: &mut DockState, hover: Option<PanelHit>, radius: f64, look: &Look, style: &Style) -> Vec<(PanelHit, Rect)> {
    let vertical = is_vertical(cfg);
    let r = radius.min(w.min(h) / 2.0);
    fill_rounded_rect(cr, 0.0, 0.0, w, h, r, (look.card.0, look.card.1, look.card.2, cfg.opacity as f64));
    stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, r, (look.fg.0, look.fg.1, look.fg.2, 0.12), 1.0);

    let count = state.items.len().max(1) as f64;
    let along = if vertical { h } else { w };
    let step = ((along - 2.0 * PAD) / count).min(pitch(cfg));
    let size = (step - SPACING).clamp(16.0, cfg.icon_size as f64);
    let start = (along - step * count) / 2.0;
    // Icons sit away from the screen edge, the indicator between them and the edge.
    let across = if vertical { w } else { h };
    let middle = match cfg.position.as_str() {
        "left" => (across + INDICATOR) / 2.0,
        _ => (across - INDICATOR) / 2.0,
    };

    let mut hits = Vec::new();
    for i in 0..state.items.len() {
        let pos = start + step * (i as f64 + 0.5);
        let (cx, cy) = if vertical { (middle, pos) } else { (pos, middle) };
        let on = hover == Some(PanelHit::DockItem(i));
        let key = state.items[i].key.clone();
        let level = state.fades.level(key, on, style);
        let item = &state.items[i];
        if level > 0.0 {
            let b = size + 8.0;
            fill_rounded_rect(cr, cx - b / 2.0, cy - b / 2.0, b, b, 10.0, (look.fg.0, look.fg.1, look.fg.2, 0.12 * level));
        }
        let shown = size * (1.0 + 0.08 * level);
        // Decoded a little larger than drawn, so the hover growth stays sharp.
        let px = (size * 1.25).round() as i32;
        let image = item.icon.as_ref().and_then(|p| cached_image(&p.to_string_lossy(), px, px, true));
        match image {
            Some(img) => unsafe {
                let (iw, ih) = image_size(img);
                let k = shown / iw.max(ih).max(1.0);
                cairo_save(cr);
                cairo_translate(cr, cx, cy);
                cairo_scale(cr, k, k);
                cairo_set_source_surface(cr, img, -iw / 2.0, -ih / 2.0);
                cairo_paint(cr);
                cairo_restore(cr);
            },
            None => draw_monogram(cr, cx, cy, shown, &item.name, look),
        }
        if !item.windows.is_empty() {
            // A dot for a running app, a longer bar for the focused one.
            let long = if item.focused { 16.0 } else { 5.0 };
            let inset = 3.0;
            let (x, y, bw, bh) = match cfg.position.as_str() {
                "left" => (inset, cy - long / 2.0, 3.0, long),
                "right" => (w - inset - 3.0, cy - long / 2.0, 3.0, long),
                _ => (cx - long / 2.0, h - inset - 3.0, long, 3.0),
            };
            let alpha = if item.focused { 1.0 } else { 0.7 };
            fill_rounded_rect(cr, x, y, bw, bh, 1.5, (look.fg.0, look.fg.1, look.fg.2, alpha));
        }
        let cell = if vertical { Rect { x: 0.0, y: pos - step / 2.0, w, h: step } } else { Rect { x: pos - step / 2.0, y: 0.0, w: step, h } };
        hits.push((PanelHit::DockItem(i), cell));
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, exec: &str, categories: &[&str]) -> DesktopApp {
        DesktopApp {
            id: id.into(),
            name: name.into(),
            generic_name: None,
            comment: None,
            exec: exec.into(),
            icon: None,
            categories: categories.iter().map(|c| c.to_string()).collect(),
            keywords: Vec::new(),
            terminal: false,
            no_display: false,
            wm_class: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn windows_find_their_app() {
        let mut steam = app("com.valvesoftware.Steam", "Steam", "steam %U", &[]);
        steam.wm_class = Some("steamwebhelper".into());
        let apps = vec![app("com.mitchellh.ghostty", "Ghostty", "ghostty", &[]), app("firefox", "Firefox", "/usr/lib/firefox/firefox %u", &[]), app("org.gnome.Nautilus", "Files", "nautilus", &[]), steam];
        let id = |w: &str| app_for(w, &apps).map(|a| a.id.clone());
        assert_eq!(id("com.mitchellh.ghostty").as_deref(), Some("com.mitchellh.ghostty"));
        assert_eq!(id("Firefox").as_deref(), Some("firefox"));
        assert_eq!(id("nautilus").as_deref(), Some("org.gnome.Nautilus"));
        assert_eq!(id("steamwebhelper").as_deref(), Some("com.valvesoftware.Steam"));
        assert_eq!(id("unknown"), None);
        assert_eq!(id(""), None);
    }

    #[test]
    fn the_terminal_named_by_the_environment_wins() {
        let apps = vec![app("org.kde.konsole", "Konsole", "konsole", &["TerminalEmulator"]), app("kitty", "kitty", "kitty", &["TerminalEmulator"]), app("firefox", "Firefox", "firefox", &[])];
        assert_eq!(pick_terminal(&apps, Some("/usr/bin/konsole -e")).as_deref(), Some("org.kde.konsole"));
        // Unknown to the system: fall back to the preferred order.
        assert_eq!(pick_terminal(&apps, Some("nothing")).as_deref(), Some("kitty"));
        assert_eq!(pick_terminal(&apps, None).as_deref(), Some("kitty"));
        assert_eq!(pick_terminal(&apps[2..], None), None);
    }

    #[test]
    fn hiding_follows_the_chosen_mode() {
        assert!(wanted("off", false, true, false));
        assert!(!wanted("always", false, false, false));
        assert!(wanted("always", true, true, false));
        assert!(wanted("smart", false, false, false));
        assert!(!wanted("smart", false, true, false));
        assert!(wanted("smart", true, true, false));
        // An open menu keeps the dock it belongs to.
        assert!(wanted("always", false, true, true));
    }

    #[test]
    fn a_long_dock_is_squeezed_onto_the_screen() {
        let cfg = DockConfig::default();
        assert_eq!(length(&cfg, 3, 2000.0), 3.0 * 60.0 + 16.0);
        assert_eq!(length(&cfg, 100, 800.0), 800.0);
    }
}
