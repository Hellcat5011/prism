// ─────────────────────────────────────────────────────────────────────────
// desktop/text.rs — Pango text with pixel sizes, alignment, ellipsis and
// QML-style "Text.Fit" shrinking, used by the desktop widgets.
// ─────────────────────────────────────────────────────────────────────────

use std::ffi::CString;

use crate::cairo_backend::*;

/// Nerd Font glyph families (the QML used CaskaydiaCove; any installed Nerd Font works).
pub const ICON_FONT: &str = "CaskaydiaCove Nerd Font Mono, MesloLGS Nerd Font Mono, Symbols Nerd Font Mono, Symbols Nerd Font";

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
}

pub struct TextStyle<'a> {
    pub family: &'a str,
    pub px: f64,
    pub bold: bool,
    pub color: (f64, f64, f64, f64),
}

const PANGO_SCALE: f64 = 1024.0;
const PANGO_ELLIPSIZE_END: i32 = 3;

fn make_layout(cr: *mut Cairo, text: &str, st: &TextStyle, max_w: Option<f64>) -> *mut PangoLayout {
    unsafe {
        let layout = pango_cairo_create_layout(cr);
        // Never ask for a weight the first-choice font does not have: Pango would synthesize it by
        // smearing the glyphs (blocky display fonts like Fast Hand then look squashed).
        let bold = st.bold && has_bold_face(st.family.split(',').next().unwrap_or("").trim());
        let spec = format!("{} {} {}px", st.family, if bold { "Bold" } else { "" }, st.px.max(1.0).round() as i32);
        if let Ok(c) = CString::new(spec) {
            let desc = pango_font_description_from_string(c.as_ptr());
            pango_layout_set_font_description(layout, desc);
            pango_font_description_free(desc);
        }
        if let Ok(c) = CString::new(text) {
            pango_layout_set_text(layout, c.as_ptr(), -1);
        }
        if let Some(w) = max_w {
            pango_layout_set_width(layout, (w.max(1.0) * PANGO_SCALE) as i32);
            pango_layout_set_ellipsize(layout, PANGO_ELLIPSIZE_END);
        }
        layout
    }
}

/// Pixel size (w, h) of `text`.
pub fn measure(cr: *mut Cairo, text: &str, st: &TextStyle) -> (f64, f64) {
    unsafe {
        let layout = make_layout(cr, text, st, None);
        let (mut w, mut h) = (0, 0);
        pango_layout_get_pixel_size(layout, &mut w, &mut h);
        g_object_unref(layout as *mut std::ffi::c_void);
        (w as f64, h as f64)
    }
}

/// Draw `text` inside the box, aligned horizontally and vertically centred; ellipsized to
/// the box width. Returns the drawn text's rectangle (x, y, w, h).
pub fn draw_in_box(cr: *mut Cairo, text: &str, st: &TextStyle, bx: f64, by: f64, bw: f64, bh: f64, align: Align) -> (f64, f64, f64, f64) {
    unsafe {
        let layout = make_layout(cr, text, st, Some(bw));
        let (mut w, mut h) = (0, 0);
        pango_layout_get_pixel_size(layout, &mut w, &mut h);
        let (w, h) = (w as f64, h as f64);
        let x = match align {
            Align::Start => bx,
            Align::Center => bx + (bw - w) / 2.0,
            Align::End => bx + bw - w,
        };
        let y = by + (bh - h) / 2.0;
        cairo_set_source_rgba(cr, st.color.0, st.color.1, st.color.2, st.color.3);
        cairo_move_to(cr, x, y);
        pango_cairo_show_layout(cr, layout);
        g_object_unref(layout as *mut std::ffi::c_void);
        (x, y, w, h)
    }
}

/// Largest pixel size <= `st.px` (and >= `min_px`) at which `text` fits the box, like QML
/// `fontSizeMode: Text.Fit`.
pub fn fit_px(cr: *mut Cairo, text: &str, st: &TextStyle, bw: f64, bh: f64, min_px: f64) -> f64 {
    let mut px = st.px;
    for _ in 0..4 {
        let (w, h) = measure(cr, text, &TextStyle { px, ..*st });
        if w <= bw && h <= bh {
            break;
        }
        let f = (bw / w.max(1.0)).min(bh / h.max(1.0));
        px = (px * f * 0.98).floor();
        if px <= min_px {
            return min_px;
        }
    }
    px.max(min_px)
}

impl<'a> Clone for TextStyle<'a> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a> Copy for TextStyle<'a> {}

/// Word-wrapped text: returns its height for `width`, drawing it at `at` when given.
pub fn wrapped(cr: *mut Cairo, text: &str, st: &TextStyle, width: f64, at: Option<(f64, f64)>) -> f64 {
    unsafe {
        let layout = make_layout(cr, text, st, None);
        pango_layout_set_width(layout, (width.max(1.0) * PANGO_SCALE) as i32);
        pango_layout_set_wrap(layout, 0); // PANGO_WRAP_WORD
        let (mut w, mut h) = (0, 0);
        pango_layout_get_pixel_size(layout, &mut w, &mut h);
        if let Some((x, y)) = at {
            cairo_set_source_rgba(cr, st.color.0, st.color.1, st.color.2, st.color.3);
            cairo_move_to(cr, x, y);
            pango_cairo_show_layout(cr, layout);
        }
        g_object_unref(layout as *mut std::ffi::c_void);
        let _ = w;
        h as f64
    }
}

thread_local! {
    static BOLD_FACES: std::cell::RefCell<Option<std::collections::HashMap<String, bool>>> = const { std::cell::RefCell::new(None) };
}

/// Whether fontconfig has a bold (weight >= 600) face for `family`. Unknown families are
/// assumed to have one (fontconfig substitutes another family anyway).
pub fn has_bold_face(family: &str) -> bool {
    let key = family.trim().trim_end_matches(" Medium").trim_end_matches(" Semi-Bold").to_lowercase();
    if key.is_empty() {
        return true;
    }
    BOLD_FACES.with(|m| {
        let mut m = m.borrow_mut();
        let map = m.get_or_insert_with(|| {
            let mut map: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
            if let Ok(o) = std::process::Command::new("fc-list").args([":", "family", "weight"]).output() {
                for line in String::from_utf8_lossy(&o.stdout).lines() {
                    // "Family A,Family B:weight=200"
                    let (fams, rest) = line.split_once(':').unwrap_or((line, ""));
                    // Variable fonts list a range ("weight=[100 900]"): use the maximum.
                    let max_weight = rest
                        .split("weight=")
                        .nth(1)
                        .unwrap_or("")
                        .split(|c: char| !c.is_ascii_digit())
                        .filter_map(|n| n.parse::<u32>().ok())
                        .max()
                        .unwrap_or(80);
                    // fontconfig weights: regular 80, semibold 180, bold 200.
                    let bold = max_weight >= 180;
                    for f in fams.split(',') {
                        let e = map.entry(f.trim().to_lowercase()).or_insert(false);
                        *e |= bold;
                    }
                }
            }
            map
        });
        map.get(&key).copied().unwrap_or(true)
    })
}
