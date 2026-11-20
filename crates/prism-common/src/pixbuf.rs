//! Image resize/re-encode through gdk-pixbuf (replaces ImageMagick for wallpaper copies).

use std::ffi::{c_char, c_void, CString};
use std::io::Read;
use std::path::Path;

use crate::filter::Filter;

#[link(name = "gdk_pixbuf-2.0")]
#[link(name = "gobject-2.0")]
#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn gdk_pixbuf_get_file_info(filename: *const c_char, width: *mut i32, height: *mut i32) -> *mut c_void;
    fn gdk_pixbuf_new(colorspace: i32, has_alpha: i32, bits_per_sample: i32, width: i32, height: i32) -> *mut c_void;
    fn gdk_pixbuf_new_from_file_at_scale(filename: *const c_char, width: i32, height: i32, preserve_aspect_ratio: i32, error: *mut *mut c_void) -> *mut c_void;
    fn gdk_pixbuf_get_width(pixbuf: *const c_void) -> i32;
    fn gdk_pixbuf_get_height(pixbuf: *const c_void) -> i32;
    fn gdk_pixbuf_get_n_channels(pixbuf: *const c_void) -> i32;
    fn gdk_pixbuf_get_rowstride(pixbuf: *const c_void) -> i32;
    fn gdk_pixbuf_get_pixels(pixbuf: *const c_void) -> *mut u8;
    fn gdk_pixbuf_scale_simple(src: *const c_void, dest_width: i32, dest_height: i32, interp_type: i32) -> *mut c_void;
    fn gdk_pixbuf_savev(
        pixbuf: *mut c_void,
        filename: *const c_char,
        type_: *const c_char,
        option_keys: *const *const c_char,
        option_values: *const *const c_char,
        error: *mut *mut c_void,
    ) -> i32;
    fn g_object_unref(object: *mut c_void);
    fn g_error_free(error: *mut c_void);
}

/// Pixel size of an image file, read from its header.
pub fn image_size(path: &Path) -> Option<(i32, i32)> {
    let c = CString::new(path.to_str()?).ok()?;
    let (mut w, mut h) = (0, 0);
    let fmt = unsafe { gdk_pixbuf_get_file_info(c.as_ptr(), &mut w, &mut h) };
    (!fmt.is_null() && w > 0 && h > 0).then_some((w, h))
}

/// Colour components of a JPEG file (1 = greyscale, 3 = colour), read from its frame header.
pub fn jpeg_components(path: &Path) -> Option<u8> {
    let mut b = Vec::new();
    std::fs::File::open(path).ok()?.take(64 * 1024).read_to_end(&mut b).ok()?;
    if b.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // Start-of-frame markers: C0..CF without DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some(b[i + 9]);
        }
        i += 2 + u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
    }
    None
}

/// Writes `src` as a JPEG at `dst`, shrunk to at most `max_w` pixels wide (never enlarged), like
/// `magick src -resize '<max_w>x>' -quality <quality> dst`.
pub fn save_jpeg_fit_width(src: &Path, dst: &Path, max_w: i32, quality: u8) -> Result<(), String> {
    save_jpeg_filtered(src, dst, max_w, quality, Filter::Normal)
}

/// `save_jpeg_fit_width` with a colour filter. Transparency is flattened onto black: gdk-pixbuf
/// writes a greyscale JPEG when it is handed a pixbuf with an alpha channel.
pub fn save_jpeg_filtered(src: &Path, dst: &Path, max_w: i32, quality: u8, filter: Filter) -> Result<(), String> {
    let (w, h) = image_size(src).ok_or_else(|| format!("cannot read image {}", src.display()))?;
    let (tw, th) = if w > max_w { (max_w, ((h as f64) * max_w as f64 / w as f64).round().max(1.0) as i32) } else { (w, h) };
    let part = dst.with_extension("part");
    let c_src = CString::new(src.to_str().ok_or("non-UTF-8 path")?).map_err(|e| e.to_string())?;
    let c_part = CString::new(part.to_str().ok_or("non-UTF-8 path")?).map_err(|e| e.to_string())?;
    unsafe {
        let mut err = std::ptr::null_mut();
        let pb = gdk_pixbuf_new_from_file_at_scale(c_src.as_ptr(), tw, th, 1, &mut err);
        if pb.is_null() {
            if !err.is_null() {
                g_error_free(err);
            }
            return Err(format!("cannot decode {}", src.display()));
        }
        let rgb = flattened(pb, filter);
        g_object_unref(pb);
        if rgb.is_null() {
            return Err(format!("out of memory for {}", src.display()));
        }
        let key = CString::new("quality").unwrap();
        let val = CString::new(quality.min(100).to_string()).unwrap();
        let keys = [key.as_ptr(), std::ptr::null()];
        let vals = [val.as_ptr(), std::ptr::null()];
        let mut err = std::ptr::null_mut();
        let ok = gdk_pixbuf_savev(rgb, c_part.as_ptr(), c"jpeg".as_ptr(), keys.as_ptr(), vals.as_ptr(), &mut err);
        g_object_unref(rgb);
        if ok == 0 {
            if !err.is_null() {
                g_error_free(err);
            }
            let _ = std::fs::remove_file(&part);
            return Err(format!("cannot write {}", dst.display()));
        }
    }
    std::fs::rename(&part, dst).map_err(|e| format!("cannot write {}: {e}", dst.display()))
}

/// Write tightly packed RGBA8 pixels (`w` x `h`) as a zlib-compressed PNG at `dst`, scaled down
/// to fit `max_side` x `max_side` when larger. Written to `<dst>.part` first, then renamed.
pub fn save_png_rgba(rgba: &[u8], w: i32, h: i32, max_side: i32, dst: &Path) -> Result<(), String> {
    if w <= 0 || h <= 0 || rgba.len() < w as usize * h as usize * 4 {
        return Err("bad image size".into());
    }
    let part = dst.with_extension("part");
    let c_part = CString::new(part.to_str().ok_or("non-UTF-8 path")?).map_err(|e| e.to_string())?;
    unsafe {
        let pb = gdk_pixbuf_new(0, 1, 8, w, h);
        if pb.is_null() {
            return Err("out of memory".into());
        }
        let (px, stride) = (gdk_pixbuf_get_pixels(pb), gdk_pixbuf_get_rowstride(pb) as usize);
        let row = w as usize * 4;
        for y in 0..h as usize {
            std::ptr::copy_nonoverlapping(rgba.as_ptr().add(y * row), px.add(y * stride), row);
        }
        let s = (max_side as f64 / w.max(h) as f64).min(1.0);
        let out = if s < 1.0 {
            const GDK_INTERP_BILINEAR: i32 = 2;
            let (tw, th) = (((w as f64 * s).round() as i32).max(1), ((h as f64 * s).round() as i32).max(1));
            let scaled = gdk_pixbuf_scale_simple(pb, tw, th, GDK_INTERP_BILINEAR);
            g_object_unref(pb);
            if scaled.is_null() {
                return Err("out of memory".into());
            }
            scaled
        } else {
            pb
        };
        let keys = [c"compression".as_ptr(), std::ptr::null()];
        let vals = [c"6".as_ptr(), std::ptr::null()];
        let mut err = std::ptr::null_mut();
        let ok = gdk_pixbuf_savev(out, c_part.as_ptr(), c"png".as_ptr(), keys.as_ptr(), vals.as_ptr(), &mut err);
        g_object_unref(out);
        if ok == 0 {
            if !err.is_null() {
                g_error_free(err);
            }
            let _ = std::fs::remove_file(&part);
            return Err(format!("cannot write {}", dst.display()));
        }
    }
    std::fs::rename(&part, dst).map_err(|e| format!("cannot write {}: {e}", dst.display()))
}

/// A new three-channel copy of `pb` with `filter` applied; null when it cannot be allocated.
unsafe fn flattened(pb: *mut c_void, filter: Filter) -> *mut c_void {
    unsafe {
        let (w, h) = (gdk_pixbuf_get_width(pb), gdk_pixbuf_get_height(pb));
        let rgb = gdk_pixbuf_new(0, 0, 8, w, h);
        if rgb.is_null() {
            return rgb;
        }
        let n = gdk_pixbuf_get_n_channels(pb) as usize;
        let (src, src_stride) = (gdk_pixbuf_get_pixels(pb), gdk_pixbuf_get_rowstride(pb) as usize);
        let (dst, dst_stride) = (gdk_pixbuf_get_pixels(rgb), gdk_pixbuf_get_rowstride(rgb) as usize);
        let grade = (filter != Filter::Normal).then(|| {
            let samples = (0..h as usize).step_by(8).flat_map(move |y| {
                (0..w as usize).step_by(8).map(move |x| {
                    let p = src.add(y * src_stride + x * n);
                    [*p, *p.add(1), *p.add(2)]
                })
            });
            filter.grade(samples)
        });
        for y in 0..h as usize {
            let (from, to) = (src.add(y * src_stride), dst.add(y * dst_stride));
            for x in 0..w as usize {
                let p = from.add(x * n);
                let a = if n == 4 { *p.add(3) as u32 } else { 255 };
                let mut c = [0, 1, 2].map(|i| (*p.add(i) as u32 * a / 255) as u8);
                if let Some(g) = &grade {
                    c = g.apply(c);
                }
                std::ptr::copy_nonoverlapping(c.as_ptr(), to.add(x * 3), 3);
            }
        }
        rgb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A saturated red PNG with an alpha channel, like the wallpapers that came out grey.
    fn red_rgba_png(path: &Path) {
        let c = CString::new(path.to_str().unwrap()).unwrap();
        unsafe {
            let pb = gdk_pixbuf_new(0, 1, 8, 8, 8);
            let (px, stride) = (gdk_pixbuf_get_pixels(pb), gdk_pixbuf_get_rowstride(pb) as usize);
            for y in 0..8 {
                for x in 0..8 {
                    std::ptr::copy_nonoverlapping([220u8, 20, 20, 255].as_ptr(), px.add(y * stride + x * 4), 4);
                }
            }
            let none = [std::ptr::null::<c_char>()];
            assert_ne!(gdk_pixbuf_savev(pb, c.as_ptr(), c"png".as_ptr(), none.as_ptr(), none.as_ptr(), std::ptr::null_mut()), 0);
            g_object_unref(pb);
        }
    }

    #[test]
    fn png_with_alpha_stays_in_colour() {
        let dir = std::env::temp_dir().join(format!("prism-pixbuf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, dst) = (dir.join("in.png"), dir.join("out.jpg"));
        red_rgba_png(&src);
        save_jpeg_fit_width(&src, &dst, 8, 95).unwrap();
        assert_eq!(jpeg_components(&dst), Some(3));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn png_rgba_is_scaled_and_compressed() {
        let dir = std::env::temp_dir().join(format!("prism-pixbuf-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("big.png");
        let (w, h) = (2000, 1000);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 40, 200, 255]).collect();
        save_png_rgba(&rgba, w, h, 1024, &dst).unwrap();
        assert_eq!(image_size(&dst), Some((1024, 512)));
        assert!(std::fs::metadata(&dst).unwrap().len() < (1024 * 512 * 4) / 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
