//! Alternative launcher designs, chosen with `launcher_theme` (Settings > UI). Classic stays in
//! launcher.rs; everything else about the themes is in this file.
//!
//! * Slices: the wallpaper cut into strips, one app each; the selected strip opens wide.
//! * Marquee: the selected name set huge, its neighbours rolling past like film credits.
//! * Prose: every app name in one flowing paragraph that reflows as you type.

use std::collections::HashMap;
use std::time::Instant;

use prism_common::apps::{search_apps, DesktopApp};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{self, Effect, Query, DOWN, END, ENTER, ESC, HOME, LEFT, MOUSE_READY, PGDN, PGUP, RIGHT, UP};
use crate::ui::{self, Fades, Rgba, Scroller, SpringValue, Style};
use crate::view::CardView;

/// Label and config id of every launcher design, for Settings.
pub const THEMES: &[(&str, &str)] = &[("Classic", "classic"), ("Slices", "slices"), ("Marquee", "marquee"), ("Prose", "prose")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Design {
    Slices,
    Marquee,
    Prose,
}

impl Design {
    fn from_id(id: &str) -> Option<Design> {
        match id {
            "slices" => Some(Design::Slices),
            "marquee" => Some(Design::Marquee),
            "prose" => Some(Design::Prose),
            _ => None,
        }
    }

    fn size(self) -> (f64, f64) {
        match self {
            Design::Slices => (1120.0, 600.0),
            Design::Marquee => (1040.0, 360.0),
            Design::Prose => (920.0, 600.0),
        }
    }
}

/// Card size of the launcher design `id`.
pub fn card_size(id: &str) -> (f64, f64) {
    Design::from_id(id).map_or((launcher::CARD_W, launcher::CARD_H), Design::size)
}

/// The launcher view for the design `id`.
pub fn view(id: &str, apps: Vec<DesktopApp>) -> Box<dyn CardView> {
    match Design::from_id(id) {
        Some(design) => Box::new(ThemedLauncher::new(design, apps)),
        None => Box::new(launcher::LauncherView::new(apps)),
    }
}

const SPRING_OPEN: (f64, f64) = (0.42, 0.82);
const SPRING_ROLL: (f64, f64) = (0.36, 0.78);
const SPRING_WORD: (f64, f64) = (0.40, 0.84);
/// Wheel travel (design px) that moves the selection by one app.
const WHEEL_STEP: f64 = 30.0;
const INK: Rgba = (0.97, 0.97, 0.97, 1.0);

const SLICES_SHOWN: usize = 9;
const SLICE_NARROW: f64 = 76.0;

const MARQUEE_FONT: &str = "Inter, Sans Bold 68px";
const MARQUEE_NEAR: f64 = 72.0;
const MARQUEE_STEP: f64 = 36.0;
const MARQUEE_FOOT: f64 = 64.0;
const MARQUEE_PAGE: i32 = 5;

const PROSE_FONT: &str = "Inter, Sans Medium 26px";
const PROSE_PAD: f64 = 44.0;
/// Room left of a word (and between words) for the selected word's icon.
const PROSE_GAP: f64 = 46.0;
const PROSE_PITCH: f64 = 54.0;
const PROSE_WORD_H: f64 = 44.0;
const PROSE_PAGE: i32 = 5;

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

/// Vertical distance of a Marquee row `d` rows from the selected one.
fn roll_offset(d: f64) -> f64 {
    d.signum() * (d.abs().min(1.0) * MARQUEE_NEAR + (d.abs() - 1.0).max(0.0) * MARQUEE_STEP)
}

/// Runs of characters (start, end) of `name` that `query` matched: the first place it appears
/// whole, else its letters in order.
fn matched_runs(name: &str, query: &str) -> Vec<(usize, usize)> {
    let fold = |s: &str| s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect::<Vec<char>>();
    let (name, query) = (fold(name), fold(query.trim()));
    if query.is_empty() || query.len() > name.len() {
        return Vec::new();
    }
    if let Some(at) = name.windows(query.len()).position(|w| w == query) {
        return vec![(at, at + query.len())];
    }
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    for q in query {
        let Some(at) = name[from..].iter().position(|c| *c == q).map(|p| p + from) else { return Vec::new() };
        match runs.last_mut() {
            Some(run) if run.1 == at => run.1 = at + 1,
            _ => runs.push((at, at + 1)),
        }
        from = at + 1;
    }
    runs
}

/// A word of the Prose paragraph.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Slot {
    line: usize,
    x: f64,
    w: f64,
}

/// Sets words of the given widths into lines of `room`, `gap` apart.
fn flow(widths: &[f64], room: f64, gap: f64) -> Vec<Slot> {
    let (mut line, mut x) = (0, 0.0);
    widths
        .iter()
        .map(|&w| {
            let w = w.min(room);
            if x > 0.0 && x + w > room {
                line += 1;
                x = 0.0;
            }
            let slot = Slot { line, x, w };
            x += w + gap;
            slot
        })
        .collect()
}

/// The word on `line` closest to the middle of word `from`; None when there is no such line.
fn nearest_on_line(slots: &[Slot], from: usize, line: usize) -> Option<usize> {
    let mid = slots.get(from).map(|s| s.x + s.w / 2.0)?;
    let dist = |s: &Slot| (s.x + s.w / 2.0 - mid).abs();
    (0..slots.len()).filter(|i| slots[*i].line == line).min_by(|a, b| dist(&slots[*a]).total_cmp(&dist(&slots[*b])))
}

/// A name on the page: where it is heading and how visible it is. Names filtered out stay until
/// they have faded.
struct Word {
    name: String,
    x: SpringValue,
    y: SpringValue,
    alpha: SpringValue,
    w: f64,
}

struct ThemedLauncher {
    design: Design,
    apps: Vec<DesktopApp>,
    results: Vec<DesktopApp>,
    query: Query,
    selected: usize,
    opened: Instant,
    mouse_active: bool,
    pointer: (f64, f64),
    /// Clickable places from the last draw, where they will be once settled.
    hits: Vec<(usize, Rect)>,
    wheel: f64,
    levels: Fades<String>,
    /// Slices: first strip of the window, and the strip row's offset.
    first: usize,
    slide: SpringValue,
    /// Marquee: the row at the centre, and the fade of the selected app's details.
    roll: SpringValue,
    reveal: SpringValue,
    shown: String,
    /// Prose.
    words: HashMap<String, Word>,
    slots: Vec<Slot>,
    reflow: bool,
    measured: HashMap<String, f64>,
    scroll: Scroller,
    page: Rect,
    pill: Option<[SpringValue; 3]>,
}

impl ThemedLauncher {
    fn new(design: Design, apps: Vec<DesktopApp>) -> Self {
        let results = apps.clone();
        Self {
            design,
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
            roll: SpringValue::new(0.0),
            reveal: SpringValue::new(1.0),
            shown: String::new(),
            words: HashMap::new(),
            slots: Vec::new(),
            reflow: true,
            measured: HashMap::new(),
            scroll: Scroller::default(),
            page: Rect::default(),
            pill: None,
        }
    }

    fn apply_filter(&mut self) {
        self.results = search_apps(&self.apps, &self.query.text);
        self.selected = 0;
        self.first = 0;
        self.slide.snap(0.0);
        self.roll.snap(0.0);
        self.scroll.reset(0.0);
        self.reflow = true;
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
        if self.results.is_empty() {
            return;
        }
        self.selected = i.min(self.results.len() - 1);
        if self.design == Design::Prose {
            if let Some(slot) = self.slots.get(self.selected) {
                self.scroll.ensure_visible(slot.line as f64 * PROSE_PITCH, PROSE_WORD_H, self.page.h);
            }
        }
    }

    fn step(&mut self, delta: i32) {
        self.select((self.selected as i32 + delta).max(0) as usize);
    }

    /// Moves the Prose selection `lines` lines up or down, staying under the same spot.
    fn step_lines(&mut self, lines: i32) {
        let Some(slot) = self.slots.get(self.selected) else { return };
        let last = self.slots.last().map_or(0, |s| s.line) as i32;
        let line = (slot.line as i32 + lines).clamp(0, last) as usize;
        if let Some(i) = nearest_on_line(&self.slots, self.selected, line) {
            self.select(i);
        }
    }

    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }

    /// Details of an app: what it is, then what it does.
    fn details(app: &DesktopApp) -> (String, String) {
        (app.generic_name.clone().unwrap_or_default(), app.comment.clone().unwrap_or_default())
    }

    // ── Slices ───────────────────────────────────────────────────────────

    fn draw_slices(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
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

    // ── Marquee ──────────────────────────────────────────────────────────

    fn draw_marquee(&mut self, cr: *mut Cairo, card: Rect, style: &Style) {
        plain_card(cr, card, style);
        let fg = style.fg;
        let n = self.results.len();
        let stage_h = card.h - MARQUEE_FOOT;
        let cy = card.y + stage_h / 2.0;
        let text_x = card.x + 132.0;
        let room = card.w - 132.0 - 320.0;

        if (self.roll.target() - self.selected as f64).abs() > 1e-6 {
            self.roll.set(self.selected as f64, SPRING_ROLL, style);
        }
        let roll = self.roll.animated(0.002);
        let id = self.results.get(self.selected).map(|a| a.id.clone()).unwrap_or_default();
        if id != self.shown {
            self.shown = id;
            self.reveal.snap(0.0);
            self.reveal.set(1.0, SPRING_ROLL, style);
        }
        let reveal = self.reveal.animated(0.004).clamp(0.0, 1.0);
        let (_, big_h) = cb::measure_pango_text(cr, "Ag", MARQUEE_FONT, 4000, Some(1));
        let big_h = big_h as f64;

        self.hits.clear();
        for i in self.selected.saturating_sub(2)..(self.selected + 3).min(n) {
            let d = i as f64 - self.selected as f64;
            let h = if i == self.selected { big_h } else { 32.0 };
            self.hits.push((i, Rect { x: text_x, y: cy + roll_offset(d) - h / 2.0, w: room, h }));
        }

        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, card.x, card.y, card.w, stage_h);
            cb::cairo_clip(cr);
        }
        let from = (roll.floor() as i64 - 3).max(0) as usize;
        let to = ((roll.ceil() as i64 + 4).max(0) as usize).min(n);
        for i in from..to {
            let d = i as f64 - roll;
            let near = d.abs().min(1.0);
            let alpha = (1.0 - 0.58 * near - 0.2 * (d.abs() - 1.0).max(0.0)).clamp(0.0, 1.0);
            if alpha <= 0.01 {
                continue;
            }
            let name = &self.results[i].name;
            let (tw, _) = cb::measure_pango_text(cr, name, MARQUEE_FONT, 100_000, Some(1));
            let fit = (room / tw.max(1) as f64).min(1.0);
            let scale = (1.0 - 0.66 * near) * fit;
            let y = cy + roll_offset(d) - big_h * scale / 2.0;
            let runs = if i == self.selected { matched_runs(name, &self.query.text) } else { Vec::new() };
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_translate(cr, text_x, y);
                cb::cairo_scale(cr, scale, scale);
            }
            let base = if runs.is_empty() { alpha } else { alpha * 0.4 };
            cb::draw_pango_text(cr, 0.0, 0.0, name, MARQUEE_FONT, faded(fg, base), None);
            for (a, b) in runs {
                let chars: Vec<char> = name.chars().collect();
                let left = text_width(cr, &chars[..a]);
                let right = text_width(cr, &chars[..b]);
                unsafe {
                    cb::cairo_save(cr);
                    cb::cairo_rectangle(cr, left, -8.0, right - left, big_h + 16.0);
                    cb::cairo_clip(cr);
                }
                cb::draw_pango_text(cr, 0.0, 0.0, name, MARQUEE_FONT, faded(fg, alpha), None);
                unsafe { cb::cairo_restore(cr) };
            }
            unsafe { cb::cairo_restore(cr) };
        }
        unsafe { cb::cairo_restore(cr) };

        if let Some(app) = self.results.get(self.selected) {
            icon(cr, app, card.x + 40.0, cy - 36.0, 72.0, reveal);
            let (kind, about) = Self::details(app);
            let x = card.x + card.w - 280.0;
            let mut y = cy - 46.0;
            if !kind.is_empty() {
                cb::draw_pango_text(cr, x, y, &kind, "Inter, Sans Bold 15px", faded(fg, reveal), Some(240));
                y += 28.0;
            }
            cb::draw_pango_text_wrapped(cr, x, y, &about, "Inter, Sans 13px", faded(fg, 0.72 * reveal), 240, Some(5));
        } else {
            centred_text(cr, Rect { h: stage_h, ..card }, "No apps match", "Inter, Sans Medium 20px", fg);
        }

        let foot = card.y + card.h - MARQUEE_FOOT;
        cb::fill_rounded_rect(cr, card.x + 40.0, foot, card.w - 80.0, 1.0, 0.0, faded(fg, 0.14));
        launcher::search_icon(cr, card.x + 40.0, foot + 23.0, 18.0, faded(fg, 0.7), 2.0);
        launcher::draw_query(cr, &self.query, "Search apps…", card.x + 70.0, foot + 20.0, card.w - 220.0, 24.0, "Inter, Sans 16px", fg, faded(fg, 0.55));
        count(cr, card.x + card.w - 40.0, foot + 22.0, self.selected, n, fg);
    }

    // ── Prose ────────────────────────────────────────────────────────────

    /// Sets the current results into lines and sends every word to its new place.
    fn reflow_words(&mut self, cr: *mut Cairo, style: &Style) {
        self.reflow = false;
        let widths: Vec<f64> = self
            .results
            .iter()
            .map(|app| {
                *self.measured.entry(app.id.clone()).or_insert_with(|| cb::measure_pango_text(cr, &app.name, PROSE_FONT, 100_000, Some(1)).0 as f64)
            })
            .collect();
        self.slots = flow(&widths, self.page.w, PROSE_GAP);
        for word in self.words.values_mut() {
            word.alpha.set(0.0, SPRING_WORD, style);
        }
        for (app, slot) in self.results.iter().zip(&self.slots) {
            let y = slot.line as f64 * PROSE_PITCH;
            let word = self.words.entry(app.id.clone()).or_insert_with(|| Word {
                name: app.name.clone(),
                x: SpringValue::new(slot.x),
                y: SpringValue::new(y),
                alpha: SpringValue::new(0.0),
                w: slot.w,
            });
            word.w = slot.w;
            word.x.set(slot.x, SPRING_WORD, style);
            word.y.set(y, SPRING_WORD, style);
            word.alpha.set(1.0, SPRING_WORD, style);
        }
        self.pill = None;
    }

    fn draw_prose(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        plain_card(cr, card, style);
        let fg = style.fg;
        let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
        let n = self.results.len();
        let left = card.x + PROSE_PAD;
        let right = card.x + card.w - PROSE_PAD;

        launcher::draw_query(cr, &self.query, "Search apps…", left, card.y + 24.0, card.w - 2.0 * PROSE_PAD - 110.0, 48.0, "Inter, Sans Light 32px", fg, faded(fg, 0.5));
        count(cr, right, card.y + 40.0, self.selected, n, fg);
        cb::fill_rounded_rect(cr, left, card.y + 84.0, right - left, 1.0, 0.0, faded(fg, 0.14));

        self.page = Rect { x: left + PROSE_GAP, y: card.y + 104.0, w: right - left - PROSE_GAP, h: card.h - 104.0 - 62.0 };
        if self.reflow {
            self.reflow_words(cr, style);
        }
        let page = self.page;
        let lines = self.slots.last().map_or(0, |s| s.line + 1) as f64;
        self.scroll.set_max((lines * PROSE_PITCH - (PROSE_PITCH - PROSE_WORD_H) - page.h).max(0.0), style);
        let scroll = self.scroll.pos();

        self.hits.clear();
        for (i, s) in self.slots.iter().enumerate() {
            let y = page.y + s.line as f64 * PROSE_PITCH - scroll;
            if y + PROSE_WORD_H > page.y && y < page.y + page.h {
                self.hits.push((i, Rect { x: page.x + s.x - PROSE_GAP + 8.0, y, w: s.w + PROSE_GAP, h: PROSE_WORD_H }));
            }
        }

        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, left - 4.0, page.y - 2.0, right - left + 8.0, page.h + 4.0);
            cb::cairo_clip(cr);
        }
        if let Some(s) = self.slots.get(self.selected).copied() {
            let to = [s.x - 38.0, s.line as f64 * PROSE_PITCH, s.w + 52.0];
            let pill = self.pill.get_or_insert_with(|| to.map(SpringValue::new));
            for (v, t) in pill.iter_mut().zip(to) {
                if (v.target() - t).abs() > 1e-6 {
                    v.set(t, ui::SPRING_KNOB, style);
                }
            }
            let [px, py, pw] = [pill[0].animated(0.2), pill[1].animated(0.2), pill[2].animated(0.2)];
            cb::fill_rounded_rect(cr, page.x + px, page.y + py - scroll, pw, PROSE_WORD_H, 12.0, faded(fg, 0.88));
        }
        let (_, th) = cb::measure_pango_text(cr, "Ag", PROSE_FONT, 4000, Some(1));
        let text_dy = ((PROSE_WORD_H - th as f64) / 2.0).round();
        let selected_id = self.results.get(self.selected).map(|a| a.id.as_str()).unwrap_or("");
        for (id, word) in &self.words {
            let alpha = word.alpha.animated(0.004).clamp(0.0, 1.0);
            let (x, y) = (page.x + word.x.animated(0.2), page.y + word.y.animated(0.2) - scroll);
            if alpha <= 0.004 || y + PROSE_WORD_H < page.y || y > page.y + page.h {
                continue;
            }
            let on = self.levels.level(id.clone(), id == selected_id, style);
            let color = ui::mix(faded(fg, 0.74), inv, on);
            cb::draw_pango_text(cr, x, y + text_dy, &word.name, PROSE_FONT, faded(color, color.3 * alpha), Some(word.w.ceil() as i32 + 1));
        }
        if let (Some(app), Some(s)) = (self.results.get(self.selected), self.slots.get(self.selected)) {
            let on = self.levels.level(app.id.clone(), true, style);
            icon(cr, app, page.x + s.x - 32.0, page.y + s.line as f64 * PROSE_PITCH - scroll + 9.0, 26.0, on);
        }
        unsafe { cb::cairo_restore(cr) };
        self.words.retain(|_, w| w.alpha.target() > 0.0 || !w.alpha.settled(0.004));

        if let Some(app) = self.results.get(self.selected) {
            let (kind, about) = Self::details(app);
            let line = if kind.is_empty() || about.is_empty() { format!("{kind}{about}") } else { format!("{kind}. {about}") };
            cb::draw_pango_text(cr, left, card.y + card.h - 42.0, &line, "Inter, Sans 14px", faded(fg, 0.72), Some((right - left) as i32));
        } else {
            centred_text(cr, Rect { x: card.x, w: card.w, ..page }, "No apps match", "Inter, Sans Medium 20px", fg);
        }
    }
}

/// Card background for the designs that draw straight on it.
fn plain_card(cr: *mut Cairo, card: Rect, style: &Style) {
    ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
    ui::card_edge(cr, card.x, card.y, card.w, card.h, style);
}

fn faded(c: Rgba, alpha: f64) -> Rgba {
    (c.0, c.1, c.2, alpha)
}

fn centred_text(cr: *mut Cairo, r: Rect, text: &str, font: &str, color: Rgba) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 4000, Some(1));
    cb::draw_pango_text(cr, r.x + (r.w - w as f64) / 2.0, r.y + (r.h - h as f64) / 2.0, text, font, color, None);
}

/// "3 / 84", right-aligned at `right`.
fn count(cr: *mut Cairo, right: f64, y: f64, selected: usize, n: usize, fg: Rgba) {
    if n == 0 {
        return;
    }
    let label = format!("{} / {n}", selected + 1);
    let font = "Inter, Sans Medium 13px";
    let (w, _) = cb::measure_pango_text(cr, &label, font, 4000, Some(1));
    cb::draw_pango_text(cr, right - w as f64, y, &label, font, faded(fg, 0.7), None);
}

fn text_width(cr: *mut Cairo, chars: &[char]) -> f64 {
    let text: String = chars.iter().collect();
    cb::measure_pango_text(cr, &text, MARQUEE_FONT, 100_000, Some(1)).0 as f64
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

/// One strip of Slices; `level` is 0 closed and 1 fully open at width `wide`.
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
        let (kind, about) = ThemedLauncher::details(app);
        let sub = if about.is_empty() { kind } else { about };
        cb::draw_pango_text(cr, tx, bottom - 26.0, &sub, "Inter, Sans 15px", faded(INK, 0.85 * open), Some(room));
    }
    unsafe { cb::cairo_restore(cr) };
}

impl CardView for ThemedLauncher {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        match self.design {
            Design::Slices => self.draw_slices(cr, card, style, theme),
            Design::Marquee => self.draw_marquee(cr, card, style),
            Design::Prose => self.draw_prose(cr, card, style, theme),
        }
    }

    /// Hover selects like Classic, except in Marquee, where the rows roll away under the pointer.
    fn motion(&mut self, x: f64, y: f64) -> bool {
        let moved = (x, y) != self.pointer;
        self.pointer = (x, y);
        if moved && !self.mouse_active && self.opened.elapsed() >= MOUSE_READY {
            self.mouse_active = true;
        }
        if !self.mouse_active || self.design == Design::Marquee {
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
            Some(i) if self.design == Design::Marquee && i != self.selected => {
                self.select(i);
                Effect::Redraw
            }
            Some(i) => self.launch(i),
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        if self.design == Design::Prose {
            return self.scroll.by(delta);
        }
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
        let last = self.results.len().saturating_sub(1);
        match special {
            ESC => return Effect::Close,
            ENTER => return self.launch(self.selected),
            HOME if !ctrl => self.select(0),
            END if !ctrl => self.select(last),
            LEFT if !ctrl => self.step(-1),
            RIGHT if !ctrl => self.step(1),
            UP | DOWN if self.design == Design::Prose => self.step_lines(if special == UP { -1 } else { 1 }),
            PGUP | PGDN if self.design == Design::Prose => self.step_lines(if special == PGUP { -PROSE_PAGE } else { PROSE_PAGE }),
            UP => self.step(-1),
            DOWN => self.step(1),
            PGUP | PGDN => {
                let page = if self.design == Design::Slices { SLICES_SHOWN as i32 } else { MARQUEE_PAGE };
                self.step(if special == PGUP { -page } else { page });
            }
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
        assert_eq!(card_size("nonsense"), (launcher::CARD_W, launcher::CARD_H));
        assert_ne!(card_size("marquee"), card_size("classic"));
    }

    #[test]
    fn every_listed_theme_exists() {
        for (_, id) in THEMES.iter().filter(|(_, id)| *id != "classic") {
            assert!(Design::from_id(id).is_some(), "{id}");
        }
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

    #[test]
    fn marquee_rows_keep_their_order() {
        assert_eq!(roll_offset(0.0), 0.0);
        assert!(roll_offset(1.0) < roll_offset(2.0));
        assert_eq!(roll_offset(-2.0), -roll_offset(2.0));
    }

    #[test]
    fn matches_a_word_then_scattered_letters() {
        assert_eq!(matched_runs("Firefox", "FOX"), vec![(4, 7)]);
        assert_eq!(matched_runs("Firefox", "frx"), vec![(0, 1), (2, 3), (6, 7)]);
        assert_eq!(matched_runs("Firefox", "fi x"), Vec::<(usize, usize)>::new());
        assert_eq!(matched_runs("Firefox", "  "), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn words_wrap_inside_the_page() {
        let slots = flow(&[100.0, 100.0, 100.0, 900.0, 50.0], 300.0, 40.0);
        assert_eq!(slots[1], Slot { line: 0, x: 140.0, w: 100.0 });
        assert_eq!(slots[2], Slot { line: 1, x: 0.0, w: 100.0 });
        assert_eq!(slots[3], Slot { line: 2, x: 0.0, w: 300.0 });
        assert_eq!(slots[4].line, 3);
    }

    #[test]
    fn up_and_down_land_on_the_closest_word() {
        let slots = flow(&[100.0, 100.0, 100.0, 40.0, 200.0], 300.0, 40.0);
        assert_eq!(slots[3].line, 1);
        assert_eq!(nearest_on_line(&slots, 1, 1), Some(3));
        assert_eq!(nearest_on_line(&slots, 1, 7), None);
    }
}
