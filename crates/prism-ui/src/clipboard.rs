//! Clipboard history (port of QML ClipboardManager.qml): a search field and the history list
//! (text up to 10 lines, images as previews). Enter or a click copies the item and closes;
//! Ctrl+Backspace or the bin button clears the history. The list refreshes every second while
//! open, as in QML.

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

pub struct ClipboardView {
    all: Vec<ClipboardItem>,
    results: Vec<usize>,
    /// Height of each result row (measured once per list change).
    heights: Vec<f64>,
    measured_w: f64,
    query: Query,
    selected: usize,
    scroll: f64,
    list: Rect,
    rows: Vec<(usize, Rect)>,
    clear_btn: Rect,
    pointer: (f64, f64),
    last_refresh: Instant,
}

impl ClipboardView {
    pub fn new(items: Vec<ClipboardItem>) -> Self {
        let mut v = Self {
            all: items,
            results: Vec::new(),
            heights: Vec::new(),
            measured_w: 0.0,
            query: Query::default(),
            selected: 0,
            scroll: 0.0,
            list: Rect::default(),
            rows: Vec::new(),
            clear_btn: Rect::default(),
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
        self.scroll = 0.0;
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
        if top < self.scroll {
            self.scroll = top;
        } else if top + h > self.scroll + self.list.h {
            self.scroll = top + h - self.list.h;
        }
    }

    fn copy(&self, i: usize) -> Effect {
        let Some(item) = self.results.get(i).and_then(|&k| self.all.get(k)).cloned() else { return Effect::None };
        std::thread::spawn(move || {
            use std::io::Write;
            use std::process::{Command, Stdio};
            prism_common::clipboard::touch_self_copy_lock();
            if item.is_image {
                let Some(path) = item.image_path.as_deref().map(|p| p.trim_start_matches("file://").to_string()) else { return };
                // Only images from Prism's own cache (as QML).
                if !std::path::Path::new(&path).starts_with(prism_common::clipboard::get_image_cache_dir()) {
                    eprintln!("[prism-ui] clipboard: refusing image outside the cache: {path}");
                    return;
                }
                if let Ok(f) = std::fs::File::open(&path) {
                    let _ = Command::new("wl-copy").args(["-t", "image/png"]).stdin(f).status();
                }
            } else if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(item.content.as_bytes());
                }
                let _ = child.wait();
            }
        });
        Effect::Close
    }

    fn clear(&mut self) -> Effect {
        std::thread::spawn(|| {
            if let Err(e) = prism_common::ipc::request(&prism_common::IpcCommand::Clipboard(prism_common::ipc::ClipboardAction::Clear)) {
                eprintln!("[prism-ui] clipboard clear: {e}");
            }
        });
        self.all.clear();
        self.apply_filter();
        Effect::Close
    }
}

impl CardView for ClipboardView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let fg = style.fg;
        let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
        let sv_text = cb::hex_to_rgba(&theme.on_surface_variant, 1.0);
        cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, style.radius, style.card_bg);
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let y = card.y + PAD;

        // Search field with the clear-history button.
        cb::fill_rounded_rect(cr, x, y, w, 48.0, 12.0, cb::hex_to_rgba(&theme.surface_variant, 1.0));
        cb::stroke_rounded_rect(cr, x + 1.0, y + 1.0, w - 2.0, 46.0, 11.0, cb::hex_to_rgba(&theme.primary, 1.0), 2.0);
        self.clear_btn = Rect { x: x + w - 12.0 - 28.0, y: y + 10.0, w: 28.0, h: 28.0 };
        let hot = self.clear_btn.contains(self.pointer.0, self.pointer.1);
        if hot {
            cb::fill_rounded_rect(cr, self.clear_btn.x, self.clear_btn.y, 28.0, 28.0, 6.0, cb::hex_to_rgba(&theme.error, 1.0));
        }
        let bf = "Inter, Sans 16px";
        let (bw, bh) = cb::measure_pango_text(cr, "🗑", bf, 100, Some(1));
        cb::draw_pango_text(cr, self.clear_btn.x + ((28.0 - bw as f64) / 2.0).round(), self.clear_btn.y + ((28.0 - bh as f64) / 2.0).round(), "🗑", bf, if hot { (1.0, 1.0, 1.0, 1.0) } else { sv_text }, None);
        draw_query(cr, &self.query, "Search Clipboard...", x + 12.0, y + 12.0, self.clear_btn.x - 12.0 - (x + 12.0), 24.0, "Inter, Sans 16px", cb::hex_to_rgba(&theme.on_surface, 1.0), sv_text);

        // List.
        self.list = Rect { x, y: y + 48.0 + 12.0, w, h: card.y + card.h - PAD - (y + 60.0) };
        self.measure(cr, w);
        let total: f64 = self.heights.iter().map(|h| h + GAP).sum::<f64>() - GAP;
        self.scroll = self.scroll.clamp(0.0, (total - self.list.h).max(0.0));
        self.rows.clear();
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, self.list.x, self.list.y, self.list.w, self.list.h);
            cb::cairo_clip(cr);
        }
        let mut ry = self.list.y - self.scroll;
        for (ri, &k) in self.results.iter().enumerate() {
            let h = self.heights[ri];
            if ry > self.list.y + self.list.h {
                break;
            }
            if ry + h >= self.list.y {
                let row = Rect { x, y: ry, w, h };
                self.rows.push((ri, row));
                let on = ri == self.selected;
                if on {
                    cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 12.0, (fg.0, fg.1, fg.2, 0.85));
                }
                let it = &self.all[k];
                if it.is_image {
                    let path = it.image_path.as_deref().unwrap_or("").trim_start_matches("file://");
                    if let Some(img) = cb::cached_image(path, (w - 24.0) as i32, IMAGE_H as i32, true) {
                        let (iw, ih) = cb::image_size(img);
                        cb::paint_cached(cr, img, row.x + 12.0 + ((w - 24.0 - iw) / 2.0).round(), row.y + 12.0 + ((IMAGE_H - ih) / 2.0).round());
                    }
                } else {
                    cb::draw_pango_text_wrapped(cr, row.x + 12.0, row.y + 12.0, &it.content, TEXT_FONT, if on { inv } else { fg }, (w - 24.0) as i32, Some(10));
                }
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
            cb::fill_rounded_rect(cr, tip.x, tip.y, tip.w, tip.h, 4.0, cb::hex_to_rgba(&theme.surface, 1.0));
            cb::stroke_rounded_rect(cr, tip.x + 0.5, tip.y + 0.5, tip.w - 1.0, tip.h - 1.0, 4.0, cb::hex_to_rgba(&theme.outline_variant, 1.0), 1.0);
            cb::draw_pango_text(cr, tip.x + 8.0, tip.y + 4.0, t, tf, cb::hex_to_rgba(&theme.on_surface, 1.0), None);
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let was_hot = self.clear_btn.contains(self.pointer.0, self.pointer.1);
        self.pointer = (x, y);
        let mut redraw = was_hot != self.clear_btn.contains(x, y);
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
        if !self.list.contains(x, y) {
            return Effect::None;
        }
        match self.rows.iter().find(|(_, r)| r.contains(x, y)) {
            Some((i, _)) => self.copy(*i),
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.scroll = (self.scroll + delta).max(0.0);
        true
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
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
        let items = ClipboardStore::load().items;
        let changed = items.len() != self.all.len() || items.first().map(|i| &i.id) != self.all.first().map(|i| &i.id);
        if changed {
            self.all = items;
            self.apply_filter();
        }
        changed
    }

    fn wake_in(&self) -> Option<Duration> {
        Some(REFRESH.saturating_sub(self.last_refresh.elapsed()))
    }

    fn clickable(&self) -> bool {
        self.clear_btn.contains(self.pointer.0, self.pointer.1) || (self.list.contains(self.pointer.0, self.pointer.1) && self.rows.iter().any(|(_, r)| r.contains(self.pointer.0, self.pointer.1)))
    }
}
