//! First-time setup guide (opens once on a fresh install, again with `prism-ctl setup` or
//! Settings > System > Setup Guide): a few pages, each with an animated sketch of what it is
//! about on the left and its options on the right. The options are the Settings rows
//! themselves (`settings::guide_rows`), so they change the same config values; every change is
//! saved at once and applies live. Finish (or Skip) marks the guide done.

use std::time::Instant;

use prism_common::{Config, Theme};

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{ENTER, ESC, LEFT, RIGHT};
use crate::settings::{guide_rows, GuideControl, GuideRow};
use crate::ui::{Rgba, Style};
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};

pub const CARD_W: f64 = 1040.0;
pub const CARD_H: f64 = 640.0;
const PAD: f64 = 28.0;
const ART_W: f64 = 420.0;

#[derive(Clone, Copy, PartialEq)]
enum Art {
    Welcome,
    Look,
    Wallpaper,
    Desktop,
    Lock,
    Notifications,
    Night,
    Capture,
    Downloads,
    Extras,
    Done,
}

struct Page {
    art: Art,
    title: &'static str,
    text: &'static str,
    rows: &'static [(usize, &'static str)],
}

const PAGES: &[Page] = &[
    Page { art: Art::Welcome, title: "Welcome to Prism", text: "A short tour of what Prism can do. Turn on what you like; everything here is also in Settings, and you can run this guide again from there.", rows: &[] },
    Page { art: Art::Look, title: "Look and feel", text: "Colours come from your wallpaper. Cards can blur what is behind them, cast shadows and move with springy animations.", rows: &[(0, "Matugen Colour Mode"), (0, "Window Background Blur"), (0, "Window Shadows"), (0, "Animations"), (0, "Dim Background")] },
    Page {
        art: Art::Wallpaper,
        title: "Wallpaper",
        text: "Pick wallpapers with Super+W (prism-ctl wallpaper toggle). Prism draws them itself, with smooth transitions, filters and video wallpapers.",
        rows: &[(1, "Wallpaper Daemon"), (1, "Wallpaper Directory"), (1, "Wallpaper Depth"), (1, "Transition"), (1, "Use Wallpaper Filters")],
    },
    Page { art: Art::Desktop, title: "Desktop", text: "A bar, a dock and widgets on the desktop. Drag widgets around in edit mode later.", rows: &[(3, "Show Bar"), (4, "Show Dock"), (2, "Digital Clock"), (2, "Calendar"), (2, "Media Player"), (2, "Weather"), (2, "Cat")] },
    Page { art: Art::Lock, title: "Lock screen", text: "Several lock screen designs; the screen can lock by itself when you step away.", rows: &[(8, "Lock Screen Theme"), (8, "Lock When Idle")] },
    Page { art: Art::Notifications, title: "Notifications", text: "Popups where you want them, a notification centre with search and snooze, and quiet hours.", rows: &[(7, "Popup Position"), (7, "Silent on a Schedule"), (7, "Notification Centre Position")] },
    Page { art: Art::Night, title: "Night light", text: "Warmer colours in the evening, on a schedule or from sunset to sunrise.", rows: &[(11, "Blue Light Filter"), (11, "Mode")] },
    Page { art: Art::Capture, title: "Screenshots and recording", text: "Print takes a screenshot of a region, window or screen; record with sound, trim, save GIFs, or keep a replay buffer.", rows: &[(14, "Copy to Clipboard"), (14, "Desktop Audio"), (14, "Replay Buffer"), (16, "Window Switcher")] },
    Page {
        art: Art::Downloads,
        title: "Downloads",
        text: "Prism has its own download manager: several connections per file, pause and resume, and videos from web pages. With the Prism Downloads extension for Firefox, browser downloads and the videos you play come to Prism. Click the pill (or run prism-ctl download toggle) to see them all.",
        rows: &[(15, "Download Folder"), (15, "Confirm Browser Downloads"), (15, "Download Progress")],
    },
    Page { art: Art::Extras, title: "Extras", text: "Phone sync with the KDE Connect app, update checks and a privacy indicator for the microphone, camera and screen sharing.", rows: &[(13, "Phone Sync"), (16, "Update Notifications"), (16, "Privacy Indicator")] },
    Page { art: Art::Done, title: "All set", text: "Open Settings any time for everything else (Super+, or prism-ctl settings toggle).", rows: &[] },
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Hit {
    Row(usize),
    Choice(usize, usize),
    /// A folder row's Choose button.
    Browse(usize),
    Back,
    Next,
    Skip,
    Dot(usize),
}

pub struct GuideView {
    page: usize,
    shown_at: Instant,
    rows: Vec<GuideRow>,
    cfg: Config,
    hits: Vec<(Hit, Rect)>,
    pointer: (f64, f64),
    knobs: crate::ui::Fades<usize>,
    hovers: crate::ui::Fades<Hit>,
    page_spring: crate::ui::SpringValue,
    done: bool,
    waker: crate::waker::Waker,
    /// A folder dialog that is open: the row it is for, and where its answer comes.
    picking: Option<(usize, std::sync::mpsc::Receiver<Result<Option<std::path::PathBuf>, String>>)>,
    /// Lower the card while the dialog is up (true), raise it again (false).
    layer_request: Option<bool>,
}

/// Marks the guide as seen.
fn mark_done() {
    let mut c = Config::load();
    if !c.setup_done {
        c.setup_done = true;
        let _ = c.save();
    }
}

impl GuideView {
    pub fn new(waker: crate::waker::Waker) -> Self {
        let mut v = GuideView {
            page: 0,
            shown_at: Instant::now(),
            rows: Vec::new(),
            cfg: Config::load(),
            hits: Vec::new(),
            pointer: (0.0, 0.0),
            knobs: crate::ui::Fades::default(),
            hovers: crate::ui::Fades::default(),
            page_spring: crate::ui::SpringValue::new(0.0),
            done: false,
            waker,
            picking: None,
            layer_request: None,
        };
        v.go(0);
        v
    }

    fn go(&mut self, page: usize) {
        self.page = page.min(PAGES.len() - 1);
        self.rows = guide_rows(PAGES[self.page].rows);
        self.shown_at = Instant::now();
        self.cfg = Config::load();
    }

    /// Change the config like Settings does: load, set, save (prismd and the desktop reload it).
    fn change(&mut self, f: impl FnOnce(&mut Config)) {
        let mut c = Config::load();
        f(&mut c);
        if let Err(e) = c.save() {
            eprintln!("[prism-ui] setup guide: {e}");
        }
        self.cfg = c;
    }

    fn finish(&mut self) -> Effect {
        mark_done();
        self.done = true;
        Effect::Close
    }

    fn palette(style: &Style, theme: &Theme) -> Palette {
        Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: cb::hex_to_rgba(&theme.on_primary, 1.0),
            outline: cb::hex_to_rgba(&theme.outline_variant, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        }
    }

    /// Ask for the folder of row `i` with the system dialog (the card steps below windows
    /// meanwhile, so the dialog shows).
    fn browse(&mut self, i: usize) {
        if self.picking.is_some() {
            return;
        }
        let Some(GuideControl::Folder(get, _)) = self.rows.get(i).map(|r| &r.control) else { return };
        let start = prism_common::config::expand_home(get(&self.cfg).trim());
        let start = if start.is_dir() { start } else { prism_common::config::expand_home("~") };
        let title = format!("Choose {}", self.rows[i].title);
        let (tx, rx) = std::sync::mpsc::channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::portal::pick_folder(&title, Some(&start)));
            waker.wake();
        });
        self.picking = Some((i, rx));
        self.layer_request = Some(true);
    }

    fn hover(&mut self, h: Hit, style: &Style) -> f64 {
        let on = self.hits.iter().any(|(k, r)| *k == h && r.contains(self.pointer.0, self.pointer.1));
        self.hovers.level(h, on, style)
    }
}

fn with_a(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, a)
}

/// Smooth 0..1..0 over `period` seconds.
fn wave(t: f64, period: f64) -> f64 {
    0.5 - 0.5 * (t / period * std::f64::consts::TAU).cos()
}

/// The page's sketch in `r` at time `t` (seconds on the page), following the live config.
fn draw_art(cr: *mut Cairo, art: Art, r: Rect, t: f64, cfg: &Config, style: &Style, theme: &Theme) {
    let fg = style.fg;
    let acc = style.accent;
    let sec = cb::hex_to_rgba(&theme.secondary, 1.0);
    let ter = cb::hex_to_rgba(&theme.primary_container, 1.0);
    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 18.0, with_a(fg, 0.06));
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    // A small screen used by several sketches.
    let sh = (r.w - 60.0) * 0.62;
    let screen = Rect { x: r.x + 30.0, y: r.y + ((r.h - sh) / 2.0).round(), w: r.w - 60.0, h: sh };
    let wall = |cr: *mut Cairo, s: Rect, a: Rgba, b: Rgba| {
        cb::fill_rounded_rect(cr, s.x, s.y, s.w, s.h, 10.0, with_a(a, 0.55));
        cb::fill_circle(cr, s.x + s.w * 0.72, s.y + s.h * 0.35, s.h * 0.22, with_a(b, 0.7));
        cb::fill_rounded_rect(cr, s.x, s.y + s.h * 0.68, s.w, s.h * 0.32, 10.0, with_a(b, 0.35));
    };
    match art {
        Art::Welcome | Art::Done => {
            // Light into a prism, a spectrum out.
            let s = 90.0;
            let (px, py) = (cx, cy + 10.0);
            let tri = [(px, py - s), (px - s * 0.9, py + s * 0.6), (px + s * 0.9, py + s * 0.6)];
            unsafe {
                cb::cairo_new_path(cr);
                cb::cairo_move_to(cr, tri[0].0, tri[0].1);
                cb::cairo_line_to(cr, tri[1].0, tri[1].1);
                cb::cairo_line_to(cr, tri[2].0, tri[2].1);
                cb::cairo_close_path(cr);
                cb::cairo_set_source_rgba(cr, fg.0, fg.1, fg.2, 0.12);
                cb::cairo_fill_preserve(cr);
                cb::cairo_set_source_rgba(cr, fg.0, fg.1, fg.2, 0.6);
                cb::cairo_set_line_width(cr, 2.0);
                cb::cairo_stroke(cr);
            }
            let grow = ((t - 0.2) / 0.8).clamp(0.0, 1.0);
            let beam_in = (r.x + 20.0, py - 10.0);
            let hit = (px - s * 0.42, py - s * 0.12);
            unsafe {
                cb::cairo_set_source_rgba(cr, 1.0, 1.0, 1.0, 0.85);
                cb::cairo_set_line_width(cr, 3.0);
                cb::cairo_move_to(cr, beam_in.0, beam_in.1);
                cb::cairo_line_to(cr, beam_in.0 + (hit.0 - beam_in.0) * grow, beam_in.1 + (hit.1 - beam_in.1) * grow);
                cb::cairo_stroke(cr);
            }
            let out = ((t - 1.0) / 0.9).clamp(0.0, 1.0);
            let colors = [(0.95, 0.3, 0.3), (0.98, 0.6, 0.2), (0.98, 0.9, 0.3), (0.4, 0.85, 0.4), (0.3, 0.6, 0.98), (0.6, 0.4, 0.95)];
            for (i, c) in colors.iter().enumerate() {
                let a = (i as f64 - 2.5) * 0.09 + 0.05 * (t * 1.3 + i as f64).sin() * 0.2;
                let len = (r.x + r.w - 20.0 - (px + s * 0.42)) * out;
                let (sx, sy) = (px + s * 0.40, py - s * 0.05);
                unsafe {
                    cb::cairo_set_source_rgba(cr, c.0, c.1, c.2, 0.9);
                    cb::cairo_set_line_width(cr, 4.0);
                    cb::cairo_move_to(cr, sx, sy);
                    cb::cairo_line_to(cr, sx + len, sy + len * a.tan());
                    cb::cairo_stroke(cr);
                }
            }
            if art == Art::Done {
                let k = ((t - 0.3) * 3.0).clamp(0.0, 1.0);
                cb::fill_circle(cr, cx, r.y + 70.0, 26.0 * k, acc);
                crate::svg::icon(cr, "check", cx - 12.0, r.y + 58.0, 24.0, with_a(cb::hex_to_rgba(&theme.on_primary, 1.0), k), false);
            }
        }
        Art::Look => {
            // A card over the wallpaper: blur, shadow and a springy pop.
            wall(cr, screen, sec, ter);
            let pop = 1.0 + 0.04 * (1.0 - ((t % 3.0) * 4.0).min(1.0)) * ((t % 3.0) * 18.0).sin() * if cfg.animations_enabled { 1.0 } else { 0.0 };
            let (w, h) = (screen.w * 0.55 * pop, screen.h * 0.6 * pop);
            let card = Rect { x: screen.x + (screen.w - w) / 2.0, y: screen.y + (screen.h - h) / 2.0, w, h };
            if cfg.dim_enabled {
                cb::fill_rounded_rect(cr, screen.x, screen.y, screen.w, screen.h, 10.0, (0.0, 0.0, 0.0, 0.3));
            }
            if cfg.shadows_enabled {
                cb::drop_shadow(cr, card.x, card.y, card.w, card.h, 14.0, 18.0, 6.0, 0.5);
            }
            let base = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
            cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, 14.0, with_a(base, if cfg.blur_enabled { 0.55 } else { 0.95 }));
            if cfg.blur_enabled {
                // Frosted look: soft light blobs.
                for k in 0..3 {
                    let ph = t * 0.6 + k as f64 * 2.1;
                    cb::fill_circle(cr, card.x + card.w * (0.3 + 0.2 * ph.sin()), card.y + card.h * (0.4 + 0.2 * ph.cos()), card.h * 0.3, (1.0, 1.0, 1.0, 0.06));
                }
            }
            for (i, c) in [acc, sec, ter].iter().enumerate() {
                cb::fill_circle(cr, card.x + 30.0 + i as f64 * 34.0, card.y + card.h - 28.0, 12.0, *c);
            }
            cb::fill_rounded_rect(cr, card.x + 20.0, card.y + 22.0, card.w * 0.6, 10.0, 5.0, with_a(fg, 0.7));
            cb::fill_rounded_rect(cr, card.x + 20.0, card.y + 42.0, card.w * 0.4, 8.0, 4.0, with_a(fg, 0.4));
        }
        Art::Wallpaper => {
            // Two wallpapers taking turns with a sweeping transition.
            let k = wave(t, 4.0);
            wall(cr, screen, sec, ter);
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_new_path(cr);
                cb::cairo_arc(cr, screen.x + screen.w * 0.5, screen.y + screen.h * 0.5, screen.w * 0.75 * k, 0.0, std::f64::consts::TAU);
                cb::cairo_clip(cr);
            }
            wall(cr, screen, ter, acc);
            unsafe { cb::cairo_restore(cr) };
            if cfg.wallpaper_filters_enabled {
                cb::fill_rounded_rect(cr, screen.x + 10.0, screen.y + 10.0, 70.0, 22.0, 11.0, (0.0, 0.0, 0.0, 0.4));
                cb::draw_pango_text(cr, screen.x + 20.0, screen.y + 13.0, "Filters", "Inter, Sans 11px", (1.0, 1.0, 1.0, 0.9), None);
            }
        }
        Art::Desktop => {
            wall(cr, screen, sec, ter);
            let s = screen;
            if cfg.bar.enabled {
                let k = ((t * 2.5).min(1.0)).powf(0.5);
                cb::fill_rounded_rect(cr, s.x + 6.0, s.y + 6.0 - (1.0 - k) * 20.0, s.w - 12.0, 16.0, 8.0, with_a((0.08, 0.08, 0.1, 1.0), 0.8 * k));
                for i in 0..4 {
                    cb::fill_circle(cr, s.x + 18.0 + i as f64 * 12.0, s.y + 14.0, 3.0, with_a(acc, k));
                }
            }
            if cfg.dock.enabled {
                let k = ((t * 2.5 - 0.3).clamp(0.0, 1.0)).powf(0.5);
                let dw = 120.0;
                cb::fill_rounded_rect(cr, s.x + (s.w - dw) / 2.0, s.y + s.h - 30.0 + (1.0 - k) * 30.0, dw, 24.0, 10.0, with_a((0.08, 0.08, 0.1, 1.0), 0.8 * k));
                for i in 0..5 {
                    let bob = if i == ((t * 1.5) as usize % 5) { -3.0 * wave(t * 3.0, 1.0) } else { 0.0 };
                    cb::fill_rounded_rect(cr, s.x + (s.w - dw) / 2.0 + 10.0 + i as f64 * 21.0, s.y + s.h - 26.0 + (1.0 - k) * 30.0 + bob, 16.0, 16.0, 4.0, with_a([acc, sec, ter, fg, acc][i], 0.9 * k));
                }
            }
            let l = &cfg.desktop_layout;
            if l.clock.visible {
                let now = chrono_hm();
                cb::draw_pango_text(cr, s.x + 16.0, s.y + 34.0, &now, "Inter, Sans Bold 26px", (1.0, 1.0, 1.0, 0.92), None);
            }
            if l.calendar.visible {
                let c = Rect { x: s.x + s.w - 90.0, y: s.y + 32.0, w: 74.0, h: 64.0 };
                cb::fill_rounded_rect(cr, c.x, c.y, c.w, c.h, 8.0, (0.0, 0.0, 0.0, 0.35));
                for k in 0..20 {
                    cb::fill_circle(cr, c.x + 10.0 + (k % 5) as f64 * 13.0, c.y + 14.0 + (k / 5) as f64 * 12.0, 2.0, (1.0, 1.0, 1.0, if k == 9 { 1.0 } else { 0.5 }));
                }
            }
            if l.mpris.visible {
                let m = Rect { x: s.x + 16.0, y: s.y + s.h - 74.0, w: 110.0, h: 36.0 };
                cb::fill_rounded_rect(cr, m.x, m.y, m.w, m.h, 8.0, (0.0, 0.0, 0.0, 0.35));
                for k in 0..8 {
                    let hgt = 6.0 + 12.0 * wave(t * 2.0 + k as f64 * 0.3, 1.0);
                    cb::fill_rounded_rect(cr, m.x + 40.0 + k as f64 * 8.0, m.y + m.h / 2.0 - hgt / 2.0, 4.0, hgt, 2.0, with_a(acc, 0.9));
                }
                cb::fill_rounded_rect(cr, m.x + 6.0, m.y + 6.0, 24.0, 24.0, 4.0, with_a(ter, 0.9));
            }
            if l.weather.visible {
                cb::fill_circle(cr, s.x + s.w - 52.0, s.y + s.h - 58.0, 12.0, (1.0, 0.85, 0.3, 0.95));
            }
            if l.cat.visible {
                // A sleeping cat that breathes.
                let b = 1.0 + 0.05 * (t * 2.0).sin();
                let (kx, ky) = (s.x + s.w * 0.5, s.y + s.h - 44.0);
                cb::fill_circle(cr, kx, ky, 12.0 * b, (0.05, 0.05, 0.06, 0.95));
                cb::fill_circle(cr, kx + 11.0, ky - 6.0, 7.0, (0.05, 0.05, 0.06, 0.95));
            }
        }
        Art::Lock => {
            wall(cr, screen, sec, ter);
            let s = screen;
            cb::fill_rounded_rect(cr, s.x, s.y, s.w, s.h, 10.0, (0.0, 0.0, 0.0, 0.25));
            let now = chrono_hm();
            let (tw, _) = cb::measure_pango_text(cr, &now, "Inter, Sans Bold 40px", 400, Some(1));
            let left = cfg.lockscreen_theme == "classic";
            let tx = if left { s.x + 24.0 } else { s.x + (s.w - tw as f64) / 2.0 };
            cb::draw_pango_text(cr, tx, s.y + s.h * 0.22, &now, "Inter, Sans Bold 40px", (1.0, 1.0, 1.0, 0.95), None);
            let th = cfg.lockscreen_theme.clone();
            let label = th[..1].to_uppercase() + &th[1..];
            cb::draw_pango_text(cr, s.x + 14.0, s.y + s.h - 26.0, &label, "Inter, Sans 11px", (1.0, 1.0, 1.0, 0.7), None);
            // The password field filling with dots.
            let f = Rect { x: s.x + s.w / 2.0 - 70.0, y: s.y + s.h * 0.62, w: 140.0, h: 26.0 };
            cb::fill_rounded_rect(cr, f.x, f.y, f.w, f.h, 13.0, (1.0, 1.0, 1.0, 0.18));
            let n = ((t * 3.0) as usize) % 9;
            for k in 0..n.min(7) {
                cb::fill_circle(cr, f.x + 18.0 + k as f64 * 16.0, f.y + 13.0, 4.0, (1.0, 1.0, 1.0, 0.9));
            }
            if cfg.manage_idle {
                let k = wave(t, 5.0);
                cb::fill_rounded_rect(cr, s.x, s.y, s.w, s.h, 10.0, (0.0, 0.0, 0.0, 0.35 * k));
            }
        }
        Art::Notifications => {
            wall(cr, screen, sec, ter);
            let s = screen;
            let pos = cfg.notification_popup_position.as_str();
            let (tw, th) = (130.0, 34.0);
            let x = if pos.ends_with("left") { s.x + 10.0 } else if pos.ends_with("center") { s.x + (s.w - tw) / 2.0 } else { s.x + s.w - tw - 10.0 };
            let top = pos.starts_with("top");
            for k in 0..3 {
                let appear = ((t - k as f64 * 0.7) * 2.0).clamp(0.0, 1.0);
                let ease = 1.0 - (1.0 - appear).powi(3);
                let dy = k as f64 * (th + 6.0);
                let y = if top { s.y + 10.0 + dy } else { s.y + s.h - 10.0 - th - dy };
                let slide = if pos.ends_with("left") { -30.0 } else { 30.0 } * (1.0 - ease);
                cb::fill_rounded_rect(cr, x + slide, y, tw, th, 10.0, with_a(cb::hex_to_rgba(&theme.inverse_primary, 1.0), 0.95 * ease));
                cb::fill_circle(cr, x + slide + 16.0, y + th / 2.0, 8.0, with_a(acc, ease));
                cb::fill_rounded_rect(cr, x + slide + 30.0, y + 9.0, 80.0, 6.0, 3.0, with_a(fg, 0.7 * ease));
                cb::fill_rounded_rect(cr, x + slide + 30.0, y + 20.0, 60.0, 5.0, 2.5, with_a(fg, 0.4 * ease));
            }
            if cfg.silent_schedule_enabled {
                crate::svg::icon(cr, "moon", r.x + r.w - 56.0, r.y + 14.0, 26.0, fg, false);
            }
        }
        Art::Night => {
            wall(cr, screen, sec, ter);
            let k = if cfg.blue_light_enabled { wave(t, 6.0) } else { 0.0 };
            cb::fill_rounded_rect(cr, screen.x, screen.y, screen.w, screen.h, 10.0, (1.0, 0.55, 0.15, 0.32 * k));
            // Sun sets, moon rises.
            let sy = screen.y + screen.h * (0.15 + 0.6 * k);
            cb::fill_circle(cr, screen.x + 40.0, sy, 16.0, (1.0, 0.85, 0.35, 1.0 - 0.6 * k));
            crate::svg::icon(cr, "moon", screen.x + screen.w - 60.0, screen.y + screen.h * (0.75 - 0.6 * k), 30.0, (1.0, 1.0, 1.0, 0.4 + 0.6 * k), false);
        }
        Art::Capture => {
            wall(cr, screen, sec, ter);
            let s = screen;
            let k = (t % 3.0 / 1.6).min(1.0);
            let e = 1.0 - (1.0 - k).powi(3);
            let sel = Rect { x: s.x + s.w * 0.2, y: s.y + s.h * 0.25, w: s.w * 0.5 * e, h: s.h * 0.45 * e };
            cb::fill_rounded_rect(cr, s.x, s.y, s.w, s.h, 10.0, (0.0, 0.0, 0.0, 0.35));
            cb::fill_rounded_rect(cr, sel.x, sel.y, sel.w, sel.h, 2.0, (1.0, 1.0, 1.0, 0.12));
            cb::stroke_rounded_rect(cr, sel.x, sel.y, sel.w, sel.h, 2.0, acc, 2.0);
            if t % 3.0 > 1.8 {
                // The flash and a thumbnail flying to the corner.
                let f = ((t % 3.0 - 1.8) / 1.2).clamp(0.0, 1.0);
                cb::fill_rounded_rect(cr, s.x, s.y, s.w, s.h, 10.0, (1.0, 1.0, 1.0, 0.4 * (1.0 - f * 3.0).max(0.0)));
                let tw = sel.w * (1.0 - 0.6 * f);
                cb::fill_rounded_rect(cr, sel.x + (s.x + s.w - 20.0 - tw - sel.x) * f, sel.y + (s.y + s.h - 20.0 - sel.h * (1.0 - 0.6 * f) - sel.y) * f, tw, sel.h * (1.0 - 0.6 * f), 6.0, with_a(ter, 0.9));
            }
            if cfg.replay_enabled {
                cb::fill_circle(cr, s.x + s.w - 16.0, s.y + 16.0, 6.0, (0.95, 0.3, 0.3, 0.5 + 0.5 * wave(t, 1.2)));
            }
        }
        Art::Downloads => {
            // A browser playing a video with the "Download video" button, its pieces coming in
            // over several connections, and the pill filling.
            let b = Rect { x: r.x + 26.0, y: r.y + 34.0, w: r.w - 52.0, h: (r.w - 52.0) * 0.6 };
            cb::fill_rounded_rect(cr, b.x, b.y, b.w, b.h, 10.0, with_a(fg, 0.85));
            cb::fill_rounded_rect(cr, b.x + 8.0, b.y + 7.0, b.w * 0.55, 10.0, 5.0, with_a(sec, 0.5));
            let v = Rect { x: b.x + 8.0, y: b.y + 24.0, w: b.w - 16.0, h: b.h - 32.0 };
            wall(cr, v, sec, ter);
            let bw = 132.0;
            let lift = 2.0 * wave(t, 2.4);
            cb::fill_rounded_rect(cr, v.x + v.w - bw - 8.0, v.y + 8.0 - lift, bw, 24.0, 12.0, (0.06, 0.07, 0.1, 0.88));
            crate::svg::draw_path(cr, "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3", v.x + v.w - bw, v.y + 13.0 - lift, 14.0, (0.35, 0.72, 1.0, 1.0), Some(2.2));
            cb::draw_pango_text(cr, v.x + v.w - bw + 18.0, v.y + 12.0 - lift, "Download video", "Inter, Sans Semi-Bold 11px", (1.0, 1.0, 1.0, 0.95), None);
            // One run every 6 s: the pieces, then the pill at 100%.
            let f = ((t % 6.0) / 5.0).min(1.0);
            let lanes = 4;
            let top = b.y + b.h + 22.0;
            for k in 0..lanes {
                let y = top + k as f64 * 12.0;
                cb::fill_rounded_rect(cr, b.x, y, b.w, 6.0, 3.0, with_a(fg, 0.10));
                // Each connection fills its own share, a little out of step.
                let own = ((f * 1.15 - k as f64 * 0.05).clamp(0.0, 1.0)) * b.w;
                cb::fill_rounded_rect(cr, b.x, y, own.max(6.0), 6.0, 3.0, with_a(acc, 0.55 + 0.1 * k as f64));
            }
            if cfg.downloads_watch {
                let pill = Rect { x: cx - 110.0, y: top + lanes as f64 * 12.0 + 18.0, w: 220.0, h: 34.0 };
                cb::fill_rounded_rect(cr, pill.x, pill.y, pill.w, pill.h, 17.0, (0.08, 0.08, 0.1, 0.92));
                cb::fill_rounded_rect(cr, pill.x + 14.0, pill.y + pill.h - 8.0, (pill.w - 28.0) * f, 3.0, 1.5, (0.55, 0.8, 1.0, 1.0));
                cb::draw_pango_text(cr, pill.x + 16.0, pill.y + 6.0, &format!("Video.mp4   {}%", (f * 100.0).round() as u32), "Inter, Sans Semi-Bold 12px", (1.0, 1.0, 1.0, 0.92), None);
            }
        }
        Art::Extras => {
            // A phone syncing.
            let ph = Rect { x: cx - 55.0, y: cy - 140.0, w: 110.0, h: 200.0 };
            cb::fill_rounded_rect(cr, ph.x, ph.y, ph.w, ph.h, 18.0, with_a(fg, 0.85));
            cb::fill_rounded_rect(cr, ph.x + 6.0, ph.y + 10.0, ph.w - 12.0, ph.h - 20.0, 12.0, with_a(sec, 0.9));
            if cfg.phone_enabled {
                for k in 0..3 {
                    let a = ((t * 1.5 - k as f64 * 0.3) % 1.5 / 1.5).clamp(0.0, 1.0);
                    crate::svg::draw_path(cr, "M5 12.55a11 11 0 0 1 14.08 0M8.53 16.11a6 6 0 0 1 6.95 0M12 20h.01", ph.x + ph.w + 6.0 + k as f64 * 4.0, ph.y + 30.0, 26.0, with_a(acc, 1.0 - a), Some(2.0));
                }
            }
        }
    }
}

/// "14:05" now.
fn chrono_hm() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    crate::cal_events::clock(t)
}

impl CardView for GuideView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let f = |px: f64, w: &str| format!("{} {w} {px}px", p.family);
        let t = self.shown_at.elapsed().as_secs_f64();
        let page = &PAGES[self.page];
        self.page_spring.set(self.page as f64, crate::ui::SPRING_KNOB, style);
        let mut new_hits: Vec<(Hit, Rect)> = Vec::new();

        // Left: the sketch.
        let art = Rect { x: card.x + PAD, y: card.y + PAD, w: ART_W, h: card.h - 2.0 * PAD - 60.0 };
        draw_art(cr, page.art, art, t, &self.cfg, style, theme);
        if style.time_scale().is_some() {
            crate::ui::keep_animating();
        }

        // Right: title, text, options.
        let x = art.x + art.w + 32.0;
        let w = card.x + card.w - PAD - x;
        let mut y = card.y + PAD + 4.0;
        cb::draw_pango_text(cr, x, y, &format!("{} / {}", self.page + 1, PAGES.len()), &f(12.0, "Semi-Bold"), with_a(style.accent, 0.9), None);
        y += 22.0;
        let (_, th) = cb::draw_pango_text_wrapped(cr, x, y, page.title, &f(26.0, "Bold"), fg, w as i32, Some(2));
        y += th as f64 + 10.0;
        let (_, bh) = cb::draw_pango_text_wrapped(cr, x, y, page.text, &f(14.0, ""), with_a(fg, 0.75), w as i32, Some(5));
        y += bh as f64 + 20.0;
        let cfg = self.cfg.clone();
        // Taken out while drawn: hover levels need `self`.
        let rows = std::mem::take(&mut self.rows);
        for (i, row) in rows.iter().enumerate() {
            if !(row.visible)(&cfg) {
                continue;
            }
            let entrance = ((t * 4.0 - i as f64 * 0.35).clamp(0.0, 1.0)).powf(0.6);
            let ry = y + (1.0 - entrance) * 12.0;
            match &row.control {
                GuideControl::Switch(get, _) => {
                    let r = Rect { x, y: ry, w, h: 44.0 };
                    let on = get(&cfg);
                    let knob = self.knobs.level(i, on, style);
                    cb::draw_pango_text(cr, x, ry + 12.0, row.title, &f(15.0, "Medium"), with_a(fg, entrance), Some((w - 70.0) as i32));
                    uikit::switch(cr, Rect { x: x + w - 50.0, y: ry + 8.0, w: 50.0, h: 28.0 }, knob, 0.0, &p);
                    new_hits.push((Hit::Row(i), r));
                    y += 48.0;
                }
                GuideControl::Choice(options, get, _) => {
                    cb::draw_pango_text(cr, x, ry, row.title, &f(15.0, "Medium"), with_a(fg, entrance), Some(w as i32));
                    let cur = get(&cfg);
                    let mut cx = x;
                    let mut cy = ry + 24.0;
                    for (k, (label, value)) in options(&cfg).iter().enumerate() {
                        let (lw, _) = cb::measure_pango_text(cr, label, &f(13.0, ""), 400, Some(1));
                        let c = Rect { x: cx, y: cy, w: lw as f64 + 26.0, h: 30.0 };
                        if c.x + c.w > x + w {
                            cx = x;
                            cy += 36.0;
                        }
                        let c = Rect { x: cx, y: cy, w: lw as f64 + 26.0, h: 30.0 };
                        let sel = *value == cur;
                        cb::fill_rounded_rect(cr, c.x, c.y, c.w, c.h, 15.0, if sel { style.accent } else { with_a(fg, 0.08) });
                        cb::draw_pango_text(cr, c.x + 13.0, c.y + 6.0, label, &f(13.0, ""), if sel { p.on_accent } else { fg }, None);
                        new_hits.push((Hit::Choice(i, k), c));
                        cx += c.w + 8.0;
                    }
                    y = cy + 30.0 + 18.0;
                }
                GuideControl::Folder(get, _) => {
                    cb::draw_pango_text(cr, x, ry, row.title, &f(15.0, "Medium"), with_a(fg, entrance), Some(w as i32));
                    let b = Rect { x: x + w - 110.0, y: ry + 22.0, w: 110.0, h: 34.0 };
                    let path = get(&cfg);
                    let shown = if path.trim().is_empty() { "Not set".to_string() } else { path };
                    let field = Rect { x, y: ry + 22.0, w: w - 122.0, h: 34.0 };
                    cb::fill_rounded_rect(cr, field.x, field.y, field.w, field.h, 10.0, with_a(fg, 0.06));
                    cb::draw_pango_text(cr, field.x + 12.0, field.y + 8.0, &shown, &f(13.0, ""), with_a(fg, 0.85 * entrance), Some((field.w - 24.0) as i32));
                    let h = self.hover(Hit::Browse(i), style);
                    let label = if self.picking.as_ref().is_some_and(|(r, _)| *r == i) { "Choosing\u{2026}" } else { "Choose\u{2026}" };
                    uikit::button(cr, b, label, ButtonKind::Outline, h, true, &p);
                    new_hits.push((Hit::Browse(i), b));
                    y += 22.0 + 34.0 + 18.0;
                }
            }
        }
        self.rows = rows;

        // Bottom: page dots, Skip, Back, Next.
        let by = card.y + card.h - PAD - 42.0;
        let dots_w = PAGES.len() as f64 * 14.0;
        let dx = art.x + (art.w - dots_w) / 2.0;
        for k in 0..PAGES.len() {
            let d = Rect { x: dx + k as f64 * 14.0, y: by + 14.0, w: 14.0, h: 14.0 };
            let near = 1.0 - (self.page_spring.value() - k as f64).abs().min(1.0);
            cb::fill_circle(cr, d.x + 7.0, d.y + 7.0, 3.5 + 1.5 * near, with_a(fg, 0.3 + 0.6 * near));
            new_hits.push((Hit::Dot(k), d));
        }
        let last = self.page + 1 == PAGES.len();
        let next = Rect { x: card.x + card.w - PAD - 150.0, y: by, w: 150.0, h: 42.0 };
        let back = Rect { x: next.x - 12.0 - 110.0, y: by, w: 110.0, h: 42.0 };
        let skip = Rect { x, y: by, w: 110.0, h: 42.0 };
        self.hits = new_hits.clone();
        self.hits.extend([(Hit::Next, next), (Hit::Back, back), (Hit::Skip, skip)]);
        let (hn, hb, hs) = (self.hover(Hit::Next, style), self.hover(Hit::Back, style), self.hover(Hit::Skip, style));
        uikit::button(cr, next, if last { "Finish" } else if self.page == 0 { "Start" } else { "Next" }, ButtonKind::Primary, hn, true, &p);
        if self.page > 0 {
            uikit::button(cr, back, "Back", ButtonKind::Outline, hb, true, &p);
        }
        if !last {
            uikit::button(cr, skip, "Skip", ButtonKind::Outline, hs, true, &p);
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        true
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        let Some((h, _)) = self.hits.iter().find(|(_, r)| r.contains(x, y)).copied() else { return Effect::None };
        match h {
            Hit::Next if self.page + 1 == PAGES.len() => return self.finish(),
            Hit::Next => self.go(self.page + 1),
            Hit::Back if self.page > 0 => self.go(self.page - 1),
            Hit::Back => {}
            Hit::Skip => return self.finish(),
            Hit::Dot(k) => self.go(k),
            Hit::Row(i) => {
                if let Some(GuideControl::Switch(get, set)) = self.rows.get(i).map(|r| &r.control) {
                    let (get, set) = (*get, *set);
                    let on = !get(&self.cfg);
                    self.change(|c| set(c, on));
                }
            }
            Hit::Browse(i) => self.browse(i),
            Hit::Choice(i, k) => {
                if let Some(GuideControl::Choice(options, _, set)) = self.rows.get(i).map(|r| &r.control) {
                    let set = *set;
                    if let Some((_, value)) = options(&self.cfg).get(k).cloned() {
                        self.change(|c| set(c, &value));
                    }
                }
            }
        }
        Effect::Redraw
    }

    fn key(&mut self, special: u32, _typed: Option<char>, _shift: bool, _ctrl: bool) -> Effect {
        match special {
            ESC => return self.finish(),
            ENTER | RIGHT if self.page + 1 == PAGES.len() => return self.finish(),
            ENTER | RIGHT => self.go(self.page + 1),
            LEFT if self.page > 0 => self.go(self.page - 1),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    fn wants_close(&self) -> bool {
        self.done
    }

    fn poll(&mut self) -> bool {
        let Some((i, rx)) = self.picking.as_ref() else { return false };
        let answer = match rx.try_recv() {
            Ok(a) => a,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Ok(None),
        };
        let i = *i;
        self.picking = None;
        self.layer_request = Some(false);
        match answer {
            Ok(Some(path)) => {
                if let Some(GuideControl::Folder(_, set)) = self.rows.get(i).map(|r| &r.control) {
                    let set = *set;
                    // Keep paths under $HOME portable, as "~/…".
                    let home = std::env::var("HOME").unwrap_or_default();
                    let text = path.to_string_lossy().into_owned();
                    let text = match text.strip_prefix(&format!("{home}/")) {
                        Some(rest) if !home.is_empty() => format!("~/{rest}"),
                        _ => text,
                    };
                    self.change(|c| {
                        set(c, &text);
                    });
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("[prism-ui] setup guide: file dialog failed: {e}"),
        }
        true
    }

    fn take_layer_request(&mut self) -> Option<bool> {
        self.layer_request.take()
    }

    fn clickable(&self) -> bool {
        self.hits.iter().any(|(_, r)| r.contains(self.pointer.0, self.pointer.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every option on a guide page is a Settings row with a switch, a choice or a folder (a
    /// renamed row, or a tab that moved, would drop it from the guide without a word).
    #[test]
    fn every_row_exists_in_settings() {
        for page in PAGES {
            let found: Vec<&str> = crate::settings::guide_rows(page.rows).iter().map(|r| r.title).collect();
            for (tab, title) in page.rows {
                assert!(found.contains(title), "{}: no row \"{title}\" on Settings tab {tab}", page.title);
            }
        }
    }
}
