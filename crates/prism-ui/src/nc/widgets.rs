//! Control-centre controls, ported from the QML Nc* components (NcButton, NcSwitch, NcTile,
//! NcRefreshButton, NcBatteryIndicator, the footer's FlatSlider). Each draws at a rectangle and
//! the caller records the rectangle as a hit region.

use std::time::Instant;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::svg;
use crate::ui::{out_cubic, Rgba};

/// Theme.radiusSmall.
pub const RADIUS: f64 = 12.0;

pub fn alpha(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, c.3 * a)
}

/// Colours every control uses: fg = onPrimaryContainer, inv = inversePrimary.
#[derive(Clone, Copy)]
pub struct Pal {
    pub fg: Rgba,
    pub inv: Rgba,
    pub error: Rgba,
    pub primary: Rgba,
    pub surface_variant: Rgba,
    pub on_surface: Rgba,
    pub on_surface_variant: Rgba,
}

pub fn font(px: f64, weight: &str) -> String {
    if weight.is_empty() {
        format!("Inter, Sans {px}px")
    } else {
        format!("Inter, Sans {weight} {px}px")
    }
}

pub fn text_size(cr: *mut Cairo, text: &str, font: &str) -> (f64, f64) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 4000, Some(1));
    (w as f64, h as f64)
}

/// Single-line text, vertically centred in `h` at `y`, elided to `max_w`.
#[allow(clippy::too_many_arguments)]
pub fn text_v(cr: *mut Cairo, x: f64, y: f64, h: f64, text: &str, font: &str, color: Rgba, max_w: Option<f64>) -> f64 {
    let (w, th) = text_size(cr, text, font);
    cb::draw_pango_text(cr, x, y + ((h - th) / 2.0).round(), text, font, color, max_w.map(|m| m.max(1.0) as i32));
    max_w.map(|m| w.min(m)).unwrap_or(w)
}

/// A simple wall-clock tween (QML Behavior with OutCubic).
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f64,
    to: f64,
    start: Instant,
    ms: f64,
}

impl Tween {
    pub fn new(v: f64) -> Self {
        Self { from: v, to: v, start: Instant::now(), ms: 0.0 }
    }

    pub fn value(&self) -> f64 {
        if self.ms <= 0.0 {
            return self.to;
        }
        let t = (self.start.elapsed().as_secs_f64() * 1000.0 / self.ms).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * out_cubic(t)
    }

    pub fn target(&self) -> f64 {
        self.to
    }

    /// Animate to `to` over `ms` (0 = jump). No-op when already heading there.
    pub fn set(&mut self, to: f64, ms: f64) {
        if (to - self.to).abs() < 1e-9 {
            return;
        }
        self.from = self.value();
        self.to = to;
        self.start = Instant::now();
        self.ms = ms;
    }

    pub fn running(&self) -> bool {
        self.ms > 0.0 && self.start.elapsed().as_secs_f64() * 1000.0 < self.ms
    }
}

/// NcStateFill-style layers of a button / tile background.
fn state_fill(cr: *mut Cairo, r: Rect, radius: f64, p: &Pal, hovered: bool, active: bool) {
    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, radius, alpha(p.fg, 0.2));
    if hovered && !active {
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, radius, alpha(p.fg, 0.14));
    }
    if active {
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, radius, alpha(p.fg, if hovered { 1.0 } else { 0.85 }));
    }
}

/// NcButton: (height, width) for its content.
pub fn button_size(cr: *mut Cairo, text: &str, icon: &str, compact: bool) -> (f64, f64) {
    let (glyph, fpx, pad) = if compact { (14.0, 12.0, 20.0) } else { (16.0, 13.0, 28.0) };
    let mut w = 0.0;
    if !icon.is_empty() {
        w += glyph;
    }
    if !text.is_empty() {
        if !icon.is_empty() {
            w += 6.0;
        }
        w += text_size(cr, text, &font(fpx, "Medium")).0;
    }
    (if compact { 28.0 } else { 34.0 }, w + pad)
}

#[allow(clippy::too_many_arguments)]
pub fn button(cr: *mut Cairo, r: Rect, text: &str, icon: &str, compact: bool, active: bool, enabled: bool, hovered: bool, p: &Pal) {
    let a = if enabled { 1.0 } else { 0.45 };
    let hovered = hovered && enabled;
    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, 0.2 * a));
    if hovered && !active {
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, 0.14));
    }
    if active {
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, (if hovered { 1.0 } else { 0.85 }) * a));
    }
    let tint = alpha(if active { p.inv } else { p.fg }, a);
    let (glyph, fpx) = if compact { (14.0, 12.0) } else { (16.0, 13.0) };
    let f = font(fpx, "Medium");
    let tw = if text.is_empty() { 0.0 } else { text_size(cr, text, &f).0 };
    let cw = if icon.is_empty() { 0.0 } else { glyph } + if !icon.is_empty() && !text.is_empty() { 6.0 } else { 0.0 } + tw;
    let mut x = r.x + ((r.w - cw) / 2.0).round();
    if !icon.is_empty() {
        svg::icon(cr, icon, x, r.y + (r.h - glyph) / 2.0, glyph, tint, false);
        x += glyph + 6.0;
    }
    if !text.is_empty() {
        text_v(cr, x, r.y, r.h, text, &f, tint, Some(r.w - (x - r.x)));
    }
}

/// NcSwitch (42x24). `knob` is the animated 0..1 position.
pub fn switch(cr: *mut Cairo, x: f64, y: f64, knob: f64, enabled: bool, p: &Pal) {
    let a = if enabled { 1.0 } else { 0.45 };
    let (w, h) = (42.0, 24.0);
    cb::fill_rounded_rect(cr, x, y, w, h, h / 2.0, alpha(p.fg, 0.25 * a));
    if knob > 0.0 {
        cb::fill_rounded_rect(cr, x, y, w, h, h / 2.0, alpha(p.fg, 0.85 * knob * a));
    }
    let kx = x + 3.0 + (w - 18.0 - 6.0) * knob;
    cb::fill_circle(cr, kx + 9.0, y + 12.0, 9.0, alpha(p.fg, a));
    if knob > 0.0 {
        cb::fill_circle(cr, kx + 9.0, y + 12.0, 9.0, alpha(p.inv, knob * a));
    }
}

/// NcRefreshButton (30x30); `angle` in degrees while spinning.
pub fn refresh_button(cr: *mut Cairo, x: f64, y: f64, angle: f64, hovered: bool, p: &Pal) {
    if hovered {
        cb::fill_circle(cr, x + 15.0, y + 15.0, 15.0, alpha(p.fg, 0.2));
    }
    svg::icon_rotated(cr, "refresh", x + 7.0, y + 7.0, 16.0, p.fg, angle);
}

pub struct TileSpec<'a> {
    pub icon: &'a str,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub count: &'a str,
    pub selected: bool,
    pub on: bool,
    pub dimmed: bool,
    pub slashed: bool,
    pub badge: usize,
    pub expanded_w: f64,
}

/// NcTile (44 high, width animated by the caller).
pub fn tile(cr: *mut Cairo, r: Rect, t: &TileSpec, hovered: bool, p: &Pal) {
    let filled = t.selected || t.on;
    unsafe {
        cb::cairo_save(cr);
        cb::rounded_rect_path(cr, r.x, r.y, r.w, r.h, RADIUS);
        cb::cairo_clip(cr);
    }
    state_fill(cr, r, RADIUS, p, hovered, filled);
    let iy = r.y + (r.h - 20.0) / 2.0;
    if filled {
        svg::icon(cr, t.icon, r.x + 12.0, iy, 20.0, p.inv, t.slashed);
        if !t.title.is_empty() && r.w > 60.0 {
            let tw = (t.expanded_w - 52.0).max(10.0);
            let tx = r.x + 40.0;
            let ft = font(13.0, "Bold");
            let fs = font(10.0, "");
            let (_, th) = text_size(cr, "Ag", &ft);
            let sh = if t.subtitle.is_empty() { 0.0 } else { text_size(cr, "Ag", &fs).1 };
            let mut y = r.y + ((r.h - th - sh) / 2.0).round();
            let pill_w = if t.count.is_empty() {
                0.0
            } else {
                (text_size(cr, t.count, &font(11.0, "Bold")).0 + 12.0).max(20.0)
            };
            let title_w = tw - if pill_w > 0.0 { pill_w + 6.0 } else { 0.0 };
            let drawn = text_v(cr, tx, y, th, t.title, &ft, p.inv, Some(title_w));
            if pill_w > 0.0 {
                let px = tx + drawn.min(title_w) + 6.0;
                let py = y + (th - 18.0) / 2.0;
                cb::fill_rounded_rect(cr, px, py, pill_w, 18.0, 9.0, p.inv);
                let cf = font(11.0, "Bold");
                let (cw, _) = text_size(cr, t.count, &cf);
                text_v(cr, px + (pill_w - cw) / 2.0, py, 18.0, t.count, &cf, p.fg, None);
            }
            y += th;
            if !t.subtitle.is_empty() {
                cb::draw_pango_text(cr, tx, y, t.subtitle, &fs, alpha(p.inv, 0.8), Some(tw as i32));
            }
        }
    } else {
        svg::icon(cr, t.icon, r.x + 12.0, iy, 20.0, alpha(p.fg, if t.dimmed { 0.5 } else { 1.0 }), t.slashed);
    }
    unsafe { cb::cairo_restore(cr) };
    if t.badge > 0 && !t.selected {
        let label = if t.badge > 99 { "99+".to_string() } else { t.badge.to_string() };
        let f = font(10.0, "Bold");
        let bw = (text_size(cr, &label, &f).0 + 8.0).max(18.0);
        let bx = r.x + r.w + 4.0 - bw;
        let by = r.y - 4.0;
        cb::fill_rounded_rect(cr, bx, by, bw, 18.0, 9.0, p.error);
        let (lw, _) = text_size(cr, &label, &f);
        text_v(cr, bx + (bw - lw) / 2.0, by, 18.0, &label, &f, (1.0, 1.0, 1.0, 1.0), None);
    }
}

/// The footer's FlatSlider: 6 px track, 14 px knob. `v` in 0..1.
pub fn slider(cr: *mut Cairo, r: Rect, v: f64, p: &Pal) {
    let v = v.clamp(0.0, 1.0);
    let ty = r.y + (r.h - 6.0) / 2.0;
    cb::fill_rounded_rect(cr, r.x, ty, r.w, 6.0, 3.0, alpha(p.fg, 0.3));
    if v > 0.0 {
        cb::fill_rounded_rect(cr, r.x, ty, (r.w * v).max(6.0), 6.0, 3.0, p.fg);
    }
    let kx = r.x + v * (r.w - 14.0);
    cb::fill_circle(cr, kx + 7.0, r.y + r.h / 2.0, 7.0, p.fg);
}

/// Slider value for a pointer x.
pub fn slider_value(r: Rect, px: f64) -> f64 {
    ((px - r.x - 7.0) / (r.w - 14.0).max(1.0)).clamp(0.0, 1.0)
}

/// NcBatteryIndicator; returns its width.
pub fn battery(cr: *mut Cairo, right: f64, y: f64, pct: u8, charging: bool, p: &Pal) -> f64 {
    let label = format!("{pct}%");
    let f = font(12.0, "");
    let (lw, _) = text_size(cr, &label, &f);
    let w = 22.0 + 4.0 + lw;
    let x = right - w;
    let by = y + (32.0 - 14.0) / 2.0 + 1.0;
    let fill = if charging || pct > 25 {
        alpha(p.fg, 0.9)
    } else if pct <= 10 {
        p.error
    } else {
        cb::hex_to_rgba("#f59e0b", 1.0)
    };
    cb::stroke_rounded_rect(cr, x + 0.75, by + 0.75, 16.5, 10.5, 2.0, p.fg, 1.5);
    let fw = (14.0 * pct as f64 / 100.0).max(0.0);
    if fw > 0.0 {
        cb::fill_rounded_rect(cr, x + 2.0, by + 2.0, fw, 8.0, 1.0, fill);
    }
    cb::fill_rounded_rect(cr, x + 18.0, by + 3.0, 3.0, 6.0, 1.0, p.fg);
    if charging {
        svg::draw_path(cr, "M6 0 L2 6 L5 6 L4 12 L8 5 L5.5 5 Z", x + 4.0, by, 24.0, p.fg, None);
    }
    text_v(cr, x + 26.0, y, 32.0, &label, &f, p.fg, None);
    w
}
