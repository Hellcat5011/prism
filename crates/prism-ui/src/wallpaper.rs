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
//
// Applying opens a strip of colour filters over the carousel: small previews
// of the wallpaper, Normal selected first, and the centre slice shows the
// selected filter large. Previews are made on their own worker thread.
// ─────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use prism_common::filter::Filter;

use crate::cairo_backend::*;
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, measure, Align, TextStyle};
use crate::ui::{Fades, SpringValue, Style};
use crate::uikit::{self, ButtonKind, Palette};
use crate::waker::Waker;

pub const PANEL_H: f64 = 618.0;
const SKEW: f64 = -0.25;
/// Carousel motion (response s, damping): settles with a hint of overshoot.
const SPRING_MOVE: (f64, f64) = (0.36, 0.88);
/// The filter strip sliding in and out, and its selection ring moving between tiles.
const SPRING_STRIP: (f64, f64) = (0.32, 0.86);
/// The large preview changing to another filter.
const SPRING_PREVIEW: (f64, f64) = (0.24, 1.0);
const TILE: f64 = 72.0;
const TILE_MIN: f64 = 44.0;
const TILE_GAP: f64 = 10.0;
const TILE_RADIUS: f64 = 10.0;
const TILE_LABEL_H: f64 = 24.0;
const STRIP_PAD: f64 = 14.0;
const STRIP_BUTTON_W: f64 = 96.0;
const STRIP_BUTTON_H: f64 = 34.0;
/// The switch above the carousel.
const PILL_LABEL: &str = "Use Wallpaper Filters";
const PILL_H: f64 = 40.0;
const PILL_TOP: f64 = 11.0;
const PILL_SWITCH: (f64, f64) = (40.0, 24.0);
/// What the switch does when on and when off.
const PILL_TIP_ON: &str = "Choose a filter each time you apply a wallpaper.";
const PILL_TIP_OFF: &str = "New wallpapers apply right away, with no filter.";
const TIP_PAD: f64 = 12.0;
const TIP_LINE: f64 = 22.0;
const TIP_LABEL_W: f64 = 38.0;
/// Decoded height of the tile previews (drawn at TILE, sharp on scaled outputs).
const TILE_PX: i32 = 160;
/// Hover ids after the tiles.
const HOVER_APPLY: usize = Filter::ALL.len();
const HOVER_BACK: usize = Filter::ALL.len() + 1;

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

enum PreviewJob {
    Tiles(PathBuf),
    Large(PathBuf, Filter),
}

enum PreviewOut {
    Tile(PathBuf, Filter, Decoded),
    Large(PathBuf, Filter, Decoded),
}

/// The open filter strip.
struct FilterStrip {
    /// Thumbnail of the wallpaper being applied.
    thumb: PathBuf,
    selected: usize,
    tiles: HashMap<Filter, *mut CairoSurface>,
    large: HashMap<Filter, *mut CairoSurface>,
    /// The large preview being faded out, and how far the selected one has faded in.
    from: Filter,
    fade: SpringValue,
    /// Selection ring position (tile index).
    ring: SpringValue,
    /// 0 hidden, 1 fully in.
    open: SpringValue,
    closing: bool,
    time_scale: Option<f64>,
    hover: Option<usize>,
    hovers: Fades<usize>,
    bounds: Rect,
    tile_rects: Vec<Rect>,
    apply_rect: Rect,
    back_rect: Rect,
}

impl FilterStrip {
    fn filter(&self) -> Filter {
        Filter::ALL[self.selected]
    }

    fn close(&mut self) {
        self.closing = true;
        self.open.set_scaled(0.0, SPRING_STRIP, self.time_scale);
    }

    fn animating(&self) -> bool {
        !self.open.settled(0.004) || !self.ring.settled(0.004) || !self.fade.settled(0.004)
    }
}

impl Drop for FilterStrip {
    fn drop(&mut self) {
        for s in self.tiles.values().chain(self.large.values()) {
            unsafe { cairo_surface_destroy(*s) };
        }
    }
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
    decode_tx: Sender<(PathBuf, PathBuf)>,
    decoded_rx: Receiver<Decoded>,
    preview_tx: Sender<PreviewJob>,
    preview_rx: Receiver<PreviewOut>,
    strip: Option<FilterStrip>,
    filters_on: bool,
    pill_rect: Rect,
    pill_hover: bool,
    pill_fades: Fades<usize>,
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
        let (decode_tx, decode_rx) = std::sync::mpsc::channel::<(PathBuf, PathBuf)>();
        let (decoded_tx, decoded_rx) = std::sync::mpsc::channel::<Decoded>();
        spawn_workers(missing, thumb_w, decode_rx, decoded_tx, slice_h, waker.clone());
        let (preview_tx, preview_jobs) = std::sync::mpsc::channel::<PreviewJob>();
        let (preview_out, preview_rx) = std::sync::mpsc::channel::<PreviewOut>();
        spawn_preview_worker(preview_jobs, preview_out, slice_h, waker.clone());

        let mut order: Vec<usize> = (0..entries.len()).collect();
        shuffle(&mut order);
        let base = order.clone();
        while !base.is_empty() && order.len() < 9 {
            order.extend(base.iter().copied());
        }

        // Fresh from disk: Settings may have changed it since this overlay's config was read.
        let switches = prism_common::Config::load();
        WallpaperView {
            entries,
            order,
            current: 0,
            pos: SpringValue::new(0.0),
            images: HashMap::new(),
            requested: Default::default(),
            decode_tx,
            decoded_rx,
            preview_tx,
            preview_rx,
            strip: None,
            filters_on: switches.wallpaper_filters_enabled,
            pill_rect: Rect::default(),
            pill_hover: false,
            pill_fades: Fades::default(),
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
        for out in self.preview_rx.try_iter() {
            let (thumb, filter, d, is_tile) = match out {
                PreviewOut::Tile(t, f, d) => (t, f, d, true),
                PreviewOut::Large(t, f, d) => (t, f, d, false),
            };
            let Some(s) = (unsafe { surface_from_argb(&d) }) else { continue };
            match self.strip.as_mut().filter(|p| p.thumb == thumb) {
                Some(p) if is_tile => {
                    p.tiles.insert(filter, s);
                }
                Some(p) => {
                    p.large.insert(filter, s);
                    if p.filter() == filter {
                        p.fade.set_scaled(1.0, SPRING_PREVIEW, p.time_scale);
                    }
                }
                None => unsafe { cairo_surface_destroy(s) },
            }
            redraw = true;
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
        !self.pos.settled(0.002) || self.strip.as_ref().is_some_and(|p| p.animating()) || self.images.values().any(|(_, t)| t.elapsed().as_secs_f64() * 1000.0 < crate::ui::ANIM_MED_MS)
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

    /// The strip is open and takes the input.
    fn picking(&self) -> bool {
        self.strip.as_ref().is_some_and(|p| !p.closing)
    }

    /// Apply the current wallpaper: through the filter strip when filters are on.
    fn choose(&mut self, style: &Style) {
        if self.filters_on {
            self.open_strip(style);
        } else {
            self.apply();
        }
    }

    fn toggle_filters(&mut self) {
        self.filters_on = !self.filters_on;
        if !self.filters_on {
            if let Some(p) = self.strip.as_mut() {
                p.close();
            }
        }
        let filters_on = self.filters_on;
        std::thread::spawn(move || {
            let mut c = prism_common::Config::load();
            c.wallpaper_filters_enabled = filters_on;
            let _ = c.save();
        });
    }

    fn draw_pill(&mut self, cr: *mut Cairo, panel_x: f64, panel_y: f64, width: f64, style: &Style, theme: &prism_common::Theme) {
        let palette = Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: hex_to_rgba(&theme.on_primary, 1.0),
            outline: hex_to_rgba(&theme.outline_variant, 1.0),
            error: hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        };
        let label = TextStyle { family: &style.font_family, px: 13.0, bold: true, color: style.fg };
        // Label, its padding, the switch and the space around it.
        let w = measure(cr, PILL_LABEL, &label).0.ceil() + 18.0 + 14.0 + PILL_SWITCH.0 + 8.0;
        let r = Rect { x: panel_x + (width - w) / 2.0, y: panel_y + PILL_TOP, w, h: PILL_H };
        self.pill_rect = r;
        let knob = self.pill_fades.settled_level(0, self.filters_on, crate::ui::SPRING_KNOB, style);
        let hover = self.pill_fades.level(1, self.pill_hover, style);
        fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, (0.0, 0.0, 0.0, 0.6));
        fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, style.card_bg);
        if hover > 0.0 {
            fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, (style.fg.0, style.fg.1, style.fg.2, 0.08 * hover));
        }
        draw_in_box(cr, PILL_LABEL, &label, r.x + 18.0, r.y, r.w - 18.0 - PILL_SWITCH.0 - 8.0, r.h, Align::Start);
        let sw = Rect { x: r.x + r.w - PILL_SWITCH.0 - 8.0, y: r.y + (r.h - PILL_SWITCH.1) / 2.0, w: PILL_SWITCH.0, h: PILL_SWITCH.1 };
        uikit::switch(cr, sw, knob, hover, &palette);
        if hover > 0.01 {
            self.draw_pill_tip(cr, hover, panel_x, width, style);
        }
    }

    /// What the switch does in each position; its current one stands out.
    fn draw_pill_tip(&self, cr: *mut Cairo, level: f64, panel_x: f64, width: f64, style: &Style) {
        let text = |bold: bool, color| TextStyle { family: &style.font_family, px: 12.0, bold, color };
        let lines = [("ON", PILL_TIP_ON, self.filters_on), ("OFF", PILL_TIP_OFF, !self.filters_on)];
        let widest = lines.iter().map(|l| measure(cr, l.1, &text(true, style.fg)).0).fold(0.0, f64::max);
        let w = (2.0 * TIP_PAD + TIP_LABEL_W + widest.ceil()).min(width - 16.0);
        let h = 2.0 * TIP_PAD + lines.len() as f64 * TIP_LINE;
        let pill = self.pill_rect;
        let x = (pill.x + (pill.w - w) / 2.0).clamp(panel_x + 8.0, panel_x + width - w - 8.0);
        let y = pill.y + pill.h + 8.0 - (1.0 - level) * 6.0;
        unsafe { cairo_push_group(cr) };
        drop_shadow(cr, x, y, w, h, 12.0, 14.0, 4.0, 0.3);
        fill_rounded_rect(cr, x, y, w, h, 12.0, (0.0, 0.0, 0.0, 0.82));
        fill_rounded_rect(cr, x, y, w, h, 12.0, style.card_bg);
        let mut ly = y + TIP_PAD;
        for (label, tip, current) in lines {
            let a = if current { 1.0 } else { 0.6 };
            let label_color = if current { style.accent } else { (style.fg.0, style.fg.1, style.fg.2, 0.6) };
            draw_in_box(cr, label, &text(true, label_color), x + TIP_PAD, ly, TIP_LABEL_W, TIP_LINE, Align::Start);
            draw_in_box(cr, tip, &text(current, (style.fg.0, style.fg.1, style.fg.2, a)), x + TIP_PAD + TIP_LABEL_W, ly, w - 2.0 * TIP_PAD - TIP_LABEL_W, TIP_LINE, Align::Start);
            ly += TIP_LINE;
        }
        unsafe {
            cairo_pop_group_to_source(cr);
            cairo_paint_with_alpha(cr, level.clamp(0.0, 1.0));
        }
    }

    /// The pointer left the selector.
    pub fn leave(&mut self) -> bool {
        let strip = self.strip.as_mut().is_some_and(|p| p.hover.take().is_some());
        std::mem::take(&mut self.pill_hover) || strip
    }

    fn open_strip(&mut self, style: &Style) {
        if self.applying || self.order.is_empty() {
            return;
        }
        let thumb = self.entries[self.order[self.current]].thumb.clone();
        let time_scale = style.time_scale();
        let mut open = SpringValue::new(0.0);
        open.set_scaled(1.0, SPRING_STRIP, time_scale);
        let _ = self.preview_tx.send(PreviewJob::Tiles(thumb.clone()));
        self.error = None;
        self.strip = Some(FilterStrip {
            thumb,
            selected: 0,
            tiles: HashMap::new(),
            large: HashMap::new(),
            from: Filter::Normal,
            fade: SpringValue::new(1.0),
            ring: SpringValue::new(0.0),
            open,
            closing: false,
            time_scale,
            hover: None,
            hovers: Fades::default(),
            bounds: Rect::default(),
            tile_rects: Vec::new(),
            apply_rect: Rect::default(),
            back_rect: Rect::default(),
        });
    }

    fn select_filter(&mut self, i: usize) {
        let Some(p) = self.strip.as_mut() else { return };
        if i == p.selected {
            return;
        }
        // Fade from whichever picture is showing more right now.
        if p.fade.value() >= 0.5 {
            p.from = p.filter();
        }
        p.selected = i;
        p.ring.set_scaled(i as f64, SPRING_STRIP, p.time_scale);
        p.fade.snap(0.0);
        let f = p.filter();
        if f == Filter::Normal || p.large.contains_key(&f) {
            p.fade.set_scaled(1.0, SPRING_PREVIEW, p.time_scale);
        } else {
            let _ = self.preview_tx.send(PreviewJob::Large(p.thumb.clone(), f));
        }
    }

    fn step_filter(&mut self, delta: i64) {
        let Some(p) = self.strip.as_ref() else { return };
        let n = Filter::ALL.len() as i64;
        self.select_filter((p.selected as i64 + delta).rem_euclid(n) as usize);
    }

    fn draw_strip(&mut self, cr: *mut Cairo, panel_x: f64, panel_y: f64, width: f64, style: &Style, theme: &prism_common::Theme) {
        let Some(p) = self.strip.as_mut() else { return };
        let n = Filter::ALL.len() as f64;
        let chrome = 2.0 * STRIP_PAD + (n - 1.0) * TILE_GAP + 2.0 * TILE_GAP + STRIP_BUTTON_W;
        let tile = ((width - 32.0 - chrome) / n).clamp(TILE_MIN, TILE);
        let strip_w = chrome + n * tile;
        let strip_h = 2.0 * STRIP_PAD + tile + TILE_LABEL_H;
        let open = p.open.animated(0.004).clamp(0.0, 1.2);
        let x = panel_x + (width - strip_w) / 2.0;
        let y = panel_y + PANEL_H - strip_h - 8.0 + (1.0 - open) * 28.0;
        p.bounds = Rect { x, y, w: strip_w, h: strip_h };
        let faded = open < 0.995;
        if faded {
            unsafe { cairo_push_group(cr) };
        }
        fill_rounded_rect(cr, x, y, strip_w, strip_h, style.radius, (0.0, 0.0, 0.0, 0.6));
        crate::ui::card_surface(cr, x, y, strip_w, strip_h, style);

        let accent = style.accent;
        let tiles_y = y + STRIP_PAD;
        p.tile_rects.clear();
        for (i, f) in Filter::ALL.into_iter().enumerate() {
            let r = Rect { x: x + STRIP_PAD + i as f64 * (tile + TILE_GAP), y: tiles_y, w: tile, h: tile };
            p.tile_rects.push(r);
            unsafe {
                cairo_save(cr);
                cairo_new_path(cr);
                rounded_rect_path(cr, r.x, r.y, r.w, r.h, TILE_RADIUS);
                cairo_clip(cr);
                cairo_set_source_rgba(cr, 1.0, 1.0, 1.0, 0.08);
                cairo_paint(cr);
                if let Some(img) = p.tiles.get(&f) {
                    let (iw, ih) = image_size(*img);
                    let s = (r.w / iw).max(r.h / ih);
                    cairo_translate(cr, r.x + (r.w - iw * s) / 2.0, r.y + (r.h - ih * s) / 2.0);
                    cairo_scale(cr, s, s);
                    cairo_set_source_surface(cr, *img, 0.0, 0.0);
                    cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
                    cairo_paint(cr);
                }
                cairo_restore(cr);
            }
            let hover = p.hovers.level_scaled(i, p.hover == Some(i), p.time_scale);
            if hover > 0.0 {
                stroke_rounded_rect(cr, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, TILE_RADIUS, (1.0, 1.0, 1.0, 0.45 * hover), 1.0);
            }
            let on = (1.0 - (p.ring.value() - i as f64).abs()).clamp(0.0, 1.0);
            let c = (style.fg.0 + (accent.0 - style.fg.0) * on, style.fg.1 + (accent.1 - style.fg.1) * on, style.fg.2 + (accent.2 - style.fg.2) * on, 0.72 + 0.28 * on);
            draw_in_box(cr, f.label(), &TextStyle { family: &style.font_family, px: 11.5, bold: i == p.selected, color: c }, r.x - TILE_GAP / 2.0, r.y + r.h + 4.0, r.w + TILE_GAP, TILE_LABEL_H - 4.0, Align::Center);
        }
        let ring_x = x + STRIP_PAD + p.ring.animated(0.004) * (tile + TILE_GAP);
        stroke_rounded_rect(cr, ring_x - 1.5, tiles_y - 1.5, tile + 3.0, tile + 3.0, TILE_RADIUS + 1.5, accent, 2.5);

        let palette = Palette {
            fg: style.fg,
            accent,
            card: style.card_bg,
            on_accent: hex_to_rgba(&theme.on_primary, 1.0),
            outline: hex_to_rgba(&theme.outline_variant, 1.0),
            error: hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        };
        let button_x = x + strip_w - STRIP_PAD - STRIP_BUTTON_W;
        let mid = y + strip_h / 2.0;
        p.apply_rect = Rect { x: button_x, y: mid - STRIP_BUTTON_H - 5.0, w: STRIP_BUTTON_W, h: STRIP_BUTTON_H };
        p.back_rect = Rect { x: button_x, y: mid + 5.0, w: STRIP_BUTTON_W, h: STRIP_BUTTON_H };
        let (apply_h, back_h) = (p.hovers.level_scaled(HOVER_APPLY, p.hover == Some(HOVER_APPLY), p.time_scale), p.hovers.level_scaled(HOVER_BACK, p.hover == Some(HOVER_BACK), p.time_scale));
        uikit::button(cr, p.apply_rect, "Apply", ButtonKind::Primary, apply_h, true, &palette);
        uikit::button(cr, p.back_rect, "Back", ButtonKind::Outline, back_h, true, &palette);
        if faded {
            unsafe {
                cairo_pop_group_to_source(cr);
                cairo_paint_with_alpha(cr, open.clamp(0.0, 1.0));
            }
        }
    }

    /// Pointer moved. Returns (redraw, over something clickable).
    pub fn motion(&mut self, x: f64, y: f64) -> (bool, bool) {
        let pill_over = self.pill_rect.contains(x, y);
        let pill_changed = pill_over != self.pill_hover;
        self.pill_hover = pill_over;
        if !self.picking() {
            return (pill_changed, pill_over);
        }
        let Some(p) = self.strip.as_mut() else { return (pill_changed, pill_over) };
        let hover = p.tile_rects.iter().position(|r| r.contains(x, y)).or_else(|| {
            if p.apply_rect.contains(x, y) {
                Some(HOVER_APPLY)
            } else if p.back_rect.contains(x, y) {
                Some(HOVER_BACK)
            } else {
                None
            }
        });
        let changed = hover != p.hover;
        p.hover = hover;
        (changed || pill_changed, hover.is_some() || pill_over)
    }

    pub fn draw(&mut self, cr: *mut Cairo, panel_x: f64, panel_y: f64, width: f64, style: &Style, theme: &prism_common::Theme) {
        self.hits.clear();
        if !self.applying && self.strip.as_ref().is_some_and(|p| p.closing && p.open.settled(0.01)) {
            self.strip = None;
        }
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
                    // The selected filter, large, on the wallpaper being applied.
                    if let Some(p) = self.strip.as_ref().filter(|p| k == self.current && p.thumb == key) {
                        let target = p.filter();
                        let paint = |s: *mut CairoSurface, a: f64| unsafe {
                            cairo_set_source_surface(cr, s, 0.0, 0.0);
                            cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
                            cairo_paint_with_alpha(cr, a);
                        };
                        if let Some(s) = p.large.get(&p.from) {
                            paint(*s, 1.0);
                        }
                        let fade = p.fade.animated(0.004).clamp(0.0, 1.0);
                        match p.large.get(&target) {
                            Some(s) => paint(*s, fade),
                            None if target == Filter::Normal => paint(*img, fade),
                            None => {}
                        }
                    }
                }
                None => {
                    if self.requested.insert(key.clone()) {
                        let _ = self.decode_tx.send((key, entry.original.clone()));
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
        self.draw_strip(cr, panel_x, panel_y, width, style, theme);
        self.draw_pill(cr, panel_x, panel_y, width, style, theme);
    }

    pub fn key(&mut self, code: u32, style: &Style) -> Effect {
        const LEFT: u32 = 105;
        const RIGHT: u32 = 106;
        const ENTER: u32 = 28;
        const ESC: u32 = 1;
        if self.picking() {
            match code {
                ESC => self.strip.as_mut().unwrap().close(),
                LEFT => self.step_filter(-1),
                RIGHT => self.step_filter(1),
                ENTER => self.apply(),
                _ => return Effect::None,
            }
            return Effect::Redraw;
        }
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
                self.choose(style);
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    pub fn scroll(&mut self, delta: f64, style: &Style) -> Effect {
        if delta.abs() < 0.01 || self.applying {
            return Effect::None;
        }
        if self.picking() {
            self.step_filter(if delta > 0.0 { 1 } else { -1 });
            return Effect::Redraw;
        }
        self.step(if delta > 0.0 { 1 } else { -1 }, style);
        Effect::Redraw
    }

    pub fn click(&mut self, x: f64, y: f64, style: &Style) -> Effect {
        if self.applying {
            return Effect::None;
        }
        if self.pill_rect.contains(x, y) {
            self.toggle_filters();
            return Effect::Redraw;
        }
        let hit = self.hits.iter().find(|(_, poly)| point_in_poly(x, y, poly)).map(|(k, _)| *k);
        if self.picking() {
            let p = self.strip.as_mut().unwrap();
            if let Some(i) = p.tile_rects.iter().position(|r| r.contains(x, y)) {
                self.select_filter(i);
            } else if p.apply_rect.contains(x, y) {
                self.apply();
            } else if p.back_rect.contains(x, y) {
                p.close();
            } else if p.bounds.contains(x, y) {
                return Effect::None;
            } else if hit == Some(self.current) {
                self.apply();
            } else {
                p.close();
            }
            return Effect::Redraw;
        }
        match hit {
            None => Effect::Close,
            Some(k) if k == self.current => {
                self.choose(style);
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
        let filter = self.strip.as_ref().map(|p| p.filter()).unwrap_or_default();
        if let Some(p) = self.strip.as_mut() {
            p.close();
        }
        let (daemon, scheme, mode) = (self.daemon.clone(), self.scheme.clone(), self.mode.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let w = self.waker.clone();
        std::thread::spawn(move || {
            let r = prism_common::wallpaper::render_filtered(&path, filter).and_then(|shown| {
                if daemon == "prism" {
                    crate::background::request_show(&shown);
                }
                prism_common::wallpaper::set_wallpaper_filtered(&path, filter, Some(&daemon), Some(&scheme), Some(&mode))
            });
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
fn spawn_workers(missing: Vec<(PathBuf, PathBuf)>, thumb_w: i32, decode_rx: Receiver<(PathBuf, PathBuf)>, out: Sender<Decoded>, slice_h: i32, waker: Waker) {
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
        for (path, original) in decode_rx {
            // The thumbnail may still be in generation; wait for it briefly.
            for _ in 0..50 {
                if path.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            // Older thumbnails of pictures with transparency were saved without their colour.
            if prism_common::pixbuf::jpeg_components(&path) == Some(1) {
                make_thumb(&original, &path, thumb_w);
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

/// Filtered previews for the strip: every tile at once, large ones on demand.
fn spawn_preview_worker(jobs: Receiver<PreviewJob>, out: Sender<PreviewOut>, slice_h: i32, waker: Waker) {
    std::thread::spawn(move || {
        // The last large decode: stepping through filters reuses it.
        let mut base: Option<Decoded> = None;
        for job in jobs {
            match job {
                PreviewJob::Tiles(thumb) => {
                    let Some(d) = decode(&thumb, TILE_PX) else { continue };
                    for f in Filter::ALL {
                        if out.send(PreviewOut::Tile(thumb.clone(), f, filtered(&d, f))).is_err() {
                            return;
                        }
                    }
                }
                PreviewJob::Large(thumb, f) => {
                    if base.as_ref().is_none_or(|b| b.key != thumb) {
                        base = decode(&thumb, slice_h);
                    }
                    let Some(d) = &base else { continue };
                    if out.send(PreviewOut::Large(thumb, f, filtered(d, f))).is_err() {
                        return;
                    }
                }
            }
            waker.wake();
        }
    });
}

/// A copy of an opaque decoded image with `filter` applied.
fn filtered(d: &Decoded, filter: Filter) -> Decoded {
    let rgb = |px: &[u8]| {
        let v = u32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
    };
    let grade = filter.grade(d.data.chunks_exact(4).step_by(16).map(rgb));
    let mut data = d.data.clone();
    for px in data.chunks_exact_mut(4) {
        let v = u32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
        let [r, g, b] = grade.apply(rgb(px));
        let v = (v & 0xFF00_0000) | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        px.copy_from_slice(&v.to_ne_bytes());
    }
    Decoded { key: d.key.clone(), w: d.w, h: d.h, data }
}

fn make_thumb(src: &Path, dst: &Path, width: i32) {
    if let Err(e) = prism_common::pixbuf::save_jpeg_fit_width(src, dst, width, 85) {
        eprintln!("[prism-ui/wallpaper] thumbnail: {e}");
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
