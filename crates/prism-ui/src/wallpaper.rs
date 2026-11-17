// ─────────────────────────────────────────────────────────────────────────
// wallpaper.rs — wallpaper selector (port of modules/WallpaperSelector.qml)
//
// Full-width, background-less carousel of skewed slices on a PathView-like
// path: the current wallpaper is a wide slice (32% of the screen width) in
// the middle, its neighbours narrow slices (12%) that fan out to both edges.
// The list is shuffled on every open and repeated until it has at least 9
// entries, like the QML picker.
//
// Performance: images are never decoded at full size on the UI thread.
// Thumbnails live in the QML cache (~/.cache/Prism/thumbs/<md5(path)>.jpg,
// 35% of the screen width) and are generated for new/changed wallpapers on
// worker threads; slices are decoded from the thumbnails on a worker thread
// and handed to the UI as ready pixel buffers. Scrolling animates a
// continuous position (250 ms OutCubic, QML highlightMoveDuration) per
// presented frame.
// ─────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use crate::cairo_backend::*;
use crate::desktop::text::{draw_in_box, Align, TextStyle};
use crate::ui::{SpringValue, Style};
use crate::waker::Waker;

pub const PANEL_H: f64 = 618.0;
const SKEW: f64 = -0.25;
/// Carousel motion (response s, damping): settles with a hint of overshoot.
const SPRING_MOVE: (f64, f64) = (0.36, 0.88);

struct Entry {
    original: PathBuf,
    thumb: PathBuf,
}

/// Decoded slice image: premultiplied ARGB32 rows ready for a cairo surface.
struct Decoded {
    key: PathBuf,
    w: i32,
    h: i32,
    data: Vec<u8>,
}

pub enum Effect {
    None,
    Redraw,
    Close,
}

pub struct WallpaperView {
    entries: Vec<Entry>,
    /// Indices into `entries` in display order (shuffled, repeated to >= 9).
    order: Vec<usize>,
    current: usize,
    /// Animated carousel position (index units).
    /// Carousel position (unwrapped index; draw() wraps it).
    pos: SpringValue,
    images: HashMap<PathBuf, (*mut CairoSurface, Instant)>,
    requested: std::collections::HashSet<PathBuf>,
    decode_tx: Sender<PathBuf>,
    decoded_rx: Receiver<Decoded>,
    apply_rx: Option<Receiver<Result<(), String>>>,
    applying: bool,
    error: Option<String>,
    hits: Vec<(usize, Vec<(f64, f64)>)>,
    waker: Waker,
    daemon: String,
    scheme: String,
    mode: String,
}

impl WallpaperView {
    pub fn new(cfg: &prism_common::Config, screen_w: f64, waker: Waker) -> Self {
        let dir = expand_home(&cfg.wallpaper_dir);
        let list = prism_common::wallpaper::scan_wallpapers(&dir);
        let thumb_w = (screen_w * 0.35).round().max(320.0) as i32;
        let cache = thumbs_dir();
        let _ = std::fs::create_dir_all(&cache);
        let entries: Vec<Entry> = list
            .into_iter()
            .map(|p| {
                let thumb = cache.join(format!("{:x}.jpg", md5::compute(p.to_string_lossy().as_bytes())));
                Entry { original: p, thumb }
            })
            .collect();

        // Generate missing / stale thumbnails in parallel (background, CPU cores).
        let missing: Vec<(PathBuf, PathBuf)> = entries
            .iter()
            .filter(|e| is_stale(&e.original, &e.thumb))
            .map(|e| (e.original.clone(), e.thumb.clone()))
            .collect();

        let slice_h = (PANEL_H * 0.80).round() as i32;
        let (decode_tx, decode_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (decoded_tx, decoded_rx) = std::sync::mpsc::channel::<Decoded>();
        spawn_workers(missing, thumb_w, decode_rx, decoded_tx, slice_h, waker.clone());

        let mut order: Vec<usize> = (0..entries.len()).collect();
        shuffle(&mut order);
        let base = order.clone();
        while !base.is_empty() && order.len() < 9 {
            order.extend(base.iter().copied());
        }

        WallpaperView {
            entries,
            order,
            current: 0,
            pos: SpringValue::new(0.0),
            images: HashMap::new(),
            requested: Default::default(),
            decode_tx,
            decoded_rx,
            apply_rx: None,
            applying: false,
            error: None,
            hits: Vec::new(),
            waker,
            daemon: cfg.wallpaper_daemon.clone(),
            scheme: cfg.matugen_scheme.clone(),
            mode: cfg.matugen_mode.clone(),
        }
    }

    /// Pull decoded images / apply results. Returns (redraw, close).
    pub fn poll(&mut self) -> (bool, bool) {
        let mut redraw = false;
        for d in self.decoded_rx.try_iter() {
            let s = unsafe { surface_from_argb(&d) };
            if let Some(s) = s {
                self.images.insert(d.key, (s, Instant::now()));
                redraw = true;
            }
        }
        let mut close = false;
        if let Some(rx) = &self.apply_rx {
            if let Ok(r) = rx.try_recv() {
                self.applying = false;
                self.apply_rx = None;
                match r {
                    Ok(()) => close = true,
                    Err(e) => self.error = Some(e),
                }
                redraw = true;
            }
        }
        (redraw, close)
    }

    pub fn animating(&self) -> bool {
        !self.pos.settled(0.002) || self.images.values().any(|(_, t)| t.elapsed().as_secs_f64() * 1000.0 < crate::ui::ANIM_MED_MS)
    }

    fn step(&mut self, delta: i64, style: &Style) {
        let n = self.order.len() as i64;
        if n == 0 {
            return;
        }
        let target = self.current as i64 + delta;
        self.current = target.rem_euclid(n) as usize;
        // Animate in unwrapped space from where we are heading (draw() wraps positions), so
        // quick repeated steps add up and the motion keeps its speed.
        let to = (self.pos.target() + delta as f64).round();
        if style.animations {
            self.pos.set(to, SPRING_MOVE, style);
        } else {
            self.pos.snap(to);
        }
    }

    /// Current (animated) carousel position.
    fn position(&mut self, _style: &Style) -> f64 {
        if self.pos.settled(0.002) { self.pos.target() } else { self.pos.value() }
    }

    pub fn draw(&mut self, cr: *mut Cairo, panel_x: f64, panel_y: f64, width: f64, style: &Style, theme: &prism_common::Theme) {
        self.hits.clear();
        let fg = style.fg;
        if self.order.is_empty() {
            let msg = "No wallpapers found. Set the folder in Settings > Wallpaper.";
            draw_in_box(cr, msg, &TextStyle { family: &style.font_family, px: 16.0, bold: true, color: fg }, panel_x, panel_y, width, PANEL_H, Align::Center);
            return;
        }
        let n = self.order.len();
        let nf = (n.max(9)) as f64;
        let pos = self.position(style);
        let h = PANEL_H * 0.80;
        let cy = panel_y + PANEL_H / 2.0;
        let primary = hex_to_rgba(&theme.primary, 1.0);

        // x (centre) and width-scale along the QML path for an offset d (in items).
        let path = |d: f64| -> (f64, f64) {
            let w = width;
            let s = d.abs();
            let dir = d.signum();
            if s <= 1.0 {
                let x = 0.5 * w + dir * s * 0.225 * w;
                let scale = 0.32 + (0.12 - 0.32) * s;
                (x, scale)
            } else {
                let end = w * (0.1 + 0.0625 * nf); // distance from centre to the path end
                let span = (nf / 2.0 - 1.0).max(1.0);
                let x = 0.5 * w + dir * (0.225 * w + (s - 1.0) / span * (end - 0.225 * w));
                (x, 0.12)
            }
        };

        // Offsets wrapped into (-n/2, n/2]; draw far slices first (zOrder).
        let mut items: Vec<(f64, usize)> = (0..n)
            .map(|k| {
                let mut d = k as f64 - pos;
                d = (d + n as f64 / 2.0).rem_euclid(n as f64) - n as f64 / 2.0;
                (d, k)
            })
            .collect();
        items.sort_by(|a, b| b.0.abs().partial_cmp(&a.0.abs()).unwrap_or(std::cmp::Ordering::Equal));

        for (d, k) in items {
            let (x_centre, scale) = path(d);
            let w = width * scale;
            let x = panel_x + x_centre - w / 2.0;
            if x + w + h * 0.25 < panel_x || x - h * 0.25 > panel_x + width {
                continue;
            }
            let top = cy - h / 2.0;
            // Parallelogram: skew -0.25, shifted right by |h*skew/2| (QML clipItem).
            let sh = (h * SKEW).abs() / 2.0;
            let poly = vec![(x + sh, top), (x + w + sh, top), (x + w - sh, top + h), (x - sh, top + h)];
            let entry = &self.entries[self.order[k]];
            unsafe {
                cairo_save(cr);
                cairo_new_path(cr);
                cairo_move_to(cr, poly[0].0, poly[0].1);
                for p in &poly[1..] {
                    cairo_line_to(cr, p.0, p.1);
                }
                cairo_close_path(cr);
                cairo_clip(cr);
                cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 0.45);
                cairo_paint(cr);
            }
            let key = entry.thumb.clone();
            match self.images.get(&key) {
                Some((img, t)) => {
                    let (iw, ih) = image_size(*img);
                    // Cover the slice's bounding box (w + skew overhang) x h, centred.
                    let box_w = w + 2.0 * sh;
                    let s = (box_w / iw).max(h / ih);
                    let alpha = (t.elapsed().as_secs_f64() * 1000.0 / crate::ui::ANIM_MED_MS).min(1.0);
                    unsafe {
                        cairo_translate(cr, x - sh + (box_w - iw * s) / 2.0, top + (h - ih * s) / 2.0);
                        cairo_scale(cr, s, s);
                        cairo_set_source_surface(cr, *img, 0.0, 0.0);
                        cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
                        cairo_paint_with_alpha(cr, alpha);
                    }
                }
                None => {
                    if self.requested.insert(key.clone()) {
                        let _ = self.decode_tx.send(key);
                    }
                }
            }
            unsafe { cairo_restore(cr) };
            // Border grows from 0 (narrow) to 3 px (centre).
            let bw = ((scale - 0.12) / 0.20 * 3.0).max(0.0);
            if bw > 0.05 {
                unsafe {
                    cairo_new_path(cr);
                    cairo_move_to(cr, poly[0].0, poly[0].1);
                    for p in &poly[1..] {
                        cairo_line_to(cr, p.0, p.1);
                    }
                    cairo_close_path(cr);
                    cairo_set_line_width(cr, bw * 2.0);
                    cairo_set_source_rgba(cr, primary.0, primary.1, primary.2, 1.0);
                    cairo_save(cr);
                    cairo_clip_preserve(cr);
                    cairo_stroke(cr);
                    cairo_restore(cr);
                }
            }
            let is_current = d.abs() < 0.5;
            if is_current && (self.applying || self.error.is_some()) {
                let text = if self.applying { "Applying…".to_string() } else { self.error.clone().unwrap_or_default() };
                unsafe {
                    cairo_save(cr);
                    cairo_new_path(cr);
                    cairo_move_to(cr, poly[0].0, poly[0].1);
                    for p in &poly[1..] {
                        cairo_line_to(cr, p.0, p.1);
                    }
                    cairo_close_path(cr);
                    cairo_clip(cr);
                    cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 0.55);
                    cairo_paint(cr);
                    cairo_restore(cr);
                }
                draw_in_box(cr, &text, &TextStyle { family: &style.font_family, px: 20.0, bold: true, color: (1.0, 1.0, 1.0, 1.0) }, x, top, w, h, Align::Center);
            }
            self.hits.push((k, poly));
        }
        // Topmost slices were drawn last; hit-test them first.
        self.hits.reverse();
    }

    pub fn key(&mut self, code: u32, style: &Style) -> Effect {
        const LEFT: u32 = 105;
        const RIGHT: u32 = 106;
        const ENTER: u32 = 28;
        const ESC: u32 = 1;
        match code {
            ESC => Effect::Close,
            LEFT => {
                self.step(-1, style);
                Effect::Redraw
            }
            RIGHT => {
                self.step(1, style);
                Effect::Redraw
            }
            ENTER => {
                self.apply();
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    pub fn scroll(&mut self, delta: f64, style: &Style) -> Effect {
        if delta.abs() < 0.01 || self.applying {
            return Effect::None;
        }
        self.step(if delta > 0.0 { 1 } else { -1 }, style);
        Effect::Redraw
    }

    pub fn click(&mut self, x: f64, y: f64, style: &Style) -> Effect {
        if self.applying {
            return Effect::None;
        }
        let hit = self.hits.iter().find(|(_, poly)| point_in_poly(x, y, poly)).map(|(k, _)| *k);
        match hit {
            None => Effect::Close,
            Some(k) if k == self.current => {
                self.apply();
                Effect::Redraw
            }
            Some(k) => {
                let n = self.order.len() as i64;
                let mut d = k as i64 - self.current as i64;
                d = (d + n / 2).rem_euclid(n) - n / 2;
                self.step(d, style);
                Effect::Redraw
            }
        }
    }

    fn apply(&mut self) {
        if self.applying || self.order.is_empty() {
            return;
        }
        let path = self.entries[self.order[self.current]].original.clone();
        let (daemon, scheme, mode) = (self.daemon.clone(), self.scheme.clone(), self.mode.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let w = self.waker.clone();
        std::thread::spawn(move || {
            let r = prism_common::wallpaper::set_wallpaper(&path, Some(&daemon), Some(&scheme), Some(&mode));
            let _ = tx.send(r);
            w.wake();
        });
        self.applying = true;
        self.error = None;
        self.apply_rx = Some(rx);
    }
}

impl Drop for WallpaperView {
    fn drop(&mut self) {
        for (s, _) in self.images.values() {
            unsafe { cairo_surface_destroy(*s) };
        }
    }
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

fn thumbs_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"))
        .join("Prism/thumbs")
}

fn is_stale(original: &Path, thumb: &Path) -> bool {
    let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (m(original), m(thumb)) {
        (Some(o), Some(t)) => o > t,
        _ => true,
    }
}

/// Thumbnail generators (one per CPU core) followed by a decoder for on-screen slices.
fn spawn_workers(missing: Vec<(PathBuf, PathBuf)>, thumb_w: i32, decode_rx: Receiver<PathBuf>, out: Sender<Decoded>, slice_h: i32, waker: Waker) {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let jobs = std::sync::Arc::new(std::sync::Mutex::new(missing));
    for _ in 0..threads {
        let jobs = jobs.clone();
        std::thread::spawn(move || loop {
            let job = jobs.lock().ok().and_then(|mut j| j.pop());
            let Some((src, dst)) = job else { break };
            make_thumb(&src, &dst, thumb_w);
        });
    }
    std::thread::spawn(move || {
        for path in decode_rx {
            // The thumbnail may still be in generation; wait for it briefly.
            for _ in 0..50 {
                if path.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            if let Some(d) = decode(&path, slice_h) {
                if out.send(d).is_err() {
                    break;
                }
                waker.wake();
            }
        }
    });
}

fn make_thumb(src: &Path, dst: &Path, width: i32) {
    let (Ok(cs), Ok(cd)) = (CString::new(src.to_string_lossy().as_bytes()), CString::new(dst.with_extension("part").to_string_lossy().as_bytes())) else { return };
    unsafe {
        let pb = gdk_pixbuf_new_from_file_at_scale(cs.as_ptr(), width, -1, 1, std::ptr::null_mut());
        if pb.is_null() {
            return;
        }
        let ty = CString::new("jpeg").unwrap();
        let k = CString::new("quality").unwrap();
        let v = CString::new("85").unwrap();
        let keys = [k.as_ptr(), std::ptr::null()];
        let vals = [v.as_ptr(), std::ptr::null()];
        let ok = gdk_pixbuf_savev(pb, cd.as_ptr(), ty.as_ptr(), keys.as_ptr(), vals.as_ptr(), std::ptr::null_mut());
        g_object_unref(pb as *mut std::ffi::c_void);
        if ok != 0 {
            let _ = std::fs::rename(dst.with_extension("part"), dst);
        }
    }
}

/// Decode a thumbnail scaled to the slice height into premultiplied ARGB32.
fn decode(path: &Path, height: i32) -> Option<Decoded> {
    let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let pb = gdk_pixbuf_new_from_file_at_scale(c.as_ptr(), -1, height, 1, std::ptr::null_mut());
        if pb.is_null() {
            return None;
        }
        let (w, h) = (gdk_pixbuf_get_width(pb), gdk_pixbuf_get_height(pb));
        let stride = gdk_pixbuf_get_rowstride(pb) as usize;
        let n = gdk_pixbuf_get_n_channels(pb) as usize;
        let src = gdk_pixbuf_get_pixels(pb);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = src.add(y * stride + x * n);
                let a = if n == 4 { *p.add(3) as u32 } else { 255 };
                let (r, g, b) = (*p as u32 * a / 255, *p.add(1) as u32 * a / 255, *p.add(2) as u32 * a / 255);
                data.extend_from_slice(&((a << 24) | (r << 16) | (g << 8) | b).to_ne_bytes());
            }
        }
        g_object_unref(pb as *mut std::ffi::c_void);
        Some(Decoded { key: path.to_path_buf(), w, h, data })
    }
}

unsafe fn surface_from_argb(d: &Decoded) -> Option<*mut CairoSurface> {
    let s = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, d.w, d.h);
    cairo_surface_flush(s);
    let dst = cairo_image_surface_get_data(s);
    if dst.is_null() {
        cairo_surface_destroy(s);
        return None;
    }
    let stride = cairo_image_surface_get_stride(s) as usize;
    for y in 0..d.h as usize {
        std::ptr::copy_nonoverlapping(d.data.as_ptr().add(y * d.w as usize * 4), dst.add(y * stride), d.w as usize * 4);
    }
    cairo_surface_mark_dirty(s);
    Some(s)
}

fn shuffle(v: &mut [usize]) {
    // xorshift seeded from the clock: good enough for a random wallpaper order.
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

fn point_in_poly(x: f64, y: f64, poly: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}
