#![allow(dead_code)]
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::path::Path;

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
    pub fn cairo_paint(cr: *mut Cairo);
    pub fn cairo_new_path(cr: *mut Cairo);
    pub fn cairo_new_sub_path(cr: *mut Cairo);
    pub fn cairo_close_path(cr: *mut Cairo);
    pub fn cairo_arc(cr: *mut Cairo, xc: f64, yc: f64, radius: f64, angle1: f64, angle2: f64);
    pub fn cairo_rectangle(cr: *mut Cairo, x: f64, y: f64, width: f64, height: f64);
    pub fn cairo_fill(cr: *mut Cairo);
    pub fn cairo_stroke(cr: *mut Cairo);
    pub fn cairo_set_line_width(cr: *mut Cairo, width: f64);
    pub fn cairo_clip(cr: *mut Cairo);
    pub fn cairo_save(cr: *mut Cairo);
    pub fn cairo_restore(cr: *mut Cairo);
    pub fn cairo_move_to(cr: *mut Cairo, x: f64, y: f64);
    pub fn cairo_line_to(cr: *mut Cairo, x: f64, y: f64);

    pub fn pango_cairo_create_layout(cr: *mut Cairo) -> *mut PangoLayout;
    pub fn pango_layout_set_text(layout: *mut PangoLayout, text: *const c_char, length: i32);
    pub fn pango_layout_set_width(layout: *mut PangoLayout, width: i32);
    pub fn pango_layout_set_ellipsize(layout: *mut PangoLayout, ellipsize: i32);
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
        if let Ok(c_font) = CString::new(font_spec) {
            let desc = pango_font_description_from_string(c_font.as_ptr());
            pango_layout_set_font_description(layout, desc);
            pango_font_description_free(desc);
        }
        if let Some(mw) = max_width {
            pango_layout_set_width(layout, mw * 1024);
            pango_layout_set_ellipsize(layout, 3);
        }

        cairo_save(cr);
        cairo_move_to(cr, x, y);
        cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        pango_cairo_show_layout(cr, layout);
        cairo_restore(cr);

        g_object_unref(layout as *mut c_void);
    }
}

pub fn draw_wallpaper_background(cr: *mut Cairo, width: i32, height: i32) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/vic".to_string());
    let full_path = format!("{}/.wa.jpg", home);

    let mut drawn = false;
    if Path::new(&full_path).exists() {
        if let Ok(c_path) = CString::new(full_path.as_str()) {
            unsafe {
                let pixbuf = gdk_pixbuf_new_from_file_at_scale(
                    c_path.as_ptr(),
                    width,
                    height,
                    0,
                    std::ptr::null_mut(),
                );
                if !pixbuf.is_null() {
                    gdk_cairo_set_source_pixbuf(cr, pixbuf, 0.0, 0.0);
                    cairo_paint(cr);
                    g_object_unref(pixbuf as *mut c_void);
                    drawn = true;
                }
            }
        }
    }

    unsafe {
        if !drawn {
            cairo_set_source_rgba(cr, 0.06, 0.08, 0.10, 1.0);
            cairo_paint(cr);
        }

        // Dark translucent scrim for contrast
        cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 0.25);
        cairo_paint(cr);
    }
}
