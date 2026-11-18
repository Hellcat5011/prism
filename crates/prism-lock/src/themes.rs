//! The lockscreen and greeter designs other than Classic (which is `App::paint` in main.rs).
//!
//! * Stencil: solid colour with the time cut out of it; unlocking flies through a digit.
//! * Horizon: one hairline with the time resting on it; unlocking splits the screen along it.
//! * Sentence: the state of the machine as one line of text; words leave one by one.
//! * Aperture: a round window onto the wallpaper that opens as you type.
//! * Tide: a grey wallpaper that keys splash colour onto; unlocking spreads it over everything.
//!
//! Everything is drawn with cairo into wl_shm buffers, so the designs behave the same on any
//! compositor (cage included). Expensive pictures are baked once per size in `Layers`; frames
//! only blit them and fill small areas. All motion is closed-form springs on the wall clock.

use std::cell::Cell;
use std::time::Instant;

use super::cairo_backend::{self as cb, Cairo, CairoSurface};
use super::{draw_spaced, spaced_width, text_size, Hit, Look, Mode, Rgba};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockTheme {
    Classic,
    Stencil,
    Horizon,
    Sentence,
    Aperture,
    Tide,
}

impl LockTheme {
    pub fn from_id(id: &str) -> LockTheme {
        match id {
            "stencil" => LockTheme::Stencil,
            "horizon" => LockTheme::Horizon,
            "sentence" => LockTheme::Sentence,
            "aperture" => LockTheme::Aperture,
            "tide" => LockTheme::Tide,
            _ => LockTheme::Classic,
        }
    }

    /// How long the unlock animation runs.
    pub fn closing_ms(self) -> f64 {
        match self {
            LockTheme::Classic => super::FADE_OUT_MS,
            LockTheme::Stencil => 850.0,
            LockTheme::Horizon => 750.0,
            LockTheme::Sentence => 950.0,
            LockTheme::Aperture => 800.0,
            LockTheme::Tide => 1200.0,
        }
    }
}

// ── Springs ──────────────────────────────────────────────────────────────

/// (response s, damping ratio).
type Params = (f64, f64);
const SPRING_LEVEL: Params = (0.34, 0.72);
const SPRING_SPLASH: Params = (0.42, 0.68);
const SPRING_SHUT: Params = (0.12, 1.0);
const SPRING_REOPEN: Params = (0.55, 0.8);
const SPRING_TICK: Params = (0.24, 0.62);

/// Position and velocity after `t` seconds, starting at `x0` with `v0` and heading for `to`.
fn spring_at((response, zeta): Params, x0: f64, v0: f64, to: f64, t: f64) -> (f64, f64) {
    let w = std::f64::consts::TAU / response.max(0.001);
    let d = x0 - to;
    let e = (-zeta * w * t).exp();
    if zeta < 1.0 {
        let wd = w * (1.0 - zeta * zeta).sqrt();
        let (s, c) = (wd * t).sin_cos();
        (to + e * (d * c + (v0 + zeta * w * d) / wd * s), e * (v0 * c - (zeta * w * v0 + w * w * d) / wd * s))
    } else {
        (to + e * (d + (v0 + w * d) * t), e * (v0 - w * (v0 + w * d) * t))
    }
}

/// A 0 to 1 progress after `t` seconds.
fn progress(p: Params, t: f64) -> f64 {
    if t <= 0.0 { 0.0 } else { spring_at(p, 0.0, 0.0, 1.0, t).0 }
}

#[derive(Clone, Copy)]
struct Spring {
    x0: f64,
    v0: f64,
    to: f64,
    start: Instant,
    params: Params,
}

impl Spring {
    fn new(value: f64, now: Instant, params: Params) -> Spring {
        Spring { x0: value, v0: 0.0, to: value, start: now, params }
    }

    fn sample(&self, now: Instant) -> (f64, f64) {
        spring_at(self.params, self.x0, self.v0, self.to, now.saturating_duration_since(self.start).as_secs_f64())
    }

    fn value(&self, now: Instant) -> f64 {
        self.sample(now).0
    }

    fn set(&mut self, to: f64, now: Instant, params: Params) {
        let (x0, v0) = self.sample(now);
        *self = Spring { x0, v0, to, start: now, params };
    }

    fn settled(&self, now: Instant) -> bool {
        let (x, v) = self.sample(now);
        (x - self.to).abs() < 0.002 && v.abs() < 0.01
    }
}

// ── Animation state ──────────────────────────────────────────────────────

/// A splash of colour (Tide). Position in 0..1 screen fractions.
struct Splash {
    x: f64,
    y: f64,
    radius: f64,
    seed: u64,
    grow: Spring,
    dying: bool,
}

/// What the designs animate between frames, driven by the App's state (`sync`).
pub struct Fx {
    theme: LockTheme,
    typed: usize,
    level: Spring,
    shut: Spring,
    splashes: Vec<Splash>,
    rng: u64,
    failed_at: Option<Instant>,
    closing: bool,
    /// Stencil: the point inside a digit that the unlock flies through.
    pivot: Cell<Option<(f64, f64)>>,
}

/// A repeatable 0..1 number for (`seed`, `i`).
fn noise(seed: u64, i: u64) -> f64 {
    let mut x = seed.wrapping_add(i.wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
}

impl Fx {
    pub fn new(theme: LockTheme, now: Instant) -> Fx {
        let rng = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
        Fx {
            theme,
            typed: 0,
            level: Spring::new(0.0, now, SPRING_LEVEL),
            shut: Spring::new(1.0, now, SPRING_REOPEN),
            splashes: Vec::new(),
            rng,
            failed_at: None,
            closing: false,
            pivot: Cell::new(None),
        }
    }

    fn next(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(1);
        noise(self.rng, 7)
    }

    /// A new splash, away from the others (the farthest of a few random spots).
    fn add_splash(&mut self, now: Instant) {
        let live: Vec<(f64, f64)> = self.splashes.iter().filter(|s| !s.dying).map(|s| (s.x, s.y)).collect();
        let mut best = (0.5, 0.5, -1.0);
        for _ in 0..7 {
            let (x, y) = (0.06 + 0.88 * self.next(), 0.08 + 0.84 * self.next());
            let d = live.iter().map(|p| (p.0 - x).powi(2) + (p.1 - y).powi(2)).fold(f64::MAX, f64::min);
            if d > best.2 {
                best = (x, y, d);
            }
        }
        let radius = 0.09 + 0.11 * self.next();
        let seed = (self.next() * 1e15) as u64;
        let mut grow = Spring::new(0.0, now, SPRING_SPLASH);
        grow.set(1.0, now, SPRING_SPLASH);
        self.splashes.push(Splash { x: best.0, y: best.1, radius, seed, grow, dying: false });
    }

    fn drop_splashes(&mut self, keep: usize, now: Instant) {
        let mut live = 0;
        for s in self.splashes.iter_mut().filter(|s| !s.dying) {
            live += 1;
            if live > keep {
                s.dying = true;
                s.grow.set(0.0, now, SPRING_SPLASH);
            }
        }
    }

    /// Follow the App: characters typed, a failed attempt, the unlock.
    pub fn sync(&mut self, typed: usize, checking: bool, failed_at: Option<Instant>, closing: bool, now: Instant) {
        if closing && !self.closing {
            self.closing = true;
            if self.theme == LockTheme::Tide {
                // Enough colour to spread from, however short the password was.
                while self.splashes.iter().filter(|s| !s.dying).count() < 6 {
                    self.add_splash(now);
                }
            }
        }
        if failed_at != self.failed_at {
            self.failed_at = failed_at;
            if failed_at.is_some() {
                self.shut.set(0.08, now, SPRING_SHUT);
                self.drop_splashes(0, now);
            }
        }
        // The aperture opens again once it has snapped shut.
        if self.shut.to < 1.0 && self.shut.value(now) < 0.14 {
            self.shut.set(1.0, now, SPRING_REOPEN);
        }
        if typed != self.typed && !checking && !self.closing {
            if self.theme == LockTheme::Tide {
                for _ in self.typed..typed {
                    self.add_splash(now);
                }
                self.drop_splashes(typed, now);
            }
            self.typed = typed;
            self.level.set(typed as f64, now, SPRING_LEVEL);
        }
        self.splashes.retain(|s| !(s.dying && s.grow.settled(now)));
    }

    pub fn animating(&self, now: Instant) -> bool {
        !self.level.settled(now) || !self.shut.settled(now) || self.splashes.iter().any(|s| !s.grow.settled(now))
    }

    fn level(&self, now: Instant) -> f64 {
        self.level.value(now).max(0.0)
    }
}

// ── Baked pictures ───────────────────────────────────────────────────────

/// The pictures a design needs at one output size.
pub struct Layers {
    pub size: (i32, i32),
    wall: *mut CairoSurface,
    /// The dimmed or greyed wallpaper the design sits on.
    base: Option<*mut CairoSurface>,
    /// Tide: the wallpaper with stronger colour, what the splashes are made of.
    vivid: Option<*mut CairoSurface>,
}

impl Drop for Layers {
    fn drop(&mut self) {
        unsafe {
            cb::cairo_surface_destroy(self.wall);
            for s in [self.base, self.vivid].into_iter().flatten() {
                cb::cairo_surface_destroy(s);
            }
        }
    }
}

impl Layers {
    pub fn render(look: &Look, w: i32, h: i32) -> Layers {
        unsafe {
            let wall = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h);
            let cr = cb::cairo_create(wall);
            let bg = look.background;
            cb::cairo_set_source_rgba(cr, bg.0, bg.1, bg.2, 1.0);
            cb::cairo_paint(cr);
            if let Some(p) = &look.wallpaper {
                cb::draw_image_cover(cr, p, 0.0, 0.0, w as f64, h as f64, 0.0);
            }
            cb::cairo_destroy(cr);
            cb::cairo_surface_flush(wall);
            let base = match look.theme {
                LockTheme::Horizon => Some(veiled(wall, w, h, 0.42)),
                LockTheme::Sentence => Some(veiled(wall, w, h, 0.62)),
                LockTheme::Tide => Some(greyed(wall, w, h)),
                _ => None,
            };
            let vivid = (look.theme == LockTheme::Tide).then(|| saturated(wall, w, h));
            Layers { size: (w, h), wall, base, vivid }
        }
    }
}

unsafe fn veiled(wall: *mut CairoSurface, w: i32, h: i32, veil: f64) -> *mut CairoSurface {
    unsafe {
        let s = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h);
        let cr = cb::cairo_create(s);
        cb::cairo_set_source_surface(cr, wall, 0.0, 0.0);
        cb::cairo_paint(cr);
        cb::cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, veil);
        cb::cairo_paint(cr);
        cb::cairo_destroy(cr);
        s
    }
}

/// The wallpaper without colour, a little darker so text and splashes stand out.
unsafe fn greyed(wall: *mut CairoSurface, w: i32, h: i32) -> *mut CairoSurface {
    unsafe {
        let s = veiled(wall, w, h, 0.18);
        cb::cairo_surface_flush(s);
        let (data, stride) = (cb::cairo_image_surface_get_data(s), cb::cairo_image_surface_get_stride(s) as usize);
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts_mut(data.add(y * stride) as *mut u32, w as usize);
            for p in row {
                let v = *p;
                let l = (54 * ((v >> 16) & 255) + 183 * ((v >> 8) & 255) + 19 * (v & 255)) >> 8;
                *p = 0xFF00_0000 | l << 16 | l << 8 | l;
            }
        }
        cb::cairo_surface_mark_dirty(s);
        s
    }
}

/// The wallpaper with its colours pushed away from grey.
unsafe fn saturated(wall: *mut CairoSurface, w: i32, h: i32) -> *mut CairoSurface {
    unsafe {
        let s = veiled(wall, w, h, 0.0);
        cb::cairo_surface_flush(s);
        let (data, stride) = (cb::cairo_image_surface_get_data(s), cb::cairo_image_surface_get_stride(s) as usize);
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts_mut(data.add(y * stride) as *mut u32, w as usize);
            for p in row {
                let (r, g, b) = (((*p >> 16) & 255) as i32, ((*p >> 8) & 255) as i32, (*p & 255) as i32);
                let l = (54 * r + 183 * g + 19 * b) >> 8;
                let push = |c: i32| (l + (c - l) * 19 / 8).clamp(0, 255) as u32;
                *p = 0xFF00_0000 | push(r) << 16 | push(g) << 8 | push(b);
            }
        }
        cb::cairo_surface_mark_dirty(s);
        s
    }
}

// ── Frame ────────────────────────────────────────────────────────────────

/// What a frame shows, read from the App.
pub struct Scene {
    pub mode: Mode,
    pub now: Instant,
    pub opened: Instant,
    pub closing: Option<Instant>,
    /// "11:42" and "WEDNESDAY / NOVEMBER 18".
    pub clock: String,
    pub date: String,
    pub weekday: String,
    /// Second of the minute and the fraction of the current second.
    pub second: (u32, f64),
    pub user: String,
    pub entering_user: bool,
    /// The name being typed (greeter user entry).
    pub typed_name: String,
    pub typed: usize,
    pub status: String,
    pub status_error: bool,
    pub caps_lock: bool,
    /// Lock screen only.
    pub notifications: usize,
    pub session: String,
    pub power_buttons: bool,
    pub hover: Option<Hit>,
    pub preview: bool,
}

type HitRect = (Hit, f64, f64, f64, f64);

const WHITE: Rgba = (1.0, 1.0, 1.0, 1.0);

fn alpha(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, c.3 * a)
}

/// `c` lifted until it reads on a dark picture.
fn bright(c: Rgba) -> Rgba {
    let luma = 0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2;
    let t = ((0.62 - luma) / (1.0 - luma).max(0.01)).clamp(0.0, 1.0);
    (c.0 + (1.0 - c.0) * t, c.1 + (1.0 - c.1) * t, c.2 + (1.0 - c.2) * t, c.3)
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
}

struct Ctx<'a> {
    cr: *mut Cairo,
    l: &'a Look,
    s: &'a Scene,
    fx: &'a Fx,
    w: f64,
    h: f64,
    sc: f64,
    hits: Vec<HitRect>,
}

impl Ctx<'_> {
    fn px(&self, v: f64) -> f64 {
        (v * self.sc).round()
    }

    fn text(&self, x: f64, y: f64, text: &str, font: &str, color: Rgba, shadow: bool) {
        if shadow {
            cb::draw_pango_text(self.cr, x + 1.0, y + 1.5, text, font, (0.0, 0.0, 0.0, 0.55 * color.3), None);
        }
        cb::draw_pango_text(self.cr, x, y, text, font, color, None);
    }

    fn spaced(&self, x: f64, y: f64, text: &str, font: &str, color: Rgba, spacing: f64, shadow: bool) {
        if shadow {
            draw_spaced(self.cr, x + 1.0, y + 1.5, text, font, (0.0, 0.0, 0.0, 0.55 * color.3), spacing);
        }
        draw_spaced(self.cr, x, y, text, font, color, spacing);
    }

    fn spaced_at(&self, x: f64, y: f64, align: Align, text: &str, font: &str, color: Rgba, spacing: f64, shadow: bool) {
        let tx = if align == Align::Center { x - spaced_width(self.cr, text, font, spacing) / 2.0 } else { x };
        self.spaced(tx, y, text, font, color, spacing, shadow);
    }

    /// The password dots, the typed user name, or a hint; then the status line under them.
    fn prompt(&self, x: f64, y: f64, align: Align, ink: Rgba, shadow: bool) {
        let s = self.s;
        let small = self.l.f("Bold", self.px(11.0));
        let gap = self.px(3.0);
        if s.entering_user {
            let font = self.l.f("Medium", self.px(24.0));
            let shown = if s.typed_name.is_empty() { "WHO GOES THERE" } else { &s.typed_name };
            let (tw, th) = text_size(self.cr, shown, &font);
            let tx = if align == Align::Center { x - tw / 2.0 } else { x };
            self.text(tx, y - th / 2.0, shown, &font, alpha(ink, if s.typed_name.is_empty() { 0.55 } else { 1.0 }), shadow);
        } else if s.typed > 0 {
            let (r, step) = (self.px(5.0), self.px(20.0));
            let n = s.typed.min(32);
            let total = (n - 1) as f64 * step;
            let x0 = if align == Align::Center { x - total / 2.0 } else { x + r };
            for i in 0..n {
                let cx = x0 + i as f64 * step;
                if shadow {
                    cb::fill_circle(self.cr, cx + 1.0, y + 1.5, r, (0.0, 0.0, 0.0, 0.5 * ink.3));
                }
                cb::fill_circle(self.cr, cx, y, r, ink);
            }
        } else if s.status.is_empty() {
            let hint = if s.mode == Mode::Greeter { "TYPE TO SIGN IN" } else { "TYPE TO UNLOCK" };
            let (_, th) = text_size(self.cr, hint, &small);
            self.spaced_at(x, y - th / 2.0, align, hint, &small, alpha(ink, 0.6), gap, shadow);
        }
        let line = y + self.px(24.0);
        let warn = bright(self.l.primary);
        if !s.status.is_empty() {
            let font = self.l.f("Medium", self.px(14.0));
            let (tw, _) = text_size(self.cr, &s.status, &font);
            let tx = if align == Align::Center { x - tw / 2.0 } else { x };
            let c = if s.status_error { bright(self.l.error) } else { alpha(ink, 0.75) };
            self.text(tx, line, &s.status, &font, alpha(c, ink.3), shadow);
        } else if s.caps_lock {
            self.spaced_at(x, line, align, "CAPS LOCK IS ON", &small, alpha(warn, ink.3), gap, shadow);
        }
    }

    /// Bottom row: user, session and power buttons. On the greeter a click on the user or the
    /// session steps to the next one; on the lock screen those two are labels.
    fn footer(&mut self, ink: Rgba, shadow: bool) {
        let s = self.s;
        let greeter = s.mode == Mode::Greeter;
        let mut items: Vec<(Hit, String)> = Vec::new();
        if !s.user.is_empty() {
            items.push((Hit::User, s.user.to_uppercase()));
        }
        if greeter || s.power_buttons {
            items.push((Hit::Session, s.session.clone()));
            items.push((Hit::Shutdown, "SHUTDOWN".to_string()));
            items.push((Hit::Reboot, "REBOOT".to_string()));
        }
        if items.is_empty() {
            return;
        }
        let font = self.l.f("Bold", self.px(12.0));
        let (spacing, gap) = (self.px(2.5), self.px(36.0));
        let widths: Vec<f64> = items.iter().map(|(_, t)| spaced_width(self.cr, t, &font, spacing)).collect();
        let mut x = (self.w - widths.iter().sum::<f64>() - gap * (items.len() - 1) as f64) / 2.0;
        let (_, th) = text_size(self.cr, "A", &font);
        let y = self.h - self.px(64.0);
        for (i, (hit, label)) in items.iter().enumerate() {
            let clickable = greeter || !matches!(hit, Hit::User | Hit::Session);
            let c = if clickable && s.hover == Some(*hit) { alpha(bright(self.l.primary), ink.3) } else { alpha(ink, 0.72) };
            self.spaced(x, y, label, &font, c, spacing, shadow);
            if clickable {
                self.hits.push((*hit, x - self.px(12.0), y - self.px(12.0), widths[i] + self.px(24.0), th + self.px(24.0)));
            }
            x += widths[i] + gap;
        }
    }

    fn preview_banner(&self, ink: Rgba) {
        if self.s.preview {
            let font = self.l.f("Bold", self.px(12.0));
            self.spaced_at(self.w / 2.0, self.px(28.0), Align::Center, "GREETER PREVIEW  ·  ESC TO CLOSE", &font, ink, self.px(2.0), true);
        }
    }

    /// Seconds since the unlock began.
    fn closing_t(&self) -> Option<f64> {
        self.s.closing.map(|t| self.s.now.saturating_duration_since(t).as_secs_f64())
    }

    fn paint_surface(&self, s: *mut CairoSurface, a: f64) {
        unsafe {
            cb::cairo_set_source_surface(self.cr, s, 0.0, 0.0);
            if a >= 1.0 { cb::cairo_paint(self.cr) } else { cb::cairo_paint_with_alpha(self.cr, a) }
        }
    }
}

/// Draw one frame of the design in `look.theme`; returns the hit rectangles.
pub unsafe fn paint(cr: *mut Cairo, look: &Look, scene: &Scene, fx: &Fx, layers: &Layers, w: i32, h: i32) -> Vec<HitRect> {
    let mut c = Ctx { cr, l: look, s: scene, fx, w: w as f64, h: h as f64, sc: (h as f64 / super::DESIGN_H).max(0.1), hits: Vec::new() };
    unsafe {
        match look.theme {
            LockTheme::Stencil => stencil(&mut c, layers),
            LockTheme::Horizon => horizon(&mut c, layers),
            LockTheme::Sentence => sentence(&mut c, layers),
            LockTheme::Aperture => aperture(&mut c, layers),
            LockTheme::Tide => tide(&mut c, layers),
            LockTheme::Classic => {}
        }
        let fade = progress((0.5, 1.0), scene.now.saturating_duration_since(scene.opened).as_secs_f64());
        if fade < 0.995 {
            let b = look.background;
            cb::cairo_set_source_rgba(cr, b.0, b.1, b.2, 1.0 - fade);
            cb::cairo_paint(cr);
        }
    }
    c.hits
}

// ── Stencil ──────────────────────────────────────────────────────────────

/// A point deep inside a digit of the text path now in `cr`, near its middle.
unsafe fn deep_point(cr: *mut Cairo, tw: f64, th: f64) -> (f64, f64) {
    const DIRS: [(f64, f64); 8] = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7, 0.7), (-0.7, 0.7), (0.7, -0.7), (-0.7, -0.7)];
    unsafe {
        for r in [0.075, 0.055, 0.04, 0.025] {
            let r = r * th;
            let mut best: Option<(f64, f64, f64)> = None;
            for j in 0..14 {
                for i in 0..36 {
                    let (x, y) = (tw * (i as f64 + 0.5) / 36.0, th * (j as f64 + 0.5) / 14.0);
                    if cb::cairo_in_fill(cr, x, y) != 0 && DIRS.iter().all(|d| cb::cairo_in_fill(cr, x + d.0 * r, y + d.1 * r) != 0) {
                        let d = (x - tw / 2.0).powi(2) + (y - th / 2.0).powi(2);
                        if best.is_none_or(|b| d < b.2) {
                            best = Some((x, y, d));
                        }
                    }
                }
            }
            if let Some((x, y, _)) = best {
                return (x, y);
            }
        }
    }
    (tw / 2.0, th / 2.0)
}

unsafe fn stencil(c: &mut Ctx, layers: &Layers) {
    let (cr, s, l) = (c.cr, c.s, c.l);
    let (w, h) = (c.w, c.h);
    let probe = l.f("Black", 300.0);
    let (pw, ph) = text_size(cr, &s.clock, &probe);
    let size = (300.0 * (0.76 * w / pw).min(0.54 * h / ph)).round();
    let font = l.f("Black", size);
    let (tw, th) = text_size(cr, &s.clock, &font);
    let (ox, oy) = ((w - tw) / 2.0, h * 0.44 - th / 2.0);
    let level = c.fx.level(s.now).min(16.0);
    let closing = c.closing_t();
    let fly = closing.map_or(0.0, |t| progress((0.62, 1.0), t));
    unsafe {
        cb::cairo_set_source_rgba(cr, l.container.0, l.container.1, l.container.2, 1.0);
        cb::cairo_paint(cr);

        // Where the zoom is anchored: the middle of the time, then a digit it can fly through.
        let mut anchor = (w / 2.0, h * 0.44);
        if closing.is_some() {
            let p = c.fx.pivot.get().unwrap_or_else(|| {
                cb::cairo_new_path(cr);
                cb::cairo_move_to(cr, 0.0, 0.0);
                cb::text_path(cr, &s.clock, &font);
                let p = deep_point(cr, tw, th);
                c.fx.pivot.set(Some(p));
                p
            });
            anchor = (ox + p.0, oy + p.1);
        }
        let screen_mid = (w / 2.0, h / 2.0);
        let target = (anchor.0 + (screen_mid.0 - anchor.0) * fly, anchor.1 + (screen_mid.1 - anchor.1) * fly);
        let zoom = (1.0 + 0.008 * level) * (fly * 95f64.ln()).exp();

        // The wallpaper shows through the digits, which swell a little with each key.
        cb::cairo_new_path(cr);
        cb::cairo_save(cr);
        cb::cairo_translate(cr, target.0 + zoom * (ox - anchor.0), target.1 + zoom * (oy - anchor.1));
        cb::cairo_scale(cr, zoom, zoom);
        cb::cairo_move_to(cr, 0.0, 0.0);
        cb::text_path(cr, &s.clock, &font);
        cb::cairo_restore(cr);
        cb::cairo_set_source_surface(cr, layers.wall, 0.0, 0.0);
        cb::cairo_set_line_join(cr, 1);
        cb::cairo_fill_preserve(cr);
        cb::cairo_set_line_width(cr, c.sc * zoom * 1.5 * level);
        cb::cairo_stroke(cr);
        cb::cairo_new_path(cr);

        // Text leaves first; the end of the flight is the plain wallpaper.
        let ink = alpha(l.fg, (1.0 - fly * 4.0).clamp(0.0, 1.0));
        if ink.3 > 0.0 {
            let df = l.f("Bold", c.px(17.0));
            let y = oy + th + c.px(6.0);
            c.spaced_at(w / 2.0, y, Align::Center, &s.date, &df, alpha(ink, 0.85), c.px(7.0), false);
            c.prompt(w / 2.0, y + c.px(74.0), Align::Center, ink, false);
            c.footer(ink, false);
            c.preview_banner(ink);
        }
        if fly > 0.8 {
            c.paint_surface(layers.wall, ((fly - 0.8) / 0.18).clamp(0.0, 1.0));
        }
    }
}

// ── Horizon ──────────────────────────────────────────────────────────────

/// Everything above and below the line; drawn once per half while the screen splits.
unsafe fn horizon_scene(c: &mut Ctx, layers: &Layers, line_y: f64, ink: Rgba) {
    let (cr, s, l) = (c.cr, c.s, c.l);
    let (w, margin) = (c.w, c.px(110.0));
    unsafe {
        if let Some(b) = layers.base {
            c.paint_surface(b, 1.0);
        }
        let thick = c.px(1.5).max(1.0);
        cb::cairo_set_source_rgba(cr, ink.0, ink.1, ink.2, 0.6);
        cb::cairo_rectangle(cr, 0.0, line_y - thick / 2.0, w, thick);
        cb::cairo_fill(cr);
    }
    // The time rests on the line, the date at the other end.
    let cf = l.f("Light", c.px(104.0));
    let (_, ch) = text_size(cr, &s.clock, &cf);
    c.text(margin, line_y - ch - c.px(4.0), &s.clock, &cf, ink, true);
    let df = l.f("Bold", c.px(13.0));
    let spacing = c.px(5.0);
    let (_, dh) = text_size(cr, "A", &df);
    let dw = spaced_width(cr, &s.date, &df, spacing);
    c.spaced(w - margin - dw, line_y - dh - c.px(16.0), &s.date, &df, alpha(ink, 0.85), spacing, true);

    // A marker steps along the line once a second.
    let tick = progress(SPRING_TICK, s.second.1);
    let along = (s.second.0 as f64 - 1.0 + tick).rem_euclid(60.0) / 60.0;
    cb::fill_circle(cr, along * w, line_y, c.px(5.0), bright(l.primary));

    if s.typed > 0 && !s.entering_user {
        let (r, step) = (c.px(5.5), c.px(26.0));
        let n = s.typed.min(40);
        let x0 = w / 2.0 - (n - 1) as f64 * step / 2.0;
        for i in 0..n {
            cb::fill_circle(cr, x0 + i as f64 * step, line_y, r, ink);
        }
        horizon_status(c, line_y, ink);
    } else {
        c.prompt(w / 2.0, line_y + c.px(34.0), Align::Center, ink, true);
    }
    c.footer(ink, true);
    c.preview_banner(ink);
}

/// The status line while the dots sit on the line itself.
fn horizon_status(c: &Ctx, line_y: f64, ink: Rgba) {
    let s = c.s;
    let y = line_y + c.px(24.0);
    if !s.status.is_empty() {
        let font = c.l.f("Medium", c.px(14.0));
        let (tw, _) = text_size(c.cr, &s.status, &font);
        let col = if s.status_error { bright(c.l.error) } else { alpha(ink, 0.75) };
        c.text((c.w - tw) / 2.0, y, &s.status, &font, col, true);
    } else if s.caps_lock {
        let font = c.l.f("Bold", c.px(11.0));
        c.spaced_at(c.w / 2.0, y, Align::Center, "CAPS LOCK IS ON", &font, bright(c.l.primary), c.px(3.0), true);
    }
}

unsafe fn horizon(c: &mut Ctx, layers: &Layers) {
    let (cr, w, h) = (c.cr, c.w, c.h);
    let line_y = (h * 0.62).round();
    let Some(t) = c.closing_t() else {
        unsafe { horizon_scene(c, layers, line_y, WHITE) };
        return;
    };
    let open = progress((0.6, 0.92), t);
    unsafe {
        c.paint_surface(layers.wall, 1.0);
        for (top, height, shift) in [(0.0, line_y, -line_y * open), (line_y, h - line_y, (h - line_y) * open)] {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, 0.0, top + shift, w, height);
            cb::cairo_clip(cr);
            cb::cairo_translate(cr, 0.0, shift);
            horizon_scene(c, layers, line_y, WHITE);
            cb::cairo_restore(cr);
        }
    }
    c.hits.clear();
}

// ── Sentence ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Word {
    Plain,
    Value,
    /// The greeter's user name: clicking it switches user.
    User,
}

unsafe fn sentence(c: &mut Ctx, layers: &Layers) {
    let (cr, s, l) = (c.cr, c.s, c.l);
    let time = s.clock.trim_start_matches('0').to_string();
    let first = vec![("It's".to_string(), Word::Plain), (time, Word::Value), ("on".to_string(), Word::Plain), (format!("{},", s.weekday), Word::Value)];
    let plain = |t: &str| t.split(' ').map(|w| (w.to_string(), Word::Plain)).collect::<Vec<_>>();
    let second: Vec<(String, Word)> = match (s.mode, s.entering_user) {
        (Mode::Greeter, true) => plain("who goes there?"),
        (Mode::Greeter, false) => {
            let mut v = plain("welcome back,");
            v.push((format!("{}.", s.user.to_lowercase()), Word::User));
            v
        }
        (Mode::Lock, _) if s.notifications == 0 => plain("and nothing needs you."),
        (Mode::Lock, _) => {
            let mut v = plain("and you have");
            v.push((s.notifications.to_string(), Word::Value));
            v.push((if s.notifications == 1 { "notification." } else { "notifications." }.to_string(), Word::Plain));
            v
        }
    };

    // The largest size at which the longest line fits.
    let margin = c.px(150.0);
    let room = c.w - 2.0 * margin;
    let line_width = |font: &str, words: &[(String, Word)]| {
        let joined = words.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" ");
        text_size(cr, &joined, font).0
    };
    let probe = l.f("Light", 100.0);
    let widest = line_width(&probe, &first).max(line_width(&probe, &second));
    let size = (100.0 * room / widest).min(c.px(92.0)).round();
    let font = l.f("Light", size);
    let (space, line_h) = (text_size(cr, "n n", &font).0 - text_size(cr, "nn", &font).0, text_size(cr, "Ag", &font).1);
    let top = c.h * 0.40 - line_h;

    let closing = c.closing_t();
    let accent = bright(l.primary);
    let hide = (c.fx.level(s.now) * 1.6).clamp(0.0, 1.0);
    let leave = |i: usize| closing.map_or(1.0, |t| 1.0 - progress((0.3, 1.0), t - i as f64 * 0.06));
    if let Some(b) = layers.base {
        c.paint_surface(b, 1.0);
    }
    if let Some(t) = closing {
        c.paint_surface(layers.wall, progress((0.75, 1.0), t - 0.15));
    }
    let mut index = 0;
    for (row, words) in [&first, &second].into_iter().enumerate() {
        let mut x = margin;
        let y = top + row as f64 * line_h * 1.12;
        // The password replaces the second half of the sentence.
        let row_alpha = if row == 1 && !s.entering_user { 1.0 - hide } else { 1.0 };
        for (word, kind) in words {
            let (tw, th) = text_size(cr, word, &font);
            let hovered = *kind == Word::User && s.hover == Some(Hit::User);
            let col = if *kind == Word::Plain && !hovered { WHITE } else { accent };
            c.text(x, y, word, &font, alpha(col, row_alpha * leave(index)), true);
            if *kind == Word::User && row_alpha > 0.5 {
                c.hits.push((Hit::User, x, y, tw, th));
            }
            x += tw + space;
            index += 1;
        }
    }
    let ink = alpha(WHITE, leave(index));
    let below = top + 2.0 * line_h * 1.12 + c.px(30.0);
    if s.entering_user {
        c.prompt(margin, below, Align::Left, ink, true);
    } else if s.typed > 0 {
        let (r, step) = (size * 0.13, size * 0.46);
        let y = top + line_h * 1.12 + line_h / 2.0;
        for i in 0..s.typed.min(28) {
            cb::fill_circle(cr, margin + r + i as f64 * step, y, r, alpha(accent, hide * leave(index)));
        }
        sentence_status(c, margin, below, ink);
    } else {
        c.prompt(margin, below, Align::Left, ink, true);
    }
    c.footer(ink, true);
    c.preview_banner(ink);
}

fn sentence_status(c: &Ctx, x: f64, y: f64, ink: Rgba) {
    let s = c.s;
    if !s.status.is_empty() {
        let font = c.l.f("Medium", c.px(15.0));
        let col = if s.status_error { bright(c.l.error) } else { alpha(ink, 0.75) };
        c.text(x, y, &s.status, &font, alpha(col, ink.3), true);
    } else if s.caps_lock {
        let font = c.l.f("Bold", c.px(11.0));
        c.spaced(x, y, "CAPS LOCK IS ON", &font, alpha(bright(c.l.primary), ink.3), c.px(3.0), true);
    }
}

// ── Aperture ─────────────────────────────────────────────────────────────

unsafe fn aperture(c: &mut Ctx, layers: &Layers) {
    let (cr, s, l) = (c.cr, c.s, c.l);
    let (w, h) = (c.w, c.h);
    let (cx, cy) = (w / 2.0, h * 0.40);
    let base = h * 0.235;
    let closing = c.closing_t();
    let open = closing.map_or(0.0, |t| progress((0.62, 1.0), t));
    let far = (w * w + h * h).sqrt();
    let r = (base * (1.0 + 0.055 * c.fx.level(s.now).min(9.0)) * c.fx.shut.value(s.now).max(0.0) + open * far).max(1.0);
    let feather = c.px(26.0).min(r);
    let tau = std::f64::consts::TAU;
    unsafe {
        cb::cairo_set_source_rgba(cr, 0.024, 0.024, 0.03, 1.0);
        cb::cairo_paint(cr);
        cb::cairo_new_path(cr);
        cb::cairo_arc(cr, cx, cy, r, 0.0, tau);
        cb::cairo_set_source_surface(cr, layers.wall, 0.0, 0.0);
        cb::cairo_fill(cr);
        // A soft edge: only the thin ring is a gradient.
        let ring = cb::cairo_pattern_create_radial(cx, cy, r - feather, cx, cy, r + 0.5);
        cb::cairo_pattern_add_color_stop_rgba(ring, 0.0, 0.024, 0.024, 0.03, 0.0);
        cb::cairo_pattern_add_color_stop_rgba(ring, 1.0, 0.024, 0.024, 0.03, 1.0);
        cb::cairo_new_path(cr);
        cb::cairo_arc(cr, cx, cy, r + 1.0, 0.0, tau);
        cb::cairo_new_sub_path(cr);
        cb::cairo_arc_negative(cr, cx, cy, r - feather, tau, 0.0);
        cb::cairo_set_source(cr, ring);
        cb::cairo_fill(cr);
        cb::cairo_pattern_destroy(ring);
    }
    let ink = alpha(WHITE, (1.0 - open * 5.0).clamp(0.0, 1.0));
    if ink.3 > 0.0 {
        let y = h * 0.775;
        let cf = l.f("Medium", c.px(40.0));
        let (tw, th) = text_size(cr, &s.clock, &cf);
        c.text((w - tw) / 2.0, y, &s.clock, &cf, ink, false);
        let df = l.f("Bold", c.px(11.0));
        c.spaced_at(w / 2.0, y + th + c.px(4.0), Align::Center, &s.date, &df, alpha(ink, 0.6), c.px(4.0), false);
        c.prompt(w / 2.0, y + th + c.px(52.0), Align::Center, ink, false);
        c.footer(ink, false);
        c.preview_banner(ink);
    }
}

// ── Tide ─────────────────────────────────────────────────────────────────

/// Adds a splash outline to the path: a wobbly blob and the droplets thrown around it.
unsafe fn splash_path(cr: *mut Cairo, x: f64, y: f64, r: f64, seed: u64) {
    const LOBES: usize = 16;
    let tau = std::f64::consts::TAU;
    let point = |i: usize| {
        let a = tau * i as f64 / LOBES as f64;
        let k = r * (0.74 + 0.5 * noise(seed, (i % LOBES) as u64));
        (x + a.cos() * k, y + a.sin() * k)
    };
    let mid = |a: (f64, f64), b: (f64, f64)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    unsafe {
        let start = mid(point(0), point(1));
        cb::cairo_new_sub_path(cr);
        cb::cairo_move_to(cr, start.0, start.1);
        for i in 1..=LOBES {
            let (p, end) = (point(i), mid(point(i), point(i + 1)));
            let from = mid(point(i - 1), p);
            let ctl = |a: f64, b: f64| a + (b - a) * 2.0 / 3.0;
            cb::cairo_curve_to(cr, ctl(from.0, p.0), ctl(from.1, p.1), ctl(end.0, p.0), ctl(end.1, p.1), end.0, end.1);
        }
        cb::cairo_close_path(cr);
        for i in 0..9u64 {
            let a = tau * noise(seed, 100 + i);
            let d = r * (1.2 + 0.85 * noise(seed, 200 + i));
            let dr = r * (0.03 + 0.1 * noise(seed, 300 + i));
            cb::cairo_new_sub_path(cr);
            cb::cairo_arc(cr, x + a.cos() * d, y + a.sin() * d, dr, 0.0, tau);
        }
    }
}

unsafe fn tide(c: &mut Ctx, layers: &Layers) {
    let (cr, s, l) = (c.cr, c.s, c.l);
    let (w, h) = (c.w, c.h);
    let closing = c.closing_t();
    let far = (w * w + h * h).sqrt();
    let unit = w.min(h);
    unsafe {
        if let Some(b) = layers.base {
            c.paint_surface(b, 1.0);
        }
        cb::cairo_new_path(cr);
        for (i, sp) in c.fx.splashes.iter().enumerate() {
            let mut r = sp.radius * unit * sp.grow.value(s.now).max(0.0);
            if let Some(t) = closing {
                // Each splash spreads a moment after the one before it, until they all meet.
                r += far * 1.3 * progress((0.8, 1.0), t - (i % 12) as f64 * 0.035).powi(2);
            }
            if r > 0.5 {
                splash_path(cr, sp.x * w, sp.y * h, r, sp.seed);
            }
        }
        cb::cairo_set_source_surface(cr, layers.vivid.unwrap_or(layers.wall), 0.0, 0.0);
        cb::cairo_fill(cr);
        // The colours settle into the real wallpaper.
        if let Some(t) = closing {
            let whole = ((t - 0.85) / 0.3).clamp(0.0, 1.0);
            if whole > 0.0 {
                c.paint_surface(layers.wall, whole);
            }
        }
    }
    let ink = alpha(WHITE, closing.map_or(1.0, |t| (1.0 - t * 5.0).clamp(0.0, 1.0)));
    if ink.3 > 0.0 {
        let (x, mut y) = (c.px(96.0), h - c.px(300.0));
        let cf = l.f("Bold", c.px(72.0));
        let (_, th) = text_size(cr, &s.clock, &cf);
        c.text(x, y, &s.clock, &cf, ink, true);
        y += th + c.px(2.0);
        let df = l.f("Bold", c.px(12.0));
        c.spaced(x, y, &s.date, &df, alpha(ink, 0.85), c.px(4.0), true);
        c.prompt(x, y + c.px(52.0), Align::Left, ink, true);
        c.footer(ink, true);
        c.preview_banner(ink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const THEMES: [(LockTheme, &str); 5] = [
        (LockTheme::Stencil, "stencil"),
        (LockTheme::Horizon, "horizon"),
        (LockTheme::Sentence, "sentence"),
        (LockTheme::Aperture, "aperture"),
        (LockTheme::Tide, "tide"),
    ];

    fn look(theme: LockTheme) -> Look {
        Look {
            fg: (0.9, 0.85, 0.95, 1.0),
            primary: (0.8, 0.5, 0.9, 1.0),
            error: (0.95, 0.4, 0.4, 1.0),
            outline: (1.0, 1.0, 1.0, 0.4),
            background: (0.07, 0.06, 0.09, 1.0),
            container: (0.28, 0.16, 0.36, 1.0),
            right: true,
            session_controls: true,
            font: "Inter".into(),
            wallpaper: std::env::var("PRISM_LOCK_RENDER_WALL").ok(),
            theme,
        }
    }

    fn scene(now: Instant, opened: Instant, closing: Option<Instant>, typed: usize, mode: Mode) -> Scene {
        Scene {
            mode,
            now,
            opened,
            closing,
            clock: "11:42".into(),
            date: "WEDNESDAY / NOVEMBER 18".into(),
            weekday: "Wednesday".into(),
            second: (24, 0.1),
            user: "operative".into(),
            entering_user: false,
            typed_name: String::new(),
            typed,
            status: String::new(),
            status_error: false,
            caps_lock: false,
            notifications: 3,
            session: "MANGO".into(),
            power_buttons: true,
            hover: None,
            preview: false,
        }
    }

    /// Paints the states every design goes through, 1440p like the design size. With
    /// PRISM_LOCK_RENDER_DIR set the frames are written there as PNGs and frame times printed.
    #[test]
    fn every_design_paints_every_state() {
        let dir = std::env::var("PRISM_LOCK_RENDER_DIR").ok();
        let (w, h) = (2560, 1440);
        for (theme, name) in THEMES {
            let l = look(theme);
            let layers = Layers::render(&l, w, h);
            let t0 = Instant::now();
            let mut fx = Fx::new(theme, t0);
            let settled = t0 + Duration::from_secs(4);
            for n in 1..=7 {
                fx.sync(n, false, None, false, t0 + Duration::from_millis(1000 + n as u64 * 200));
            }
            let close_at = settled + Duration::from_secs(1);
            let frames: [(&str, Instant, Option<Instant>, usize); 5] = [
                ("idle", settled, None, 0),
                ("typing", settled, None, 7),
                ("unlock-early", close_at + Duration::from_millis(180), Some(close_at), 7),
                ("unlock-mid", close_at + Duration::from_millis(380), Some(close_at), 7),
                ("unlock-end", close_at + Duration::from_millis(theme.closing_ms() as u64), Some(close_at), 7),
            ];
            for (state, now, closing, typed) in frames {
                let mut sc = scene(now, t0, closing, typed, Mode::Lock);
                if state == "idle" {
                    sc.typed = 0;
                }
                let mut f = Fx::new(theme, t0);
                for n in 1..=typed {
                    f.sync(n, false, None, false, t0 + Duration::from_millis(1000 + n as u64 * 200));
                }
                if closing.is_some() {
                    f.sync(typed, false, None, true, close_at);
                }
                let img = unsafe { cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h) };
                let cr = unsafe { cb::cairo_create(img) };
                let started = Instant::now();
                let hits = unsafe { paint(cr, &l, &sc, &f, &layers, w, h) };
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                if closing.is_none() {
                    assert!(hits.iter().any(|h| h.0 == Hit::Shutdown), "{name} {state}");
                }
                unsafe { cb::cairo_destroy(cr) };
                if let Some(d) = &dir {
                    println!("{name:9} {state:13} {ms:6.1} ms");
                    let path = std::ffi::CString::new(format!("{d}/{name}-{state}.png")).unwrap();
                    unsafe { cb::cairo_surface_write_to_png(img, path.as_ptr()) };
                }
                unsafe { cb::cairo_surface_destroy(img) };
            }
            drop(fx);
        }
    }

    #[test]
    fn the_greeter_always_offers_session_and_power() {
        let (w, h) = (1280, 720);
        for (theme, name) in THEMES {
            let l = look(theme);
            let layers = Layers::render(&l, w, h);
            let t0 = Instant::now();
            let fx = Fx::new(theme, t0);
            let mut sc = scene(t0 + Duration::from_secs(3), t0, None, 0, Mode::Greeter);
            sc.power_buttons = false;
            let img = unsafe { cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h) };
            let cr = unsafe { cb::cairo_create(img) };
            let hits = unsafe { paint(cr, &l, &sc, &fx, &layers, w, h) };
            unsafe {
                cb::cairo_destroy(cr);
                cb::cairo_surface_destroy(img);
            }
            for want in [Hit::User, Hit::Session, Hit::Shutdown, Hit::Reboot] {
                assert!(hits.iter().any(|h| h.0 == want), "{name} {want:?}");
            }
        }
    }

    #[test]
    fn the_lock_screen_shows_user_and_session_as_labels() {
        let (w, h) = (1280, 720);
        for (theme, name) in THEMES {
            let l = look(theme);
            let layers = Layers::render(&l, w, h);
            let t0 = Instant::now();
            let fx = Fx::new(theme, t0);
            let sc = scene(t0 + Duration::from_secs(3), t0, None, 0, Mode::Lock);
            let img = unsafe { cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h) };
            let cr = unsafe { cb::cairo_create(img) };
            let hits = unsafe { paint(cr, &l, &sc, &fx, &layers, w, h) };
            unsafe {
                cb::cairo_destroy(cr);
                cb::cairo_surface_destroy(img);
            }
            assert!(!hits.iter().any(|h| matches!(h.0, Hit::User | Hit::Session)), "{name}");
            assert!(hits.iter().any(|h| h.0 == Hit::Reboot), "{name}");
        }
    }

    #[test]
    fn typing_adds_splashes_and_a_failure_clears_them() {
        let t0 = Instant::now();
        let mut fx = Fx::new(LockTheme::Tide, t0);
        fx.sync(4, false, None, false, t0);
        assert_eq!(fx.splashes.iter().filter(|s| !s.dying).count(), 4);
        fx.sync(3, false, None, false, t0);
        assert_eq!(fx.splashes.iter().filter(|s| !s.dying).count(), 3);
        fx.sync(0, false, Some(t0), false, t0);
        assert_eq!(fx.splashes.iter().filter(|s| !s.dying).count(), 0);
    }

    #[test]
    fn the_unlock_always_has_colour_to_spread() {
        let t0 = Instant::now();
        let mut fx = Fx::new(LockTheme::Tide, t0);
        fx.sync(0, false, None, true, t0);
        assert!(fx.splashes.len() >= 6);
    }
}
