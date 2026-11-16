use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

#[repr(C)]
pub struct CairoSurface(c_void);

#[repr(C)]
pub struct Cairo(c_void);

#[repr(C)]
pub struct PangoLayout(c_void);

#[repr(C)]
pub struct PangoFontDescription(c_void);

#[repr(C)]
pub struct GdkPixbuf(c_void);

pub const CAIRO_FORMAT_ARGB32: i32 = 0;

#[link(name = "cairo")]
#[link(name = "pangocairo-1.0")]
#[link(name = "pango-1.0")]
#[link(name = "gobject-2.0")]
#[link(name = "gdk_pixbuf-2.0")]
#[link(name = "gdk-3")]
extern "C" {
    pub fn cairo_image_surface_create_for_data(
        data: *mut u8,
        format: i32,
        width: i32,
        height: i32,
        stride: i32,
    ) -> *mut CairoSurface;
    pub fn cairo_create(target: *mut CairoSurface) -> *mut Cairo;
    pub fn cairo_destroy(cr: *mut Cairo);
    pub fn cairo_surface_destroy(surface: *mut CairoSurface);

    pub fn cairo_set_source_rgba(cr: *mut Cairo, r: f64, g: f64, b: f64, a: f64);
    pub fn cairo_set_operator(cr: *mut Cairo, op: i32);
    pub fn cairo_translate(cr: *mut Cairo, tx: f64, ty: f64);
    pub fn cairo_scale(cr: *mut Cairo, sx: f64, sy: f64);
    pub fn cairo_paint(cr: *mut Cairo);
    pub fn cairo_new_path(cr: *mut Cairo);
    pub fn cairo_new_sub_path(cr: *mut Cairo);
    pub fn cairo_close_path(cr: *mut Cairo);
    pub fn cairo_arc(cr: *mut Cairo, xc: f64, yc: f64, radius: f64, angle1: f64, angle2: f64);
    pub fn cairo_rectangle(cr: *mut Cairo, x: f64, y: f64, width: f64, height: f64);
    pub fn cairo_fill(cr: *mut Cairo);
    pub fn cairo_stroke(cr: *mut Cairo);
    pub fn cairo_set_line_width(cr: *mut Cairo, width: f64);
    pub fn cairo_set_dash(cr: *mut Cairo, dashes: *const f64, num_dashes: i32, offset: f64);
    pub fn cairo_clip(cr: *mut Cairo);
    pub fn cairo_save(cr: *mut Cairo);
    pub fn cairo_restore(cr: *mut Cairo);
    pub fn cairo_move_to(cr: *mut Cairo, x: f64, y: f64);
    pub fn cairo_line_to(cr: *mut Cairo, x: f64, y: f64);

    pub fn pango_cairo_create_layout(cr: *mut Cairo) -> *mut PangoLayout;
    pub fn pango_layout_set_text(layout: *mut PangoLayout, text: *const c_char, length: i32);
    pub fn pango_layout_set_width(layout: *mut PangoLayout, width: i32);
    pub fn pango_layout_set_height(layout: *mut PangoLayout, height: i32);
    pub fn pango_layout_set_wrap(layout: *mut PangoLayout, wrap: i32);
    pub fn pango_layout_set_ellipsize(layout: *mut PangoLayout, ellipsize: i32);
    pub fn pango_layout_get_pixel_size(layout: *mut PangoLayout, width: *mut i32, height: *mut i32);
    pub fn pango_font_description_from_string(str: *const c_char) -> *mut PangoFontDescription;
    pub fn pango_layout_set_font_description(layout: *mut PangoLayout, desc: *mut PangoFontDescription);
    pub fn pango_font_description_free(desc: *mut PangoFontDescription);
    pub fn pango_cairo_show_layout(cr: *mut Cairo, layout: *mut PangoLayout);

    pub fn g_object_unref(object: *mut c_void);
    pub fn gdk_pixbuf_new_from_file_at_scale(
        filename: *const c_char,
        width: i32,
        height: i32,
        preserve_aspect_ratio: i32,
        error: *mut *mut c_void,
    ) -> *mut GdkPixbuf;
    pub fn gdk_cairo_set_source_pixbuf(cr: *mut Cairo, pixbuf: *mut GdkPixbuf, pixbuf_x: f64, pixbuf_y: f64);
    pub fn gdk_pixbuf_get_width(pixbuf: *mut GdkPixbuf) -> i32;
    pub fn gdk_pixbuf_get_height(pixbuf: *mut GdkPixbuf) -> i32;
    pub fn cairo_image_surface_create(format: i32, width: i32, height: i32) -> *mut CairoSurface;
    pub fn cairo_paint_with_alpha(cr: *mut Cairo, alpha: f64);
    pub fn cairo_get_source(cr: *mut Cairo) -> *mut c_void;
    pub fn cairo_pattern_set_filter(pattern: *mut c_void, filter: i32);
    pub fn cairo_pattern_create_linear(x0: f64, y0: f64, x1: f64, y1: f64) -> *mut c_void;
    pub fn cairo_pattern_add_color_stop_rgba(pattern: *mut c_void, offset: f64, r: f64, g: f64, b: f64, a: f64);
    pub fn cairo_set_source(cr: *mut Cairo, pattern: *mut c_void);
    pub fn cairo_pattern_destroy(pattern: *mut c_void);
    pub fn cairo_set_line_cap(cr: *mut Cairo, cap: i32);
    pub fn cairo_set_line_join(cr: *mut Cairo, join: i32);
    pub fn cairo_push_group(cr: *mut Cairo);
    pub fn cairo_pop_group_to_source(cr: *mut Cairo);
    pub fn cairo_curve_to(cr: *mut Cairo, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64);
    pub fn cairo_arc_negative(cr: *mut Cairo, xc: f64, yc: f64, radius: f64, angle1: f64, angle2: f64);
    pub fn cairo_image_surface_get_data(surface: *mut CairoSurface) -> *mut u8;
    pub fn cairo_image_surface_get_stride(surface: *mut CairoSurface) -> i32;
    pub fn cairo_surface_flush(surface: *mut CairoSurface);
    pub fn cairo_surface_mark_dirty(surface: *mut CairoSurface);
    pub fn cairo_rotate(cr: *mut Cairo, angle: f64);
    pub fn cairo_fill_preserve(cr: *mut Cairo);
    pub fn gdk_pixbuf_get_file_info(filename: *const c_char, width: *mut i32, height: *mut i32) -> *mut c_void;
    pub fn gdk_pixbuf_savev(
        pixbuf: *mut GdkPixbuf,
        filename: *const c_char,
        type_: *const c_char,
        option_keys: *const *const c_char,
        option_values: *const *const c_char,
        error: *mut *mut c_void,
    ) -> i32;
    pub fn cairo_clip_preserve(cr: *mut Cairo);
    pub fn gdk_pixbuf_get_pixels(pixbuf: *mut GdkPixbuf) -> *mut u8;
    pub fn gdk_pixbuf_get_rowstride(pixbuf: *mut GdkPixbuf) -> i32;
    pub fn gdk_pixbuf_get_n_channels(pixbuf: *mut GdkPixbuf) -> i32;
    pub fn cairo_set_source_surface(cr: *mut Cairo, surface: *mut CairoSurface, x: f64, y: f64);
    pub fn cairo_image_surface_get_width(surface: *mut CairoSurface) -> i32;
    pub fn cairo_image_surface_get_height(surface: *mut CairoSurface) -> i32;
}


// ─────────────────────────────────────────────────────────────────────────
// Image cache
//
// Icons and wallpaper thumbnails used to be decoded from disk with gdk-pixbuf
// on every draw call, i.e. dozens of synchronous decodes per frame during
// animations. Images are now decoded once into ready-to-paint cairo image
// surfaces keyed by (path, mtime, size, aspect mode). Missing/undecodable files
// are cached as misses too. The cache is bounded and simply cleared when full.
// ─────────────────────────────────────────────────────────────────────────

const IMAGE_CACHE_MAX: usize = 96;

#[derive(Hash, PartialEq, Eq, Clone)]
struct ImageKey {
    path: String,
    mtime: Option<std::time::SystemTime>,
    w: i32,
    h: i32,
    preserve_aspect: bool,
}

struct CachedSurface(*mut CairoSurface);

impl Drop for CachedSurface {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { cairo_surface_destroy(self.0) };
        }
    }
}

thread_local! {
    static IMAGE_CACHE: std::cell::RefCell<std::collections::HashMap<ImageKey, CachedSurface>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static ICON_PATH_CACHE: std::cell::RefCell<std::collections::HashMap<String, Option<PathBuf>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static ICON_CACHE_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Decode `path` scaled to fit (w, h) once, then reuse. Returns a borrowed surface pointer that
/// stays valid until the cache is next cleared (callers paint it immediately), or None.
pub fn cached_image(path: &str, w: i32, h: i32, preserve_aspect: bool) -> Option<*mut CairoSurface> {
    let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    mtime?;
    let key = ImageKey { path: path.to_string(), mtime, w, h, preserve_aspect };

    if let Some(hit) = IMAGE_CACHE.with(|c| c.borrow().get(&key).map(|s| s.0)) {
        return if hit.is_null() { None } else { Some(hit) };
    }

    let surface = unsafe {
        match CString::new(path) {
            Ok(c_path) => {
                let pixbuf = gdk_pixbuf_new_from_file_at_scale(
                    c_path.as_ptr(),
                    w,
                    h,
                    if preserve_aspect { 1 } else { 0 },
                    std::ptr::null_mut(),
                );
                if pixbuf.is_null() {
                    std::ptr::null_mut()
                } else {
                    let pw = gdk_pixbuf_get_width(pixbuf);
                    let ph = gdk_pixbuf_get_height(pixbuf);
                    let img = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, pw.max(1), ph.max(1));
                    let icr = cairo_create(img);
                    gdk_cairo_set_source_pixbuf(icr, pixbuf, 0.0, 0.0);
                    cairo_paint(icr);
                    cairo_destroy(icr);
                    g_object_unref(pixbuf as *mut c_void);
                    img
                }
            }
            Err(_) => std::ptr::null_mut(),
        }
    };

    IMAGE_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= IMAGE_CACHE_MAX {
            c.clear();
        }
        c.insert(key, CachedSurface(surface));
    });
    if surface.is_null() { None } else { Some(surface) }
}

/// Decode `path` scaled to *cover* (w, h) keeping its aspect ratio (QML PreserveAspectCrop);
/// the result is at least as large as the box and is meant to be drawn centred and clipped.
pub fn cached_image_cover(path: &str, w: i32, h: i32) -> Option<*mut CairoSurface> {
    let c = CString::new(path).ok()?;
    let (mut iw, mut ih) = (0i32, 0i32);
    let fmt = unsafe { gdk_pixbuf_get_file_info(c.as_ptr(), &mut iw, &mut ih) };
    if fmt.is_null() || iw <= 0 || ih <= 0 {
        return cached_image(path, w, h, false);
    }
    let s = (w as f64 / iw as f64).max(h as f64 / ih as f64);
    let (dw, dh) = ((iw as f64 * s).ceil() as i32, (ih as f64 * s).ceil() as i32);
    cached_image(path, dw.max(1), dh.max(1), true)
}

/// Draw `path` covering the rectangle (cropped, centred), clipped to a rounded rect.
pub fn draw_image_cover(cr: *mut Cairo, path: &str, x: f64, y: f64, w: f64, h: f64, radius: f64) -> bool {
    let Some(img) = cached_image_cover(path, w.round() as i32, h.round() as i32) else { return false };
    let (iw, ih) = image_size(img);
    unsafe {
        cairo_save(cr);
        rounded_rect_path(cr, x, y, w, h, radius);
        cairo_clip(cr);
        paint_cached(cr, img, x + (w - iw) / 2.0, y + (h - ih) / 2.0);
        cairo_restore(cr);
    }
    true
}

/// Paint a cached image with its top-left corner at (x, y). Returns its (w, h).
pub fn paint_cached(cr: *mut Cairo, img: *mut CairoSurface, x: f64, y: f64) -> (f64, f64) {
    unsafe {
        let iw = cairo_image_surface_get_width(img) as f64;
        let ih = cairo_image_surface_get_height(img) as f64;
        cairo_set_source_surface(cr, img, x, y);
        cairo_paint(cr);
        (iw, ih)
    }
}

pub fn image_size(img: *mut CairoSurface) -> (f64, f64) {
    unsafe { (cairo_image_surface_get_width(img) as f64, cairo_image_surface_get_height(img) as f64) }
}

pub const CAIRO_OPERATOR_CLEAR: i32 = 0;
pub const CAIRO_FILTER_BILINEAR: i32 = 4;

thread_local! {
    static GLOBAL_FONT: std::cell::RefCell<String> = std::cell::RefCell::new(String::from("Inter, Sans"));
}

/// Set the UI font family used by every text draw (from the global `global_font` setting).
pub fn set_global_font(family: &str) {
    GLOBAL_FONT.with(|f| *f.borrow_mut() = family.to_string());
}

/// Font specs in the code use the default family "Inter, Sans"; swap in the configured one.
/// Specs naming another family (icon fonts, monospace) are left untouched.
fn apply_global_font(spec: &str) -> String {
    match spec.strip_prefix("Inter, Sans") {
        Some(rest) => GLOBAL_FONT.with(|f| format!("{}{}", f.borrow(), rest)),
        None => spec.to_string(),
    }
}

pub fn hex_to_rgba(hex: &str, alpha: f64) -> (f64, f64, f64, f64) {
    let clean = hex.trim_start_matches('#');
    if clean.len() >= 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).unwrap_or(0) as f64 / 255.0;
        let g = u8::from_str_radix(&clean[2..4], 16).unwrap_or(0) as f64 / 255.0;
        let b = u8::from_str_radix(&clean[4..6], 16).unwrap_or(0) as f64 / 255.0;
        (r, g, b, alpha)
    } else {
        (1.0, 1.0, 1.0, alpha)
    }
}

pub fn rounded_rect_path(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let deg = std::f64::consts::PI / 180.0;
    unsafe {
        cairo_new_sub_path(cr);
        cairo_arc(cr, x + w - r, y + r, r, -90.0 * deg, 0.0 * deg);
        cairo_arc(cr, x + w - r, y + h - r, r, 0.0 * deg, 90.0 * deg);
        cairo_arc(cr, x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
        cairo_arc(cr, x + r, y + r, r, 180.0 * deg, 270.0 * deg);
        cairo_close_path(cr);
    }
}

pub fn rounded_rect_left_path(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let deg = std::f64::consts::PI / 180.0;
    unsafe {
        cairo_new_sub_path(cr);
        cairo_move_to(cr, x + w, y);
        cairo_line_to(cr, x + w, y + h);
        cairo_arc(cr, x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
        cairo_arc(cr, x + r, y + r, r, 180.0 * deg, 270.0 * deg);
        cairo_close_path(cr);
    }
}

pub fn rounded_rect_right_path(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let deg = std::f64::consts::PI / 180.0;
    unsafe {
        cairo_new_sub_path(cr);
        cairo_arc(cr, x + w - r, y + r, r, -90.0 * deg, 0.0 * deg);
        cairo_arc(cr, x + w - r, y + h - r, r, 0.0 * deg, 90.0 * deg);
        cairo_line_to(cr, x, y + h);
        cairo_line_to(cr, x, y);
        cairo_close_path(cr);
    }
}

pub fn fill_rounded_rect(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64, color: (f64, f64, f64, f64)) {
    unsafe {
        rounded_rect_path(cr, x, y, w, h, r);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        cairo_fill(cr);
    }
}

pub fn stroke_rounded_rect(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    color: (f64, f64, f64, f64),
    line_width: f64,
) {
    unsafe {
        rounded_rect_path(cr, x, y, w, h, r);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        cairo_set_line_width(cr, line_width);
        cairo_stroke(cr);
    }
}

pub fn draw_window_frame(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    bg_color: (f64, f64, f64, f64),
    border_color: (f64, f64, f64, f64),
    border_width: f64,
) {
    fill_rounded_rect(cr, x, y, w, h, r, bg_color);
    if border_width > 0.0 && border_color.3 > 0.0 {
        stroke_rounded_rect(cr, x, y, w, h, r, border_color, border_width);
    }
}

pub fn draw_scrollbar_thumb(
    cr: *mut Cairo,
    track_x: f64,
    track_y: f64,
    track_h: f64,
    total_content_h: f64,
    scroll_offset: f64,
    thumb_w: f64,
    color: (f64, f64, f64, f64),
) {
    if total_content_h <= track_h || track_h <= 0.0 {
        return;
    }
    let min_thumb = 24.0f64.min(track_h);
    let thumb_h = (track_h / total_content_h * track_h).clamp(min_thumb, track_h);
    let max_scroll = (total_content_h - track_h).max(1.0);
    let progress = (scroll_offset / max_scroll).clamp(0.0, 1.0);
    let thumb_y = track_y + progress * (track_h - thumb_h);
    fill_rounded_rect(cr, track_x, thumb_y, thumb_w, thumb_h, thumb_w / 2.0, color);
}

pub fn draw_pango_text(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    text: &str,
    font_spec: &str,
    color: (f64, f64, f64, f64),
    max_width: Option<i32>,
) {
    unsafe {
        let layout = pango_cairo_create_layout(cr);
        if let Ok(c_text) = CString::new(text) {
            pango_layout_set_text(layout, c_text.as_ptr(), -1);
        }
        if let Ok(c_font) = CString::new(apply_global_font(font_spec)) {
            let desc = pango_font_description_from_string(c_font.as_ptr());
            pango_layout_set_font_description(layout, desc);
            pango_font_description_free(desc);
        }
        if let Some(mw) = max_width {
            // Pango width in PANGO_SCALE (1024)
            pango_layout_set_width(layout, mw * 1024);
            pango_layout_set_ellipsize(layout, 3); // PANGO_ELLIPSIZE_END
        }

        cairo_save(cr);
        cairo_move_to(cr, x, y);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        pango_cairo_show_layout(cr, layout);
        cairo_restore(cr);

        g_object_unref(layout as *mut c_void);
    }
}

pub fn draw_pango_text_wrapped(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    text: &str,
    font_spec: &str,
    color: (f64, f64, f64, f64),
    max_width: i32,
    max_lines: Option<i32>,
) -> (i32, i32) {
    unsafe {
        let layout = pango_cairo_create_layout(cr);
        if let Ok(c_text) = CString::new(text) {
            pango_layout_set_text(layout, c_text.as_ptr(), -1);
        }
        if let Ok(c_font) = CString::new(apply_global_font(font_spec)) {
            let desc = pango_font_description_from_string(c_font.as_ptr());
            pango_layout_set_font_description(layout, desc);
            pango_font_description_free(desc);
        }
        pango_layout_set_width(layout, max_width * 1024);
        pango_layout_set_wrap(layout, 0); // PANGO_WRAP_WORD
        if let Some(lines) = max_lines {
            pango_layout_set_ellipsize(layout, 3); // PANGO_ELLIPSIZE_END
            pango_layout_set_height(layout, -lines);
        }

        let mut pw = 0;
        let mut ph = 0;
        pango_layout_get_pixel_size(layout, &mut pw, &mut ph);

        cairo_save(cr);
        cairo_move_to(cr, x, y);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        pango_cairo_show_layout(cr, layout);
        cairo_restore(cr);

        g_object_unref(layout as *mut c_void);
        (pw, ph)
    }
}

pub fn measure_pango_text(
    cr: *mut Cairo,
    text: &str,
    font_spec: &str,
    max_width: i32,
    max_lines: Option<i32>,
) -> (i32, i32) {
    unsafe {
        let layout = pango_cairo_create_layout(cr);
        if let Ok(c_text) = CString::new(text) {
            pango_layout_set_text(layout, c_text.as_ptr(), -1);
        }
        if let Ok(c_font) = CString::new(apply_global_font(font_spec)) {
            let desc = pango_font_description_from_string(c_font.as_ptr());
            pango_layout_set_font_description(layout, desc);
            pango_font_description_free(desc);
        }
        pango_layout_set_width(layout, max_width * 1024);
        pango_layout_set_wrap(layout, 0);
        if let Some(lines) = max_lines {
            pango_layout_set_ellipsize(layout, 3);
            pango_layout_set_height(layout, -lines);
        }

        let mut pw = 0;
        let mut ph = 0;
        pango_layout_get_pixel_size(layout, &mut pw, &mut ph);

        g_object_unref(layout as *mut c_void);
        (pw, ph)
    }
}

pub fn draw_wallpaper_panel(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let home = std::env::var("HOME").unwrap_or_default();
    let thumb_path = format!("{}/.wa-thumb.jpg", home);
    let full_path = format!("{}/.wa.jpg", home);

    let path_to_use = if Path::new(&thumb_path).exists() {
        Some(thumb_path)
    } else if Path::new(&full_path).exists() {
        Some(full_path)
    } else {
        None
    };

    unsafe {
        cairo_save(cr);
        rounded_rect_left_path(cr, x, y, w, h, radius);
        cairo_clip(cr);

        let mut loaded = false;
        if let Some(p) = path_to_use {
            // Cover-crop into the panel (the old code stretched the image to the panel size).
            if let Some(img) = cached_image_cover(&p, w.round() as i32, h.round() as i32) {
                let (iw, ih) = image_size(img);
                paint_cached(cr, img, x + (w - iw) / 2.0, y + (h - ih) / 2.0);
                loaded = true;
            }
        }

        if !loaded {
            // Elegant fallback gradient
            cairo_set_source_rgba(cr, 0.15, 0.18, 0.22, 1.0);
            cairo_paint(cr);
        }

        cairo_restore(cr);
    }
}

/// Resolve an icon name through the gsettings icon theme (see icons.rs). Memoized per name
/// and invalidated when the icon theme changes.
pub fn fill_circle(cr: *mut Cairo, cx: f64, cy: f64, radius: f64, color: (f64, f64, f64, f64)) {
    unsafe {
        cairo_new_sub_path(cr);
        cairo_arc(cr, cx, cy, radius, 0.0, 2.0 * std::f64::consts::PI);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        cairo_fill(cr);
    }
}

pub fn stroke_circle(
    cr: *mut Cairo,
    cx: f64,
    cy: f64,
    radius: f64,
    color: (f64, f64, f64, f64),
    line_width: f64,
) {
    unsafe {
        cairo_new_sub_path(cr);
        cairo_arc(cr, cx, cy, radius, 0.0, 2.0 * std::f64::consts::PI);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        cairo_set_line_width(cr, line_width);
        cairo_stroke(cr);
    }
}

pub fn draw_slider(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    value: f64, // 0.0 to 1.0
    primary_color: (f64, f64, f64, f64),
    track_color: (f64, f64, f64, f64),
    knob_color: (f64, f64, f64, f64),
) {
    let track_h = 6.0;
    let track_y = y + (h - track_h) / 2.0;
    let track_r = 3.0;
    let clamped_val = value.clamp(0.0, 1.0);

    // Background track
    fill_rounded_rect(cr, x, track_y, w, track_h, track_r, track_color);

    // Active progress track
    let active_w = (w * clamped_val).max(track_r * 2.0);
    fill_rounded_rect(cr, x, track_y, active_w, track_h, track_r, primary_color);

    // Thumb knob
    let knob_cx = x + (w * clamped_val);
    let knob_cy = y + h / 2.0;
    let knob_r = 7.0;
    fill_circle(cr, knob_cx, knob_cy, knob_r, knob_color);
    stroke_circle(cr, knob_cx, knob_cy, knob_r, primary_color, 1.5);
}

pub fn draw_search_pill(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    query: &str,
    placeholder: &str,
    bg_color: (f64, f64, f64, f64),
    border_color: (f64, f64, f64, f64),
    text_color: (f64, f64, f64, f64),
    placeholder_color: (f64, f64, f64, f64),
) {
    let radius = 12.0;
    fill_rounded_rect(cr, x, y, w, h, radius, bg_color);
    stroke_rounded_rect(cr, x, y, w, h, radius, border_color, 1.5);

    // Magnifier icon: Unicode 🔍
    draw_pango_text(cr, x + 14.0, y + 13.0, "🔍", "Inter, Sans 13", placeholder_color, None);

    // Query text or placeholder
    if query.is_empty() {
        draw_pango_text(
            cr,
            x + 40.0,
            y + 13.0,
            placeholder,
            "Inter, Sans 13",
            placeholder_color,
            Some((w - 50.0) as i32),
        );
    } else {
        let display_text = format!("{}_", query);
        draw_pango_text(
            cr,
            x + 40.0,
            y + 13.0,
            &display_text,
            "Inter, Sans Bold 13",
            text_color,
            Some((w - 50.0) as i32),
        );
    }
}

pub fn draw_quick_tile(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    icon: &str,
    label: &str,
    subtext: &str,
    is_active: bool,
    active_bg: (f64, f64, f64, f64),
    inactive_bg: (f64, f64, f64, f64),
    active_fg: (f64, f64, f64, f64),
    inactive_fg: (f64, f64, f64, f64),
) {
    let radius = 12.0;
    let bg = if is_active { active_bg } else { inactive_bg };
    let fg = if is_active { active_fg } else { inactive_fg };

    fill_rounded_rect(cr, x, y, w, h, radius, bg);
    if is_active {
        stroke_rounded_rect(cr, x, y, w, h, radius, active_fg, 1.0);
    }

    let header = format!("{} {}", icon, label);
    draw_pango_text(cr, x + 8.0, y + 6.0, &header, "Inter, Sans Bold 10", fg, Some((w - 12.0) as i32));

    let mut sub_fg = fg;
    sub_fg.3 *= 0.75;
    draw_pango_text(cr, x + 8.0, y + 24.0, subtext, "Inter, Sans 9", sub_fg, Some((w - 12.0) as i32));
}

pub fn draw_image_clipped(
    cr: *mut Cairo,
    path: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    radius: f64,
    preserve_aspect: bool,
) -> bool {
    if !preserve_aspect {
        // "Fill" requests are cover-cropped instead of stretched.
        return draw_image_cover(cr, path, x, y, w, h, radius);
    }
    let Some(img) = cached_image(path, w as i32, h as i32, preserve_aspect) else {
        return false;
    };
    unsafe {
        cairo_save(cr);
        if radius > 0.0 {
            rounded_rect_path(cr, x, y, w, h, radius);
            cairo_clip(cr);
        }
        let (pw, ph) = image_size(img);
        paint_cached(cr, img, x + (w - pw) / 2.0, y + (h - ph) / 2.0);
        cairo_restore(cr);
    }
    true
}

pub fn draw_grand_power_menu(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    radius: f64,
    bg_color: (f64, f64, f64, f64),
    border_color: (f64, f64, f64, f64),
    selected_idx: usize,
    primary_color: (f64, f64, f64, f64),
    fg_color: (f64, f64, f64, f64),
    sub_color: (f64, f64, f64, f64),
) {
    fill_rounded_rect(cr, x, y, w, h, radius, bg_color);
    stroke_rounded_rect(cr, x, y, w, h, radius, border_color, 1.0);

    let pad = 32.0;
    let spacing = 24.0;
    let btn_w = (w - pad * 2.0 - spacing * 3.0) / 4.0;
    let btn_h = h - pad * 2.0;

    let actions = [
        ("🔒", "Lock", "L"),
        ("🚪", "Logout", "E"),
        ("🔄", "Reboot", "R"),
        ("⏻", "Shutdown", "P"),
    ];

    for (i, (icon, label, key)) in actions.iter().enumerate() {
        let bx = x + pad + (i as f64) * (btn_w + spacing);
        let by = y + pad;
        let is_selected = i == selected_idx;

        let item_bg = if is_selected {
            (primary_color.0, primary_color.1, primary_color.2, 0.25)
        } else {
            (fg_color.0, fg_color.1, fg_color.2, 0.05)
        };

        fill_rounded_rect(cr, bx, by, btn_w, btn_h, 20.0, item_bg);
        if is_selected {
            stroke_rounded_rect(cr, bx, by, btn_w, btn_h, 20.0, primary_color, 2.0);
        } else {
            stroke_rounded_rect(cr, bx, by, btn_w, btn_h, 20.0, (border_color.0, border_color.1, border_color.2, 0.40), 1.0);
        }

        let icon_color = if is_selected { primary_color } else { fg_color };
        let label_color = if is_selected { primary_color } else { fg_color };

        // Center large icon
        draw_pango_text(cr, bx + (btn_w / 2.0) - 24.0, by + 40.0, icon, "Inter, Sans 48", icon_color, None);

        // Bold Title
        draw_pango_text(cr, bx + (btn_w / 2.0) - 34.0, by + 180.0, label, "Inter, Sans Bold 20", label_color, None);

        // Key hint pill
        let key_str = format!("Press [{}]", key);
        draw_pango_text(cr, bx + (btn_w / 2.0) - 28.0, by + 225.0, &key_str, "Inter, Sans 12", sub_color, None);
    }
}

pub fn draw_toggle_switch(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    enabled: bool,
    active_color: (f64, f64, f64, f64),
    inactive_color: (f64, f64, f64, f64),
    knob_color: (f64, f64, f64, f64),
) {
    let w = 44.0;
    let h = 24.0;
    let radius = 12.0;
    let bg = if enabled { active_color } else { inactive_color };
    fill_rounded_rect(cr, x, y, w, h, radius, bg);

    let knob_r = 8.0;
    let knob_x = if enabled { x + w - radius } else { x + radius };
    let knob_y = y + radius;
    fill_circle(cr, knob_x, knob_y, knob_r, knob_color);
}

pub fn draw_segmented_pills(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    options: &[&str],
    selected_idx: usize,
    active_bg: (f64, f64, f64, f64),
    inactive_bg: (f64, f64, f64, f64),
    active_fg: (f64, f64, f64, f64),
    inactive_fg: (f64, f64, f64, f64),
) {
    let n = options.len();
    if n == 0 { return; }
    let item_w = w / (n as f64);
    for (i, opt) in options.iter().enumerate() {
        let ix = x + (i as f64) * item_w;
        let is_sel = i == selected_idx;
        let bg = if is_sel { active_bg } else { inactive_bg };
        let fg = if is_sel { active_fg } else { inactive_fg };
        fill_rounded_rect(cr, ix + 2.0, y, item_w - 4.0, h, 8.0, bg);
        draw_pango_text(cr, ix + 8.0, y + (h - 16.0) / 2.0, opt, "Inter, Sans Bold 11", fg, Some((item_w - 16.0) as i32));
    }
}

pub fn draw_battery_indicator(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    percent: u8,
    charging: bool,
    fg_color: (f64, f64, f64, f64),
) {
    let body_w = 18.0;
    let body_h = 11.0;
    let r = 2.0;

    // Body outline
    stroke_rounded_rect(cr, x, y, body_w, body_h, r, fg_color, 1.2);

    // Terminal nub
    let nub_w = 2.5;
    let nub_h = 5.0;
    let nub_x = x + body_w;
    let nub_y = y + (body_h - nub_h) / 2.0;
    fill_rounded_rect(cr, nub_x, nub_y, nub_w, nub_h, 1.0, fg_color);

    // Fill
    let pct = (percent.clamp(0, 100) as f64) / 100.0;
    let fill_max_w = body_w - 4.0;
    let fill_w = (fill_max_w * pct).max(1.0);
    let fill_h = body_h - 4.0;
    let fill_color = if charging {
        fg_color
    } else if percent <= 15 {
        (0.95, 0.25, 0.25, 0.95)
    } else if percent <= 25 {
        (0.95, 0.65, 0.15, 0.95)
    } else {
        fg_color
    };
    fill_rounded_rect(cr, x + 2.0, y + 2.0, fill_w, fill_h, 1.0, fill_color);

    // Charging lightning bolt
    if charging {
        unsafe {
            cairo_save(cr);
            cairo_set_source_rgba(cr, fg_color.0, fg_color.1, fg_color.2, 1.0);
            cairo_new_path(cr);
            let cx = x + body_w / 2.0;
            let cy = y + body_h / 2.0;
            cairo_move_to(cr, cx + 1.0, cy - 4.5);
            cairo_line_to(cr, cx - 2.5, cy);
            cairo_line_to(cr, cx, cy);
            cairo_line_to(cr, cx - 1.0, cy + 4.5);
            cairo_line_to(cr, cx + 2.5, cy);
            cairo_line_to(cr, cx, cy);
            cairo_close_path(cr);
            cairo_fill(cr);
            cairo_restore(cr);
        }
    }

    // Percentage text
    let pct_text = format!("{}%", percent);
    draw_pango_text(cr, x + body_w + nub_w + 5.0, y - 1.0, &pct_text, "Inter, Sans 11", fg_color, None);
}

