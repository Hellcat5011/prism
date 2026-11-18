#![allow(dead_code)]
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
extern "C" {
    pub fn cairo_set_fill_rule(cr: *mut Cairo, rule: i32);
    pub fn cairo_surface_write_to_png(surface: *mut CairoSurface, filename: *const std::ffi::c_char) -> i32;
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
    pub fn cairo_surface_reference(surface: *mut CairoSurface) -> *mut CairoSurface;

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
    fn pango_cairo_font_map_get_default() -> *mut c_void;
    fn pango_font_map_create_context(fontmap: *mut c_void) -> *mut c_void;
    fn pango_cairo_update_context(cr: *mut Cairo, context: *mut c_void);
    fn pango_layout_new(context: *mut c_void) -> *mut PangoLayout;
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
// are cached as misses too. The cache is bounded by memory: large images (wallpapers, art) are
// dropped first, so small icons stay decoded.
// ─────────────────────────────────────────────────────────────────────────

/// Decoded bytes kept before large images are dropped.
const IMAGE_CACHE_BUDGET: usize = 96 << 20;
/// Images up to this many pixels (64x64) count as icons: dropped only when the cache is
/// full of them too.
const SMALL_IMAGE_PX: i64 = 64 * 64;

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

/// Pixels decoded off the UI thread (cairo ARGB32, premultiplied), ready for `insert_decoded`.
pub struct Decoded {
    key: ImageKey,
    w: i32,
    h: i32,
    data: Vec<u8>,
}

impl Decoded {
    /// Premultiplied ARGB32 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.data
    }
}

/// Decode `path` scaled to fit (w, h) with gdk-pixbuf. Touches no cairo or thread-local state,
/// so it can run on a worker thread (icon preloading). None if the file cannot be read.
pub fn decode_image(path: &str, w: i32, h: i32, preserve_aspect: bool) -> Option<Decoded> {
    let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    mtime?;
    let key = ImageKey { path: path.to_string(), mtime, w, h, preserve_aspect };
    decode_key(key)
}

fn decode_key(key: ImageKey) -> Option<Decoded> {
    let c_path = CString::new(key.path.as_str()).ok()?;
    unsafe {
        let pixbuf = gdk_pixbuf_new_from_file_at_scale(c_path.as_ptr(), key.w, key.h, if key.preserve_aspect { 1 } else { 0 }, std::ptr::null_mut());
        if pixbuf.is_null() {
            return None;
        }
        let (pw, ph) = (gdk_pixbuf_get_width(pixbuf).max(1), gdk_pixbuf_get_height(pixbuf).max(1));
        let (n, rs) = (gdk_pixbuf_get_n_channels(pixbuf) as usize, gdk_pixbuf_get_rowstride(pixbuf) as usize);
        let src = std::slice::from_raw_parts(gdk_pixbuf_get_pixels(pixbuf), rs * (ph as usize - 1) + pw as usize * n);
        let mut data = vec![0u8; (pw * ph * 4) as usize];
        for y in 0..ph as usize {
            for x in 0..pw as usize {
                let p = &src[y * rs + x * n..];
                let a = if n >= 4 { p[3] as u32 } else { 255 };
                let pm = |c: u8| (c as u32 * a + 127) / 255;
                // ARGB32 is a native-endian u32 with premultiplied colour.
                let v = (a << 24) | (pm(p[0]) << 16) | (pm(p[1]) << 8) | pm(p[2]);
                let o = (y * pw as usize + x) * 4;
                data[o..o + 4].copy_from_slice(&v.to_ne_bytes());
            }
        }
        g_object_unref(pixbuf as *mut c_void);
        Some(Decoded { key, w: pw, h: ph, data })
    }
}

/// Put pixels from `decode_image` into the image cache (UI thread).
pub fn insert_decoded(d: Decoded) {
    let surface = unsafe {
        let img = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, d.w, d.h);
        let stride = cairo_image_surface_get_stride(img) as usize;
        let dst = cairo_image_surface_get_data(img);
        let row = d.w as usize * 4;
        for y in 0..d.h as usize {
            std::ptr::copy_nonoverlapping(d.data.as_ptr().add(y * row), dst.add(y * stride), row);
        }
        cairo_surface_mark_dirty(img);
        img
    };
    cache_insert(d.key, surface);
}

fn cache_insert(key: ImageKey, surface: *mut CairoSurface) {
    IMAGE_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let bytes = |s: &CachedSurface| {
            if s.0.is_null() {
                return 0usize;
            }
            unsafe { (cairo_image_surface_get_stride(s.0) * cairo_image_surface_get_height(s.0)) as usize }
        };
        let total: usize = c.values().map(bytes).sum();
        if total > IMAGE_CACHE_BUDGET {
            c.retain(|k, _| (k.w as i64) * (k.h as i64) <= SMALL_IMAGE_PX);
            let total: usize = c.values().map(bytes).sum();
            if total > IMAGE_CACHE_BUDGET {
                c.clear();
            }
        }
        c.insert(key, CachedSurface(surface));
    });
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
    match decode_key(key.clone()) {
        Some(d) => {
            insert_decoded(d);
            IMAGE_CACHE.with(|c| c.borrow().get(&key).map(|s| s.0)).filter(|s| !s.is_null())
        }
        None => {
            cache_insert(key, std::ptr::null_mut()); // remember the miss
            None
        }
    }
}

/// `cached_image` for callers that keep the surface across draws: the result stays valid when
/// the cache is cleared, and the caller releases it with `cairo_surface_destroy`.
pub fn owned_image(path: &str, w: i32, h: i32, preserve_aspect: bool) -> Option<*mut CairoSurface> {
    cached_image(path, w, h, preserve_aspect).map(|s| unsafe { cairo_surface_reference(s) })
}

/// Decode `path` scaled to *cover* (w, h) keeping its aspect ratio (QML PreserveAspectCrop);
/// the result is at least as large as the box and is meant to be drawn centred and clipped.
pub fn cached_image_cover(path: &str, w: i32, h: i32) -> Option<*mut CairoSurface> {
    let (dw, dh, keep) = cover_params(path, w, h);
    cached_image(path, dw, dh, keep)
}

/// Decode size and aspect flag that `cached_image_cover` uses for (w, h).
fn cover_params(path: &str, w: i32, h: i32) -> (i32, i32, bool) {
    let Some((iw, ih)) = image_dims(path) else { return (w, h, false) };
    let s = (w as f64 / iw as f64).max(h as f64 / ih as f64);
    (((iw as f64 * s).ceil() as i32).max(1), ((ih as f64 * s).ceil() as i32).max(1), true)
}

/// Pixel size of the image file at `path`. gdk_pixbuf_get_file_info opens and parses the file,
/// so sizes are remembered per path and modification time (this runs on every draw of a
/// cover image). Thread-safe: the worker thread uses it too.
pub fn image_dims(path: &str) -> Option<(i32, i32)> {
    type Dims = std::collections::HashMap<String, (std::time::SystemTime, i32, i32)>;
    static DIMS: std::sync::Mutex<Option<Dims>> = std::sync::Mutex::new(None);
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    if let Some(&(t, w, h)) = DIMS.lock().ok()?.get_or_insert_with(Default::default).get(path) {
        if t == mtime {
            return Some((w, h));
        }
    }
    let c = CString::new(path).ok()?;
    let (mut iw, mut ih) = (0i32, 0i32);
    let fmt = unsafe { gdk_pixbuf_get_file_info(c.as_ptr(), &mut iw, &mut ih) };
    if fmt.is_null() || iw <= 0 || ih <= 0 {
        return None;
    }
    let mut dims = DIMS.lock().ok()?;
    let map = dims.get_or_insert_with(Default::default);
    if map.len() >= 512 {
        map.clear();
    }
    map.insert(path.to_string(), (mtime, iw, ih));
    Some((iw, ih))
}

/// `decode_image` with the parameters `cached_image_cover` will look up (worker thread).
pub fn decode_image_cover(path: &str, w: i32, h: i32) -> Option<Decoded> {
    let (dw, dh, keep) = cover_params(path, w, h);
    decode_image(path, dw, dh, keep)
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
pub const CAIRO_OPERATOR_DEST_OUT: i32 = 9;
pub const CAIRO_FILTER_BILINEAR: i32 = 4;

thread_local! {
    static GLOBAL_FONT: std::cell::RefCell<String> = std::cell::RefCell::new(String::from("Inter, Sans"));
}

/// Set the UI font family used by every text draw (from the global `global_font` setting).
pub fn set_global_font(family: &str) {
    GLOBAL_FONT.with(|f| *f.borrow_mut() = family.to_string());
}

thread_local! {
    /// One Pango context per thread, synced to each cairo context before use. Creating a
    /// context per text draw (pango_cairo_create_layout) re-resolved the fonts every time.
    static PANGO_CONTEXT: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// Parsed font descriptions by spec string.
    static FONT_DESCS: std::cell::RefCell<std::collections::HashMap<String, *mut PangoFontDescription>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// A new layout for drawing on `cr` with the font `spec` (already resolved, see
/// apply_global_font). The caller unrefs it.
pub fn new_layout(cr: *mut Cairo, spec: &str) -> *mut PangoLayout {
    unsafe {
        let ctx = PANGO_CONTEXT.with(|c| {
            if c.get().is_null() {
                c.set(pango_font_map_create_context(pango_cairo_font_map_get_default()));
            }
            c.get()
        });
        pango_cairo_update_context(cr, ctx);
        let layout = pango_layout_new(ctx);
        FONT_DESCS.with(|d| {
            let mut d = d.borrow_mut();
            if !d.contains_key(spec) {
                if d.len() >= 256 {
                    for (_, desc) in d.drain() {
                        pango_font_description_free(desc);
                    }
                }
                let Ok(c) = CString::new(spec) else { return };
                d.insert(spec.to_string(), pango_font_description_from_string(c.as_ptr()));
            }
            // The layout keeps its own copy.
            pango_layout_set_font_description(layout, d[spec]);
        });
        layout
    }
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
        // By checked range: a malformed palette may hold non-ASCII text.
        let byte = |at: usize| clean.get(at..at + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
        let r = byte(0) as f64 / 255.0;
        let g = byte(2) as f64 / 255.0;
        let b = byte(4) as f64 / 255.0;
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

/// Fill a rounded rect with a vertical gradient from `top` to `bottom` (offsets 0..1 of `h`
/// given by `from`/`to`; outside them the end colours continue).
#[allow(clippy::too_many_arguments)]
pub fn fill_rounded_rect_gradient(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64, top: (f64, f64, f64, f64), bottom: (f64, f64, f64, f64), from: f64, to: f64) {
    unsafe {
        let pat = cairo_pattern_create_linear(0.0, y + h * from, 0.0, y + h * to);
        cairo_pattern_add_color_stop_rgba(pat, 0.0, top.0, top.1, top.2, top.3);
        cairo_pattern_add_color_stop_rgba(pat, 1.0, bottom.0, bottom.1, bottom.2, bottom.3);
        rounded_rect_path(cr, x, y, w, h, r);
        cairo_set_source(cr, pat);
        cairo_fill(cr);
        cairo_pattern_destroy(pat);
    }
}

/// Stroke a rounded rect with a vertical gradient (`top` colour at the top edge).
#[allow(clippy::too_many_arguments)]
pub fn stroke_rounded_rect_gradient(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64, top: (f64, f64, f64, f64), bottom: (f64, f64, f64, f64), line_width: f64) {
    unsafe {
        let pat = cairo_pattern_create_linear(0.0, y, 0.0, y + h);
        cairo_pattern_add_color_stop_rgba(pat, 0.0, top.0, top.1, top.2, top.3);
        cairo_pattern_add_color_stop_rgba(pat, 1.0, bottom.0, bottom.1, bottom.2, bottom.3);
        rounded_rect_path(cr, x, y, w, h, r);
        cairo_set_source(cr, pat);
        cairo_set_line_width(cr, line_width);
        cairo_stroke(cr);
        cairo_pattern_destroy(pat);
    }
}

/// Soft drop shadow under a floating rounded rect (dropdowns, tooltips): stacked rounded rects
/// growing outwards with falling alpha approximate a gaussian blur of `spread` px, offset down
/// by `dy`. Draw it before the element itself.
pub fn drop_shadow(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, r: f64, spread: f64, dy: f64, alpha: f64) {
    const STEPS: usize = 8;
    for i in (0..STEPS).rev() {
        let t = (i as f64 + 1.0) / STEPS as f64;
        let g = spread * t;
        // Each layer adds a little: the sum falls off smoothly towards the outer edge.
        let a = alpha * (1.0 - t).powi(2) / STEPS as f64 * 3.0;
        fill_rounded_rect(cr, x - g, y - g + dy, w + 2.0 * g, h + 2.0 * g, r + g, (0.0, 0.0, 0.0, a));
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
        let layout = new_layout(cr, &apply_global_font(font_spec));
        if let Ok(c_text) = CString::new(text) {
            pango_layout_set_text(layout, c_text.as_ptr(), -1);
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
        let layout = new_layout(cr, &apply_global_font(font_spec));
        if let Ok(c_text) = CString::new(text) {
            pango_layout_set_text(layout, c_text.as_ptr(), -1);
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
        let layout = new_layout(cr, &apply_global_font(font_spec));
        if let Ok(c_text) = CString::new(text) {
            pango_layout_set_text(layout, c_text.as_ptr(), -1);
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

/// The picture the launcher's wallpaper panel shows: the thumbnail, else the wallpaper.
pub fn wallpaper_panel_path() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    [format!("{home}/.wa-thumb.jpg"), format!("{home}/.wa.jpg")].into_iter().find(|p| Path::new(p).exists())
}

pub fn draw_wallpaper_panel(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let path_to_use = wallpaper_panel_path();

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
pub fn resolve_icon_path(icon_name: &str) -> Option<PathBuf> {
    if icon_name.is_empty() {
        return None;
    }
    let gen = crate::icons::generation();
    if ICON_CACHE_GEN.with(|g| g.replace(gen)) != gen {
        ICON_PATH_CACHE.with(|c| c.borrow_mut().clear());
    }
    if let Some(hit) = ICON_PATH_CACHE.with(|c| c.borrow().get(icon_name).cloned()) {
        return hit;
    }
    let found = crate::icons::lookup(icon_name, 48);
    ICON_PATH_CACHE.with(|c| c.borrow_mut().insert(icon_name.to_string(), found.clone()));
    found
}

pub fn draw_app_icon(
    cr: *mut Cairo,
    icon_name: &str,
    x: f64,
    y: f64,
    size: i32,
    fallback_letter: &str,
    accent_color: (f64, f64, f64, f64),
) {
    let mut rendered = false;
    if let Some(path) = resolve_icon_path(icon_name) {
        if let Some(path_str) = path.to_str() {
            if let Some(img) = cached_image(path_str, size, size, true) {
                let (pw, ph) = image_size(img);
                let off_x = x + (size as f64 - pw) / 2.0;
                let off_y = y + (size as f64 - ph) / 2.0;
                paint_cached(cr, img, off_x, off_y);
                rendered = true;
            }
        }
    }

    if !rendered {
        // Fallback: draw rounded icon badge with letter
        fill_rounded_rect(cr, x, y, size as f64, size as f64, 8.0, accent_color);
        draw_pango_text(
            cr,
            x + (size as f64 * 0.32),
            y + (size as f64 * 0.18),
            fallback_letter,
            "Inter, Sans Bold 12",
            (1.0, 1.0, 1.0, 0.95),
            None,
        );
    }
}

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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_off_thread_and_caches() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/settings-gear.svg");
        let d = std::thread::spawn(move || decode_image(path, 32, 32, true)).join().unwrap().expect("decode");
        assert!(d.w <= 32 && d.h <= 32 && d.w > 0);
        assert_eq!(d.data.len(), (d.w * d.h * 4) as usize);
        // Premultiplied: no colour channel exceeds alpha.
        assert!(d.data.chunks_exact(4).all(|p| {
            let v = u32::from_ne_bytes([p[0], p[1], p[2], p[3]]);
            let a = v >> 24;
            (v >> 16 & 255) <= a && (v >> 8 & 255) <= a && (v & 255) <= a
        }));
        assert!(d.data.chunks_exact(4).any(|p| u32::from_ne_bytes([p[0], p[1], p[2], p[3]]) >> 24 > 0));
        insert_decoded(d);
        let hit = cached_image(path, 32, 32, true).expect("cache hit");
        assert!(image_size(hit).0 <= 32.0);
    }

    #[test]
    fn owned_image_outlives_the_cache() {
        extern "C" {
            fn cairo_surface_get_reference_count(surface: *mut CairoSurface) -> u32;
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/settings-gear.svg");
        let img = owned_image(path, 48, 48, true).expect("decode");
        unsafe { assert_eq!(cairo_surface_get_reference_count(img), 2) };
        IMAGE_CACHE.with(|c| c.borrow_mut().clear());
        unsafe { assert_eq!(cairo_surface_get_reference_count(img), 1) };
        assert!(image_size(img).0 <= 48.0);
        unsafe { cairo_surface_destroy(img) };
    }
}
