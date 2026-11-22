//! App launcher (port of QML AppLauncher.qml), plus the query input both search cards share.
//!
//!   ┌───────────────────────────┬──────────────────┐
//!   │                           │ [🔍 Search apps…] │
//!   │   current wallpaper       │ 12 results        │
//!   │   (576 px, cover-crop)    │ ▣ Firefox  Web…   │
//!   │                           │ ▣ Files           │
//!   └───────────────────────────┴──────────────────┘
//!
//! Hover only selects once the pointer has moved after the card opened (QML mouseActivated),
//! so a pointer resting where the card appears does not steal the keyboard selection.

use std::time::{Duration, Instant};

use prism_common::apps::DesktopApp;
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::svg;
use crate::ui::{Rgba, Style};

pub const CARD_W: f64 = 960.0;
pub const CARD_H: f64 = 620.0;
pub const IMAGE_W: f64 = 576.0;

/// Width of the wallpaper panel on a card of width `card_w`.
pub fn panel_w(card_w: f64) -> f64 {
    IMAGE_W.min(card_w * 0.6)
}
const ROW_H: f64 = 54.0;
const ROW_GAP: f64 = 4.0;
const RADIUS: f64 = 12.0;
/// QML mouseReadyTimer.
pub const MOUSE_READY: Duration = Duration::from_millis(150);

pub const ESC: u32 = 1;
pub const BACKSPACE: u32 = 14;
pub const TAB: u32 = 15;
pub const ENTER: u32 = 28;
pub const HOME: u32 = 102;
pub const UP: u32 = 103;
pub const PGUP: u32 = 104;
pub const LEFT: u32 = 105;
pub const RIGHT: u32 = 106;
pub const END: u32 = 107;
pub const DOWN: u32 = 108;
pub const PGDN: u32 = 109;
pub const DELETE: u32 = 111;

pub enum Effect {
    None,
    Redraw,
    Close,
}

// ── Query input ──────────────────────────────────────────────────────────

/// Single-line text input with a caret (char index).
#[derive(Default, Clone)]
pub struct Query {
    pub text: String,
    caret: usize,
}

impl Query {
    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(self.text.len())
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// `text` with the caret after it.
    pub fn with_text(text: &str) -> Self {
        Query { text: text.to_string(), caret: text.chars().count() }
    }

    /// Apply an editing key. Returns Some(text_changed) when the key was an editing key.
    pub fn key(&mut self, special: u32, typed: Option<char>, ctrl: bool) -> Option<bool> {
        match (special, typed) {
            (_, Some(c)) if !ctrl && !c.is_control() => {
                let b = self.byte(self.caret);
                self.text.insert(b, c);
                self.caret += 1;
                Some(true)
            }
            (BACKSPACE, _) if self.caret > 0 => {
                let start = if ctrl { self.word_start() } else { self.caret - 1 };
                let (a, b) = (self.byte(start), self.byte(self.caret));
                self.text.replace_range(a..b, "");
                self.caret = start;
                Some(true)
            }
            (DELETE, _) if self.caret < self.len() => {
                let (a, b) = (self.byte(self.caret), self.byte(self.caret + 1));
                self.text.replace_range(a..b, "");
                Some(true)
            }
            (BACKSPACE, _) | (DELETE, _) => Some(false),
            (LEFT, _) => {
                self.caret = if ctrl { self.word_start() } else { self.caret.saturating_sub(1) };
                Some(false)
            }
            (RIGHT, _) => {
                self.caret = if ctrl { self.word_end() } else { (self.caret + 1).min(self.len()) };
                Some(false)
            }
            (HOME, _) => {
                self.caret = 0;
                Some(false)
            }
            (END, _) => {
                self.caret = self.len();
                Some(false)
            }
            // Paste: one line (line breaks and tabs become spaces).
            (_, Some('v' | 'V')) if ctrl => {
                let pasted = prism_common::wlclip::paste_text().unwrap_or_default();
                let line = pasted.split(['\r', '\n', '\t']).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
                let line = line.trim();
                if line.is_empty() {
                    return Some(false);
                }
                let b = self.byte(self.caret);
                self.text.insert_str(b, line);
                self.caret += line.chars().count();
                Some(true)
            }
            (_, Some('u')) if ctrl => {
                let changed = !self.text.is_empty();
                self.text.clear();
                self.caret = 0;
                Some(changed)
            }
            _ => None,
        }
    }

    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_end(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Caret position (char index).
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Put the caret at char index `ci` (a click in the field).
    pub fn set_caret(&mut self, ci: usize) {
        self.caret = ci.min(self.len());
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }
}

/// QML's search glyph ("M 7 7 m -5 0 a 5 5 0 1 0 10 0 a 5 5 0 1 0 -10 0 M 11 11 L 16 16"), in
/// a `size` box (the path spans 0..16 of a 24 box, drawn at size/16 scale).
pub fn search_icon(cr: *mut Cairo, x: f64, y: f64, size: f64, color: Rgba, stroke: f64) {
    svg::draw_path(
        cr,
        "M 7 7 m -5 0 a 5 5 0 1 0 10 0 a 5 5 0 1 0 -10 0 M 11 11 L 16 16",
        x,
        y,
        size * 24.0 / 16.0,
        color,
        Some(stroke * 16.0 / size),
    );
}

/// Draw the query text (or placeholder) and caret, vertically centred in `h`.
#[allow(clippy::too_many_arguments)]
pub fn draw_query(cr: *mut Cairo, q: &Query, placeholder: &str, x: f64, y: f64, w: f64, h: f64, font: &str, color: Rgba, placeholder_color: Rgba) {
    draw_text_caret(cr, &q.text, Some(q.caret), placeholder, x, y, w, h, font, color, placeholder_color);
}

/// How far text scrolls left so the caret (at `before` px) stays inside `w`.
fn text_shift(before: f64, w: f64) -> f64 {
    (before - (w - 2.0)).max(0.0)
}

/// One line of text (or the placeholder) with a blinking caret at char `caret` (None: no
/// caret), vertically centred in `h` and scrolled so the caret stays visible. `text` may be a
/// masked form of the value (one dot per char).
#[allow(clippy::too_many_arguments)]
pub fn draw_text_caret(cr: *mut Cairo, text: &str, caret: Option<usize>, placeholder: &str, x: f64, y: f64, w: f64, h: f64, font: &str, color: Rgba, placeholder_color: Rgba) {
    let (_, th) = cb::measure_pango_text(cr, "Ag", font, 4000, Some(1));
    let ty = y + ((h - th as f64) / 2.0).round();
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_rectangle(cr, x, y, w + 2.0, h);
        cb::cairo_clip(cr);
    }
    let on = caret.is_some() && crate::ui::caret_on();
    if text.is_empty() {
        cb::draw_pango_text(cr, x, ty, placeholder, font, placeholder_color, Some(w as i32));
        if on {
            cb::fill_rounded_rect(cr, x, ty + 1.0, 1.5, th as f64 - 2.0, 0.0, color);
        }
    } else {
        let ci = caret.unwrap_or(0);
        let before = &text[..text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(text.len())];
        let (bw, _) = cb::measure_pango_text(cr, before, font, 100_000, Some(1));
        let shift = if caret.is_some() { text_shift(bw as f64, w) } else { 0.0 };
        cb::draw_pango_text(cr, x - shift, ty, text, font, color, if caret.is_some() { None } else { Some(w as i32) });
        if on {
            cb::fill_rounded_rect(cr, x - shift + bw as f64, ty + 1.0, 1.5, th as f64 - 2.0, 0.0, color);
        }
    }
    unsafe { cb::cairo_restore(cr) };
}

/// The char index nearest to `px` in text drawn by `draw_text_caret` at `x` (width `w`) with
/// the caret at `caret`: where a click puts the caret.
pub fn text_index_at(text: &str, caret: usize, font: &str, x: f64, w: f64, px: f64) -> usize {
    unsafe {
        let surf = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, 1, 1);
        let cr = cb::cairo_create(surf);
        let width = |s: &str| cb::measure_pango_text(cr, s, font, 100_000, Some(1)).0 as f64;
        let before = &text[..text.char_indices().nth(caret).map(|(b, _)| b).unwrap_or(text.len())];
        let target = px - x + text_shift(width(before), w);
        let mut best = (0, f64::MAX);
        for (ci, b) in text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).enumerate() {
            let d = (width(&text[..b]) - target).abs();
            if d < best.1 {
                best = (ci, d);
            }
        }
        cb::cairo_destroy(cr);
        cb::cairo_surface_destroy(surf);
        best.0
    }
}

/// Themed icon at (x, y) in a `size` box; falls back to `fallback`.
pub fn draw_icon(cr: *mut Cairo, name: &str, fallback: &str, x: f64, y: f64, size: f64) {
    let px = size.round() as u32;
    let path = [name, fallback].iter().filter(|n| !n.is_empty()).find_map(|n| crate::icons::lookup(n, px));
    if let Some(img) = path.and_then(|p| cb::cached_image(&p.to_string_lossy(), px as i32, px as i32, true)) {
        let (w, h) = cb::image_size(img);
        unsafe {
            cb::cairo_set_source_surface(cr, img, x + (size - w) / 2.0, y + (size - h) / 2.0);
            cb::cairo_paint(cr);
        }
    }
}

// ── Launcher ─────────────────────────────────────────────────────────────

pub struct LauncherView {
    apps: Vec<DesktopApp>,
    results: Vec<DesktopApp>,
    query: Query,
    selected: usize,
    scroll: crate::ui::Scroller,
    /// Selection highlight (content y), glides between rows.
    glide: crate::ui::Glide,
    opened: Instant,
    mouse_active: bool,
    pointer: (f64, f64),
    /// Row hit rectangles (index, rect) from the last draw.
    rows: Vec<(usize, Rect)>,
    list: Rect,
}

impl LauncherView {
    pub fn new(apps: Vec<DesktopApp>) -> Self {
        let results = apps.clone();
        Self {
            apps,
            results,
            query: Query::default(),
            selected: 0,
            scroll: crate::ui::Scroller::default(),
            glide: crate::ui::Glide::default(),
            opened: Instant::now(),
            mouse_active: false,
            pointer: (-1.0, -1.0),
            rows: Vec::new(),
            list: Rect::default(),
        }
    }

    fn apply_filter(&mut self) {
        self.results = crate::quick::launcher_results(&self.apps, &self.query.text);
        self.selected = 0;
        self.scroll.reset(0.0);
        self.glide.reset();
    }

    /// Tab: the selected app's name in the field, which lists its actions.
    fn complete(&mut self) -> bool {
        match self.results.get(self.selected).and_then(crate::quick::complete_actions) {
            Some(text) => {
                self.query = Query::with_text(&text);
                true
            }
            None => false,
        }
    }

    fn launch(&self, i: usize) -> Effect {
        if let Some(app) = self.results.get(i) {
            if crate::quick::launch_entry(app, false) {
                return Effect::Close;
            }
            match app.launch() {
                Ok(child) => crate::reap(child),
                Err(e) => eprintln!("[prism-ui] cannot launch {}: {}", app.name, e),
            }
            return Effect::Close;
        }
        Effect::None
    }

    fn move_selection(&mut self, delta: i32) {
        if self.results.is_empty() {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, self.results.len() as i32 - 1) as usize;
        self.scroll.ensure_visible(self.selected as f64 * (ROW_H + ROW_GAP), ROW_H, self.list.h);
    }

    pub fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let fg = style.fg;
        let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
        let sv_text = cb::hex_to_rgba(&theme.on_surface_variant, 1.0);
        let r = style.radius;
        // Card background with the wallpaper filling the left part (card clips its children).
        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, r);
            cb::cairo_clip(cr);
        }
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        cb::draw_wallpaper_panel(cr, card.x, card.y, panel_w(card.w), card.h, 0.0);
        unsafe { cb::cairo_restore(cr) };
        crate::ui::card_edge(cr, card.x, card.y, card.w, card.h, style);

        let x = card.x + IMAGE_W.min(card.w * 0.6) + 20.0;
        let w = card.x + card.w - 20.0 - x;
        let mut y = card.y + 20.0;

        // Search field.
        cb::fill_rounded_rect(cr, x, y, w, 48.0, RADIUS, cb::hex_to_rgba(&theme.surface_variant, 1.0));
        cb::stroke_rounded_rect(cr, x + 1.0, y + 1.0, w - 2.0, 46.0, RADIUS - 1.0, cb::hex_to_rgba(&theme.primary, 1.0), 2.0);
        search_icon(cr, x + 12.0, y + 15.0, 18.0, sv_text, 2.0);
        draw_query(cr, &self.query, "Search apps…", x + 38.0, y + 12.0, w - 50.0, 24.0, "Inter, Sans 17px", cb::hex_to_rgba(&theme.on_surface, 1.0), sv_text);
        y += 48.0 + 12.0;

        if !self.query.text.trim().is_empty() {
            let n = self.results.len();
            let label = format!("{n} result{}", if n == 1 { "" } else { "s" });
            cb::draw_pango_text(cr, x + 4.0, y, &label, "Inter, Sans 12px", sv_text, None);
            y += 16.0 + 12.0;
        }

        self.list = Rect { x, y, w, h: card.y + card.h - 20.0 - y };
        let pitch = ROW_H + ROW_GAP;
        let max = (self.results.len() as f64 * pitch - ROW_GAP - self.list.h).max(0.0);
        self.scroll.set_max(max, style);
        let scroll = self.scroll.pos();
        self.rows.clear();
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, self.list.x, self.list.y, self.list.w, self.list.h);
            cb::cairo_clip(cr);
        }
        let first = (scroll.max(0.0) / pitch).floor() as usize;
        // Selection highlight: one pill that glides between rows (under the rows' content).
        let (hy, hh) = self.glide.to(self.selected as f64 * pitch, ROW_H, style);
        let hy = self.list.y + hy - scroll;
        if !self.results.is_empty() {
            let (a, dy) = crate::ui::stagger(self.selected.saturating_sub(first));
            cb::fill_rounded_rect(cr, x + 2.0, hy + dy + 2.0, w - 4.0, hh - 4.0, RADIUS, (fg.0, fg.1, fg.2, 0.85 * a));
        }
        for i in first..self.results.len() {
            let ry = self.list.y + i as f64 * pitch - scroll;
            if ry > self.list.y + self.list.h {
                break;
            }
            let row = Rect { x, y: ry, w, h: ROW_H };
            self.rows.push((i, row));
            crate::ui::staggered(cr, i - first, row, || {
                let app = &self.results[i];
                let on = crate::ui::coverage(hy, hh, row.y, row.h);
                draw_icon(cr, app.icon.as_deref().unwrap_or(""), "application-x-executable", row.x + 12.0, row.y + 11.0, 32.0);
                let tx = row.x + 12.0 + 32.0 + 12.0;
                let right = row.x + row.w - 12.0;
                let sub = app.generic_name.clone().unwrap_or_default();
                let sub_font = "Inter, Sans 12px";
                let sub_w = if sub.is_empty() { 0.0 } else { (cb::measure_pango_text(cr, &sub, sub_font, 4000, Some(1)).0 as f64).min((right - tx) * 0.45) };
                let title_w = right - tx - if sub_w > 0.0 { sub_w + 12.0 } else { 0.0 };
                let title_font = "Inter, Sans 15px";
                let (_, th) = cb::measure_pango_text(cr, "Ag", title_font, 4000, Some(1));
                cb::draw_pango_text(cr, tx, row.y + ((ROW_H - th as f64) / 2.0).round(), &app.name, title_font, crate::ui::mix(fg, inv, on), Some(title_w as i32));
                if sub_w > 0.0 {
                    let (_, sh) = cb::measure_pango_text(cr, "Ag", sub_font, 4000, Some(1));
                    let c = crate::ui::mix(sv_text, (inv.0, inv.1, inv.2, 0.7), on);
                    cb::draw_pango_text(cr, right - sub_w, row.y + ((ROW_H - sh as f64) / 2.0).round(), &sub, sub_font, c, Some(sub_w.ceil() as i32));
                }
            });
        }
        unsafe { cb::cairo_restore(cr) };
    }

    fn row_at(&self, x: f64, y: f64) -> Option<usize> {
        if !self.list.contains(x, y) {
            return None;
        }
        self.rows.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }

    pub fn over_row(&self) -> bool {
        self.row_at(self.pointer.0, self.pointer.1).is_some()
    }

    pub fn motion(&mut self, x: f64, y: f64) -> bool {
        let moved = (x, y) != self.pointer;
        self.pointer = (x, y);
        if moved && !self.mouse_active && self.opened.elapsed() >= MOUSE_READY {
            self.mouse_active = true;
        }
        if self.mouse_active {
            if let Some(i) = self.row_at(x, y) {
                if i != self.selected {
                    self.selected = i;
                    return true;
                }
            }
        }
        false
    }

    pub fn click(&mut self, x: f64, y: f64) -> Effect {
        self.mouse_active = true;
        match self.row_at(x, y) {
            Some(i) => self.launch(i),
            None => Effect::None,
        }
    }

    pub fn scroll(&mut self, delta: f64) -> bool {
        self.scroll.by(delta)
    }

    pub fn key(&mut self, special: u32, typed: Option<char>, ctrl: bool) -> Effect {
        match special {
            ESC => return Effect::Close,
            ENTER => return self.launch(self.selected),
            DOWN => self.move_selection(1),
            UP => self.move_selection(-1),
            PGDN => self.move_selection(((self.list.h / (ROW_H + ROW_GAP)) as i32).max(1)),
            PGUP => self.move_selection(-((self.list.h / (ROW_H + ROW_GAP)) as i32).max(1)),
            _ => match if special == TAB { self.complete().then_some(true) } else { self.query.key(special, typed, ctrl) } {
                Some(true) => self.apply_filter(),
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_editing() {
        let mut q = Query::default();
        for c in "hello world".chars() {
            q.key(0, Some(c), false);
        }
        assert_eq!(q.key(BACKSPACE, None, true), Some(true));
        assert_eq!(q.text, "hello ");
        q.key(LEFT, None, false);
        q.key(0, Some('X'), false);
        assert_eq!(q.text, "helloX ");
        q.key(HOME, None, false);
        q.key(DELETE, None, false);
        assert_eq!(q.text, "elloX ");
        assert_eq!(q.key(0, Some('u'), true), Some(true));
        assert!(q.text.is_empty());
    }
}

impl crate::view::CardView for LauncherView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        LauncherView::draw(self, cr, card, style, theme)
    }
    fn motion(&mut self, x: f64, y: f64) -> bool {
        LauncherView::motion(self, x, y)
    }
    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        LauncherView::click(self, x, y)
    }
    fn scroll(&mut self, delta: f64) -> bool {
        LauncherView::scroll(self, delta)
    }
    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        LauncherView::key(self, special, typed, ctrl)
    }
    fn clickable(&self) -> bool {
        self.over_row()
    }
}
