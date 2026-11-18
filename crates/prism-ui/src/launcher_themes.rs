//! Alternative launcher designs, chosen with `launcher_theme` (Settings > UI). Classic stays in
//! launcher.rs; everything else about the themes is in this file.
//!
//! * Slices: the wallpaper cut into strips, one app each; the selected strip opens wide.
//! * Spectrum: a mosaic of apps sorted by the colour of their icons.
//! * Deck: a fanned hand of cards tinted from each icon; the selected card comes forward.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use prism_common::apps::{search_apps, DesktopApp};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{self, Effect, Query, DOWN, END, ENTER, ESC, HOME, LEFT, MOUSE_READY, PGDN, PGUP, RIGHT, UP};
use crate::ui::{self, Fades, Rgba, SpringValue, Style};
use crate::view::CardView;

/// Label and config id of every launcher design, for Settings.
pub const THEMES: &[(&str, &str)] = &[("Classic", "classic"), ("Slices", "slices"), ("Spectrum", "spectrum"), ("Deck", "deck")];

const SLICES_SIZE: (f64, f64) = (1120.0, 600.0);
const SPECTRUM_SIZE: (f64, f64) = (1040.0, 640.0);
const DECK_SIZE: (f64, f64) = (1120.0, 620.0);

/// Card size of the launcher design `id`.
pub fn card_size(id: &str) -> (f64, f64) {
    match id {
        "slices" => SLICES_SIZE,
        "spectrum" => SPECTRUM_SIZE,
        "deck" => DECK_SIZE,
        _ => (launcher::CARD_W, launcher::CARD_H),
    }
}

/// Size the design `id` draws the wallpaper at.
pub fn wallpaper_size(id: &str) -> (f64, f64) {
    if id == "slices" { SLICES_SIZE } else { (launcher::panel_w(launcher::CARD_W), launcher::CARD_H) }
}

/// The launcher view for the design `id`.
pub fn view(id: &str, apps: Vec<DesktopApp>) -> Box<dyn CardView> {
    match id {
        "slices" => Box::new(SlicesLauncher::new(apps)),
        "spectrum" => Box::new(SpectrumLauncher::new(apps)),
        "deck" => Box::new(DeckLauncher::new(apps)),
        _ => Box::new(launcher::LauncherView::new(apps)),
    }
}

/// Works out icon colours ahead of the first open (runs on the preload thread).
pub fn prepare(id: &str, apps: &[DesktopApp]) {
    if matches!(id, "spectrum" | "deck") {
        compute_tints(apps);
    }
}

const SPRING_OPEN: (f64, f64) = (0.42, 0.82);
const SPRING_FAN: (f64, f64) = (0.44, 0.70);
const SPRING_TILE: (f64, f64) = (0.46, 0.84);
/// Wheel travel (design px) that moves the selection by one app.
const WHEEL_STEP: f64 = 30.0;
const INK: Rgba = (0.97, 0.97, 0.97, 1.0);

// ── Icon colours ─────────────────────────────────────────────────────────

/// The colour an icon reads as.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Tint {
    rgb: (f64, f64, f64),
    hue: f64,
    /// Has a colour of its own; greys sort after the rainbow.
    colourful: bool,
    value: f64,
}

const NEUTRAL: Tint = Tint { rgb: (0.45, 0.45, 0.48), hue: 0.0, colourful: false, value: 0.48 };

static TINTS: LazyLock<Mutex<HashMap<String, Tint>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Counts finished batches, so open views know to look again.
static TINT_BATCH: AtomicUsize = AtomicUsize::new(0);
static TINTS_BUSY: AtomicBool = AtomicBool::new(false);

fn hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    if delta <= f64::EPSILON {
        return (0.0, 0.0, max);
    }
    let h = if max == r {
        (g - b) / delta
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    ((h * 60.0).rem_euclid(360.0), delta / max, max)
}

/// Reads the main colour of an icon from its premultiplied ARGB32 pixels.
fn tint_of(pixels: &[u8]) -> Tint {
    const BINS: usize = 36;
    let mut samples = Vec::new();
    for p in pixels.as_chunks::<4>().0 {
        let v = u32::from_ne_bytes([p[0], p[1], p[2], p[3]]);
        let a = (v >> 24) as f64;
        if a < 40.0 {
            continue;
        }
        let channel = |shift: u32| (((v >> shift) & 255) as f64 / a).min(1.0);
        samples.push((channel(16), channel(8), channel(0)));
    }
    if samples.is_empty() {
        return NEUTRAL;
    }
    let mean = |set: &[(f64, f64, f64)]| {
        let n = set.len().max(1) as f64;
        let sum = set.iter().fold((0.0, 0.0, 0.0), |s, c| (s.0 + c.0, s.1 + c.1, s.2 + c.2));
        (sum.0 / n, sum.1 / n, sum.2 / n)
    };
    let mut hist = [0.0; BINS];
    let mut vivid = 0usize;
    for &(r, g, b) in &samples {
        let (h, s, v) = hsv(r, g, b);
        if s > 0.25 && v > 0.2 {
            hist[(h / 10.0) as usize % BINS] += s * v;
            vivid += 1;
        }
    }
    if vivid * 8 < samples.len() {
        let rgb = mean(&samples);
        return Tint { rgb, hue: 0.0, colourful: false, value: hsv(rgb.0, rgb.1, rgb.2).2 };
    }
    let score = |i: usize| hist[(i + BINS - 1) % BINS] + 2.0 * hist[i] + hist[(i + 1) % BINS];
    let best = (0..BINS).max_by(|a, b| score(*a).total_cmp(&score(*b))).unwrap_or(0);
    let key = best as f64 * 10.0 + 5.0;
    let near: Vec<(f64, f64, f64)> = samples
        .iter()
        .copied()
        .filter(|&(r, g, b)| {
            let (h, s, v) = hsv(r, g, b);
            let d = (h - key).abs();
            s > 0.25 && v > 0.2 && d.min(360.0 - d) <= 20.0
        })
        .collect();
    let rgb = mean(&near);
    let (hue, _, value) = hsv(rgb.0, rgb.1, rgb.2);
    Tint { rgb, hue, colourful: true, value }
}

fn icon_name(app: &DesktopApp) -> &str {
    app.icon.as_deref().unwrap_or("")
}

/// Decodes the icons whose colour is not known yet. Safe to run on any thread.
fn compute_tints(apps: &[DesktopApp]) {
    for app in apps {
        let name = icon_name(app);
        if TINTS.lock().map_or(true, |t| t.contains_key(name)) {
            continue;
        }
        let path = [name, "application-x-executable"].iter().filter(|n| !n.is_empty()).find_map(|n| crate::icons::lookup(n, 32));
        let decoded = path.and_then(|p| cb::decode_image(&p.to_string_lossy(), 32, 32, true));
        let tint = decoded.map_or(NEUTRAL, |d| tint_of(d.pixels()));
        if let Ok(mut t) = TINTS.lock() {
            t.insert(name.to_string(), tint);
        }
    }
    TINT_BATCH.fetch_add(1, Ordering::SeqCst);
}

/// Starts working out the colours of `apps` in the background when some are missing.
fn ensure_tints(apps: &[DesktopApp]) {
    let missing = TINTS.lock().is_ok_and(|t| apps.iter().any(|a| !t.contains_key(icon_name(a))));
    if !missing || TINTS_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let apps = apps.to_vec();
    let spawned = std::thread::Builder::new().name("prism-tints".into()).spawn(move || {
        compute_tints(&apps);
        TINTS_BUSY.store(false, Ordering::SeqCst);
    });
    if spawned.is_err() {
        TINTS_BUSY.store(false, Ordering::SeqCst);
    }
}

fn tint(app: &DesktopApp) -> Tint {
    TINTS.lock().ok().and_then(|t| t.get(icon_name(app)).copied()).unwrap_or(NEUTRAL)
}

// ── Shared by the designs ────────────────────────────────────────────────

/// Query, results, selection and pointer state every design has.
struct Picker {
    apps: Vec<DesktopApp>,
    results: Vec<DesktopApp>,
    query: Query,
    selected: usize,
    opened: Instant,
    mouse_active: bool,
    pointer: (f64, f64),
    /// Clickable places from the last draw, where they will be once settled.
    hits: Vec<(usize, Rect)>,
    wheel: f64,
    /// With at least this many results, stepping past either end comes round to the other.
    wraps_from: usize,
}

impl Picker {
    fn new(apps: Vec<DesktopApp>) -> Self {
        let results = apps.clone();
        Self { apps, results, query: Query::default(), selected: 0, opened: Instant::now(), mouse_active: false, pointer: (-1.0, -1.0), hits: Vec::new(), wheel: 0.0, wraps_from: usize::MAX }
    }

    fn filter(&mut self) {
        self.results = search_apps(&self.apps, &self.query.text);
        self.selected = 0;
    }

    fn launch(&self, i: usize) -> Effect {
        if let Some(app) = self.results.get(i) {
            match app.launch() {
                Ok(child) => crate::reap(child),
                Err(e) => eprintln!("[prism-ui] cannot launch {}: {}", app.name, e),
            }
            return Effect::Close;
        }
        Effect::None
    }

    fn select(&mut self, i: usize) {
        if !self.results.is_empty() {
            self.selected = i.min(self.results.len() - 1);
        }
    }

    fn wraps(&self) -> bool {
        self.results.len() >= self.wraps_from
    }

    fn step(&mut self, delta: i32) {
        let next = self.selected as i32 + delta;
        self.select(if self.wraps() { next.rem_euclid(self.results.len() as i32) } else { next.max(0) } as usize);
    }

    fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }

    fn clickable(&self) -> bool {
        self.hit(self.pointer.0, self.pointer.1).is_some()
    }

    /// Records the pointer; true once it may select things. The first report only says where the
    /// pointer rests, which must not take the selection from the keyboard.
    fn pointer_moved(&mut self, x: f64, y: f64) -> bool {
        let moved = self.pointer != (-1.0, -1.0) && (x, y) != self.pointer;
        self.pointer = (x, y);
        if moved && !self.mouse_active && self.opened.elapsed() >= MOUSE_READY {
            self.mouse_active = true;
        }
        self.mouse_active
    }

    /// Hover selects what is under the pointer; true when the selection changed.
    fn hover(&mut self, x: f64, y: f64) -> bool {
        if !self.pointer_moved(x, y) {
            return false;
        }
        match self.hit(x, y) {
            Some(i) if i != self.selected => {
                self.selected = i;
                true
            }
            _ => false,
        }
    }

    /// The wheel moves the selection one app per `WHEEL_STEP`.
    fn wheel(&mut self, delta: f64) -> bool {
        self.wheel += delta;
        let steps = (self.wheel / WHEEL_STEP).trunc() as i32;
        if steps == 0 {
            return false;
        }
        self.wheel -= steps as f64 * WHEEL_STEP;
        let before = self.selected;
        self.step(steps);
        self.selected != before
    }

    /// Keys of designs that lay their apps out in one line. None: the key edits the query.
    fn line_key(&mut self, special: u32, ctrl: bool, page: i32) -> Option<Effect> {
        match special {
            ESC => return Some(Effect::Close),
            ENTER => return Some(self.launch(self.selected)),
            HOME if !ctrl => self.select(0),
            END if !ctrl => self.select(self.results.len().saturating_sub(1)),
            LEFT | UP if !ctrl || special == UP => self.step(-1),
            RIGHT | DOWN if !ctrl || special == DOWN => self.step(1),
            PGUP => self.step(-page),
            PGDN => self.step(page),
            _ => return None,
        }
        Some(Effect::Redraw)
    }
}

fn faded(c: Rgba, alpha: f64) -> Rgba {
    (c.0, c.1, c.2, alpha)
}

fn shade(c: (f64, f64, f64), k: f64, alpha: f64) -> Rgba {
    (c.0 * k, c.1 * k, c.2 * k, alpha)
}

fn centred_text(cr: *mut Cairo, r: Rect, text: &str, font: &str, color: Rgba) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 4000, Some(1));
    cb::draw_pango_text(cr, r.x + (r.w - w as f64) / 2.0, r.y + (r.h - h as f64) / 2.0, text, font, color, None);
}

/// What an app does, else what kind of app it is.
fn about(app: &DesktopApp) -> String {
    app.comment.clone().filter(|c| !c.is_empty()).or_else(|| app.generic_name.clone()).unwrap_or_default()
}

/// An app's icon at (x, y), `alpha` visible.
fn icon(cr: *mut Cairo, app: &DesktopApp, x: f64, y: f64, size: f64, alpha: f64) {
    let name = icon_name(app);
    if alpha >= 0.999 {
        launcher::draw_icon(cr, name, "application-x-executable", x, y, size);
    } else if alpha > 0.004 {
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, x, y, size, size);
            cb::cairo_clip(cr);
            cb::cairo_push_group(cr);
        }
        launcher::draw_icon(cr, name, "application-x-executable", x, y, size);
        unsafe {
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, alpha);
            cb::cairo_restore(cr);
        }
    }
}

/// The search field that floats over a design, at (x, y).
fn search_field(cr: *mut Cairo, query: &Query, x: f64, y: f64, w: f64, ink: Rgba, field: Rgba) {
    cb::fill_rounded_rect(cr, x, y, w, 44.0, 12.0, field);
    launcher::search_icon(cr, x + 13.0, y + 13.0, 18.0, faded(ink, 0.8), 2.0);
    launcher::draw_query(cr, query, "Search apps…", x + 40.0, y + 10.0, w - 52.0, 24.0, "Inter, Sans 16px", ink, faded(ink, 0.7));
}

/// "3 / 44" in a small plate that ends at `right`.
fn counter(cr: *mut Cairo, selected: usize, n: usize, right: f64, y: f64, ink: Rgba, plate: Rgba) {
    let label = format!("{} / {n}", selected + 1);
    let font = "Inter, Sans Medium 13px";
    let (tw, _) = cb::measure_pango_text(cr, &label, font, 4000, Some(1));
    let w = tw as f64 + 24.0;
    cb::fill_rounded_rect(cr, right - w, y, w, 32.0, 10.0, plate);
    cb::draw_pango_text(cr, right - w + 12.0, y + 8.0, &label, font, ink, None);
}

// ── Slices ───────────────────────────────────────────────────────────────

const SLICES_SHOWN: usize = 9;
const SLICE_NARROW: f64 = 76.0;

/// First strip of the window of `shown` strips that keeps `selected` in view.
fn window_first(first: usize, selected: usize, n: usize, shown: usize) -> usize {
    let first = if selected < first {
        selected
    } else if selected >= first + shown {
        selected + 1 - shown
    } else {
        first
    };
    first.min(n.saturating_sub(shown))
}

/// Width of the open strip when `n` strips share `card_w`.
fn slice_wide(card_w: f64, n: usize) -> f64 {
    card_w - n.clamp(1, SLICES_SHOWN).saturating_sub(1) as f64 * SLICE_NARROW
}

struct SlicesLauncher {
    picker: Picker,
    levels: Fades<String>,
    /// First strip of the window, and the strip row's offset.
    first: usize,
    slide: SpringValue,
}

impl SlicesLauncher {
    fn new(apps: Vec<DesktopApp>) -> Self {
        Self { picker: Picker::new(apps), levels: Fades::default(), first: 0, slide: SpringValue::new(0.0) }
    }
}

/// One strip; `level` is 0 closed and 1 fully open at width `wide`.
fn draw_slice(cr: *mut Cairo, app: &DesktopApp, r: Rect, level: f64, wide: f64, accent: Rgba, divider: bool) {
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_rectangle(cr, r.x, r.y, r.w, r.h);
        cb::cairo_clip(cr);
    }
    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 0.0, (0.0, 0.0, 0.0, 0.6 * (1.0 - level)));
    cb::fill_rounded_rect_gradient(cr, r.x, r.y, r.w, r.h, 0.0, (0.0, 0.0, 0.0, 0.0), (0.0, 0.0, 0.0, 0.78 * level), 0.45, 1.0);
    if divider {
        cb::fill_rounded_rect(cr, r.x, r.y, 1.0, r.h, 0.0, faded(INK, 0.16));
    }
    cb::fill_rounded_rect(cr, r.x, r.y + r.h - 4.0, r.w, 4.0, 0.0, faded(accent, level));

    let closed = (1.0 - level * 2.2).clamp(0.0, 1.0);
    if closed > 0.0 {
        let bottom = r.y + r.h - 24.0;
        icon(cr, app, r.x + (SLICE_NARROW - 40.0) / 2.0, bottom - 40.0, 40.0, closed);
        let font = "Inter, Sans Medium 15px";
        let (_, th) = cb::measure_pango_text(cr, "Ag", font, 4000, Some(1));
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_translate(cr, r.x + (SLICE_NARROW - th as f64) / 2.0, bottom - 56.0);
            cb::cairo_rotate(cr, -std::f64::consts::FRAC_PI_2);
        }
        cb::draw_pango_text(cr, 0.0, 0.0, &app.name, font, faded(INK, closed), Some((r.h - 190.0) as i32));
        unsafe { cb::cairo_restore(cr) };
    }
    let open = ((level - 0.45) / 0.55).clamp(0.0, 1.0);
    if open > 0.0 {
        let (x, bottom) = (r.x + 28.0, r.y + r.h - 32.0);
        icon(cr, app, x, bottom - 72.0, 72.0, open);
        let tx = x + 72.0 + 20.0;
        let room = (wide - 28.0 - 72.0 - 20.0 - 28.0).max(40.0) as i32;
        cb::draw_pango_text(cr, tx, bottom - 76.0, &app.name, "Inter, Sans Bold 34px", faded(INK, open), Some(room));
        cb::draw_pango_text(cr, tx, bottom - 26.0, &about(app), "Inter, Sans 15px", faded(INK, 0.85 * open), Some(room));
    }
    unsafe { cb::cairo_restore(cr) };
}

impl CardView for SlicesLauncher {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let p = &mut self.picker;
        let n = p.results.len();
        let wide = slice_wide(card.w, n);
        self.first = window_first(self.first, p.selected, n, SLICES_SHOWN);
        let slide_to = self.first as f64 * SLICE_NARROW;
        if (self.slide.target() - slide_to).abs() > 1e-6 {
            self.slide.set(slide_to, SPRING_OPEN, style);
        }
        let slide = self.slide.animated(0.3);

        p.hits.clear();
        for i in self.first..(self.first + SLICES_SHOWN).min(n) {
            let x = card.x + (i - self.first) as f64 * SLICE_NARROW + if i > p.selected { wide - SLICE_NARROW } else { 0.0 };
            p.hits.push((i, Rect { x, y: card.y, w: if i == p.selected { wide } else { SLICE_NARROW }, h: card.h }));
        }

        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, style.radius);
            cb::cairo_clip(cr);
        }
        ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        if let Some(img) = cb::wallpaper_panel_path().and_then(|f| cb::cached_image_cover(&f, card.w.round() as i32, card.h.round() as i32)) {
            let (iw, ih) = cb::image_size(img);
            cb::paint_cached(cr, img, card.x + (card.w - iw) / 2.0, card.y + (card.h - ih) / 2.0);
        }
        if n == 0 {
            cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, 0.0, (0.0, 0.0, 0.0, 0.58));
            centred_text(cr, card, "No apps match", "Inter, Sans Medium 20px", INK);
        }

        let accent = cb::hex_to_rgba(&theme.primary, 1.0);
        let mut x = card.x - slide;
        for i in 0..n {
            let id = p.results[i].id.clone();
            let level = self.levels.settled_level(id, i == p.selected, SPRING_OPEN, style);
            let w = SLICE_NARROW + level * (wide - SLICE_NARROW);
            if x < card.x + card.w && x + w > card.x {
                draw_slice(cr, &p.results[i], Rect { x, y: card.y, w, h: card.h }, level, wide, accent, i > 0);
            }
            x += w;
        }

        search_field(cr, &p.query, card.x + 20.0, card.y + 20.0, 340.0, INK, (0.0, 0.0, 0.0, 0.5));
        if n > SLICES_SHOWN {
            counter(cr, p.selected, n, card.x + card.w - 20.0, card.y + 26.0, INK, (0.0, 0.0, 0.0, 0.5));
        }
        unsafe { cb::cairo_restore(cr) };
        ui::card_edge(cr, card.x, card.y, card.w, card.h, style);
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.picker.hover(x, y)
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        self.picker.mouse_active = true;
        self.picker.hit(x, y).map_or(Effect::None, |i| self.picker.launch(i))
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.picker.wheel(delta)
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        if let Some(effect) = self.picker.line_key(special, ctrl, SLICES_SHOWN as i32) {
            return effect;
        }
        match self.picker.query.key(special, typed, ctrl) {
            Some(true) => {
                self.picker.filter();
                self.first = 0;
                self.slide.snap(0.0);
                Effect::Redraw
            }
            Some(false) => Effect::Redraw,
            None => Effect::None,
        }
    }

    fn clickable(&self) -> bool {
        self.picker.clickable()
    }
}

// ── Spectrum ─────────────────────────────────────────────────────────────

const SPECTRUM_FOOT: f64 = 76.0;
const SPECTRUM_MAX_ROWS: usize = 6;
const SPECTRUM_MIN_CELL: f64 = 92.0;
const SPECTRUM_ICON: f64 = 64.0;

/// A place in the mosaic, relative to the top left of the field.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Cell {
    col: usize,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Rainbow order: hue first, greys last and light to dark.
fn colour_order(a: &Tint, b: &Tint) -> std::cmp::Ordering {
    a.colourful.cmp(&b.colourful).reverse().then(if a.colourful { a.hue.total_cmp(&b.hue) } else { b.value.total_cmp(&a.value) })
}

/// Groups the tints into columns running through the rainbow, each light to dark, shaped to suit
/// a field of `w` by `h`.
fn spectrum_columns(tints: &[Tint], w: f64, h: f64) -> Vec<Vec<usize>> {
    let n = tints.len();
    if n == 0 {
        return Vec::new();
    }
    let cols = ((n as f64 * w / h.max(1.0)).sqrt().ceil() as usize).clamp(1, n);
    let rows = n.div_ceil(cols).min(SPECTRUM_MAX_ROWS);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| colour_order(&tints[*a], &tints[*b]));
    order
        .chunks(rows)
        .map(|chunk| {
            let mut column = chunk.to_vec();
            column.sort_by(|a, b| tints[*b].value.total_cmp(&tints[*a].value));
            column
        })
        .collect()
}

/// Places columns of the given lengths so that they fill the field; each column shares the full height.
fn spectrum_cells(columns: &[usize], w: f64, h: f64) -> Vec<Cell> {
    let cw = (w / columns.len().max(1) as f64).max(SPECTRUM_MIN_CELL);
    let mut cells = Vec::new();
    for (col, &len) in columns.iter().enumerate() {
        let ch = h / len.max(1) as f64;
        cells.extend((0..len).map(|i| Cell { col, x: col as f64 * cw, y: i as f64 * ch, w: cw, h: ch }));
    }
    cells
}

/// The cell in column `col` level with cell `from`.
fn cell_beside(cells: &[Cell], from: usize, col: usize) -> Option<usize> {
    let mid = cells.get(from).map(|c| c.y + c.h / 2.0)?;
    let dist = |c: &Cell| (c.y + c.h / 2.0 - mid).abs();
    (0..cells.len()).filter(|i| cells[*i].col == col).min_by(|a, b| dist(&cells[*a]).total_cmp(&dist(&cells[*b])))
}

/// An app in the mosaic. Tiles filtered out stay until they have faded.
struct Tile {
    app: DesktopApp,
    tint: Tint,
    rect: [SpringValue; 4],
    alpha: SpringValue,
}

struct SpectrumLauncher {
    picker: Picker,
    levels: Fades<String>,
    cells: Vec<Cell>,
    tiles: HashMap<String, Tile>,
    /// Field size and tint batch the mosaic was arranged for.
    arranged: Option<(f64, f64, usize)>,
    /// The results changed: arrange them on the next draw.
    stale: bool,
    batch: usize,
    ring: Option<[SpringValue; 4]>,
    pan: SpringValue,
    field_w: f64,
}

impl SpectrumLauncher {
    fn new(apps: Vec<DesktopApp>) -> Self {
        ensure_tints(&apps);
        Self {
            picker: Picker::new(apps),
            levels: Fades::default(),
            cells: Vec::new(),
            tiles: HashMap::new(),
            arranged: None,
            stale: true,
            batch: TINT_BATCH.load(Ordering::SeqCst),
            ring: None,
            pan: SpringValue::new(0.0),
            field_w: 0.0,
        }
    }

    /// Sorts the results by colour and sends every tile to its place, keeping the selected app.
    fn arrange(&mut self, w: f64, h: f64, style: &Style) {
        self.stale = false;
        self.arranged = Some((w, h, self.batch));
        let p = &mut self.picker;
        let keep = p.results.get(p.selected).map(|a| a.id.clone());
        let tints: Vec<Tint> = p.results.iter().map(tint).collect();
        let columns = spectrum_columns(&tints, w, h);
        let lengths: Vec<usize> = columns.iter().map(Vec::len).collect();
        let sorted: Vec<DesktopApp> = columns.iter().flatten().map(|i| p.results[*i].clone()).collect();
        p.results = sorted;
        p.selected = keep.and_then(|id| p.results.iter().position(|a| a.id == id)).unwrap_or(0);
        self.cells = spectrum_cells(&lengths, w, h);

        for tile in self.tiles.values_mut() {
            tile.alpha.set(0.0, SPRING_TILE, style);
        }
        for (app, cell) in p.results.iter().zip(&self.cells) {
            let to = [cell.x, cell.y, cell.w, cell.h];
            let tile = self.tiles.entry(app.id.clone()).or_insert_with(|| Tile {
                app: app.clone(),
                tint: tint(app),
                rect: to.map(SpringValue::new),
                alpha: SpringValue::new(0.0),
            });
            tile.tint = tint(app);
            for (v, t) in tile.rect.iter_mut().zip(to) {
                v.set(t, SPRING_TILE, style);
            }
            tile.alpha.set(1.0, SPRING_TILE, style);
        }
    }

    /// Moves the selection to the neighbouring column.
    fn step_column(&mut self, delta: i32) {
        let Some(cell) = self.cells.get(self.picker.selected) else { return };
        let col = cell.col as i32 + delta;
        if col >= 0 {
            if let Some(i) = cell_beside(&self.cells, self.picker.selected, col as usize) {
                self.picker.select(i);
            }
        }
    }

    /// Moves the selection within its column.
    fn step_row(&mut self, delta: i32) {
        let i = self.picker.selected as i32 + delta;
        let same = |c: Option<&Cell>| c.map(|c| c.col);
        if i >= 0 && same(self.cells.get(i as usize)) == same(self.cells.get(self.picker.selected)) {
            self.picker.select(i as usize);
        }
    }
}

impl CardView for SpectrumLauncher {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, _theme: &Theme) {
        let field = Rect { h: card.h - SPECTRUM_FOOT, ..card };
        if self.stale || self.arranged != Some((field.w, field.h, self.batch)) {
            self.arrange(field.w, field.h, style);
        }
        self.field_w = field.w;
        let fg = style.fg;
        let selected = self.cells.get(self.picker.selected).copied();

        let content_w = self.cells.last().map_or(0.0, |c| c.x + c.w);
        let mut pan_to = self.pan.target().clamp(0.0, (content_w - field.w).max(0.0));
        if let Some(c) = selected {
            pan_to = pan_to.min(c.x).max(c.x + c.w - field.w);
        }
        if (self.pan.target() - pan_to).abs() > 1e-6 {
            self.pan.set(pan_to, SPRING_OPEN, style);
        }
        let pan = self.pan.animated(0.3);

        self.picker.hits.clear();
        for (i, c) in self.cells.iter().enumerate() {
            let x = field.x + c.x - pan_to;
            if x + c.w > field.x && x < field.x + field.w {
                self.picker.hits.push((i, Rect { x, y: field.y + c.y, w: c.w, h: c.h }));
            }
        }

        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, style.radius);
            cb::cairo_clip(cr);
        }
        ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, field.x, field.y, field.w, field.h);
            cb::cairo_clip(cr);
        }
        let on_screen = |x: f64, w: f64| x + w > field.x && x < field.x + field.w;
        for tile in self.tiles.values() {
            let alpha = tile.alpha.animated(0.004).clamp(0.0, 1.0);
            let [x, y, w, h] = [tile.rect[0].animated(0.2), tile.rect[1].animated(0.2), tile.rect[2].animated(0.2), tile.rect[3].animated(0.2)];
            let x = field.x + x - pan;
            if alpha > 0.004 && on_screen(x, w) {
                cb::fill_rounded_rect(cr, x, field.y + y, w + 0.6, h + 0.6, 0.0, shade(tile.tint.rgb, 0.72, 0.92 * alpha));
            }
        }
        if let Some(c) = selected {
            let to = [c.x + 6.0, c.y + 6.0, c.w - 12.0, c.h - 12.0];
            let ring = self.ring.get_or_insert_with(|| to.map(SpringValue::new));
            for (v, t) in ring.iter_mut().zip(to) {
                if (v.target() - t).abs() > 1e-6 {
                    v.set(t, ui::SPRING_KNOB, style);
                }
            }
            let [x, y, w, h] = [ring[0].animated(0.2), ring[1].animated(0.2), ring[2].animated(0.2), ring[3].animated(0.2)];
            cb::fill_rounded_rect(cr, field.x + x - pan, field.y + y, w, h, 14.0, faded(INK, 0.16));
            cb::stroke_rounded_rect(cr, field.x + x - pan, field.y + y, w, h, 14.0, faded(INK, 0.95), 3.0);
        }
        let selected_id = self.picker.results.get(self.picker.selected).map(|a| a.id.clone()).unwrap_or_default();
        for (id, tile) in &self.tiles {
            let alpha = tile.alpha.animated(0.004).clamp(0.0, 1.0);
            let [x, y, w, h] = [tile.rect[0].animated(0.2), tile.rect[1].animated(0.2), tile.rect[2].animated(0.2), tile.rect[3].animated(0.2)];
            let x = field.x + x - pan;
            if alpha <= 0.004 || !on_screen(x, w) {
                continue;
            }
            let on = self.levels.settled_level(id.clone(), *id == selected_id, ui::SPRING_KNOB, style);
            let scale = 0.72 + 0.28 * on;
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_translate(cr, x + w / 2.0, field.y + y + h / 2.0);
                cb::cairo_scale(cr, scale, scale);
            }
            icon(cr, &tile.app, -SPECTRUM_ICON / 2.0, -SPECTRUM_ICON / 2.0, SPECTRUM_ICON, alpha);
            unsafe { cb::cairo_restore(cr) };
        }
        self.tiles.retain(|_, t| t.alpha.target() > 0.0 || !t.alpha.settled(0.004));
        unsafe { cb::cairo_restore(cr) };

        let (foot_y, right) = (field.y + field.h, card.x + card.w - 16.0);
        match self.picker.results.get(self.picker.selected) {
            Some(app) => {
                let room = (card.w - 400.0) as i32;
                cb::draw_pango_text(cr, card.x + 24.0, foot_y + 13.0, &app.name, "Inter, Sans Bold 20px", fg, Some(room));
                cb::draw_pango_text(cr, card.x + 24.0, foot_y + 43.0, &about(app), "Inter, Sans 13px", faded(fg, 0.72), Some(room));
            }
            None => centred_text(cr, field, "No apps match", "Inter, Sans Medium 20px", fg),
        }
        search_field(cr, &self.picker.query, right - 320.0, foot_y + 16.0, 320.0, fg, faded(fg, 0.10));
        unsafe { cb::cairo_restore(cr) };
        ui::card_edge(cr, card.x, card.y, card.w, card.h, style);
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.picker.hover(x, y)
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        self.picker.mouse_active = true;
        self.picker.hit(x, y).map_or(Effect::None, |i| self.picker.launch(i))
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.picker.wheel(delta)
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        match special {
            ESC => return Effect::Close,
            ENTER => return self.picker.launch(self.picker.selected),
            UP => self.step_row(-1),
            DOWN => self.step_row(1),
            LEFT if !ctrl => self.step_column(-1),
            RIGHT if !ctrl => self.step_column(1),
            HOME if !ctrl => self.picker.select(0),
            END if !ctrl => self.picker.select(self.picker.results.len().saturating_sub(1)),
            PGUP => self.step_column(-3),
            PGDN => self.step_column(3),
            _ => match self.picker.query.key(special, typed, ctrl) {
                Some(true) => {
                    self.picker.filter();
                    self.stale = true;
                }
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }

    fn poll(&mut self) -> bool {
        let batch = TINT_BATCH.load(Ordering::SeqCst);
        let changed = batch != self.batch;
        self.batch = batch;
        changed
    }

    fn wake_in(&self) -> Option<Duration> {
        TINTS_BUSY.load(Ordering::SeqCst).then_some(Duration::from_millis(100))
    }

    fn clickable(&self) -> bool {
        self.picker.clickable()
    }
}

// ── Deck ─────────────────────────────────────────────────────────────────

const DECK_CARD: (f64, f64) = (250.0, 350.0);
/// Cards shown on each side of the selected one.
const DECK_REACH: i64 = 5;
const DECK_PAGE: i32 = 5;
/// Enough cards to fill both sides of the hand: from here on it is an endless loop.
const DECK_LOOP: usize = 2 * DECK_REACH as usize + 2;

/// Shortest way round a loop of `n` places from `from` to `to`, signed.
fn shortest_turn(from: i64, to: i64, n: i64) -> i64 {
    let turn = (to - from).rem_euclid(n);
    if turn * 2 > n { turn - n } else { turn }
}

/// How a card `d` places from the selected one is held in the hand.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Pose {
    x: f64,
    y: f64,
    angle: f64,
    scale: f64,
    alpha: f64,
}

fn deck_pose(d: f64) -> Pose {
    let near = d.abs().min(1.0);
    let far = (d.abs() - 1.0).max(0.0);
    let side = if d < 0.0 { -1.0 } else { 1.0 };
    Pose {
        x: side * (near * 168.0 + far * 78.0),
        y: near * 44.0 + far * 16.0 + far * far * 4.0,
        angle: side * (near * 7.0 + far * 4.5).to_radians(),
        scale: 1.0 - near * 0.16 - far * 0.02,
        alpha: (DECK_REACH as f64 + 0.2 - d.abs()).clamp(0.0, 1.0),
    }
}

struct DeckLauncher {
    picker: Picker,
    fan: SpringValue,
    glow: [SpringValue; 3],
    batch: usize,
}

impl DeckLauncher {
    fn new(apps: Vec<DesktopApp>) -> Self {
        ensure_tints(&apps);
        let start = apps.first().map_or(NEUTRAL, tint).rgb;
        let mut picker = Picker::new(apps);
        picker.wraps_from = DECK_LOOP;
        Self {
            picker,
            fan: SpringValue::new(0.0),
            glow: [start.0, start.1, start.2].map(SpringValue::new),
            batch: TINT_BATCH.load(Ordering::SeqCst),
        }
    }
}

/// The face of a card, centred on the origin; `front` is 1 for the selected card.
fn draw_deck_card(cr: *mut Cairo, app: &DesktopApp, front: f64) {
    let (w, h) = DECK_CARD;
    let (x, y) = (-w / 2.0, -h / 2.0);
    let t = tint(app).rgb;
    cb::fill_rounded_rect(cr, x - 2.0, y + 8.0, w + 4.0, h + 4.0, 24.0, (0.0, 0.0, 0.0, 0.26));
    cb::fill_rounded_rect_gradient(cr, x, y, w, h, 22.0, shade(t, 0.74, 1.0), shade(t, 0.30, 1.0), 0.0, 1.0);
    cb::stroke_rounded_rect(cr, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 22.0, faded(INK, 0.14 + 0.2 * front), 1.0);
    icon(cr, app, x + 26.0, y + 28.0, 96.0, 1.0);
    let room = (w - 52.0) as i32;
    let (_, name_h) = cb::draw_pango_text_wrapped(cr, x + 26.0, y + 158.0, &app.name, "Inter, Sans Bold 25px", INK, room, Some(2));
    if front > 0.01 {
        cb::draw_pango_text_wrapped(cr, x + 26.0, y + 166.0 + name_h as f64, &about(app), "Inter, Sans 14px", faded(INK, 0.82 * front), room, Some(4));
    }
}

impl CardView for DeckLauncher {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, _theme: &Theme) {
        let fg = style.fg;
        let p = &mut self.picker;
        let n = p.results.len();
        let wraps = p.wraps();
        let place = self.fan.target().round() as i64;
        let turn = if wraps { shortest_turn(place, p.selected as i64, n as i64) } else { p.selected as i64 - place };
        if turn != 0 {
            self.fan.set((place + turn) as f64, SPRING_FAN, style);
        }
        let fan = self.fan.animated(0.002);
        let (cx, cy) = (card.x + card.w / 2.0, card.y + card.h / 2.0 + 26.0);

        // Hit places, front card first because it overlaps its neighbours.
        p.hits.clear();
        let mut near: Vec<i64> = (-DECK_REACH + 1..DECK_REACH).collect();
        near.sort_by_key(|d| d.abs());
        for d in near {
            let i = if wraps { (p.selected as i64 + d).rem_euclid(n as i64) } else { p.selected as i64 + d };
            if i >= 0 && (i as usize) < n {
                let pose = deck_pose(d as f64);
                let (w, h) = (DECK_CARD.0 * pose.scale, DECK_CARD.1 * pose.scale);
                p.hits.push((i as usize, Rect { x: cx + pose.x - w / 2.0, y: cy + pose.y - h / 2.0, w, h }));
            }
        }

        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, style.radius);
            cb::cairo_clip(cr);
        }
        ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let glow_to = p.results.get(p.selected).map_or(NEUTRAL, tint).rgb;
        for (v, t) in self.glow.iter_mut().zip([glow_to.0, glow_to.1, glow_to.2]) {
            if (v.target() - t).abs() > 1e-6 {
                v.set(t, SPRING_OPEN, style);
            }
        }
        let glow = (self.glow[0].animated(0.004), self.glow[1].animated(0.004), self.glow[2].animated(0.004));
        cb::fill_rounded_rect_gradient(cr, card.x, card.y, card.w, card.h, 0.0, shade(glow, 1.0, 0.0), shade(glow, 0.9, 0.5), 0.15, 1.0);

        let (mut from, mut to) = (fan.floor() as i64 - DECK_REACH, fan.ceil() as i64 + DECK_REACH);
        if !wraps {
            (from, to) = (from.max(0), to.min(n as i64 - 1));
        }
        let mut order: Vec<i64> = (from..=to).collect();
        order.sort_by(|a, b| (*b as f64 - fan).abs().total_cmp(&(*a as f64 - fan).abs()));
        for i in order {
            let d = i as f64 - fan;
            let pose = deck_pose(d);
            if pose.alpha <= 0.01 {
                continue;
            }
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_translate(cr, cx + pose.x, cy + pose.y);
                cb::cairo_rotate(cr, pose.angle);
                cb::cairo_scale(cr, pose.scale, pose.scale);
                cb::cairo_push_group(cr);
            }
            draw_deck_card(cr, &p.results[i.rem_euclid(n.max(1) as i64) as usize], (1.0 - d.abs()).clamp(0.0, 1.0));
            unsafe {
                cb::cairo_pop_group_to_source(cr);
                cb::cairo_paint_with_alpha(cr, pose.alpha);
                cb::cairo_restore(cr);
            }
        }
        if n == 0 {
            centred_text(cr, card, "No apps match", "Inter, Sans Medium 20px", fg);
        }
        search_field(cr, &p.query, card.x + 20.0, card.y + 20.0, 340.0, fg, faded(fg, 0.10));
        if n > 0 {
            counter(cr, p.selected, n, card.x + card.w - 20.0, card.y + 26.0, fg, faded(fg, 0.10));
        }
        unsafe { cb::cairo_restore(cr) };
        ui::card_edge(cr, card.x, card.y, card.w, card.h, style);
    }

    /// Cards move under the pointer, so hovering does not select; a click brings a card forward.
    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.picker.pointer_moved(x, y);
        false
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        self.picker.mouse_active = true;
        match self.picker.hit(x, y) {
            Some(i) if i == self.picker.selected => self.picker.launch(i),
            Some(i) => {
                self.picker.select(i);
                Effect::Redraw
            }
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.picker.wheel(delta)
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        if let Some(effect) = self.picker.line_key(special, ctrl, DECK_PAGE) {
            return effect;
        }
        match self.picker.query.key(special, typed, ctrl) {
            Some(true) => {
                self.picker.filter();
                self.fan.snap(0.0);
                Effect::Redraw
            }
            Some(false) => Effect::Redraw,
            None => Effect::None,
        }
    }

    fn poll(&mut self) -> bool {
        let batch = TINT_BATCH.load(Ordering::SeqCst);
        let changed = batch != self.batch;
        self.batch = batch;
        changed
    }

    fn wake_in(&self) -> Option<Duration> {
        TINTS_BUSY.load(Ordering::SeqCst).then_some(Duration::from_millis(100))
    }

    fn clickable(&self) -> bool {
        self.picker.clickable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colour(hue: f64, value: f64) -> Tint {
        Tint { rgb: (0.5, 0.5, 0.5), hue, colourful: true, value }
    }

    fn grey(value: f64) -> Tint {
        Tint { rgb: (value, value, value), hue: 0.0, colourful: false, value }
    }

    /// Opaque premultiplied ARGB32 pixels of one colour.
    fn pixels(rgb: [u8; 3], count: usize) -> Vec<u8> {
        let v = 0xFF00_0000u32 | (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
        v.to_ne_bytes().repeat(count)
    }

    #[test]
    fn unknown_theme_is_classic_sized() {
        assert_eq!(card_size("classic"), (launcher::CARD_W, launcher::CARD_H));
        assert_eq!(card_size("prose"), (launcher::CARD_W, launcher::CARD_H));
        assert_eq!(card_size("deck"), DECK_SIZE);
    }

    #[test]
    fn slice_window_follows_the_selection() {
        assert_eq!(window_first(0, 3, 20, 9), 0);
        assert_eq!(window_first(0, 9, 20, 9), 1);
        assert_eq!(window_first(5, 2, 20, 9), 2);
        assert_eq!(window_first(15, 19, 20, 9), 11);
        assert_eq!(window_first(4, 1, 3, 9), 0);
    }

    #[test]
    fn strips_always_fill_the_card() {
        for n in 1..14 {
            let shown = n.min(SLICES_SHOWN);
            let total = slice_wide(1120.0, n) + (shown - 1) as f64 * SLICE_NARROW;
            assert_eq!(total, 1120.0, "{n}");
        }
    }

    #[test]
    fn icon_colour_is_its_main_hue() {
        let mut icon = pixels([220, 30, 30], 90);
        icon.extend(pixels([30, 30, 220], 10));
        let t = tint_of(&icon);
        assert!(t.colourful);
        assert!(t.hue < 15.0 || t.hue > 345.0, "{}", t.hue);
    }

    #[test]
    fn a_grey_icon_has_no_colour() {
        assert!(!tint_of(&pixels([120, 120, 120], 64)).colourful);
        assert_eq!(tint_of(&[0; 64]), NEUTRAL);
    }

    #[test]
    fn columns_run_through_the_rainbow_and_end_in_grey() {
        let tints = [grey(0.5), colour(200.0, 0.5), colour(10.0, 0.5), colour(100.0, 0.5)];
        let flat: Vec<usize> = spectrum_columns(&tints, 1000.0, 500.0).concat();
        assert_eq!(flat, vec![2, 3, 1, 0]);
    }

    #[test]
    fn every_app_gets_one_cell_within_the_row_limit() {
        for n in 1..120 {
            let tints: Vec<Tint> = (0..n).map(|i| colour(i as f64 * 3.0, 0.5)).collect();
            let columns = spectrum_columns(&tints, 1040.0, 564.0);
            assert_eq!(columns.iter().map(Vec::len).sum::<usize>(), n);
            assert!(columns.iter().all(|c| c.len() <= SPECTRUM_MAX_ROWS), "{n}");
        }
    }

    #[test]
    fn cells_fill_the_field_height() {
        let cells = spectrum_cells(&[3, 2], 1000.0, 600.0);
        assert_eq!(cells[2].y + cells[2].h, 600.0);
        assert_eq!(cells[4].y + cells[4].h, 600.0);
        assert_eq!(cells[4].x + cells[4].w, 1000.0);
    }

    #[test]
    fn sideways_lands_on_the_level_cell() {
        let cells = spectrum_cells(&[4, 2], 1000.0, 400.0);
        assert_eq!(cell_beside(&cells, 3, 1), Some(5));
        assert_eq!(cell_beside(&cells, 0, 1), Some(4));
        assert_eq!(cell_beside(&cells, 0, 9), None);
    }

    #[test]
    fn a_resting_pointer_does_not_select() {
        let mut p = Picker::new(Vec::new());
        p.opened = Instant::now() - Duration::from_secs(1);
        assert!(!p.pointer_moved(40.0, 40.0));
        assert!(!p.pointer_moved(40.0, 40.0));
        assert!(p.pointer_moved(41.0, 40.0));
    }

    #[test]
    fn the_hand_turns_the_short_way_round() {
        assert_eq!(shortest_turn(0, 43, 44), -1);
        assert_eq!(shortest_turn(43, 0, 44), 1);
        assert_eq!(shortest_turn(-1, 43, 44), 0);
        assert_eq!(shortest_turn(5, 9, 44), 4);
    }

    #[test]
    fn the_selected_card_faces_forward() {
        let front = deck_pose(0.0);
        assert_eq!((front.x, front.y, front.angle, front.scale, front.alpha), (0.0, 0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn the_hand_is_symmetric_and_fades_at_its_ends() {
        for d in [0.5, 1.0, 2.5, 4.0] {
            let (a, b) = (deck_pose(d), deck_pose(-d));
            assert_eq!(a.x, -b.x);
            assert_eq!(a.angle, -b.angle);
            assert_eq!(a.y, b.y);
        }
        assert!(deck_pose(1.0).x < deck_pose(2.0).x);
        assert_eq!(deck_pose(DECK_REACH as f64 + 1.0).alpha, 0.0);
    }
}
