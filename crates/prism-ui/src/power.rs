//! Power menu. One view with five designs (`power_theme`, Settings > Power), drawn large: nothing
//! behind it matters while it is open.
//!
//! * Glide Bar: a slim bar of icons; a springy pill slides along and opens to show the name.
//! * Split Panel: the wallpaper with the time, next to a list of actions.
//! * Shutters: the wallpaper cut into strips, one per action; the selected strip opens wide.
//! * Orbit: actions around a ring with the clock in the middle, on a splash of colour.
//! * Floating Cards: a looping hand of cards with nothing behind them.
//!
//! Arrow keys move, Enter, Space or a click runs, and the shortcut keys pick an action directly. With
//! `power_hold_to_confirm`, actions that end the session run only after being held for a second.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use prism_common::power::PowerSupport;
use prism_common::{Config, Theme};

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{DOWN, END, ENTER, ESC, HOME, LEFT, PGDN, PGUP, RIGHT, UP};
use crate::launcher_themes::Sprite;
use crate::svg;
use crate::ui::{self, Fades, Glide, Rgba, SpringValue, Style};
use crate::view::{CardView, Effect};

/// Label and config id of every design, for Settings.
pub const THEMES: &[(&str, &str)] = &[
    ("Glide Bar", "glide"),
    ("Split Panel", "split"),
    ("Shutters", "shutters"),
    ("Orbit", "orbit"),
    ("Floating Cards", "cards"),
];

/// How long an action has to be held.
const HOLD: Duration = Duration::from_millis(1000);
const INK: Rgba = (0.97, 0.97, 0.97, 1.0);
const SPRING_OPEN: (f64, f64) = (0.42, 0.82);
const SPRING_FAN: (f64, f64) = (0.44, 0.70);
const SPRING_FALL: (f64, f64) = (0.24, 1.0);

/// How much of the screen the menu may fill, and how far it is enlarged.
const SCREEN_W: f64 = 0.88;
const SCREEN_H: f64 = 0.85;
const GLIDE_SLOT: f64 = 64.0;
const GLIDE_H: f64 = 116.0;
const SPLIT_W: f64 = 860.0;
const SPLIT_PANEL: f64 = 340.0;
const SPLIT_PITCH: f64 = 60.0;
const SHUTTER_NARROW: f64 = 104.0;
const SHUTTER_OPEN: f64 = 520.0;
const SHUTTER_H: f64 = 520.0;
const ORBIT_SIZE: f64 = 680.0;
const SPLASH_RADIUS: f64 = 292.0;
const SPLASH_LAYERS: usize = 36;
const ORBIT_RADIUS: f64 = 218.0;
const CARDS_SIZE: (f64, f64) = (1040.0, 520.0);
const CARD_FACE: (f64, f64) = (200.0, 280.0);
const CARD_MARGIN: f64 = 30.0;

/// Something the power menu can do.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Act {
    Lock,
    Logout,
    Suspend,
    Sleep,
    Hibernate,
    Reboot,
    Shutdown,
}

impl Act {
    const ALL: [Act; 7] = [Act::Lock, Act::Logout, Act::Suspend, Act::Sleep, Act::Hibernate, Act::Reboot, Act::Shutdown];

    fn label(self) -> &'static str {
        match self {
            Act::Lock => "Lock",
            Act::Logout => "Logout",
            Act::Suspend => "Suspend",
            Act::Sleep => "Sleep",
            Act::Hibernate => "Hibernate",
            Act::Reboot => "Reboot",
            Act::Shutdown => "Shutdown",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Act::Lock => "Keep everything open behind the lock screen.",
            Act::Logout => "Close your apps and end this session.",
            Act::Suspend => "Pause in memory and wake up quickly.",
            Act::Sleep => "Pause in memory with a safe copy on disk.",
            Act::Hibernate => "Save everything to disk and power off.",
            Act::Reboot => "Close your apps and restart the computer.",
            Act::Shutdown => "Close your apps and power off.",
        }
    }

    /// The shortcut key.
    fn key(self) -> char {
        match self {
            Act::Lock => 'l',
            Act::Logout => 'e',
            Act::Suspend => 'u',
            Act::Sleep => 'z',
            Act::Hibernate => 'h',
            Act::Reboot => 'r',
            Act::Shutdown => 's',
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Act::Lock => "M7 10h10a3 3 0 0 1 3 3v6a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3v-6a3 3 0 0 1 3-3zM7 10V6a5 5 0 0 1 10 0v4",
            Act::Logout => "M10 22H5a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h5M16 17l5-5-5-5M21 12H9",
            Act::Suspend => "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z",
            Act::Sleep => "M2 4v16M2 8h18a2 2 0 0 1 2 2v10M2 17h20M6 8v9",
            Act::Hibernate => "M2 12h20M12 2v20M20 16l-4-4 4-4M4 8l4 4-4 4M16 4l-4 4-4-4M8 20l4-4 4 4",
            Act::Reboot => "M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8M21 3v5h-5",
            Act::Shutdown => "M18.36 6.64a9 9 0 1 1-12.73 0M12 2v10",
        }
    }

    /// Ends the session, so unsaved work is at stake.
    fn needs_hold(self) -> bool {
        matches!(self, Act::Logout | Act::Reboot | Act::Shutdown)
    }

    fn turned_on(self, cfg: &Config) -> bool {
        match self {
            Act::Lock => cfg.power_lock,
            Act::Logout => cfg.power_logout,
            Act::Suspend => cfg.power_suspend,
            Act::Sleep => cfg.power_sleep,
            Act::Hibernate => cfg.power_hibernate,
            Act::Reboot => cfg.power_reboot,
            Act::Shutdown => cfg.power_shutdown,
        }
    }

    fn available(self, support: PowerSupport) -> bool {
        match self {
            Act::Suspend => support.suspend,
            Act::Sleep => support.sleep,
            Act::Hibernate => support.hibernate,
            _ => true,
        }
    }
}

/// The actions to offer: turned on in Settings and possible on this computer.
fn pick(cfg: &Config, support: PowerSupport) -> Vec<Act> {
    Act::ALL.into_iter().filter(|a| a.turned_on(cfg) && a.available(support)).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Design {
    Glide,
    Split,
    Shutters,
    Orbit,
    Cards,
}

impl Design {
    fn from_id(id: &str) -> Design {
        match id {
            "split" => Design::Split,
            "shutters" => Design::Shutters,
            "orbit" => Design::Orbit,
            "cards" => Design::Cards,
            _ => Design::Glide,
        }
    }

    /// Size the design is laid out at for `n` actions, before it is enlarged to fit the screen.
    fn base_size(self, n: usize) -> (f64, f64) {
        let n = n.max(1) as f64;
        match self {
            Design::Glide => (n * GLIDE_SLOT + 220.0, GLIDE_H),
            Design::Split => (SPLIT_W, (n * SPLIT_PITCH + 84.0).max(380.0)),
            Design::Shutters => ((n - 1.0) * SHUTTER_NARROW + SHUTTER_OPEN, SHUTTER_H),
            Design::Orbit => (ORBIT_SIZE, ORBIT_SIZE),
            Design::Cards => CARDS_SIZE,
        }
    }

    /// How far the design is enlarged at most; the busiest ones stop earlier to stay smooth.
    fn max_zoom(self) -> f64 {
        match self {
            Design::Shutters => 2.0,
            Design::Cards => 1.9,
            _ => 2.4,
        }
    }

    /// Drawn without a card behind it.
    fn floats(self) -> bool {
        matches!(self, Design::Orbit | Design::Cards)
    }
}

/// The enlargement that fits a design of `base` size on a screen, up to `max`.
fn fit(base: (f64, f64), sw: f64, sh: f64, max: f64) -> f64 {
    (sw * SCREEN_W / base.0).min(sh * SCREEN_H / base.1).clamp(1.0, max)
}

/// The design a saved `power_theme` stands for, as its config id.
pub fn theme_id(id: &str) -> String {
    THEMES.iter().find(|(_, t)| *t == id).map_or("glide", |(_, t)| *t).to_string()
}

/// Card size of the configured design on a screen of `sw` by `sh`.
pub fn card_size(cfg: &Config, sw: f64, sh: f64) -> (f64, f64) {
    let design = Design::from_id(&cfg.power_theme);
    let base = design.base_size(pick(cfg, prism_common::power::support()).len());
    let zoom = fit(base, sw, sh, design.max_zoom());
    ((base.0 * zoom).round(), (base.1 * zoom).round())
}

/// The design has no card behind it: its surface must stay clear of blur and the card shadow.
pub fn floating(id: &str) -> bool {
    Design::from_id(id).floats()
}

/// Shortest way round a loop of `n` places from `from` to `to`, signed.
fn shortest_turn(from: i64, to: i64, n: i64) -> i64 {
    let turn = (to - from).rem_euclid(n);
    if turn * 2 > n { turn - n } else { turn }
}

/// Where place `i` is relative to `fan` on a loop of `n`, nearest way round.
fn loop_distance(i: usize, fan: f64, n: usize) -> f64 {
    let n = n as f64;
    (i as f64 - fan + n / 2.0).rem_euclid(n) - n / 2.0
}

/// How a card `d` places from the front is held in the hand.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Pose {
    x: f64,
    y: f64,
    angle: f64,
    scale: f64,
}

fn card_pose(d: f64) -> Pose {
    let near = d.abs().min(1.0);
    let far = (d.abs() - 1.0).max(0.0);
    let side = if d < 0.0 { -1.0 } else { 1.0 };
    Pose {
        x: side * (near * 150.0 + far * 92.0),
        y: near * 34.0 + far * 18.0 + far * far * 5.0,
        angle: side * (near * 6.0 + far * 5.0).to_radians(),
        scale: 1.0 - near * 0.14 - far * 0.03,
    }
}

/// "Up 4h 12m" from the first number of /proc/uptime.
fn uptime_label(text: &str) -> String {
    let secs = text.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) as u64;
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("Up {m}m"),
        (0, _) => format!("Up {h}h {m}m"),
        _ => format!("Up {d}d {h}h"),
    }
}

/// The time ("2:30") and date ("Wednesday, November 18") now.
fn clock() -> (String, String) {
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let hour = if tm.tm_hour % 12 == 0 { 12 } else { tm.tm_hour % 12 };
    let date = format!("{}, {} {}", DAYS[tm.tm_wday.rem_euclid(7) as usize], MONTHS[tm.tm_mon.rem_euclid(12) as usize], tm.tm_mday);
    (format!("{hour}:{:02}", tm.tm_min), date)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeldBy {
    Pointer,
    /// The raw code of the key: layouts can put Enter on any physical key.
    Keyboard(u32),
}

/// An action being held down until it runs.
#[derive(Clone, Copy, Debug)]
struct Hold {
    index: usize,
    by: HeldBy,
    since: Instant,
}

pub struct PowerView {
    design: Design,
    actions: Vec<Act>,
    hold_to_confirm: bool,
    selected: usize,
    pointer: (f64, f64),
    hover: Option<usize>,
    /// Clickable places from the last draw, frontmost first.
    hits: Vec<(usize, Rect)>,
    hold: Option<Hold>,
    /// Raw code of the key pressed last.
    last_key: u32,
    /// The ring or fill left by a released hold, falling back to nothing.
    fall: SpringValue,
    done: bool,
    levels: Fades<usize>,
    pill: Option<[SpringValue; 2]>,
    glide: Glide,
    turn: SpringValue,
    faces: HashMap<Act, Sprite>,
    faces_scale: f64,
    /// Split Panel's and Shutters' wallpaper, drawn once at this scale.
    backdrop: Vec<Sprite>,
    backdrop_scale: f64,
    /// Orbit's backdrop and the card size it was drawn for.
    splash: Option<(f64, f64, Sprite)>,
    /// The enlarged layout: top left corner on screen, enlargement, and the card's corner radius.
    origin: (f64, f64),
    zoom: f64,
    radius: f64,
    uptime: String,
}

impl PowerView {
    pub fn new(cfg: &Config) -> Self {
        let uptime = std::fs::read_to_string("/proc/uptime").map(|t| uptime_label(&t)).unwrap_or_default();
        Self::with_actions(Design::from_id(&cfg.power_theme), pick(cfg, prism_common::power::support()), cfg.power_hold_to_confirm, uptime)
    }

    fn with_actions(design: Design, actions: Vec<Act>, hold_to_confirm: bool, uptime: String) -> Self {
        Self {
            design,
            actions,
            hold_to_confirm,
            selected: 0,
            pointer: (-1.0, -1.0),
            hover: None,
            hits: Vec::new(),
            hold: None,
            last_key: 0,
            fall: SpringValue::new(0.0),
            done: false,
            levels: Fades::default(),
            pill: None,
            glide: Glide::default(),
            turn: SpringValue::new(0.0),
            faces: HashMap::new(),
            faces_scale: 1.0,
            backdrop: Vec::new(),
            backdrop_scale: 0.0,
            splash: None,
            origin: (0.0, 0.0),
            zoom: 1.0,
            radius: 12.0,
            uptime,
        }
    }

    fn run(&mut self, i: usize) -> Effect {
        #[cfg(test)]
        panic!("tests must never run a power action");
        #[cfg(not(test))]
        {
            let Some(&act) = self.actions.get(i) else { return Effect::None };
            std::thread::spawn(move || {
                use prism_common::PowerAction;
                let result = match act {
                    Act::Lock => prism_common::ipc::request(&prism_common::IpcCommand::Lock(prism_common::ipc::LockAction::Lock)).map(|_| String::new()),
                    Act::Logout => prism_common::power::execute_power(PowerAction::Logout),
                    Act::Suspend => prism_common::power::execute_power(PowerAction::Suspend),
                    Act::Sleep => prism_common::power::execute_power(PowerAction::Sleep),
                    Act::Hibernate => prism_common::power::execute_power(PowerAction::Hibernate),
                    Act::Reboot => prism_common::power::execute_power(PowerAction::Reboot),
                    Act::Shutdown => prism_common::power::execute_power(PowerAction::Shutdown),
                };
                if let Err(e) = result {
                    eprintln!("[prism-ui] power action failed: {e}");
                }
            });
            self.done = true;
            Effect::Close
        }
    }

    /// Runs action `i`, or starts holding it when it has to be confirmed.
    fn activate(&mut self, i: usize, by: HeldBy) -> Effect {
        let Some(act) = self.actions.get(i) else { return Effect::None };
        if self.hold_to_confirm && act.needs_hold() {
            self.hold = Some(Hold { index: i, by, since: Instant::now() });
            return Effect::Redraw;
        }
        self.run(i)
    }

    fn progress(&self) -> f64 {
        self.hold.map_or(0.0, |h| (h.since.elapsed().as_secs_f64() / HOLD.as_secs_f64()).clamp(0.0, 1.0))
    }

    /// Lets go of a hold; its progress falls back.
    fn release_hold(&mut self) {
        if self.hold.is_some() {
            self.fall.snap(self.progress());
            self.hold = None;
            self.fall.set_scaled(0.0, SPRING_FALL, Some(1.0));
        }
    }

    fn select(&mut self, i: usize) {
        if i != self.selected {
            self.release_hold();
            self.selected = i;
        }
    }

    fn step(&mut self, delta: i64) {
        let n = self.actions.len() as i64;
        if n > 0 {
            self.select((self.selected as i64 + delta).rem_euclid(n) as usize);
        }
    }

    /// A point on screen in the coordinates the design is laid out in.
    fn local(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.origin.0) / self.zoom, (y - self.origin.1) / self.zoom)
    }

    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        let (x, y) = self.local(x, y);
        self.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }

    /// How far the hold has come, 0..1, for drawing.
    fn hold_level(&self) -> f64 {
        if self.hold.is_some() {
            ui::keep_animating();
            self.progress()
        } else {
            self.fall.animated(0.01).clamp(0.0, 1.0)
        }
    }

    fn hint(&self) -> Option<&'static str> {
        let act = self.actions.get(self.selected)?;
        (self.hold_to_confirm && act.needs_hold()).then_some(if self.hold.is_some() { "Keep holding\u{2026}" } else { "Hold to confirm" })
    }

    /// Heads for the selected place on the loop; the current position.
    fn fan(&mut self, spring: (f64, f64), style: &Style) -> f64 {
        let n = self.actions.len().max(1) as i64;
        let place = self.turn.target().round() as i64;
        let turn = shortest_turn(place, self.selected as i64, n);
        if turn != 0 {
            self.turn.set((place + turn) as f64, spring, style);
        }
        self.turn.animated(0.002)
    }

    // ── Glide Bar ────────────────────────────────────────────────────────

    fn draw_glide(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let (fg, inv) = (style.fg, cb::hex_to_rgba(&theme.inverse_primary, 1.0));
        let font = "Inter, Sans Semi-Bold 16px";
        let label_w: Vec<f64> = self.actions.iter().map(|a| cb::measure_pango_text(cr, a.label(), font, 1000, Some(1)).0 as f64 + 22.0).collect();
        let (y, h) = (card.y + 14.0, GLIDE_SLOT);

        // Where everything settles, for the pill and the pointer.
        let total = self.actions.len() as f64 * GLIDE_SLOT + label_w[self.selected];
        let mut x = card.x + (card.w - total) / 2.0;
        self.hits.clear();
        for (i, label) in label_w.iter().enumerate() {
            let w = GLIDE_SLOT + if i == self.selected { *label } else { 0.0 };
            self.hits.push((i, Rect { x, y, w, h }));
            x += w;
        }
        let target = self.hits[self.selected].1;
        let to = [target.x - card.x, target.w];
        let pill = self.pill.get_or_insert_with(|| to.map(SpringValue::new));
        for (v, t) in pill.iter_mut().zip(to) {
            if (v.target() - t).abs() > 1e-6 {
                v.set(t, ui::SPRING_KNOB, style);
            }
        }
        let (px, pw) = (card.x + pill[0].animated(0.2), pill[1].animated(0.2));
        let pill_rect = Rect { x: px, y, w: pw, h };
        cb::fill_rounded_rect(cr, px, y, pw, h, h / 2.0, faded(fg, 0.88));
        fill_across(cr, pill_rect, h / 2.0, self.hold_level(), faded(inv, 0.38));

        // Slots open and close as the pill arrives.
        let levels: Vec<f64> = (0..self.actions.len()).map(|i| self.levels.settled_level(i, i == self.selected, ui::SPRING_KNOB, style)).collect();
        let total: f64 = levels.iter().zip(&label_w).map(|(l, w)| GLIDE_SLOT + l * w).sum();
        let mut x = card.x + (card.w - total) / 2.0;
        for (i, act) in self.actions.iter().enumerate() {
            let level = levels[i];
            let color = ui::mix(fg, inv, level);
            svg::draw_path(cr, act.icon(), x + (GLIDE_SLOT - 28.0) / 2.0, y + (h - 28.0) / 2.0, 28.0, color, Some(1.8));
            if level > 0.02 {
                let (_, th) = cb::measure_pango_text(cr, "Ag", font, 1000, Some(1));
                unsafe {
                    cb::cairo_save(cr);
                    cb::cairo_rectangle(cr, x + GLIDE_SLOT - 6.0, y, level * label_w[i], h);
                    cb::cairo_clip(cr);
                }
                cb::draw_pango_text(cr, x + GLIDE_SLOT - 6.0, y + (h - th as f64) / 2.0, act.label(), font, faded(inv, level), None);
                unsafe { cb::cairo_restore(cr) };
            }
            x += GLIDE_SLOT + level * label_w[i];
        }
        let hint = self.hint().unwrap_or("Arrow keys to move, Enter or Space to choose");
        centred(cr, Rect { y: y + h + 4.0, h: card.y + card.h - y - h - 8.0, ..card }, hint, "Inter, Sans Medium 12px", faded(fg, 0.72));
    }

    // ── Split Panel ──────────────────────────────────────────────────────

    fn draw_split(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let (fg, inv) = (style.fg, cb::hex_to_rgba(&theme.inverse_primary, 1.0));
        let k = style.scale * self.zoom;
        if self.backdrop_scale != k {
            let (zoom, radius) = (self.zoom, self.radius);
            self.backdrop = vec![Sprite::new(SPLIT_PANEL, card.h, k, |cr| {
                unsafe {
                    cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, radius);
                    cb::cairo_clip(cr);
                }
                wallpaper(cr, Rect { w: SPLIT_PANEL, ..card }, zoom);
                cb::fill_rounded_rect_gradient(cr, card.x, card.y, SPLIT_PANEL, card.h, 0.0, (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.72), 0.4, 1.0);
            })];
            self.backdrop_scale = k;
        }
        self.backdrop[0].paint_corner(cr, k, 1.0);
        let (time, date) = clock();
        let bottom = card.y + card.h - 28.0;
        cb::draw_pango_text(cr, card.x + 28.0, bottom - 118.0, &time, "Inter, Sans Bold 58px", INK, None);
        cb::draw_pango_text(cr, card.x + 28.0, bottom - 42.0, &date, "Inter, Sans Medium 14px", faded(INK, 0.9), Some((SPLIT_PANEL - 56.0) as i32));
        cb::draw_pango_text(cr, card.x + 28.0, bottom - 20.0, &self.uptime, "Inter, Sans 13px", faded(INK, 0.72), None);

        let list = Rect { x: card.x + SPLIT_PANEL + 20.0, y: card.y + 24.0, w: card.w - SPLIT_PANEL - 40.0, h: 0.0 };
        let (hy, hh) = self.glide.to(self.selected as f64 * SPLIT_PITCH, SPLIT_PITCH - 4.0, style);
        let row = Rect { x: list.x, y: list.y + hy, w: list.w, h: hh };
        cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 14.0, faded(fg, 0.88));
        fill_across(cr, row, 14.0, self.hold_level(), faded(inv, 0.38));
        self.hits.clear();
        for (i, act) in self.actions.iter().enumerate() {
            let r = Rect { x: list.x, y: list.y + i as f64 * SPLIT_PITCH, w: list.w, h: SPLIT_PITCH - 4.0 };
            self.hits.push((i, r));
            let on = ui::coverage(row.y, row.h, r.y, r.h);
            let color = ui::mix(fg, inv, on);
            svg::draw_path(cr, act.icon(), r.x + 16.0, r.y + 15.0, 26.0, color, Some(1.8));
            cb::draw_pango_text(cr, r.x + 58.0, r.y + 8.0, act.label(), "Inter, Sans Semi-Bold 16px", color, None);
            cb::draw_pango_text(cr, r.x + 58.0, r.y + 31.0, act.about(), "Inter, Sans 12px", faded(color, 0.76), Some((r.w - 104.0) as i32));
            let key = act.key().to_ascii_uppercase().to_string();
            centred(cr, Rect { x: r.x + r.w - 42.0, y: r.y + 15.0, w: 26.0, h: 26.0 }, &key, "Inter, Sans Bold 12px", faded(color, 0.72));
            cb::stroke_rounded_rect(cr, r.x + r.w - 42.0, r.y + 15.0, 26.0, 26.0, 7.0, faded(color, 0.36), 1.0);
        }
        if let Some(hint) = self.hint() {
            centred(cr, Rect { x: list.x, y: card.y + card.h - 40.0, w: list.w, h: 32.0 }, hint, "Inter, Sans Medium 13px", faded(fg, 0.72));
        }
    }

    // ── Shutters ─────────────────────────────────────────────────────────

    fn draw_shutters(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let n = self.actions.len();
        let wide = card.w - (n - 1) as f64 * SHUTTER_NARROW;
        let accent = cb::hex_to_rgba(&theme.primary, 1.0);
        self.hits.clear();
        for i in 0..n {
            let x = card.x + i as f64 * SHUTTER_NARROW + if i > self.selected { wide - SHUTTER_NARROW } else { 0.0 };
            self.hits.push((i, Rect { x, y: card.y, w: if i == self.selected { wide } else { SHUTTER_NARROW }, h: card.h }));
        }
        // The strips show one of two finished pictures, so moving them only copies pixels.
        let k = style.scale * self.zoom;
        if self.backdrop_scale != k {
            let (zoom, radius) = (self.zoom, self.radius);
            let picture = |shade: &dyn Fn(*mut Cairo)| {
                Sprite::new(card.w, card.h, k, |cr| {
                    unsafe {
                        cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, radius);
                        cb::cairo_clip(cr);
                    }
                    wallpaper(cr, card, zoom);
                    shade(cr);
                })
            };
            let closed = picture(&|cr| cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, 0.0, (0.0, 0.0, 0.0, 0.6)));
            let open = picture(&|cr| cb::fill_rounded_rect_gradient(cr, card.x, card.y, card.w, card.h, 0.0, (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.78), 0.4, 1.0));
            self.backdrop = vec![closed, open];
            self.backdrop_scale = k;
        }
        let hold = self.hold_level();
        let hint = self.hint();
        let mut x = card.x;
        for i in 0..n {
            let level = self.levels.settled_level(i, i == self.selected, SPRING_OPEN, style);
            let w = SHUTTER_NARROW + level * (wide - SHUTTER_NARROW);
            let r = Rect { x, y: card.y, w, h: card.h };
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, r.x, r.y, r.w, r.h);
                cb::cairo_clip(cr);
            }
            if level < 0.996 {
                self.backdrop[0].paint_corner(cr, k, 1.0);
            }
            if level > 0.004 {
                self.backdrop[1].paint_corner(cr, k, level.min(1.0));
            }
            unsafe { cb::cairo_restore(cr) };
            draw_shutter(cr, self.actions[i], r, level, wide, accent, i > 0, self.radius);
            if i == self.selected {
                fill_upwards(cr, r, self.radius, hold, faded(accent, 0.34));
                if let Some(hint) = hint {
                    let open = ((level - 0.45) / 0.55).clamp(0.0, 1.0);
                    cb::draw_pango_text(cr, r.x + 28.0, r.y + 28.0, hint, "Inter, Sans Medium 14px", faded(INK, 0.9 * open), None);
                }
            }
            x += w;
        }
    }

    // ── Orbit ────────────────────────────────────────────────────────────

    fn draw_orbit(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let (fg, inv) = (style.fg, cb::hex_to_rgba(&theme.inverse_primary, 1.0));
        let accent = cb::hex_to_rgba(&theme.primary, 1.0);
        let (cx, cy) = (card.x + card.w / 2.0, card.y + card.h / 2.0);
        let n = self.actions.len();
        let spot = |place: f64| {
            let a = place / n as f64 * std::f64::consts::TAU - std::f64::consts::FRAC_PI_2;
            (cx + ORBIT_RADIUS * a.cos(), cy + ORBIT_RADIUS * a.sin())
        };
        ring(cr, cx, cy, ORBIT_RADIUS, 0.0, 1.0, faded(fg, 0.14), 1.5);
        let place = self.fan(ui::SPRING_KNOB, style);
        let (hx, hy) = spot(place);
        cb::fill_circle(cr, hx, hy, 42.0, faded(fg, 0.88));
        self.hits.clear();
        for (i, act) in self.actions.iter().enumerate() {
            let (x, y) = spot(i as f64);
            self.hits.push((i, Rect { x: x - 42.0, y: y - 42.0, w: 84.0, h: 84.0 }));
            let near = (1.0 - loop_distance(i, place, n).abs()).clamp(0.0, 1.0);
            svg::draw_path(cr, act.icon(), x - 16.0, y - 16.0, 32.0, ui::mix(fg, inv, near), Some(1.8));
        }
        let level = self.hold_level();
        if level > 0.002 {
            ring(cr, cx, cy, 146.0, 0.0, level, accent, 6.0);
        }
        let (time, _) = clock();
        centred(cr, Rect { x: card.x, y: cy - 72.0, w: card.w, h: 70.0 }, &time, "Inter, Sans Bold 56px", fg);
        if let Some(act) = self.actions.get(self.selected) {
            centred(cr, Rect { x: card.x, y: cy + 4.0, w: card.w, h: 30.0 }, act.label(), "Inter, Sans Semi-Bold 20px", fg);
            let line = self.hint().unwrap_or(act.about());
            centred(cr, Rect { x: cx - 130.0, y: cy + 36.0, w: 260.0, h: 24.0 }, line, "Inter, Sans 12px", faded(fg, 0.72));
        }
    }

    // ── Floating Cards ───────────────────────────────────────────────────

    fn draw_cards(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let k = style.scale * self.zoom;
        if self.faces_scale != k {
            self.faces.clear();
            self.faces_scale = k;
        }
        let n = self.actions.len();
        let fan = self.fan(SPRING_FAN, style);
        let moving = !self.turn.settled(0.002);
        let (cx, cy) = ((card.x + card.w / 2.0).round(), (card.y + 196.0).round());
        let (fw, fh) = (CARD_FACE.0 + 2.0 * CARD_MARGIN, CARD_FACE.1 + 2.0 * CARD_MARGIN);

        // Hit places, front card first because it overlaps its neighbours.
        let mut near: Vec<usize> = (0..n).collect();
        near.sort_by(|a, b| loop_distance(*a, self.selected as f64, n).abs().total_cmp(&loop_distance(*b, self.selected as f64, n).abs()));
        self.hits.clear();
        for i in near {
            let pose = card_pose(loop_distance(i, self.selected as f64, n));
            let (w, h) = (CARD_FACE.0 * pose.scale, CARD_FACE.1 * pose.scale);
            self.hits.push((i, Rect { x: cx + pose.x - w / 2.0, y: cy + pose.y - h / 2.0, w, h }));
        }

        let level = self.hold_level();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| loop_distance(*b, fan, n).abs().total_cmp(&loop_distance(*a, fan, n).abs()));
        for i in order {
            let d = loop_distance(i, fan, n);
            let pose = card_pose(d);
            let act = self.actions[i];
            let face = self.faces.entry(act).or_insert_with(|| card_face(act, theme, k));
            let hold_pose = |cr: *mut Cairo| unsafe {
                cb::cairo_translate(cr, cx + pose.x, cy + pose.y);
                cb::cairo_rotate(cr, pose.angle);
                cb::cairo_scale(cr, pose.scale, pose.scale);
            };
            // The next card towards the front hides most of this one: skip those pixels.
            let nearer = card_pose(d - d.signum());
            match card_cover(&nearer).filter(|_| d.abs() >= 1.0) {
                Some((w, h)) => {
                    for part in crate::launcher_themes::deck_uncovered(cx + nearer.x, cy + nearer.y, w, h) {
                        unsafe {
                            cb::cairo_save(cr);
                            cb::cairo_rectangle(cr, part.x, part.y, part.w, part.h);
                            cb::cairo_clip(cr);
                        }
                        hold_pose(cr);
                        face.paint(cr, fw, fh, k, 1.0, moving && d.abs() >= 0.35);
                        unsafe { cb::cairo_restore(cr) };
                    }
                }
                None => {
                    unsafe { cb::cairo_save(cr) };
                    hold_pose(cr);
                    face.paint(cr, fw, fh, k, 1.0, moving && d.abs() >= 0.35);
                    if i == self.selected && level > 0.002 {
                        let r = Rect { x: -CARD_FACE.0 / 2.0, y: -CARD_FACE.1 / 2.0, w: CARD_FACE.0, h: CARD_FACE.1 };
                        fill_upwards(cr, r, 22.0, level, faded(INK, 0.26));
                    }
                    unsafe { cb::cairo_restore(cr) };
                }
            }
        }
        if let Some(act) = self.actions.get(self.selected) {
            let text = match self.hint() {
                Some(hint) => format!("{}  \u{00B7}  {hint}", act.label()),
                None => act.label().to_string(),
            };
            let font = "Inter, Sans Medium 14px";
            let w = cb::measure_pango_text(cr, &text, font, 4000, Some(1)).0 as f64 + 48.0;
            let plate = Rect { x: cx - w / 2.0, y: cy + 212.0, w, h: 42.0 };
            cb::drop_shadow(cr, plate.x, plate.y, plate.w, plate.h, 21.0, 16.0, 5.0, 0.32);
            cb::fill_rounded_rect(cr, plate.x, plate.y, plate.w, plate.h, 21.0, faded(style.card_bg, 0.97));
            cb::stroke_rounded_rect(cr, plate.x + 0.5, plate.y + 0.5, plate.w - 1.0, plate.h - 1.0, 21.0, faded(INK, 0.16), 1.0);
            centred(cr, plate, &text, font, style.fg);
        }
    }
}

/// The part of the screen a card held at `pose` certainly covers, around its centre: half width
/// and half height of the largest upright box inside it.
fn card_cover(pose: &Pose) -> Option<(f64, f64)> {
    let (sin, cos) = (pose.angle.abs().sin(), pose.angle.cos());
    let (hw, hh) = (CARD_FACE.0 / 2.0 * pose.scale, CARD_FACE.1 / 2.0 * pose.scale);
    let (w, h) = (hw * cos - hh * sin - 2.0, hh * cos - hw * sin - 2.0);
    (w > 0.0 && h > 0.0).then_some((w, h))
}

/// The outline of the splash at angle `a`, as a factor of its radius.
fn splash_outline(a: f64) -> f64 {
    1.0 + 0.07 * (3.0 * a + 1.0).sin() + 0.05 * (5.0 * a + 2.4).sin() + 0.03 * (9.0 * a + 0.7).sin()
}

/// Orbit's backdrop: colour that thins out to nothing towards a splash-shaped edge. Stacked
/// outlines, each a little smaller, build up the colour towards the middle.
fn orbit_splash(card: Rect, zoom: f64, k: f64, color: Rgba) -> Sprite {
    const CENTRE_ALPHA: f64 = 0.84;
    const DROPLETS: [(f64, f64, f64); 7] = [(0.4, 318.0, 13.0), (1.3, 322.0, 8.0), (2.2, 316.0, 15.0), (3.0, 324.0, 7.0), (3.9, 318.0, 12.0), (4.9, 322.0, 9.0), (5.7, 316.0, 14.0)];
    let layer = 1.0 - (1.0 - CENTRE_ALPHA).powf(1.0 / SPLASH_LAYERS as f64);
    Sprite::new(card.w, card.h, k, |cr| unsafe {
        cb::cairo_translate(cr, card.w / 2.0, card.h / 2.0);
        cb::cairo_scale(cr, zoom, zoom);
        cb::cairo_set_source_rgba(cr, color.0, color.1, color.2, layer);
        for i in 0..SPLASH_LAYERS {
            let size = SPLASH_RADIUS * (1.0 - 0.46 * i as f64 / (SPLASH_LAYERS - 1) as f64);
            cb::cairo_new_path(cr);
            for step in 0..160 {
                let a = step as f64 / 160.0 * std::f64::consts::TAU;
                let r = size * splash_outline(a);
                cb::cairo_line_to(cr, r * a.cos(), r * a.sin());
            }
            cb::cairo_close_path(cr);
            cb::cairo_fill(cr);
        }
        for (a, dist, r) in DROPLETS {
            for i in 0..6 {
                cb::fill_circle(cr, dist * a.cos(), dist * a.sin(), r * (1.0 - i as f64 * 0.13), faded(color, 0.12));
            }
        }
    })
}

/// The wallpaper filling `r`, decoded at the size it appears on screen.
fn wallpaper(cr: *mut Cairo, r: Rect, zoom: f64) {
    let Some(img) = cb::wallpaper_panel_path().and_then(|f| cb::cached_image_cover(&f, (r.w * zoom).round() as i32, (r.h * zoom).round() as i32)) else { return };
    let (iw, ih) = cb::image_size(img);
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_rectangle(cr, r.x, r.y, r.w, r.h);
        cb::cairo_clip(cr);
        cb::cairo_translate(cr, r.x + r.w / 2.0, r.y + r.h / 2.0);
        cb::cairo_scale(cr, 1.0 / zoom, 1.0 / zoom);
    }
    cb::paint_cached(cr, img, -iw / 2.0, -ih / 2.0);
    unsafe { cb::cairo_restore(cr) };
}

fn faded(c: Rgba, alpha: f64) -> Rgba {
    (c.0, c.1, c.2, alpha)
}

fn centred(cr: *mut Cairo, r: Rect, text: &str, font: &str, color: Rgba) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 4000, Some(1));
    cb::draw_pango_text(cr, r.x + ((r.w - w as f64) / 2.0).round(), r.y + ((r.h - h as f64) / 2.0).round(), text, font, color, None);
}

/// Fills the rounded rectangle `r` from the bottom up to `level`.
fn fill_upwards(cr: *mut Cairo, r: Rect, radius: f64, level: f64, color: Rgba) {
    if level > 0.002 {
        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, r.x, r.y, r.w, r.h, radius);
            cb::cairo_clip(cr);
        }
        cb::fill_rounded_rect(cr, r.x, r.y + r.h * (1.0 - level), r.w, r.h * level, 0.0, color);
        unsafe { cb::cairo_restore(cr) };
    }
}

/// Fills the rounded rectangle `r` from the left up to `level`.
fn fill_across(cr: *mut Cairo, r: Rect, radius: f64, level: f64, color: Rgba) {
    if level > 0.002 {
        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, r.x, r.y, r.w, r.h, radius);
            cb::cairo_clip(cr);
        }
        cb::fill_rounded_rect(cr, r.x, r.y, r.w * level, r.h, 0.0, color);
        unsafe { cb::cairo_restore(cr) };
    }
}

/// Part of a circle, clockwise from the top; `from` and `to` are fractions of a turn.
fn ring(cr: *mut Cairo, cx: f64, cy: f64, r: f64, from: f64, to: f64, color: Rgba, width: f64) {
    let start = -std::f64::consts::FRAC_PI_2;
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_new_path(cr);
        cb::cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        cb::cairo_set_line_width(cr, width);
        cb::cairo_set_line_cap(cr, 1);
        cb::cairo_arc(cr, cx, cy, r, start + from * std::f64::consts::TAU, start + to * std::f64::consts::TAU);
        cb::cairo_stroke(cr);
        cb::cairo_restore(cr);
    }
}

/// One strip of Shutters; `level` is 0 closed and 1 fully open at width `wide`.
#[allow(clippy::too_many_arguments)]
fn draw_shutter(cr: *mut Cairo, act: Act, r: Rect, level: f64, wide: f64, accent: Rgba, divider: bool, radius: f64) {
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_rectangle(cr, r.x, r.y, r.w, r.h);
        cb::cairo_clip(cr);
    }
    if divider {
        cb::fill_rounded_rect(cr, r.x, r.y, 1.0, r.h, 0.0, faded(INK, 0.16));
    }
    cb::fill_rounded_rect(cr, r.x + radius, r.y + r.h - 4.0, (r.w - 2.0 * radius).max(0.0), 4.0, 2.0, faded(accent, level));

    let closed = (1.0 - level * 2.2).clamp(0.0, 1.0);
    if closed > 0.0 {
        let bottom = r.y + r.h - 28.0;
        svg::draw_path(cr, act.icon(), r.x + (SHUTTER_NARROW - 34.0) / 2.0, bottom - 34.0, 34.0, faded(INK, closed), Some(1.8));
        let font = "Inter, Sans Medium 16px";
        let (_, th) = cb::measure_pango_text(cr, "Ag", font, 4000, Some(1));
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_translate(cr, r.x + (SHUTTER_NARROW - th as f64) / 2.0, bottom - 52.0);
            cb::cairo_rotate(cr, -std::f64::consts::FRAC_PI_2);
        }
        cb::draw_pango_text(cr, 0.0, 0.0, act.label(), font, faded(INK, closed), None);
        unsafe { cb::cairo_restore(cr) };
    }
    let open = ((level - 0.45) / 0.55).clamp(0.0, 1.0);
    if open > 0.0 {
        let (x, bottom) = (r.x + 28.0, r.y + r.h - 34.0);
        svg::draw_path(cr, act.icon(), x, bottom - 68.0, 68.0, faded(INK, open), Some(1.6));
        let tx = x + 68.0 + 22.0;
        let room = (wide - 28.0 - 68.0 - 22.0 - 28.0).max(40.0) as i32;
        cb::draw_pango_text(cr, tx, bottom - 72.0, act.label(), "Inter, Sans Bold 34px", faded(INK, open), Some(room));
        cb::draw_pango_text(cr, tx, bottom - 24.0, act.about(), "Inter, Sans 15px", faded(INK, 0.85 * open), Some(room));
    }
    unsafe { cb::cairo_restore(cr) };
}

/// A card of Floating Cards, drawn once and then only moved around.
fn card_face(act: Act, theme: &Theme, k: f64) -> Sprite {
    let (w, h) = CARD_FACE;
    let tint = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
    let shade = |f: f64| (tint.0 * f, tint.1 * f, tint.2 * f, 1.0);
    Sprite::new(w + 2.0 * CARD_MARGIN, h + 2.0 * CARD_MARGIN, k, |cr| {
        let (x, y) = (CARD_MARGIN, CARD_MARGIN);
        cb::drop_shadow(cr, x, y, w, h, 22.0, 24.0, 10.0, 0.42);
        cb::fill_rounded_rect_gradient(cr, x, y, w, h, 22.0, shade(1.0), shade(0.42), 0.0, 1.0);
        cb::stroke_rounded_rect(cr, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 22.0, faded(INK, 0.22), 1.0);
        svg::draw_path(cr, act.icon(), x + 22.0, y + 26.0, 68.0, INK, Some(1.6));
        cb::draw_pango_text(cr, x + 22.0, y + 124.0, act.label(), "Inter, Sans Bold 25px", INK, Some((w - 44.0) as i32));
        cb::draw_pango_text_wrapped(cr, x + 22.0, y + 166.0, act.about(), "Inter, Sans 14px", faded(INK, 0.84), (w - 44.0) as i32, Some(4));
    })
}

impl CardView for PowerView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        if self.actions.is_empty() {
            ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
            centred(cr, card, "Turn on actions in Settings > Power", "Inter, Sans Medium 18px", style.fg);
            return;
        }
        let (bw, bh) = self.design.base_size(self.actions.len());
        self.zoom = (card.w / bw).min(card.h / bh);
        // On whole pixels, so finished pictures are copied without resampling.
        let snap = |v: f64| (v * style.scale).round() / style.scale;
        self.origin = (snap(card.x + (card.w - bw * self.zoom) / 2.0), snap(card.y + (card.h - bh * self.zoom) / 2.0));
        self.radius = style.radius / self.zoom;
        if !self.design.floats() {
            ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        }
        if self.design == Design::Orbit {
            if self.splash.as_ref().is_none_or(|(w, h, _)| (*w, *h) != (card.w, card.h)) {
                let color = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
                self.splash = Some((card.w, card.h, orbit_splash(card, self.zoom, style.scale, color)));
            }
            if let Some((_, _, splash)) = &self.splash {
                splash.paint_at(cr, card.x, card.y, style.scale);
            }
        }
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_translate(cr, self.origin.0, self.origin.1);
            cb::cairo_scale(cr, self.zoom, self.zoom);
        }
        let inner = Rect { x: 0.0, y: 0.0, w: bw, h: bh };
        match self.design {
            Design::Glide => self.draw_glide(cr, inner, style, theme),
            Design::Split => self.draw_split(cr, inner, style, theme),
            Design::Shutters => self.draw_shutters(cr, inner, style, theme),
            Design::Orbit => self.draw_orbit(cr, inner, style, theme),
            Design::Cards => self.draw_cards(cr, inner, style, theme),
        }
        unsafe { cb::cairo_restore(cr) };
        if !self.design.floats() {
            ui::card_edge(cr, card.x, card.y, card.w, card.h, style);
        }
    }

    /// Hover selects, except where the cards would move away under the pointer. The first report
    /// only says where the pointer rests, which must not take the selection from the keyboard.
    fn motion(&mut self, x: f64, y: f64) -> bool {
        let moved = self.pointer != (-1.0, -1.0) && (x, y) != self.pointer;
        self.pointer = (x, y);
        let over = self.hit(x, y);
        let mut changed = over != self.hover;
        self.hover = over;
        if self.hold.is_some_and(|h| h.by == HeldBy::Pointer && over != Some(h.index)) {
            self.release_hold();
            changed = true;
        }
        if moved && self.design != Design::Cards {
            if let Some(i) = over.filter(|i| *i != self.selected) {
                self.select(i);
                changed = true;
            }
        }
        changed
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        match self.hit(x, y) {
            Some(i) if self.design == Design::Cards && i != self.selected => {
                self.select(i);
                Effect::Redraw
            }
            Some(i) => {
                self.select(i);
                self.activate(i, HeldBy::Pointer)
            }
            // Nothing is drawn around a floating design: that is a click outside.
            None if self.design.floats() => Effect::Close,
            None => Effect::None,
        }
    }

    fn release(&mut self) -> Effect {
        if self.hold.is_some_and(|h| h.by == HeldBy::Pointer) {
            self.release_hold();
            return Effect::Redraw;
        }
        Effect::None
    }

    fn key_down(&mut self, key: u32) {
        self.last_key = key;
    }

    fn key_release(&mut self, key: u32) -> Effect {
        if self.hold.is_some_and(|h| h.by == HeldBy::Keyboard(key)) {
            self.release_hold();
            return Effect::Redraw;
        }
        Effect::None
    }

    fn focus_lost(&mut self) -> Effect {
        if self.hold.is_some() {
            self.release_hold();
            return Effect::Redraw;
        }
        Effect::None
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.step(if delta > 0.0 { 1 } else { -1 });
        true
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, _ctrl: bool) -> Effect {
        match (special, typed.map(|c| c.to_ascii_lowercase())) {
            (ESC, _) => return Effect::Close,
            (LEFT | UP | PGUP, _) => self.step(-1),
            (RIGHT | DOWN | PGDN, _) => self.step(1),
            (HOME, _) => self.select(0),
            (END, _) => self.select(self.actions.len().saturating_sub(1)),
            // A held key repeats: only the first press counts.
            (ENTER, _) | (_, Some(' ')) if self.hold.is_some() => return Effect::None,
            (ENTER, _) | (_, Some(' ')) => return self.activate(self.selected, HeldBy::Keyboard(self.last_key)),
            (_, Some(c)) => {
                let by_number = c.to_digit(10).and_then(|d| (d as usize).checked_sub(1)).filter(|i| *i < self.actions.len());
                let Some(i) = by_number.or_else(|| self.actions.iter().position(|a| a.key() == c)) else { return Effect::None };
                self.select(i);
                // Shortcut keys choose; an action that must be held still takes a held Enter or Space.
                if !(self.hold_to_confirm && self.actions[i].needs_hold()) {
                    return self.run(i);
                }
            }
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// Runs a held action once it has been held long enough.
    fn poll(&mut self) -> bool {
        if let Some(h) = self.hold.filter(|h| h.since.elapsed() >= HOLD) {
            self.hold = None;
            let _ = self.run(h.index);
            return true;
        }
        false
    }

    fn wake_in(&self) -> Option<Duration> {
        self.hold.map(|_| Duration::from_millis(16))
    }

    fn wants_close(&self) -> bool {
        self.done
    }

    fn clickable(&self) -> bool {
        self.hover.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: PowerSupport = PowerSupport { suspend: true, sleep: true, hibernate: true };

    fn view(actions: Vec<Act>, hold: bool) -> PowerView {
        PowerView::with_actions(Design::Glide, actions, hold, String::new())
    }

    #[test]
    fn settings_choose_what_the_menu_offers() {
        let mut cfg = Config::default();
        assert_eq!(pick(&cfg, ALL).len(), 7);
        cfg.power_suspend = false;
        cfg.power_lock = false;
        assert_eq!(pick(&cfg, ALL), vec![Act::Logout, Act::Sleep, Act::Hibernate, Act::Reboot, Act::Shutdown]);
    }

    #[test]
    fn what_the_computer_cannot_do_is_left_out() {
        let none = PowerSupport { suspend: true, sleep: false, hibernate: false };
        let acts = pick(&Config::default(), none);
        assert!(acts.contains(&Act::Suspend));
        assert!(!acts.contains(&Act::Sleep) && !acts.contains(&Act::Hibernate));
    }

    #[test]
    fn only_session_ending_actions_are_held() {
        let held: Vec<Act> = Act::ALL.into_iter().filter(|a| a.needs_hold()).collect();
        assert_eq!(held, vec![Act::Logout, Act::Reboot, Act::Shutdown]);
    }

    #[test]
    fn shortcut_keys_are_unique() {
        let keys: std::collections::HashSet<char> = Act::ALL.iter().map(|a| a.key()).collect();
        assert_eq!(keys.len(), Act::ALL.len());
    }

    #[test]
    fn enter_starts_a_hold_and_letting_go_cancels_it() {
        let mut v = view(vec![Act::Shutdown], true);
        // A remapped layout: Enter arrives from a key whose raw code is not the usual one.
        v.key_down(200);
        assert!(matches!(v.key(ENTER, None, false, false), Effect::Redraw));
        assert!(v.hold.is_some());
        // Key repeat must not restart the hold.
        let since = v.hold.unwrap().since;
        v.key(ENTER, None, false, false);
        assert_eq!(v.hold.unwrap().since, since);
        // Another key going up changes nothing.
        v.key_release(42);
        assert!(v.hold.is_some());
        v.key_release(200);
        assert!(v.hold.is_none());
        assert!(!v.poll() && !v.done);
    }

    #[test]
    fn space_works_like_enter() {
        let mut v = view(vec![Act::Shutdown], true);
        v.key_down(57);
        assert!(matches!(v.key(0, Some(' '), false, false), Effect::Redraw));
        // Key repeat must not restart the hold.
        let since = v.hold.unwrap().since;
        assert!(matches!(v.key(0, Some(' '), false, false), Effect::None));
        assert_eq!(v.hold.unwrap().since, since);
        v.key_release(57);
        assert!(v.hold.is_none() && !v.done);
    }

    #[test]
    fn a_hold_is_cancelled_by_moving_or_losing_focus() {
        let mut v = view(vec![Act::Reboot, Act::Shutdown], true);
        v.key(ENTER, None, false, false);
        v.key(RIGHT, None, false, false);
        assert!(v.hold.is_none());
        v.key(ENTER, None, false, false);
        v.focus_lost();
        assert!(v.hold.is_none());
    }

    #[test]
    fn a_shortcut_key_never_runs_a_held_action() {
        let mut v = view(vec![Act::Lock, Act::Shutdown], true);
        assert!(matches!(v.key(0, Some('s'), false, false), Effect::Redraw));
        assert_eq!(v.selected, 1);
        assert!(v.hold.is_none() && !v.done);
    }

    #[test]
    fn a_pointer_hold_ends_on_release_or_leaving_the_action() {
        let mut v = view(vec![Act::Shutdown], true);
        v.hits = vec![(0, Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 })];
        v.motion(5.0, 5.0);
        v.click(5.0, 5.0, false);
        assert!(v.hold.is_some());
        v.motion(50.0, 50.0);
        assert!(v.hold.is_none());
        v.click(5.0, 5.0, false);
        v.release();
        assert!(v.hold.is_none());
    }

    #[test]
    fn a_resting_pointer_does_not_select() {
        let mut v = view(vec![Act::Lock, Act::Shutdown], true);
        v.hits = vec![(1, Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 })];
        v.motion(5.0, 5.0);
        assert_eq!(v.selected, 0);
        v.motion(6.0, 5.0);
        assert_eq!(v.selected, 1);
    }

    #[test]
    fn selection_wraps_around() {
        let mut v = view(vec![Act::Lock, Act::Suspend, Act::Reboot], true);
        v.key(LEFT, None, false, false);
        assert_eq!(v.selected, 2);
        v.key(RIGHT, None, false, false);
        assert_eq!(v.selected, 0);
    }

    #[test]
    fn the_loop_turns_the_short_way() {
        assert_eq!(shortest_turn(0, 6, 7), -1);
        assert_eq!(shortest_turn(6, 0, 7), 1);
        assert_eq!(shortest_turn(1, 3, 7), 2);
        assert!((loop_distance(6, 0.0, 7) + 1.0).abs() < 1e-9);
        assert!((loop_distance(1, 0.0, 7) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_front_card_faces_forward() {
        assert_eq!(card_pose(0.0), Pose { x: 0.0, y: 0.0, angle: 0.0, scale: 1.0 });
        assert_eq!(card_pose(2.0).x, -card_pose(-2.0).x);
    }

    #[test]
    fn uptime_reads_naturally() {
        assert_eq!(uptime_label("300.5 100.0"), "Up 5m");
        assert_eq!(uptime_label("15120.0 1.0"), "Up 4h 12m");
        assert_eq!(uptime_label("180000.0 1.0"), "Up 2d 2h");
    }

    #[test]
    fn the_glide_bar_replaces_anything_unknown() {
        assert_eq!(Design::from_id("classic"), Design::Glide);
        assert_eq!(theme_id("classic"), "glide");
        assert_eq!(theme_id("orbit"), "orbit");
        assert_eq!(Config::default().power_theme, "glide");
        assert!(floating("cards") && floating("orbit") && !floating("glide"));
    }

    #[test]
    fn the_menu_is_large_but_stays_on_screen() {
        for design in [Design::Glide, Design::Split, Design::Shutters, Design::Orbit, Design::Cards] {
            for n in 1..=7 {
                let base = design.base_size(n);
                let zoom = fit(base, 2560.0, 1440.0, design.max_zoom());
                assert!(zoom > 1.5, "{design:?}");
                assert!(base.0 * zoom <= 2560.0 && base.1 * zoom <= 1440.0, "{design:?}");
            }
        }
    }

    #[test]
    fn the_splash_fades_out_before_the_edge_of_its_surface() {
        let widest = (0..3600).map(|i| splash_outline(i as f64 / 3600.0 * std::f64::consts::TAU)).fold(0.0, f64::max);
        assert!(SPLASH_RADIUS * widest < ORBIT_SIZE / 2.0);
    }

    #[test]
    fn the_pointer_finds_actions_on_the_enlarged_menu() {
        let mut v = view(vec![Act::Lock], true);
        v.hits = vec![(0, Rect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 })];
        (v.origin, v.zoom) = ((100.0, 200.0), 2.0);
        assert_eq!(v.hit(140.0, 240.0), Some(0));
        assert_eq!(v.hit(15.0, 15.0), None);
    }

    #[test]
    fn a_covering_card_box_stays_inside_the_card() {
        let pose = card_pose(1.0);
        let (w, h) = card_cover(&pose).unwrap();
        assert!(w < CARD_FACE.0 / 2.0 * pose.scale && h < CARD_FACE.1 / 2.0 * pose.scale);
    }
}
