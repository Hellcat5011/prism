// ─────────────────────────────────────────────────────────────────────────
// uikit.rs — reusable controls drawn in the global style
//
// Switch, slider, text field, button and a searchable dropdown, styled after
// the QML controls (PillSwitch, StyledComboBox, TextField with a primary@0.1
// pill background). Used by the settings window and desktop edit mode.
// ─────────────────────────────────────────────────────────────────────────

use crate::cairo_backend::*;
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, measure, Align, TextStyle};

pub type Rgba = (f64, f64, f64, f64);

/// Colours the controls need (derived from the global Style + Theme).
#[derive(Clone)]
pub struct Palette {
    pub fg: Rgba,
    pub accent: Rgba,
    pub card: Rgba,
    pub on_accent: Rgba,
    pub outline: Rgba,
    pub error: Rgba,
    pub family: String,
}

fn with_alpha(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, a)
}

/// Linear mix of two colours.
fn mix(a: Rgba, b: Rgba, t: f64) -> Rgba {
    crate::ui::mix(a, b, t)
}

/// PillSwitch.qml: 50x28 track, knob inset 4 px. `on`: knob position 0 (off) .. 1 (on), may
/// overshoot slightly while it springs; `hover`: hover level 0..1 (see `ui::Fades`).
pub fn switch(cr: *mut Cairo, r: Rect, on: f64, hover: f64, p: &Palette) {
    let t = on.clamp(0.0, 1.0);
    let off_track = with_alpha(p.fg, 0.18 + 0.07 * hover);
    fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, mix(off_track, p.accent, t));
    let d = r.h - 8.0;
    // The knob stretches a little while it travels (wider mid-way), like a drop of liquid.
    let stretch = 1.0 + 0.25 * (1.0 - (2.0 * t - 1.0).powi(2));
    let kw = d * stretch;
    let kx = r.x + 4.0 + (r.w - 8.0 - kw) * on.clamp(-0.05, 1.05);
    fill_rounded_rect(cr, kx, r.y + 4.0, kw, d, d / 2.0, mix(p.fg, p.on_accent, t));
}

/// Horizontal slider; `t` in 0..1.
/// `hover`: hover (or drag) level 0..1; the knob grows with it.
pub fn slider(cr: *mut Cairo, r: Rect, t: f64, hover: f64, p: &Palette) {
    let t = t.clamp(0.0, 1.0);
    let cy = r.y + r.h / 2.0;
    fill_rounded_rect(cr, r.x, cy - 3.0, r.w, 6.0, 3.0, with_alpha(p.fg, 0.18));
    fill_rounded_rect(cr, r.x, cy - 3.0, (r.w * t).max(6.0), 6.0, 3.0, p.accent);
    let k = 9.0 + 1.5 * hover;
    fill_rounded_rect(cr, r.x + r.w * t - k, cy - k, 2.0 * k, 2.0 * k, k, p.accent);
    fill_rounded_rect(cr, r.x + r.w * t - k + 3.0, cy - k + 3.0, 2.0 * k - 6.0, 2.0 * k - 6.0, k - 3.0, p.on_accent);
}

/// Slider value from a pointer x position.
pub fn slider_value(r: Rect, x: f64) -> f64 {
    ((x - r.x) / r.w).clamp(0.0, 1.0)
}

/// Single-line text field. `editing` shows the caret and the focus ring.
/// `hover`: hover level 0..1.
pub fn text_field(cr: *mut Cairo, r: Rect, text: &str, placeholder: &str, editing: bool, hover: f64, invalid: bool, p: &Palette) {
    fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, with_alpha(p.accent, if editing { 0.16 } else { 0.10 + 0.03 * hover }));
    let ring = if invalid {
        Some(p.error)
    } else if editing {
        Some(p.accent)
    } else if hover > 0.0 {
        Some(with_alpha(p.accent, 0.5 * hover))
    } else {
        None
    };
    if let Some(c) = ring {
        stroke_rounded_rect(cr, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, r.h / 2.0, c, 1.0);
    }
    let pad = 14.0;
    let (shown, color) = if text.is_empty() && !editing { (placeholder, with_alpha(p.fg, 0.45)) } else { (text, p.fg) };
    let st = TextStyle { family: &p.family, px: 13.0, bold: false, color };
    // Keep the end of long text (where the caret is) visible while editing.
    let (tw, _) = measure(cr, shown, &st);
    let avail = r.w - 2.0 * pad;
    unsafe {
        cairo_save(cr);
        cairo_rectangle(cr, r.x + pad - 2.0, r.y, avail + 4.0, r.h);
        cairo_clip(cr);
    }
    let x = if editing && tw > avail { r.x + pad + avail - tw } else { r.x + pad };
    let width = if editing { tw + 4.0 } else { avail };
    let (bx, _, bw, _) = draw_in_box(cr, shown, &st, x, r.y, width.max(1.0), r.h, Align::Start);
    if editing {
        let cx = if text.is_empty() { r.x + pad } else { bx + bw + 1.0 };
        unsafe {
            cairo_set_source_rgba(cr, p.accent.0, p.accent.1, p.accent.2, 1.0);
            cairo_rectangle(cr, cx, r.y + 8.0, 1.5, r.h - 16.0);
            cairo_fill(cr);
        }
    }
    unsafe { cairo_restore(cr) };
}

#[derive(Clone, Copy, PartialEq)]
pub enum ButtonKind {
    /// Primary-tinted pill (QML Save button: primary@0.2, hover 0.4).
    Primary,
    /// Outlined pill.
    Outline,
    /// Destructive (error tint on hover).
    Danger,
}

/// `hover`: hover level 0..1 (0 while disabled).
pub fn button(cr: *mut Cairo, r: Rect, label: &str, kind: ButtonKind, hover: f64, enabled: bool, p: &Palette) {
    let a = if enabled { 1.0 } else { 0.4 };
    let hover = if enabled { hover.clamp(0.0, 1.0) } else { 0.0 };
    let tint = match kind {
        ButtonKind::Danger => mix(p.fg, p.error, hover),
        ButtonKind::Outline => p.fg,
        ButtonKind::Primary => p.accent,
    };
    let fill = match kind {
        ButtonKind::Primary => 0.20 + 0.20 * hover,
        _ => 0.16 * hover,
    };
    fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, with_alpha(tint, fill * a));
    let border = if kind == ButtonKind::Primary { with_alpha(p.accent, a) } else { with_alpha(p.outline, a) };
    stroke_rounded_rect(cr, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, r.h / 2.0, border, 1.0);
    let st = TextStyle { family: &p.family, px: 13.0, bold: true, color: with_alpha(p.fg, a) };
    draw_in_box(cr, label, &st, r.x + 8.0, r.y, r.w - 16.0, r.h, Align::Center);
}

/// The closed state of a combo box: current label and a chevron.
/// `hover`: hover level 0..1.
pub fn combo(cr: *mut Cairo, r: Rect, label: &str, open: bool, hover: f64, p: &Palette, font: Option<&str>) {
    let h = if open { 1.0 } else { hover.clamp(0.0, 1.0) };
    fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, with_alpha(p.accent, 0.10 + 0.06 * h));
    if h > 0.0 {
        stroke_rounded_rect(cr, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, r.h / 2.0, with_alpha(p.accent, 0.6 * h), 1.0);
    }
    let fam = font.map(|f| format!("{}, {}", f, p.family)).unwrap_or_else(|| p.family.clone());
    draw_in_box(cr, label, &TextStyle { family: &fam, px: 13.0, bold: false, color: p.fg }, r.x + 14.0, r.y, r.w - 40.0, r.h, Align::Start);
    draw_in_box(cr, if open { "\u{25b4}" } else { "\u{25be}" }, &TextStyle { family: &p.family, px: 13.0, bold: false, color: p.fg }, r.x + r.w - 26.0, r.y, 16.0, r.h, Align::Center);
}

// ── Dropdown list ──────────────────────────────────────────────────────────

/// Open dropdown state: scroll position, type-to-filter text and hovered row.
#[derive(Default, Clone)]
pub struct Dropdown {
    pub scroll: f64,
    pub filter: String,
    pub hover: Option<usize>,
    /// Indices into the full option list for rows currently shown (after filtering).
    pub rows: Vec<(usize, Rect)>,
    pub rect: Rect,
}

pub const DROPDOWN_ROW: f64 = 32.0;
const DROPDOWN_MAX_ROWS: usize = 9;

impl Dropdown {
    /// Indices of options matching the filter (case-insensitive substring).
    pub fn visible(&self, options: &[String]) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        options.iter().enumerate().filter(|(_, o)| f.is_empty() || o.to_lowercase().contains(&f)).map(|(i, _)| i).collect()
    }

    /// Draw below (or above, if there is no room) `anchor`, within `bounds`.
    /// `use_font` renders each option in its own font (font pickers).
    pub fn draw(&mut self, cr: *mut Cairo, anchor: Rect, bounds: Rect, options: &[String], selected: Option<usize>, use_font: bool, p: &Palette) {
        let shown = self.visible(options);
        let searchable = options.len() > DROPDOWN_MAX_ROWS;
        let header = if searchable { 40.0 } else { 0.0 };
        let rows_visible = shown.len().clamp(1, DROPDOWN_MAX_ROWS);
        let h = header + rows_visible as f64 * DROPDOWN_ROW + 12.0;
        let w = anchor.w.max(220.0);
        let x = anchor.x.min(bounds.x + bounds.w - w).max(bounds.x);
        let below = anchor.y + anchor.h + 6.0;
        let y = if below + h <= bounds.y + bounds.h { below } else { (anchor.y - h - 6.0).max(bounds.y) };
        self.rect = Rect { x, y, w, h };

        let bg = (p.card.0, p.card.1, p.card.2, 0.98);
        drop_shadow(cr, x, y, w, h, 12.0, 14.0, 4.0, 0.45);
        fill_rounded_rect(cr, x, y, w, h, 12.0, bg);
        stroke_rounded_rect(cr, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 12.0, with_alpha(p.outline, 1.0), 1.0);

        if searchable {
            let f = Rect { x: x + 8.0, y: y + 6.0, w: w - 16.0, h: 30.0 };
            text_field(cr, f, &self.filter, "Type to search…", true, 0.0, false, p);
        }

        let list = Rect { x, y: y + 6.0 + header, w, h: rows_visible as f64 * DROPDOWN_ROW };
        let content_h = shown.len() as f64 * DROPDOWN_ROW;
        self.scroll = self.scroll.clamp(0.0, (content_h - list.h).max(0.0));
        unsafe {
            cairo_save(cr);
            cairo_rectangle(cr, list.x, list.y, list.w, list.h);
            cairo_clip(cr);
        }
        self.rows.clear();
        if shown.is_empty() {
            draw_in_box(cr, "No matches", &TextStyle { family: &p.family, px: 12.0, bold: false, color: with_alpha(p.fg, 0.5) }, list.x + 16.0, list.y, list.w - 32.0, DROPDOWN_ROW, Align::Start);
        }
        for (k, &idx) in shown.iter().enumerate() {
            let ry = list.y + k as f64 * DROPDOWN_ROW - self.scroll;
            if ry + DROPDOWN_ROW < list.y || ry > list.y + list.h {
                continue;
            }
            let r = Rect { x: x + 6.0, y: ry, w: w - 12.0, h: DROPDOWN_ROW };
            let is_sel = selected == Some(idx);
            if is_sel {
                fill_rounded_rect(cr, r.x, r.y + 2.0, r.w, r.h - 4.0, 8.0, with_alpha(p.fg, 0.85));
            } else if self.hover == Some(idx) {
                fill_rounded_rect(cr, r.x, r.y + 2.0, r.w, r.h - 4.0, 8.0, with_alpha(p.fg, 0.15));
            }
            let color = if is_sel { p.card_text_on_fg() } else { p.fg };
            let fam = if use_font { format!("{}, {}", options[idx], p.family) } else { p.family.clone() };
            draw_in_box(cr, &options[idx], &TextStyle { family: &fam, px: 13.0, bold: is_sel, color }, r.x + 10.0, r.y, r.w - 20.0, r.h, Align::Start);
            // Only rows inside the visible list are clickable.
            let clip = Rect { x: r.x, y: r.y.max(list.y), w: r.w, h: (r.y + r.h).min(list.y + list.h) - r.y.max(list.y) };
            if clip.h > 0.0 {
                self.rows.push((idx, clip));
            }
        }
        unsafe { cairo_restore(cr) };
        if content_h > list.h {
            let track_h = list.h;
            let thumb_h = (track_h * list.h / content_h).max(24.0);
            let ty = list.y + (track_h - thumb_h) * (self.scroll / (content_h - list.h));
            fill_rounded_rect(cr, x + w - 7.0, ty, 3.0, thumb_h, 1.5, with_alpha(p.fg, 0.35));
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        self.rect.contains(x, y)
    }

    /// Option index under the pointer.
    pub fn pick(&self, x: f64, y: f64) -> Option<usize> {
        self.rows.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }

    pub fn scroll_by(&mut self, delta: f64) {
        self.scroll = (self.scroll + delta).max(0.0);
    }

    /// The option Enter would pick: the highlighted one if still shown, else the first shown.
    pub fn current(&self, options: &[String]) -> Option<usize> {
        let shown = self.visible(options);
        self.hover.filter(|h| shown.contains(h)).or_else(|| shown.first().copied())
    }

    /// Move the highlight `delta` rows (keyboard) and keep it in view.
    pub fn step(&mut self, options: &[String], delta: i32) {
        let shown = self.visible(options);
        if shown.is_empty() {
            return;
        }
        let k = match self.hover.and_then(|h| shown.iter().position(|&i| i == h)) {
            Some(p) => (p as i32 + delta).clamp(0, shown.len() as i32 - 1) as usize,
            None => 0,
        };
        self.hover = Some(shown[k]);
        let list_h = shown.len().min(DROPDOWN_MAX_ROWS) as f64 * DROPDOWN_ROW;
        let top = k as f64 * DROPDOWN_ROW;
        if top < self.scroll {
            self.scroll = top;
        } else if top + DROPDOWN_ROW > self.scroll + list_h {
            self.scroll = top + DROPDOWN_ROW - list_h;
        }
    }

    /// Typing filters the list (searchable dropdowns); returns true if handled.
    pub fn key(&mut self, typed: Option<char>, backspace: bool) -> bool {
        if backspace {
            self.filter.pop();
        } else if let Some(c) = typed {
            self.filter.push(c);
        } else {
            return false;
        }
        self.scroll = 0.0;
        true
    }
}

impl Palette {
    /// Text colour on a selected (fg-filled) row: the card colour, opaque.
    pub fn card_text_on_fg(&self) -> Rgba {
        (self.card.0, self.card.1, self.card.2, 1.0)
    }
}

// ── Fonts ──────────────────────────────────────────────────────────────────

thread_local! {
    static FONT_FAMILIES: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Installed font families (fontconfig), sorted, cached for the process lifetime.
pub fn font_families() -> Vec<String> {
    FONT_FAMILIES.with(|f| {
        f.borrow_mut()
            .get_or_insert_with(|| {
                let out = std::process::Command::new("fc-list").args([":", "family"]).output();
                let mut v: Vec<String> = out
                    .map(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .filter_map(|l| l.split(',').next().map(|s| s.trim().to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                v.retain(|s| !s.is_empty() && !s.starts_with('.'));
                v.sort_by_key(|s| s.to_lowercase());
                v.dedup();
                v
            })
            .clone()
    })
}
