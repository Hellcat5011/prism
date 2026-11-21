//! Clipboard history (port of QML ClipboardManager.qml): a search field and the history list
//! (text up to 10 lines, images as previews). Enter or a click copies the item and closes;
//! Ctrl+Backspace or the bin button clears the history (pinned entries stay). The selected
//! entry has buttons: pin (Ctrl+P; pinned entries are listed first and never expire), copy the
//! text in an image (Ctrl+T, tesseract) and delete (Shift+Delete). The list refreshes every
//! second while open, as in QML.

use std::time::{Duration, Instant};

use prism_common::clipboard::{ClipboardItem, ClipboardStore};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{draw_query, Query, BACKSPACE, DELETE, DOWN, ENTER, ESC, PGDN, PGUP, UP};
use crate::ui::Style;
use crate::view::{CardView, Effect};

pub const CARD_W: f64 = 800.0;
pub const CARD_H: f64 = 600.0;
const PAD: f64 = 16.0;
const GAP: f64 = 8.0;
const IMAGE_H: f64 = 200.0;
const TEXT_FONT: &str = "Inter, Sans 14px";
const REFRESH: Duration = Duration::from_secs(1);
/// Row buttons: size and gap.
const BTN: f64 = 28.0;
const BTN_GAP: f64 = 4.0;

/// A button on the selected row.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Btn {
    Pin,
    Ocr,
    Delete,
}

impl Btn {
    fn tip(self, pinned: bool) -> &'static str {
        match self {
            Btn::Pin if pinned => "Unpin (Ctrl+P)",
            Btn::Pin => "Pin (Ctrl+P)",
            Btn::Ocr => "Copy Text in Image (Ctrl+T)",
            Btn::Delete => "Delete (Shift+Delete)",
        }
    }
}

/// Pinned entries first, otherwise newest first (the store's order).
fn ordered(mut items: Vec<ClipboardItem>) -> Vec<ClipboardItem> {
    items.sort_by_key(|i| !i.pinned);
    items
}

pub struct ClipboardView {
    all: Vec<ClipboardItem>,
    results: Vec<usize>,
    /// Height of each result row (measured once per list change).
    heights: Vec<f64>,
    measured_w: f64,
    query: Query,
    selected: usize,
    scroll: crate::ui::Scroller,
    /// Selection highlight (content y and height), glides between rows.
    glide: crate::ui::Glide,
    /// Hover fade of the clear-history button.
    hot_fade: crate::ui::Fades<()>,
    list: Rect,
    rows: Vec<(usize, Rect)>,
    clear_btn: Rect,
    /// Buttons of the selected row as drawn.
    row_btns: Vec<(Btn, Rect)>,
    btn_fades: crate::ui::Fades<Btn>,
    pointer: (f64, f64),
    last_refresh: Instant,
}

impl ClipboardView {
    pub fn new(items: Vec<ClipboardItem>) -> Self {
        let mut v = Self {
            all: ordered(items),
            results: Vec::new(),
            heights: Vec::new(),
            measured_w: 0.0,
            query: Query::default(),
            selected: 0,
            scroll: crate::ui::Scroller::default(),
            glide: crate::ui::Glide::default(),
            hot_fade: crate::ui::Fades::default(),
            list: Rect::default(),
            rows: Vec::new(),
            clear_btn: Rect::default(),
            row_btns: Vec::new(),
            btn_fades: crate::ui::Fades::default(),
            pointer: (-1.0, -1.0),
            last_refresh: Instant::now(),
        };
        v.apply_filter();
        v
    }

    fn apply_filter(&mut self) {
        let q = self.query.text.trim().to_lowercase();
        self.results = (0..self.all.len()).filter(|&i| q.is_empty() || self.all[i].content.to_lowercase().contains(&q)).collect();
        self.heights.clear();
        self.selected = 0;
        self.scroll.reset(0.0);
        self.glide.reset();
    }

    fn measure(&mut self, cr: *mut Cairo, w: f64) {
        if self.heights.len() == self.results.len() && (self.measured_w - w).abs() < 0.5 {
            return;
        }
        self.measured_w = w;
        self.heights = self
            .results
            .iter()
            .map(|&i| {
                let it = &self.all[i];
                let content = if it.is_image { IMAGE_H } else { cb::measure_pango_text(cr, &it.content, TEXT_FONT, (w - 24.0) as i32, Some(10)).1 as f64 };
                content + 24.0
            })
            .collect();
    }

    fn row_top(&self, i: usize) -> f64 {
        self.heights[..i.min(self.heights.len())].iter().map(|h| h + GAP).sum()
    }

    fn move_selection(&mut self, delta: i32) {
        if self.results.is_empty() {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, self.results.len() as i32 - 1) as usize;
        let top = self.row_top(self.selected);
        let h = self.heights.get(self.selected).copied().unwrap_or(0.0);
        self.scroll.ensure_visible(top, h, self.list.h);
    }

    fn copy(&self, i: usize) -> Effect {
        let Some(item) = self.results.get(i).and_then(|&k| self.all.get(k)).cloned() else { return Effect::None };
        std::thread::spawn(move || {
            if item.is_image {
                let Some(path) = item.image_path.as_deref().map(|p| p.trim_start_matches("file://").to_string()) else { return };
                // Only images from Prism's own cache (as QML).
                if !std::path::Path::new(&path).starts_with(prism_common::clipboard::get_image_cache_dir()) {
                    eprintln!("[prism-ui] clipboard: refusing image outside the cache: {path}");
                    return;
                }
                if let Ok(bytes) = std::fs::read(&path) {
                    prism_common::wlclip::copy(&["image/png"], bytes, false);
                }
            } else {
                prism_common::wlclip::copy_text(&item.content, false);
            }
        });
        Effect::Close
    }

    fn selected_item(&self) -> Option<&ClipboardItem> {
        self.results.get(self.selected).and_then(|&k| self.all.get(k))
    }

    /// Run a row button on the selected entry.
    fn press(&mut self, b: Btn) -> Effect {
        let Some(item) = self.selected_item().cloned() else { return Effect::None };
        use prism_common::ipc::ClipboardAction as A;
        let request = |a: A| {
            std::thread::spawn(move || {
                if let Err(e) = prism_common::ipc::request(&prism_common::IpcCommand::Clipboard(a)) {
                    eprintln!("[prism-ui] clipboard: {e}");
                }
            });
        };
        match b {
            Btn::Pin => {
                request(A::Pin(item.id.clone(), !item.pinned));
                for it in self.all.iter_mut().filter(|i| i.id == item.id) {
                    it.pinned = !item.pinned;
                }
                self.all = ordered(std::mem::take(&mut self.all));
                self.refilter_keeping(&item.id);
            }
            Btn::Delete => {
                request(A::Remove(item.id.clone()));
                self.all.retain(|i| i.id != item.id);
                let keep = self.selected;
                self.refilter_keeping("");
                self.selected = keep.min(self.results.len().saturating_sub(1));
            }
            Btn::Ocr => {
                let Some(path) = item.image_path.as_deref().map(|p| std::path::PathBuf::from(p.trim_start_matches("file://"))) else { return Effect::None };
                crate::capture::copy_text_from(path, prism_common::Config::load().ocr_language, false);
                return Effect::Close;
            }
        }
        Effect::Redraw
    }

    /// Filter again and keep the entry `id` selected (if it is still shown).
    fn refilter_keeping(&mut self, id: &str) {
        let scroll = self.scroll.pos();
        self.apply_filter();
        self.scroll.reset(scroll);
        if let Some(i) = self.results.iter().position(|&k| self.all[k].id == id) {
            self.selected = i;
            let top = self.row_top(i);
            let h = self.heights.get(i).copied().unwrap_or(0.0);
            self.scroll.ensure_visible(top, h, self.list.h);
        }
    }

    fn clear(&mut self) -> Effect {
        std::thread::spawn(|| {
            if let Err(e) = prism_common::ipc::request(&prism_common::IpcCommand::Clipboard(prism_common::ipc::ClipboardAction::Clear)) {
                eprintln!("[prism-ui] clipboard clear: {e}");
            }
        });
        self.all.retain(|i| i.pinned);
        self.apply_filter();
        Effect::Close
    }
}

impl CardView for ClipboardView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let fg = style.fg;
        let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
        let sv_text = cb::hex_to_rgba(&theme.on_surface_variant, 1.0);
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let y = card.y + PAD;

        // Search field with the clear-history button.
        cb::fill_rounded_rect(cr, x, y, w, 48.0, 12.0, cb::hex_to_rgba(&theme.surface_variant, 1.0));
        cb::stroke_rounded_rect(cr, x + 1.0, y + 1.0, w - 2.0, 46.0, 11.0, cb::hex_to_rgba(&theme.primary, 1.0), 2.0);
        self.clear_btn = Rect { x: x + w - 12.0 - 28.0, y: y + 10.0, w: 28.0, h: 28.0 };
        let hot = self.clear_btn.contains(self.pointer.0, self.pointer.1);
        let hot_t = self.hot_fade.level((), hot, style);
        if hot_t > 0.0 {
            cb::fill_rounded_rect(cr, self.clear_btn.x, self.clear_btn.y, 28.0, 28.0, 6.0, cb::hex_to_rgba(&theme.error, hot_t));
        }
        let bf = "Inter, Sans 16px";
        let (bw, bh) = cb::measure_pango_text(cr, "🗑", bf, 100, Some(1));
        cb::draw_pango_text(cr, self.clear_btn.x + ((28.0 - bw as f64) / 2.0).round(), self.clear_btn.y + ((28.0 - bh as f64) / 2.0).round(), "🗑", bf, crate::ui::mix(sv_text, (1.0, 1.0, 1.0, 1.0), hot_t), None);
        draw_query(cr, &self.query, "Search Clipboard...", x + 12.0, y + 12.0, self.clear_btn.x - 12.0 - (x + 12.0), 24.0, "Inter, Sans 16px", cb::hex_to_rgba(&theme.on_surface, 1.0), sv_text);

        // List.
        self.list = Rect { x, y: y + 48.0 + 12.0, w, h: card.y + card.h - PAD - (y + 60.0) };
        self.measure(cr, w);
        let total: f64 = self.heights.iter().map(|h| h + GAP).sum::<f64>() - GAP;
        self.scroll.set_max(total - self.list.h, style);
        let scroll = self.scroll.pos();
        self.rows.clear();
        self.row_btns.clear();
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, self.list.x, self.list.y, self.list.w, self.list.h);
            cb::cairo_clip(cr);
        }
        // Selection highlight: one pill gliding (and resizing) between rows.
        let (hy, hh) = if self.results.is_empty() {
            (0.0, 0.0)
        } else {
            let (top, h) = (self.row_top(self.selected), self.heights.get(self.selected).copied().unwrap_or(0.0));
            let (gy, gh) = self.glide.to(top, h, style);
            let (a, dy) = crate::ui::stagger(0);
            cb::fill_rounded_rect(cr, x, self.list.y + gy - scroll + dy, w, gh, 12.0, (fg.0, fg.1, fg.2, 0.85 * a));
            (self.list.y + gy - scroll, gh)
        };
        let mut ry = self.list.y - scroll;
        let mut vis = 0usize;
        for (ri, &k) in self.results.iter().enumerate() {
            let h = self.heights[ri];
            if ry > self.list.y + self.list.h {
                break;
            }
            if ry + h >= self.list.y {
                let row = Rect { x, y: ry, w, h };
                self.rows.push((ri, row));
                let on = crate::ui::coverage(hy, hh, row.y, row.h);
                let it = &self.all[k];
                crate::ui::staggered(cr, vis, row, || {
                    if it.is_image {
                        let path = it.image_path.as_deref().unwrap_or("").trim_start_matches("file://");
                        if let Some(img) = cb::cached_image(path, (w - 24.0) as i32, IMAGE_H as i32, true) {
                            let (iw, ih) = cb::image_size(img);
                            cb::paint_cached(cr, img, row.x + 12.0 + ((w - 24.0 - iw) / 2.0).round(), row.y + 12.0 + ((IMAGE_H - ih) / 2.0).round());
                        }
                    } else {
                        cb::draw_pango_text_wrapped(cr, row.x + 12.0, row.y + 12.0, &it.content, TEXT_FONT, crate::ui::mix(fg, inv, on), (w - 24.0) as i32, Some(10));
                    }
                });
                // Buttons on the selected row; a pin mark on other pinned rows.
                let color = crate::ui::mix(fg, inv, on);
                if ri == self.selected {
                    let mut btns = vec![Btn::Pin];
                    if it.is_image {
                        btns.push(Btn::Ocr);
                    }
                    btns.push(Btn::Delete);
                    let n = btns.len() as f64;
                    let bar = Rect { x: row.x + row.w - 8.0 - n * BTN - (n - 1.0) * BTN_GAP - 4.0, y: row.y + 6.0, w: n * BTN + (n - 1.0) * BTN_GAP + 8.0, h: BTN + 8.0 };
                    cb::fill_rounded_rect(cr, bar.x, bar.y, bar.w, bar.h, bar.h / 2.0, (inv.0, inv.1, inv.2, 0.9));
                    for (bi, b) in btns.into_iter().enumerate() {
                        let r = Rect { x: bar.x + 4.0 + bi as f64 * (BTN + BTN_GAP), y: bar.y + 4.0, w: BTN, h: BTN };
                        let hot = self.btn_fades.level(b, r.contains(self.pointer.0, self.pointer.1), style);
                        if hot > 0.0 {
                            cb::fill_circle(cr, r.x + BTN / 2.0, r.y + BTN / 2.0, BTN / 2.0, (fg.0, fg.1, fg.2, 0.18 * hot));
                        }
                        let (name, on_icon) = match b {
                            Btn::Pin => ("pin", it.pinned),
                            Btn::Ocr => ("scan-text", false),
                            Btn::Delete => ("trash", false),
                        };
                        let icon_c = if b == Btn::Delete { crate::ui::mix(fg, cb::hex_to_rgba(&theme.error, 1.0), hot) } else { fg };
                        crate::svg::icon(cr, name, r.x + 5.0, r.y + 5.0, 18.0, (icon_c.0, icon_c.1, icon_c.2, if b == Btn::Pin && !on_icon { 0.75 } else { 1.0 }), false);
                        if b == Btn::Pin && on_icon {
                            // Pinned: a dot under the pin.
                            cb::fill_circle(cr, r.x + BTN / 2.0, r.y + BTN - 2.0, 1.5, fg);
                        }
                        self.row_btns.push((b, r));
                    }
                } else if it.pinned {
                    crate::svg::icon(cr, "pin", row.x + row.w - 30.0, row.y + 10.0, 16.0, (color.0, color.1, color.2, 0.7), false);
                }
                vis += 1;
            }
            ry += h + GAP;
        }
        unsafe { cb::cairo_restore(cr) };

        if hot {
            // Tooltip left of the button.
            let tf = "Inter, Sans 12px";
            let t = "Clear History (Ctrl+Backspace)";
            let (tw, th) = cb::measure_pango_text(cr, t, tf, 1000, Some(1));
            let tip = Rect { x: self.clear_btn.x - 8.0 - tw as f64 - 16.0, y: self.clear_btn.y + 14.0 - (th as f64 + 8.0) / 2.0, w: tw as f64 + 16.0, h: th as f64 + 8.0 };
            cb::drop_shadow(cr, tip.x, tip.y, tip.w, tip.h, 4.0, 8.0, 2.0, 0.4);
            cb::fill_rounded_rect(cr, tip.x, tip.y, tip.w, tip.h, 4.0, cb::hex_to_rgba(&theme.surface, 1.0));
            cb::stroke_rounded_rect(cr, tip.x + 0.5, tip.y + 0.5, tip.w - 1.0, tip.h - 1.0, 4.0, cb::hex_to_rgba(&theme.outline_variant, 1.0), 1.0);
            cb::draw_pango_text(cr, tip.x + 8.0, tip.y + 4.0, t, tf, cb::hex_to_rgba(&theme.on_surface, 1.0), None);
        }
        let pinned = self.selected_item().is_some_and(|i| i.pinned);
        if let Some((b, r)) = self.row_btns.iter().find(|(_, r)| r.contains(self.pointer.0, self.pointer.1)).copied() {
            // Tooltip under the button.
            let tf = "Inter, Sans 12px";
            let t = b.tip(pinned);
            let (tw, th) = cb::measure_pango_text(cr, t, tf, 1000, Some(1));
            let tip = Rect { x: (r.x + BTN / 2.0 - (tw as f64 + 16.0) / 2.0).min(card.x + card.w - 8.0 - tw as f64 - 16.0), y: r.y + BTN + 10.0, w: tw as f64 + 16.0, h: th as f64 + 8.0 };
            cb::drop_shadow(cr, tip.x, tip.y, tip.w, tip.h, 4.0, 8.0, 2.0, 0.4);
            cb::fill_rounded_rect(cr, tip.x, tip.y, tip.w, tip.h, 4.0, cb::hex_to_rgba(&theme.surface, 1.0));
            cb::stroke_rounded_rect(cr, tip.x + 0.5, tip.y + 0.5, tip.w - 1.0, tip.h - 1.0, 4.0, cb::hex_to_rgba(&theme.outline_variant, 1.0), 1.0);
            cb::draw_pango_text(cr, tip.x + 8.0, tip.y + 4.0, t, tf, cb::hex_to_rgba(&theme.on_surface, 1.0), None);
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let was_hot = self.clear_btn.contains(self.pointer.0, self.pointer.1);
        let btn_before = self.row_btns.iter().find(|(_, r)| r.contains(self.pointer.0, self.pointer.1)).map(|(b, _)| *b);
        self.pointer = (x, y);
        let btn_now = self.row_btns.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b);
        let mut redraw = was_hot != self.clear_btn.contains(x, y) || btn_before != btn_now;
        if btn_now.is_some() {
            return redraw;
        }
        if self.list.contains(x, y) {
            if let Some((i, _)) = self.rows.iter().find(|(_, r)| r.contains(x, y)) {
                if *i != self.selected {
                    self.selected = *i;
                    redraw = true;
                }
            }
        }
        redraw
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        if self.clear_btn.contains(x, y) {
            return self.clear();
        }
        if let Some((b, _)) = self.row_btns.iter().find(|(_, r)| r.contains(x, y)).copied() {
            return self.press(b);
        }
        if !self.list.contains(x, y) {
            return Effect::None;
        }
        match self.rows.iter().find(|(_, r)| r.contains(x, y)) {
            Some((i, _)) => self.copy(*i),
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.scroll.by(delta)
    }

    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        match (special, typed) {
            (_, Some('p' | 'P')) if ctrl => return self.press(Btn::Pin),
            (_, Some('t' | 'T')) if ctrl && self.selected_item().is_some_and(|i| i.is_image) => return self.press(Btn::Ocr),
            (DELETE, _) if shift && !ctrl => return self.press(Btn::Delete),
            _ => {}
        }
        match special {
            ESC => return Effect::Close,
            ENTER => return self.copy(self.selected),
            BACKSPACE | DELETE if ctrl => return self.clear(),
            DOWN => self.move_selection(1),
            UP => self.move_selection(-1),
            PGDN => self.move_selection(5),
            PGUP => self.move_selection(-5),
            _ => match self.query.key(special, typed, ctrl) {
                Some(true) => self.apply_filter(),
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }

    /// Live refresh while open (QML liveUpdateTimer): reload when the newest item changed.
    fn poll(&mut self) -> bool {
        if self.last_refresh.elapsed() < REFRESH {
            return false;
        }
        self.last_refresh = Instant::now();
        let items = ordered(ClipboardStore::load().items);
        let same = items.len() == self.all.len() && items.iter().zip(&self.all).all(|(a, b)| a.id == b.id && a.pinned == b.pinned);
        if !same {
            let keep = self.selected_item().map(|i| i.id.clone()).unwrap_or_default();
            self.all = items;
            self.refilter_keeping(&keep);
        }
        !same
    }

    fn wake_in(&self) -> Option<Duration> {
        Some(REFRESH.saturating_sub(self.last_refresh.elapsed()))
    }

    fn clickable(&self) -> bool {
        self.clear_btn.contains(self.pointer.0, self.pointer.1) || self.row_btns.iter().any(|(_, r)| r.contains(self.pointer.0, self.pointer.1)) || (self.list.contains(self.pointer.0, self.pointer.1) && self.rows.iter().any(|(_, r)| r.contains(self.pointer.0, self.pointer.1)))
    }
}
