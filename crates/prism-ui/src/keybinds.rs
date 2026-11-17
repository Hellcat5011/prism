//! Keybinding viewer (port of QML KeybindViewer.qml): the compositor's bindings in two columns
//! grouped by section, split at a section boundary near the middle. The parser script runs on
//! a worker thread; the card shows "Loading keybindings…" until it returns.

use std::sync::mpsc::Receiver;

use prism_common::keybinds::KeybindEntry;
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{DOWN, END, ESC, HOME, PGDN, PGUP, UP};
use crate::ui::Style;
use crate::view::{CardView, Effect};
use crate::waker::Waker;

const ROW_H: f64 = 38.0;
const SPACING: f64 = 4.0;
const HEADER_H: f64 = 20.0;

/// Card size for a screen (QML: min(82 % width, 1400) x 78 % height).
pub fn card_size(sw: f64, sh: f64) -> (f64, f64) {
    ((sw * 0.82).min(1400.0).round(), (sh * 0.78).round())
}

/// Display name of the running compositor.
fn compositor_name() -> &'static str {
    if std::env::var_os("MANGO_INSTANCE_SIGNATURE").is_some() {
        "MangoWM"
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        "Hyprland"
    } else if std::env::var_os("NIRI_SOCKET").is_some() {
        "Niri"
    } else {
        ""
    }
}

/// QML KeybindRow._prettyKey.
fn pretty_key(k: &str) -> String {
    let k = k.trim();
    let low = k.to_lowercase();
    let s = match low.as_str() {
        "return" | "enter" => "↵",
        "print" | "prtsc" => "PrtSc",
        "next" | "page_down" | "pgdn" => "PgDn",
        "prior" | "page_up" | "pgup" => "PgUp",
        "left" => "←",
        "right" => "→",
        "up" => "↑",
        "down" => "↓",
        "mouse:272" | "btn_left" | "lmb" => "LMB",
        "mouse:273" | "btn_right" | "rmb" => "RMB",
        "mouse:274" | "btn_middle" | "mmb" => "MMB",
        "mouse_down" | "scroll down" | "scroll ↓" => "Scroll ↓",
        "mouse_up" | "scroll up" | "scroll ↑" => "Scroll ↑",
        "bracketright" => "]",
        "bracketleft" => "[",
        "space" => "␣",
        "escape" | "esc" => "Esc",
        "delete" | "del" => "Del",
        _ => {
            if let Some(r) = k.strip_prefix("XF86Audio") {
                return format!("🔊 {r}");
            }
            if let Some(r) = k.strip_prefix("XF86MonBrightness") {
                return format!("🔆 {r}");
            }
            return k.to_string();
        }
    };
    s.to_string()
}

/// Split index near the middle that falls on a section boundary (within 8 rows), as QML.
fn split_point(binds: &[KeybindEntry]) -> usize {
    let n = binds.len();
    if n == 0 {
        return 0;
    }
    let mid = n.div_ceil(2);
    for off in 0..mid.min(8) {
        for idx in [mid + off, mid.wrapping_sub(off)] {
            if idx > 0 && idx < n && binds[idx].category != binds[idx - 1].category {
                return idx;
            }
        }
    }
    mid
}

pub struct KeybindsView {
    binds: Option<Vec<KeybindEntry>>,
    rx: Receiver<Vec<KeybindEntry>>,
    scroll: crate::ui::Scroller,
    /// Hover fade of the scrollbar thumb.
    thumb_hover: crate::ui::Fades<()>,
    view_h: f64,
    pointer: (f64, f64),
}

impl KeybindsView {
    pub fn new(waker: Waker) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(prism_common::keybinds::load_keybinds());
            waker.wake();
        });
        Self { binds: None, rx, scroll: crate::ui::Scroller::default(), thumb_hover: crate::ui::Fades::default(), view_h: 0.0, pointer: (-1.0, -1.0) }
    }

    /// Height of one column's content.
    fn column_height(binds: &[KeybindEntry]) -> f64 {
        let mut h = 0.0;
        for (i, b) in binds.iter().enumerate() {
            if i == 0 || b.category != binds[i - 1].category {
                h += if i > 0 { 16.0 } else { 4.0 } + HEADER_H + 6.0 + SPACING;
            }
            h += ROW_H + SPACING;
        }
        h
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_column(&self, cr: *mut Cairo, binds: &[KeybindEntry], x: f64, mut y: f64, w: f64, clip: Rect, style: &Style, primary: crate::ui::Rgba) {
        let fg = style.fg;
        for (i, b) in binds.iter().enumerate() {
            if i == 0 || b.category != binds[i - 1].category {
                y += if i > 0 { 16.0 } else { 4.0 };
                cb::draw_pango_text(cr, x + 8.0, y, &b.category, "Inter, Sans Semi-Bold 14px", primary, Some((w - 16.0) as i32));
                y += HEADER_H + 6.0 + SPACING;
            }
            if y + ROW_H >= clip.y && y <= clip.y + clip.h {
                let row = Rect { x, y, w, h: ROW_H };
                if row.contains(self.pointer.0, self.pointer.1) && clip.contains(self.pointer.0, self.pointer.1) {
                    cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 12.0, (fg.0, fg.1, fg.2, 0.08));
                }
                let mut kx = x + 12.0;
                let keys_w = (w - 24.0) * 0.42;
                let combo = b.formatted_combo();
                for part in combo.split('+').map(str::trim).filter(|p| !p.is_empty()) {
                    let label = pretty_key(part);
                    let f = "monospace Semi-Bold 12px";
                    let (lw, lh) = cb::measure_pango_text(cr, &label, f, 1000, Some(1));
                    let cw = lw as f64 + 18.0;
                    let cy = y + (ROW_H - 26.0) / 2.0;
                    cb::fill_rounded_rect(cr, kx, cy, cw, 26.0, 6.0, (primary.0, primary.1, primary.2, 0.18));
                    cb::stroke_rounded_rect(cr, kx + 0.5, cy + 0.5, cw - 1.0, 25.0, 6.0, (primary.0, primary.1, primary.2, 0.35), 1.0);
                    cb::draw_pango_text(cr, kx + 9.0, cy + ((26.0 - lh as f64) / 2.0).round(), &label, f, fg, None);
                    kx += cw + 6.0;
                }
                let ax = (x + 12.0 + keys_w + 16.0).max(kx + 10.0);
                let af = "Inter, Sans 14px";
                let (_, ah) = cb::measure_pango_text(cr, "Ag", af, 1000, Some(1));
                cb::draw_pango_text(cr, ax, y + ((ROW_H - ah as f64) / 2.0).round(), &b.action, af, (fg.0, fg.1, fg.2, 0.85), Some((x + w - 12.0 - ax).max(10.0) as i32));
            }
            y += ROW_H + SPACING;
        }
    }
}

impl CardView for KeybindsView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let fg = style.fg;
        let primary = cb::hex_to_rgba(&theme.primary, 1.0);
        let outline = cb::hex_to_rgba(&theme.outline_variant, 1.0);
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let x = card.x + 28.0;
        let w = card.w - 56.0;
        let mut y = card.y + 28.0;

        // Header.
        let tf = "Inter, Sans Bold 22px";
        let (tw, th) = cb::measure_pango_text(cr, "⌨  Keybindings", tf, 2000, Some(1));
        cb::draw_pango_text(cr, x, y, "⌨  Keybindings", tf, fg, None);
        let name = compositor_name();
        if !name.is_empty() {
            let cf = "Inter, Sans Semi-Bold 12px";
            let (cw, ch) = cb::measure_pango_text(cr, name, cf, 1000, Some(1));
            let chip = Rect { x: x + tw as f64 + 12.0, y: y + ((th as f64 - ch as f64 - 6.0) / 2.0).round(), w: cw as f64 + 14.0, h: ch as f64 + 6.0 };
            cb::fill_rounded_rect(cr, chip.x, chip.y, chip.w, chip.h, 6.0, (primary.0, primary.1, primary.2, 0.15));
            cb::stroke_rounded_rect(cr, chip.x + 0.5, chip.y + 0.5, chip.w - 1.0, chip.h - 1.0, 6.0, (primary.0, primary.1, primary.2, 0.3), 1.0);
            cb::draw_pango_text(cr, chip.x + 7.0, chip.y + 3.0, name, cf, primary, None);
        }
        let ef = "Inter, Sans 13px";
        let (ew, eh) = cb::measure_pango_text(cr, "ESC to close", ef, 1000, Some(1));
        cb::draw_pango_text(cr, x + w - ew as f64, y + ((th - eh) as f64 / 2.0).round(), "ESC to close", ef, (fg.0, fg.1, fg.2, 0.45), None);
        y += th as f64 + 16.0;
        cb::fill_rounded_rect(cr, x, y, w, 1.0, 0.0, (outline.0, outline.1, outline.2, 0.5));
        y += 1.0 + 12.0;

        let area = Rect { x, y, w, h: card.y + card.h - 28.0 - y };
        cb::fill_rounded_rect(cr, area.x, area.y, area.w, area.h, 12.0, cb::hex_to_rgba(&theme.surface_variant, 0.15));
        let Some(binds) = self.binds.as_ref().filter(|b| !b.is_empty()) else {
            let msg = if self.binds.is_none() { "Loading keybindings…" } else { "No keybindings found" };
            let mf = "Inter, Sans Medium 16px";
            let (mw, mh) = cb::measure_pango_text(cr, msg, mf, 2000, Some(1));
            let mut my = area.y + (area.h - mh as f64) / 2.0;
            if self.binds.is_some() {
                my -= 12.0;
            }
            cb::draw_pango_text(cr, area.x + ((area.w - mw as f64) / 2.0).round(), my.round(), msg, mf, (fg.0, fg.1, fg.2, 0.5), None);
            if self.binds.is_some() {
                let sub = format!("Configured parser for {} returned no bindings.", compositor_name());
                let (sw, _) = cb::measure_pango_text(cr, &sub, "Inter, Sans 12px", 4000, Some(1));
                cb::draw_pango_text(cr, area.x + ((area.w - sw as f64) / 2.0).round(), my + mh as f64 + 8.0, &sub, "Inter, Sans 12px", (fg.0, fg.1, fg.2, 0.3), None);
            }
            return;
        };
        let split = split_point(binds);
        let (left, right) = binds.split_at(split);
        let view = Rect { x: area.x, y: area.y, w: area.w - 10.0, h: area.h };
        self.view_h = view.h;
        let content = Self::column_height(left).max(Self::column_height(right)) + 24.0;
        let max = (content - view.h).max(0.0);
        self.scroll.set_max(max, style);
        let scroll = self.scroll.pos();
        let col_w = (view.w - 1.0 - 24.0) / 2.0;
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, view.x, view.y, view.w, view.h);
            cb::cairo_clip(cr);
        }
        let top = view.y + 12.0 - scroll;
        self.draw_column(cr, left, view.x, top, col_w, view, style, primary);
        let div_h = Self::column_height(left).max(Self::column_height(right));
        cb::fill_rounded_rect(cr, view.x + col_w, top, 1.0, div_h, 0.0, (outline.0, outline.1, outline.2, 0.35));
        self.draw_column(cr, right, view.x + col_w + 1.0 + 24.0, top, col_w, view, style, primary);
        unsafe { cb::cairo_restore(cr) };

        if max > 0.0 {
            let track = Rect { x: area.x + area.w - 8.0, y: area.y + 4.0, w: 4.0, h: area.h - 8.0 };
            cb::fill_rounded_rect(cr, track.x, track.y, track.w, track.h, 2.0, (outline.0, outline.1, outline.2, 0.15));
            let th = (track.h * view.h / content).max(30.0);
            let ty = track.y + (scroll / max).clamp(0.0, 1.0) * (track.h - th);
            let hot = self.thumb_hover.level((), track.contains(self.pointer.0 + 2.0, self.pointer.1), style);
            cb::fill_rounded_rect(cr, track.x, ty, track.w, th, 2.0, (primary.0, primary.1, primary.2, 0.35 + 0.35 * hot));
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        true
    }

    fn click(&mut self, _x: f64, _y: f64, _shift: bool) -> Effect {
        Effect::None
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.scroll.by(delta)
    }

    fn key(&mut self, special: u32, _typed: Option<char>, _shift: bool, _ctrl: bool) -> Effect {
        let page = (self.view_h - 40.0).max(40.0);
        match special {
            ESC => return Effect::Close,
            DOWN => self.scroll.to(self.scroll.target() + ROW_H),
            UP => self.scroll.to(self.scroll.target() - ROW_H),
            PGDN => self.scroll.to(self.scroll.target() + page),
            PGUP => self.scroll.to(self.scroll.target() - page),
            HOME => self.scroll.to(0.0),
            END => self.scroll.to(f64::MAX / 4.0),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    fn poll(&mut self) -> bool {
        match self.rx.try_recv() {
            Ok(b) => {
                self.binds = Some(b);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(cat: &str) -> KeybindEntry {
        KeybindEntry { mods: String::new(), key: "a".into(), action: "x".into(), category: cat.into() }
    }

    #[test]
    fn split_prefers_section_boundary() {
        let binds: Vec<_> = ["A", "A", "A", "B", "B", "B", "B", "B"].iter().map(|c| bind(c)).collect();
        assert_eq!(split_point(&binds), 3);
        assert_eq!(pretty_key(" Return "), "↵");
        assert_eq!(pretty_key("XF86AudioMute"), "🔊 Mute");
    }
}
