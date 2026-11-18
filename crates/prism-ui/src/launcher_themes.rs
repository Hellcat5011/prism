//! Alternative launcher designs, chosen with `launcher_theme` (Settings > UI). Classic stays in
//! launcher.rs; everything else about the themes is in this file.
//!
//! * Slices: the wallpaper cut into strips, one app each; the selected strip opens wide.

use std::time::Instant;

use prism_common::apps::{search_apps, DesktopApp};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{self, Effect, Query, DOWN, END, ENTER, ESC, HOME, LEFT, MOUSE_READY, PGDN, PGUP, RIGHT, UP};
use crate::ui::{self, Fades, Rgba, SpringValue, Style};
use crate::view::CardView;

/// Label and config id of every launcher design, for Settings.
pub const THEMES: &[(&str, &str)] = &[("Classic", "classic"), ("Slices", "slices")];

const SLICES_SIZE: (f64, f64) = (1120.0, 600.0);

/// Card size of the launcher design `id`.
pub fn card_size(id: &str) -> (f64, f64) {
    if id == "slices" { SLICES_SIZE } else { (launcher::CARD_W, launcher::CARD_H) }
}

/// The launcher view for the design `id`.
pub fn view(id: &str, apps: Vec<DesktopApp>) -> Box<dyn CardView> {
    if id == "slices" { Box::new(SlicesLauncher::new(apps)) } else { Box::new(launcher::LauncherView::new(apps)) }
}

const SPRING_OPEN: (f64, f64) = (0.42, 0.82);
/// Wheel travel (design px) that moves the selection by one app.
const WHEEL_STEP: f64 = 30.0;
const INK: Rgba = (0.97, 0.97, 0.97, 1.0);

const SLICES_SHOWN: usize = 9;
const SLICE_NARROW: f64 = 76.0;

/// First strip of the window of `shown` strips that keeps `selected` in view.
fn window_first(first: usize, selected: usize, n: usize, shown: usize) -> usize {
    let first = if selected < first {
        selected
    } else if selected >= first + shown {
        selected + 1 - shown
    } else {
        first
    };
    first.min(n.saturating_sub(shown))
}

/// Width of the open strip when `n` strips share `card_w`.
fn slice_wide(card_w: f64, n: usize) -> f64 {
    card_w - n.clamp(1, SLICES_SHOWN).saturating_sub(1) as f64 * SLICE_NARROW
}

struct SlicesLauncher {
    apps: Vec<DesktopApp>,
    results: Vec<DesktopApp>,
    query: Query,
    selected: usize,
    opened: Instant,
    mouse_active: bool,
    pointer: (f64, f64),
    /// Clickable strips from the last draw, where they will be once settled.
    hits: Vec<(usize, Rect)>,
    wheel: f64,
    levels: Fades<String>,
    /// First strip of the window, and the strip row's offset.
    first: usize,
    slide: SpringValue,
}

impl SlicesLauncher {
    fn new(apps: Vec<DesktopApp>) -> Self {
        let results = apps.clone();
        Self {
            apps,
            results,
            query: Query::default(),
            selected: 0,
            opened: Instant::now(),
            mouse_active: false,
            pointer: (-1.0, -1.0),
            hits: Vec::new(),
            wheel: 0.0,
            levels: Fades::default(),
            first: 0,
            slide: SpringValue::new(0.0),
        }
    }

    fn apply_filter(&mut self) {
        self.results = search_apps(&self.apps, &self.query.text);
        self.selected = 0;
        self.first = 0;
        self.slide.snap(0.0);
    }

    fn launch(&self, i: usize) -> Effect {
        if let Some(app) = self.results.get(i) {
            match app.launch() {
                Ok(child) => crate::reap(child),
                Err(e) => eprintln!("[prism-ui] cannot launch {}: {}", app.name, e),
            }
            return Effect::Close;
        }
        Effect::None
    }

    fn select(&mut self, i: usize) {
        if !self.results.is_empty() {
            self.selected = i.min(self.results.len() - 1);
        }
    }

    fn step(&mut self, delta: i32) {
        self.select((self.selected as i32 + delta).max(0) as usize);
    }

    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }
}

fn faded(c: Rgba, alpha: f64) -> Rgba {
    (c.0, c.1, c.2, alpha)
}

fn centred_text(cr: *mut Cairo, r: Rect, text: &str, font: &str, color: Rgba) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 4000, Some(1));
    cb::draw_pango_text(cr, r.x + (r.w - w as f64) / 2.0, r.y + (r.h - h as f64) / 2.0, text, font, color, None);
}

/// An app's icon at (x, y), `alpha` visible.
fn icon(cr: *mut Cairo, app: &DesktopApp, x: f64, y: f64, size: f64, alpha: f64) {
    let name = app.icon.as_deref().unwrap_or("");
    if alpha >= 0.999 {
        launcher::draw_icon(cr, name, "application-x-executable", x, y, size);
    } else if alpha > 0.004 {
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, x, y, size, size);
            cb::cairo_clip(cr);
            cb::cairo_push_group(cr);
        }
        launcher::draw_icon(cr, name, "application-x-executable", x, y, size);
        unsafe {
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, alpha);
            cb::cairo_restore(cr);
        }
    }
}

/// One strip; `level` is 0 closed and 1 fully open at width `wide`.
fn draw_slice(cr: *mut Cairo, app: &DesktopApp, r: Rect, level: f64, wide: f64, accent: Rgba, divider: bool) {
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_rectangle(cr, r.x, r.y, r.w, r.h);
        cb::cairo_clip(cr);
    }
    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 0.0, (0.0, 0.0, 0.0, 0.6 * (1.0 - level)));
    cb::fill_rounded_rect_gradient(cr, r.x, r.y, r.w, r.h, 0.0, (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.78 * level), 0.45, 1.0);
    if divider {
        cb::fill_rounded_rect(cr, r.x, r.y, 1.0, r.h, 0.0, faded(INK, 0.16));
    }
    cb::fill_rounded_rect(cr, r.x, r.y + r.h - 4.0, r.w, 4.0, 0.0, faded(accent, level));

    let closed = (1.0 - level * 2.2).clamp(0.0, 1.0);
    if closed > 0.0 {
        let bottom = r.y + r.h - 24.0;
        icon(cr, app, r.x + (SLICE_NARROW - 40.0) / 2.0, bottom - 40.0, 40.0, closed);
        let font = "Inter, Sans Medium 15px";
        let (_, th) = cb::measure_pango_text(cr, "Ag", font, 4000, Some(1));
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_translate(cr, r.x + (SLICE_NARROW - th as f64) / 2.0, bottom - 56.0);
            cb::cairo_rotate(cr, -std::f64::consts::FRAC_PI_2);
        }
        cb::draw_pango_text(cr, 0.0, 0.0, &app.name, font, faded(INK, closed), Some((r.h - 190.0) as i32));
        unsafe { cb::cairo_restore(cr) };
    }
    let open = ((level - 0.45) / 0.55).clamp(0.0, 1.0);
    if open > 0.0 {
        let (x, bottom) = (r.x + 28.0, r.y + r.h - 32.0);
        icon(cr, app, x, bottom - 72.0, 72.0, open);
        let tx = x + 72.0 + 20.0;
        let room = (wide - 28.0 - 72.0 - 20.0 - 28.0).max(40.0) as i32;
        cb::draw_pango_text(cr, tx, bottom - 76.0, &app.name, "Inter, Sans Bold 34px", faded(INK, open), Some(room));
        let sub = app.comment.clone().filter(|c| !c.is_empty()).or_else(|| app.generic_name.clone()).unwrap_or_default();
        cb::draw_pango_text(cr, tx, bottom - 26.0, &sub, "Inter, Sans 15px", faded(INK, 0.85 * open), Some(room));
    }
    unsafe { cb::cairo_restore(cr) };
}

impl CardView for SlicesLauncher {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let n = self.results.len();
        let wide = slice_wide(card.w, n);
        self.first = window_first(self.first, self.selected, n, SLICES_SHOWN);
        let slide_to = self.first as f64 * SLICE_NARROW;
        if (self.slide.target() - slide_to).abs() > 1e-6 {
            self.slide.set(slide_to, SPRING_OPEN, style);
        }
        let slide = self.slide.animated(0.3);

        self.hits.clear();
        for i in self.first..(self.first + SLICES_SHOWN).min(n) {
            let x = card.x + (i - self.first) as f64 * SLICE_NARROW + if i > self.selected { wide - SLICE_NARROW } else { 0.0 };
            self.hits.push((i, Rect { x, y: card.y, w: if i == self.selected { wide } else { SLICE_NARROW }, h: card.h }));
        }

        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, style.radius);
            cb::cairo_clip(cr);
        }
        ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        if let Some(img) = cb::wallpaper_panel_path().and_then(|p| cb::cached_image_cover(&p, card.w.round() as i32, card.h.round() as i32)) {
            let (iw, ih) = cb::image_size(img);
            cb::paint_cached(cr, img, card.x + (card.w - iw) / 2.0, card.y + (card.h - ih) / 2.0);
        }
        if n == 0 {
            cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, 0.0, (0.0, 0.0, 0.0, 0.58));
            centred_text(cr, card, "No apps match", "Inter, Sans Medium 20px", INK);
        }

        let accent = cb::hex_to_rgba(&theme.primary, 1.0);
        let mut x = card.x - slide;
        for i in 0..n {
            let id = self.results[i].id.clone();
            let level = self.levels.settled_level(id, i == self.selected, SPRING_OPEN, style);
            let w = SLICE_NARROW + level * (wide - SLICE_NARROW);
            if x < card.x + card.w && x + w > card.x {
                draw_slice(cr, &self.results[i], Rect { x, y: card.y, w, h: card.h }, level, wide, accent, i > 0);
            }
            x += w;
        }

        // The search field floats over the strips.
        let (fx, fy) = (card.x + 20.0, card.y + 20.0);
        cb::fill_rounded_rect(cr, fx, fy, 340.0, 44.0, 12.0, (0.0, 0.0, 0.0, 0.5));
        launcher::search_icon(cr, fx + 13.0, fy + 13.0, 18.0, faded(INK, 0.8), 2.0);
        launcher::draw_query(cr, &self.query, "Search apps…", fx + 40.0, fy + 10.0, 288.0, 24.0, "Inter, Sans 16px", INK, faded(INK, 0.7));
        if n > SLICES_SHOWN {
            let label = format!("{} / {n}", self.selected + 1);
            let font = "Inter, Sans Medium 13px";
            let (tw, _) = cb::measure_pango_text(cr, &label, font, 4000, Some(1));
            let w = tw as f64 + 24.0;
            cb::fill_rounded_rect(cr, card.x + card.w - 20.0 - w, fy + 6.0, w, 32.0, 10.0, (0.0, 0.0, 0.0, 0.5));
            cb::draw_pango_text(cr, card.x + card.w - 20.0 - w + 12.0, fy + 14.0, &label, font, INK, None);
        }
        unsafe { cb::cairo_restore(cr) };
        ui::card_edge(cr, card.x, card.y, card.w, card.h, style);
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let moved = (x, y) != self.pointer;
        self.pointer = (x, y);
        if moved && !self.mouse_active && self.opened.elapsed() >= MOUSE_READY {
            self.mouse_active = true;
        }
        if !self.mouse_active {
            return false;
        }
        match self.hit(x, y) {
            Some(i) if i != self.selected => {
                self.selected = i;
                true
            }
            _ => false,
        }
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        self.mouse_active = true;
        match self.hit(x, y) {
            Some(i) => self.launch(i),
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.wheel += delta;
        let steps = (self.wheel / WHEEL_STEP).trunc() as i32;
        if steps == 0 {
            return false;
        }
        self.wheel -= steps as f64 * WHEEL_STEP;
        let before = self.selected;
        self.step(steps);
        self.selected != before
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        match special {
            ESC => return Effect::Close,
            ENTER => return self.launch(self.selected),
            HOME if !ctrl => self.select(0),
            END if !ctrl => self.select(self.results.len().saturating_sub(1)),
            LEFT if !ctrl => self.step(-1),
            RIGHT if !ctrl => self.step(1),
            UP => self.step(-1),
            DOWN => self.step(1),
            PGUP => self.step(-(SLICES_SHOWN as i32)),
            PGDN => self.step(SLICES_SHOWN as i32),
            _ => match self.query.key(special, typed, ctrl) {
                Some(true) => self.apply_filter(),
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }

    fn clickable(&self) -> bool {
        self.hit(self.pointer.0, self.pointer.1).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_theme_is_classic_sized() {
        assert_eq!(card_size("classic"), (launcher::CARD_W, launcher::CARD_H));
        assert_eq!(card_size("prose"), (launcher::CARD_W, launcher::CARD_H));
        assert_eq!(card_size("slices"), SLICES_SIZE);
    }

    #[test]
    fn slice_window_follows_the_selection() {
        assert_eq!(window_first(0, 3, 20, 9), 0);
        assert_eq!(window_first(0, 9, 20, 9), 1);
        assert_eq!(window_first(5, 2, 20, 9), 2);
        assert_eq!(window_first(15, 19, 20, 9), 11);
        assert_eq!(window_first(4, 1, 3, 9), 0);
    }

    #[test]
    fn strips_always_fill_the_card() {
        for n in 1..14 {
            let shown = n.min(SLICES_SHOWN);
            let total = slice_wide(1120.0, n) + (shown - 1) as f64 * SLICE_NARROW;
            assert_eq!(total, 1120.0, "{n}");
        }
    }
}
