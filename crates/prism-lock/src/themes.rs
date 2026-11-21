//! The lockscreen and greeter designs other than Classic (which is `App::paint` in main.rs).
//!
//! * Stencil: solid colour with the time cut out of it; unlocking flies through a digit.
//! * Sentence: the state of the machine as one line of text; words leave one by one.
//! * Aperture: a round window onto the wallpaper that opens as you type.
//! * Tide: a grey wallpaper that keys splash colour onto; unlocking spreads it over everything.
//! * Ink: a paper-white wallpaper that keys drop bleeding ink blots onto; unlocking floods it.
//! * Rings: the wallpaper cut into concentric rings turned out of line; each key turns them, and
//!   unlocking turns every ring on until the picture lines up.
//! * Fireflies: the wallpaper at night, lit by a firefly per key; unlocking swarms into daylight.
//!   Over a video each firefly is a soft hole in the night.
//! * Halftone: the wallpaper as a dot screen whose dots swell and merge as you type; unlocking merges them into it.
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
    Sentence,
    Aperture,
    Tide,
    Ink,
    Rings,
    Fireflies,
    Halftone,
}

impl LockTheme {
    pub fn from_id(id: &str) -> LockTheme {
        match id {
            "stencil" => LockTheme::Stencil,
            "sentence" => LockTheme::Sentence,
            "aperture" => LockTheme::Aperture,
            "tide" => LockTheme::Tide,
            "ink" => LockTheme::Ink,
            "rings" => LockTheme::Rings,
            "fireflies" => LockTheme::Fireflies,
            "halftone" => LockTheme::Halftone,
            _ => LockTheme::Classic,
        }
    }

    /// How long the unlock animation runs.
    pub fn closing_ms(self) -> f64 {
        match self {
            LockTheme::Classic => super::FADE_OUT_MS,
            LockTheme::Stencil => 850.0,
            LockTheme::Sentence => 950.0,
            LockTheme::Aperture => 800.0,
            LockTheme::Tide => 1200.0,
            LockTheme::Ink => 1300.0,
            LockTheme::Rings => 1500.0,
            LockTheme::Fireflies => 1300.0,
            LockTheme::Halftone => 1000.0,
        }
    }

    /// The design reworks the wallpaper picture itself, which a video wallpaper does not allow.
    pub fn needs_picture(self) -> bool {
        matches!(self, LockTheme::Tide | LockTheme::Ink | LockTheme::Halftone | LockTheme::Rings)
    }


    /// Each key adds a mark (a splash, blot or firefly) to `Fx::splashes`.
    fn marks(self) -> bool {
        matches!(self, LockTheme::Tide | LockTheme::Ink | LockTheme::Fireflies)
    }
}

// ── Springs ──────────────────────────────────────────────────────────────

/// (response s, damping ratio).
type Params = (f64, f64);
const SPRING_LEVEL: Params = (0.34, 0.72);
const SPRING_SPLASH: Params = (0.42, 0.68);
const SPRING_SHUT: Params = (0.12, 1.0);
const SPRING_REOPEN: Params = (0.55, 0.8);
const SPRING_FIREFLY: Params = (0.7, 0.75);
const SPRING_RING: Params = (0.5, 0.78);
const SPRING_RING_HOME: Params = (0.6, 0.72);
const SPRING_RING_DONE: Params = (0.75, 0.9);
/// Rings: how many rings the wallpaper is cut into.
const RINGS: usize = 8;
/// Ink: how long a blot keeps bleeding outward (s).
const BLEED_S: f64 = 4.0;
/// Halftone: how long a failure shakes the dots (s).
const SHUDDER_S: f64 = 0.6;
/// Fireflies: at most this many are drawn.
const MAX_FIREFLIES: usize = 24;

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

/// A mark made by a key: a splash of colour (Tide), an ink blot or a firefly. Position in 0..1
/// screen fractions.
struct Splash {
    x: f64,
    y: f64,
    radius: f64,
    seed: u64,
    grow: Spring,
    dying: bool,
    born: Instant,
}

impl Splash {
    fn age(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.born).as_secs_f64()
    }
}

/// What the designs animate between frames, driven by the App's state (`sync`).
pub struct Fx {
    theme: LockTheme,
    typed: usize,
    level: Spring,
    shut: Spring,
    splashes: Vec<Splash>,
    rng: u64,
    /// Fixed for the whole lock: what must not change between frames (the Rings' turns).
    seed: u64,
    failed_at: Option<Instant>,
    closing: bool,
    /// Stencil: the point inside a digit that the unlock flies through.
    pivot: Cell<Option<(f64, f64)>>,
    /// Sentence: what it says without notifications, chosen once per lock.
    idle_line: &'static str,
    /// Rings: each ring's turn (radians) as a spring, the scrambled turn it rests at, and how far
    /// one key turns it.
    turns: Vec<Spring>,
    home: Vec<f64>,
    steps: Vec<f64>,
    /// Rings: which ring the n-th key turns (every ring once, in a random order, then again).
    order: Vec<usize>,
}

/// Sentence: the second line when no notifications are waiting.
const IDLE_LINES: [&str; 14] = [
    "You don't have any pending notifications.",
    "You're all caught up.",
    "All quiet for now.",
    "No news is good news.",
    "All clear, take your time.",
    "Everything's taken care of.",
    "You're free to relax.",
    "A perfectly peaceful moment.",
    "All good here.",
    "Enjoy the calm.",
    "Peace and quiet, just for you.",
    "Not a single thing to worry about.",
    "Time to breathe.",
    "Welcome back, all is well.",
];

/// The idle line for a random `seed`.
fn idle_line(seed: u64) -> &'static str {
    IDLE_LINES[(noise(seed, 11) * IDLE_LINES.len() as f64) as usize % IDLE_LINES.len()]
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
            seed: rng,
            idle_line: idle_line(rng),
            turns: Vec::new(),
            home: Vec::new(),
            steps: Vec::new(),
            order: Vec::new(),
            failed_at: None,
            closing: false,
            pivot: Cell::new(None),
        }
        .with_rings(now)
    }

    /// Rings: every ring turned out of line, by at least a little, alternately one way and the
    /// other; a key turns one ring on by its own step (a dial's worth).
    fn with_rings(mut self, now: Instant) -> Fx {
        if self.theme == LockTheme::Rings {
            for i in 0..RINGS as u64 {
                let side = if noise(self.seed, 40 + i) < 0.5 { -1.0 } else { 1.0 };
                let home = side * (0.45 + 2.2 * noise(self.seed, 50 + i));
                self.home.push(home);
                self.turns.push(Spring::new(home, now, SPRING_RING));
                let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
                self.steps.push(dir * (0.6 + 0.5 * noise(self.seed, 60 + i)));
            }
            let mut order: Vec<usize> = (0..RINGS).collect();
            order.sort_by(|a, b| noise(self.seed, 70 + *a as u64).total_cmp(&noise(self.seed, 70 + *b as u64)));
            self.order = order;
        }
        self
    }

    /// Rings: each ring's turn now.
    fn ring_turns(&self, now: Instant) -> Vec<f64> {
        self.turns.iter().map(|t| t.value(now)).collect()
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
        let params = if self.theme == LockTheme::Fireflies { SPRING_FIREFLY } else { SPRING_SPLASH };
        let mut grow = Spring::new(0.0, now, params);
        grow.set(1.0, now, params);
        self.splashes.push(Splash { x: best.0, y: best.1, radius, seed, grow, dying: false, born: now });
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
            // Enough to spread from (colour, ink, a swarm), however short the password was.
            let least = match self.theme {
                LockTheme::Tide | LockTheme::Ink => 6,
                LockTheme::Fireflies => 5,
                _ => 0,
            };
            while self.splashes.iter().filter(|s| !s.dying).count() < least {
                self.add_splash(now);
            }
            // Rings: each ring keeps turning the way it was going until it lines up, the inner
            // ones first.
            let tau = std::f64::consts::TAU;
            for (i, turn) in self.turns.iter_mut().enumerate() {
                let (x, v) = turn.sample(now);
                let dir = if v.abs() > 0.05 { v.signum() } else { self.steps[i].signum() };
                let mut to = if dir > 0.0 { (x / tau).ceil() * tau } else { (x / tau).floor() * tau };
                if (to - x).abs() < 0.6 {
                    to += dir * tau;
                }
                let start = now + std::time::Duration::from_secs_f64(0.05 * i as f64);
                *turn = Spring { x0: x, v0: v, to, start, params: SPRING_RING_DONE };
            }
        }
        if failed_at != self.failed_at {
            self.failed_at = failed_at;
            if failed_at.is_some() {
                self.shut.set(0.08, now, SPRING_SHUT);
                self.drop_splashes(0, now);
                // Rings: back to their scrambled turns, one after another in a random order.
                let tau = std::f64::consts::TAU;
                let salt = self.next();
                for (i, turn) in self.turns.iter_mut().enumerate() {
                    let (x, v) = turn.sample(now);
                    let home = self.home[i] + tau * ((x - self.home[i]) / tau).round();
                    let start = now + std::time::Duration::from_secs_f64(0.5 * noise((salt * 1e15) as u64, i as u64));
                    *turn = Spring { x0: x, v0: v, to: home, start, params: SPRING_RING_HOME };
                }
            }
        }
        // The aperture opens again once it has snapped shut.
        if self.shut.to < 1.0 && self.shut.value(now) < 0.14 {
            self.shut.set(1.0, now, SPRING_REOPEN);
        }
        if typed != self.typed && !checking && !self.closing {
            // Rings: each key turns one ring a step; backspace turns that ring back. The password
            // being cleared after a failure is not a turn: the rings are already going home.
            let cleared = typed == 0 && self.failed_at.is_some_and(|t| now.saturating_duration_since(t).as_secs_f64() < 1.5);
            if !cleared && !self.order.is_empty() {
                let (keys, sign) = if typed > self.typed { (self.typed..typed, 1.0) } else { (typed..self.typed, -1.0) };
                for k in keys {
                    let i = self.order[k % self.order.len()];
                    let to = self.turns[i].to + sign * self.steps[i];
                    self.turns[i].set(to, now, SPRING_RING);
                }
            }
            if self.theme.marks() {
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
        let moving = match self.theme {
            LockTheme::Ink => self.splashes.iter().any(|s| s.age(now) < BLEED_S),
            LockTheme::Rings => self.turns.iter().any(|t| !t.settled(now)),
            // Fireflies never stop wandering.
            LockTheme::Fireflies => !self.splashes.is_empty(),
            LockTheme::Halftone => self.shudder(now) > 0.0,
            _ => false,
        };
        moving || !self.level.settled(now) || !self.shut.settled(now) || self.splashes.iter().any(|s| !s.grow.settled(now))
    }

    /// Halftone: how far into the shake after a failure (0 when not shaking).
    fn shudder(&self, now: Instant) -> f64 {
        self.failed_at.map_or(0.0, |t| {
            let a = now.saturating_duration_since(t).as_secs_f64();
            if a < SHUDDER_S { a.max(1e-6) } else { 0.0 }
        })
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
    /// Tide: the wallpaper with stronger colour, what the splashes are made of. Ink: the ink.
    vivid: Option<*mut CairoSurface>,
    /// Halftone: the dot screen it was drawn from, and the dots as last swollen (`merged_at`).
    dots: Option<Dots>,
    merged: Option<*mut CairoSurface>,
    merged_at: Cell<f64>,
    /// Rings: which ring each pixel is in, and the scrambled picture as last drawn (`turned_at`).
    rings: Option<RingMap>,
    turned: Option<*mut CairoSurface>,
    turned_at: std::cell::RefCell<Vec<f64>>,
}

/// Halftone: one dot per cell, coloured from the wallpaper.
struct Dots {
    cell: f64,
    cols: usize,
    rows: usize,
    /// Colour and radius per cell, row by row.
    dot: Vec<(Rgba, f64)>,
}

/// Halftone: the colour behind the dots.
const DOT_BG: Rgba = (0.035, 0.035, 0.045, 1.0);

impl Drop for Layers {
    fn drop(&mut self) {
        unsafe {
            cb::cairo_surface_destroy(self.wall);
            for s in [self.base, self.vivid, self.merged, self.turned].into_iter().flatten() {
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
            // With a video the wallpaper is clear: the video plays underneath this surface.
            if look.video.is_none() {
                let bg = look.background;
                cb::cairo_set_source_rgba(cr, bg.0, bg.1, bg.2, 1.0);
                cb::cairo_paint(cr);
                if let Some(p) = &look.wallpaper {
                    cb::draw_image_cover(cr, p, 0.0, 0.0, w as f64, h as f64, 0.0);
                }
            }
            cb::cairo_destroy(cr);
            cb::cairo_surface_flush(wall);
            let dots = (look.theme == LockTheme::Halftone).then(|| dot_screen(wall, w, h));
            let base = match look.theme {
                LockTheme::Sentence => Some(veiled(wall, w, h, 0.62)),
                LockTheme::Tide => Some(greyed(wall, w, h)),
                LockTheme::Ink => Some(recoloured(wall, w, h, PAPER_DARK, PAPER)),
                LockTheme::Fireflies => Some(veiled(wall, w, h, 0.86)),
                LockTheme::Halftone => dots.as_ref().map(|d| draw_dots(d, w, h)),
                _ => None,
            };
            let vivid = match look.theme {
                LockTheme::Tide => Some(saturated(wall, w, h)),
                LockTheme::Ink => {
                    let c = look.container;
                    let dark = (c.0 * 0.25, c.1 * 0.25, c.2 * 0.25, 1.0);
                    let light = (c.0 + (1.0 - c.0) * 0.45, c.1 + (1.0 - c.1) * 0.45, c.2 + (1.0 - c.2) * 0.45, 1.0);
                    Some(recoloured(wall, w, h, dark, light))
                }
                LockTheme::Fireflies => Some(warmed(wall, w, h)),
                _ => None,
            };
            let rings = (look.theme == LockTheme::Rings).then(|| RingMap::new(w as usize, h as usize));
            let turned = rings.is_some().then(|| cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h));
            let merged = dots.is_some().then(|| cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h));
            Layers { size: (w, h), wall, base, vivid, dots, merged, merged_at: Cell::new(f64::NAN), rings, turned, turned_at: Default::default() }
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

/// Ink: the paper, and the darkest the paper gets where the wallpaper is black.
const PAPER: Rgba = (0.965, 0.95, 0.915, 1.0);
const PAPER_DARK: Rgba = (0.80, 0.78, 0.74, 1.0);

/// The wallpaper's brightness mapped from `dark` (black) to `light` (white).
unsafe fn recoloured(wall: *mut CairoSurface, w: i32, h: i32, dark: Rgba, light: Rgba) -> *mut CairoSurface {
    unsafe {
        let s = veiled(wall, w, h, 0.0);
        cb::cairo_surface_flush(s);
        let ramp: Vec<u32> = (0..256u32)
            .map(|l| {
                let t = l as f64 / 255.0;
                let ch = |a: f64, b: f64| ((a + (b - a) * t) * 255.0).round().clamp(0.0, 255.0) as u32;
                0xFF00_0000 | ch(dark.0, light.0) << 16 | ch(dark.1, light.1) << 8 | ch(dark.2, light.2)
            })
            .collect();
        let (data, stride) = (cb::cairo_image_surface_get_data(s), cb::cairo_image_surface_get_stride(s) as usize);
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts_mut(data.add(y * stride) as *mut u32, w as usize);
            for p in row {
                let v = *p;
                *p = ramp[((54 * ((v >> 16) & 255) + 183 * ((v >> 8) & 255) + 19 * (v & 255)) >> 8) as usize];
            }
        }
        cb::cairo_surface_mark_dirty(s);
        s
    }
}

/// `src` drawn at `factor` of its size into a new surface (bilinear when growing).
unsafe fn rescaled(src: *mut CairoSurface, w: i32, h: i32, factor: f64) -> *mut CairoSurface {
    unsafe {
        let s = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h);
        let cr = cb::cairo_create(s);
        cb::cairo_scale(cr, factor, factor);
        cb::cairo_set_source_surface(cr, src, 0.0, 0.0);
        cb::cairo_pattern_set_filter(cb::cairo_get_source(cr), cb::CAIRO_FILTER_BILINEAR);
        // Repeat the edge pixels rather than blend in the transparent outside.
        cb::cairo_pattern_set_extend(cb::cairo_get_source(cr), 3);
        cb::cairo_paint(cr);
        cb::cairo_destroy(cr);
        s
    }
}

/// Halftone: the cells and their dots, sampled from the wallpaper shrunk to one pixel per cell.
unsafe fn dot_screen(wall: *mut CairoSurface, w: i32, h: i32) -> Dots {
    let cell = (18.0 * h as f64 / super::DESIGN_H).round().max(6.0);
    let (cols, rows) = ((w as f64 / cell).ceil() as usize, (h as f64 / cell).ceil() as usize);
    let mut dot = Vec::with_capacity(cols * rows);
    unsafe {
        let small = rescaled(wall, cols as i32, rows as i32, 1.0 / cell);
        cb::cairo_surface_flush(small);
        let (data, stride) = (cb::cairo_image_surface_get_data(small), cb::cairo_image_surface_get_stride(small) as usize);
        for y in 0..rows {
            let row = std::slice::from_raw_parts(data.add(y * stride) as *const u32, cols);
            for &v in row {
                let (r, g, b) = (((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0);
                let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                // Colour pushed from grey and lifted, so even dark parts read as dots.
                let push = |c: f64| (l + (c - l) * 1.6).clamp(0.0, 1.0);
                let lift = |c: f64| 0.18 + 0.82 * c;
                dot.push(((lift(push(r)), lift(push(g)), lift(push(b)), 1.0), cell * 0.5 * (0.22 + 0.78 * l.sqrt())));
            }
        }
        cb::cairo_surface_destroy(small);
    }
    Dots { cell, cols, rows, dot }
}

unsafe fn draw_dots(d: &Dots, w: i32, h: i32) -> *mut CairoSurface {
    unsafe {
        let s = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h);
        let cr = cb::cairo_create(s);
        cb::cairo_set_source_rgba(cr, DOT_BG.0, DOT_BG.1, DOT_BG.2, 1.0);
        cb::cairo_paint(cr);
        for (i, (c, r)) in d.dot.iter().enumerate() {
            let (x, y) = ((i % d.cols) as f64 + 0.5, (i / d.cols) as f64 + 0.5);
            cb::fill_circle(cr, x * d.cell, y * d.cell, *r, *c);
        }
        cb::cairo_destroy(cr);
        s
    }
}

/// Fireflies: the lights are drawn at 1/this of the screen's size (they are soft anyway): many
/// large lights at full size cost a frame its time, most of all as they grow on the unlock.
const LIGHT_SCALE: f64 = 4.0;
/// Fireflies: the radius of a firefly's light, as a fraction of the shorter screen side.
const FIREFLY_LIGHT: f64 = 0.24;

/// Fireflies: the wallpaper as a firefly lights it, brighter and a little warm.
unsafe fn warmed(wall: *mut CairoSurface, w: i32, h: i32) -> *mut CairoSurface {
    unsafe {
        let s = veiled(wall, w, h, 0.0);
        let cr = cb::cairo_create(s);
        cb::cairo_set_source_rgba(cr, 1.0, 0.86, 0.45, 0.16);
        cb::cairo_paint(cr);
        cb::cairo_destroy(cr);
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

    /// Bring the wallpaper back over what is drawn, `a` of the way.
    fn reveal_wallpaper(&self, layers: &Layers, a: f64) {
        if self.l.video.is_none() {
            return self.paint_surface(layers.wall, a);
        }
        unsafe {
            cb::cairo_save(self.cr);
            cb::cairo_set_operator(self.cr, cb::CAIRO_OPERATOR_DEST_OUT);
            cb::cairo_set_source_rgba(self.cr, 0.0, 0.0, 0.0, a.clamp(0.0, 1.0));
            cb::cairo_paint(self.cr);
            cb::cairo_restore(self.cr);
        }
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
        if look.video.is_some() {
            // The buffer is reused: what is not drawn this frame must show the video.
            cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_CLEAR);
            cb::cairo_paint(cr);
            cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_OVER);
        }
        match look.theme {
            LockTheme::Stencil => stencil(&mut c, layers),
            LockTheme::Sentence => sentence(&mut c, layers),
            LockTheme::Aperture => aperture(&mut c, layers),
            LockTheme::Tide => tide(&mut c, layers),
            LockTheme::Ink => ink(&mut c, layers),
            LockTheme::Rings => rings(&mut c, layers),
            LockTheme::Fireflies => fireflies(&mut c, layers),
            LockTheme::Halftone => halftone(&mut c, layers),
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
        cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_SOURCE);
        cb::cairo_set_line_join(cr, 1);
        cb::cairo_fill_preserve(cr);
        cb::cairo_set_line_width(cr, c.sc * zoom * 1.5 * level);
        cb::cairo_stroke(cr);
        cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_OVER);
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
            c.reveal_wallpaper(layers, ((fly - 0.8) / 0.18).clamp(0.0, 1.0));
        }
    }
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
        (Mode::Lock, _) if s.notifications == 0 => plain(c.fx.idle_line),
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
        c.reveal_wallpaper(layers, progress((0.75, 1.0), t - 0.15));
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
        cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_SOURCE);
        cb::cairo_fill(cr);
        cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_OVER);
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
    let (cr, s) = (c.cr, c.s);
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
        corner_block(c, ink, true);
    }
}

/// Time, date and prompt in the bottom-left corner, then the footer.
fn corner_block(c: &mut Ctx, ink: Rgba, shadow: bool) {
    let (s, l) = (c.s, c.l);
    let (x, mut y) = (c.px(96.0), c.h - c.px(300.0));
    let cf = l.f("Bold", c.px(72.0));
    let (_, th) = text_size(c.cr, &s.clock, &cf);
    c.text(x, y, &s.clock, &cf, ink, shadow);
    y += th + c.px(2.0);
    let df = l.f("Bold", c.px(12.0));
    c.spaced(x, y, &s.date, &df, alpha(ink, 0.85), c.px(4.0), shadow);
    c.prompt(x, y + c.px(52.0), Align::Left, ink, shadow);
    c.footer(ink, shadow);
    c.preview_banner(ink);
}

/// A large light time centred at `y`, the date and prompt under it, then the footer.
fn center_block(c: &mut Ctx, y: f64, ink: Rgba, shadow: bool) {
    let (s, l) = (c.s, c.l);
    let cf = l.f("Light", c.px(112.0));
    let (tw, th) = text_size(c.cr, &s.clock, &cf);
    c.text((c.w - tw) / 2.0, y - th / 2.0, &s.clock, &cf, ink, shadow);
    let df = l.f("Bold", c.px(13.0));
    let dy = y + th / 2.0 + c.px(2.0);
    c.spaced_at(c.w / 2.0, dy, Align::Center, &s.date, &df, alpha(ink, 0.85), c.px(5.0), shadow);
    c.prompt(c.w / 2.0, dy + c.px(70.0), Align::Center, ink, shadow);
    c.footer(ink, shadow);
    c.preview_banner(ink);
}

/// Text fades out over the first fifth of a second of the unlock.
fn text_fade(closing: Option<f64>) -> f64 {
    closing.map_or(1.0, |t| (1.0 - t * 5.0).clamp(0.0, 1.0))
}

// ── Ink ──────────────────────────────────────────────────────────────────

/// The blots as one path: each grows with its spring, keeps bleeding for a few seconds, and
/// on the unlock floods outward a moment after the one before.
unsafe fn ink_path(c: &Ctx) {
    let (s, w, h) = (c.s, c.w, c.h);
    let unit = w.min(h);
    let far = (w * w + h * h).sqrt();
    unsafe {
        cb::cairo_new_path(c.cr);
        for (i, sp) in c.fx.splashes.iter().enumerate() {
            let bleed = 1.0 + 0.4 * (1.0 - (-sp.age(s.now) / 1.6).exp());
            let mut r = sp.radius * 0.7 * unit * sp.grow.value(s.now).max(0.0) * bleed;
            if let Some(t) = c.closing_t() {
                r += far * 1.3 * progress((0.95, 1.0), t - (i % 12) as f64 * 0.05).powi(2);
            }
            if r > 0.5 {
                splash_path(c.cr, sp.x * w, sp.y * h, r, sp.seed);
            }
        }
    }
}

unsafe fn ink(c: &mut Ctx, layers: &Layers) {
    let cr = c.cr;
    let closing = c.closing_t();
    unsafe {
        if let Some(b) = layers.base {
            c.paint_surface(b, 1.0);
        }
        ink_path(c);
        cb::cairo_set_source_surface(cr, layers.vivid.unwrap_or(layers.wall), 0.0, 0.0);
        cb::cairo_fill(cr);
        let fade = text_fade(closing);
        if fade > 0.0 {
            // Dark ink on the paper, paper colour where a blot runs under the text.
            let y = c.h * 0.36;
            let dark = (0.10, 0.10, 0.14, fade);
            center_block(c, y, dark, false);
            let kept = c.hits.len();
            cb::cairo_save(cr);
            ink_path(c);
            cb::cairo_clip(cr);
            center_block(c, y, alpha(PAPER, fade), false);
            cb::cairo_restore(cr);
            c.hits.truncate(kept);
        }
        if let Some(t) = closing {
            let whole = ((t - 0.95) / 0.3).clamp(0.0, 1.0);
            if whole > 0.0 {
                c.paint_surface(layers.wall, whole);
            }
        }
    }
}

// ── Rings ────────────────────────────────────────────────────────────────

/// Rings: which ring every pixel lies in (rings of equal width out to the screen's corners).
struct RingMap {
    w: usize,
    h: usize,
    ring: Vec<u8>,
}

impl RingMap {
    fn new(w: usize, h: usize) -> RingMap {
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let reach = (cx * cx + cy * cy).sqrt();
        let mut ring = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let r = (x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy);
                ring.push(((r / reach * RINGS as f64) as usize).min(RINGS - 1) as u8);
            }
        }
        RingMap { w, h, ring }
    }

    /// The outer radius of ring `i`.
    fn radius(&self, i: usize) -> f64 {
        (self.w as f64).hypot(self.h as f64) / 2.0 * (i + 1) as f64 / RINGS as f64
    }
}

/// Rings: the wallpaper with every ring turned by `turns`, drawn straight into `dst` (bilinear,
/// in bands on several threads: cairo would resample the whole screen once per ring). Points
/// that turn in from beyond the screen's edge take the edge's colour.
unsafe fn raster_rings(dst: *mut CairoSurface, wall: *mut CairoSurface, map: &RingMap, turns: &[f64]) {
    let (w, h) = (map.w, map.h);
    unsafe {
        cb::cairo_surface_flush(wall);
        cb::cairo_surface_flush(dst);
    }
    let src_stride = unsafe { cb::cairo_image_surface_get_stride(wall) as usize } / 4;
    let dst_stride = unsafe { cb::cairo_image_surface_get_stride(dst) as usize } / 4;
    let src = unsafe { std::slice::from_raw_parts(cb::cairo_image_surface_get_data(wall) as *const u32, src_stride * h) };
    let out = unsafe { std::slice::from_raw_parts_mut(cb::cairo_image_surface_get_data(dst) as *mut u32, dst_stride * h) };
    let rot: Vec<(f32, f32)> = turns.iter().map(|a| (a.cos() as f32, a.sin() as f32)).collect();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    // Positions in 1/256 px; past the screen's edge the edge pixels carry on.
    let (max_x, max_y) = (((w - 1) * 256) as i32, ((h - 1) * 256) as i32);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (k, rows) in out.chunks_mut(band * dst_stride).enumerate() {
            let rot = &rot;
            scope.spawn(move || {
                let y0 = k * band;
                for (dy_row, line) in rows.chunks_mut(dst_stride).enumerate() {
                    let y = y0 + dy_row;
                    if y >= h {
                        break;
                    }
                    let dy = y as f32 + 0.5 - cy;
                    let rings = &map.ring[y * w..(y + 1) * w];
                    for (x, (p, &ring)) in line[..w].iter_mut().zip(rings).enumerate() {
                        let (c, s) = rot[ring as usize];
                        let dx = x as f32 + 0.5 - cx;
                        // Turning the picture by `a` shows, at each point, what lay `a` back.
                        let sx = (((cx + c * dx + s * dy - 0.5) * 256.0) as i32).clamp(0, max_x);
                        let sy = (((cy - s * dx + c * dy - 0.5) * 256.0) as i32).clamp(0, max_y);
                        let (x0, y0) = ((sx >> 8) as usize, (sy >> 8) as usize);
                        let (fx, fy) = ((sx & 255) as u32, (sy & 255) as u32);
                        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
                        let (r0, r1) = (y0 * src_stride, y1 * src_stride);
                        let top = mix_argb(src[r0 + x0], src[r0 + x1], fx);
                        let bottom = mix_argb(src[r1 + x0], src[r1 + x1], fx);
                        *p = 0xFF00_0000 | mix_argb(top, bottom, fy);
                    }
                }
            });
        }
    });
    unsafe { cb::cairo_surface_mark_dirty(dst) };
}

/// Two RGB pixels mixed `f`/256 of the way from `a` to `b` (two channels per multiply).
#[inline]
fn mix_argb(a: u32, b: u32, f: u32) -> u32 {
    let g = 256 - f;
    let rb = (((a & 0x00FF_00FF) * g + (b & 0x00FF_00FF) * f) >> 8) & 0x00FF_00FF;
    let gg = (((a & 0x0000_FF00) * g + (b & 0x0000_FF00) * f) >> 8) & 0x0000_FF00;
    rb | gg
}

unsafe fn rings(c: &mut Ctx, layers: &Layers) {
    let (cr, s) = (c.cr, c.s);
    let (w, h) = (c.w, c.h);
    let closing = c.closing_t();
    let tau = std::f64::consts::TAU;
    let (Some(map), Some(turned)) = (layers.rings.as_ref(), layers.turned) else { return };
    let turns = c.fx.ring_turns(s.now);
    // How far the rings are from lining up (radians), the furthest one.
    let off = turns.iter().map(|a| { let r = a.rem_euclid(tau); r.min(tau - r) }).fold(0.0, f64::max);
    unsafe {
        if off < 0.0005 {
            c.paint_surface(layers.wall, 1.0);
        } else {
            let mut last = layers.turned_at.borrow_mut();
            if last.len() != turns.len() || last.iter().zip(&turns).any(|(a, b)| (a - b).abs() > 1e-4) {
                raster_rings(turned, layers.wall, map, &turns);
                *last = turns.clone();
            }
            c.paint_surface(turned, 1.0);
        }
        // Fine seams between the rings, gone once the picture is whole again.
        let seam = if closing.is_some() { (off / 0.35).min(1.0) } else { 1.0 };
        if seam > 0.01 {
            cb::cairo_new_path(cr);
            for i in 0..RINGS - 1 {
                cb::cairo_new_sub_path(cr);
                cb::cairo_arc(cr, w / 2.0, h / 2.0, map.radius(i), 0.0, tau);
            }
            cb::cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 0.45 * seam);
            cb::cairo_set_line_width(cr, c.px(3.0).max(1.5));
            cb::cairo_stroke_preserve(cr);
            cb::cairo_set_source_rgba(cr, 1.0, 1.0, 1.0, 0.12 * seam);
            cb::cairo_set_line_width(cr, c.px(1.0).max(0.75));
            cb::cairo_stroke(cr);
        }
    }
    let ink = alpha(WHITE, text_fade(closing));
    if ink.3 > 0.0 {
        corner_block(c, ink, true);
    }
}

// ── Fireflies ────────────────────────────────────────────────────────────

/// Fireflies: how lit each point is (0 to 255), at 1/`LIGHT_SCALE` of the screen's size.
struct LightMap {
    w: usize,
    h: usize,
    a: Vec<u8>,
}

/// The lights of `glows` (x, y, radius, brightness in screen pixels) on one small map: each a
/// soft disc, brightest in the middle; where they overlap the light adds up toward full.
fn light_map(sw: f64, sh: f64, glows: &[(f64, f64, f64, f64)]) -> LightMap {
    let (w, h) = ((sw / LIGHT_SCALE).ceil() as usize, (sh / LIGHT_SCALE).ceil() as usize);
    // The falloff by squared distance (0 to 1 in 1/1024 steps), so no square root per point.
    let lut: Vec<f32> = (0..=1024).map(|i| falloff((i as f32 / 1024.0).sqrt())).collect();
    let glows: Vec<(f32, f32, f32, f32)> = glows.iter().map(|&(x, y, r, b)| ((x / LIGHT_SCALE) as f32, (y / LIGHT_SCALE) as f32, (r / LIGHT_SCALE).max(1.0) as f32, b.min(1.0) as f32)).collect();
    // How dark each point stays (1 - light), multiplied over the lights; rows in bands on threads.
    let mut dark = vec![1.0f32; w * h];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band = h.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (k, chunk) in dark.chunks_mut(band * w).enumerate() {
            let (glows, lut) = (&glows, &lut);
            scope.spawn(move || {
                let (top, rows) = (k * band, chunk.len() / w);
                for &(x, y, r, b) in glows {
                    let (x0, x1) = (((x - r).floor().max(0.0)) as usize, ((x + r).ceil().max(0.0) as usize).min(w));
                    let (y0, y1) = (((y - r).floor().max(top as f32)) as usize, ((y + r).ceil().max(0.0) as usize).min(top + rows));
                    let inv = 1.0 / (r * r);
                    for py in y0..y1 {
                        let dy = py as f32 + 0.5 - y;
                        let row = &mut chunk[(py - top) * w..(py - top + 1) * w];
                        for (px, d) in (x0..x1).zip(row[x0..x1].iter_mut()) {
                            let dx = px as f32 + 0.5 - x;
                            let t2 = (dx * dx + dy * dy) * inv;
                            if t2 < 1.0 {
                                *d *= 1.0 - b * lut[(t2 * 1024.0) as usize];
                            }
                        }
                    }
                }
            });
        }
    });
    LightMap { w, h, a: dark.into_iter().map(|d| ((1.0 - d) * 255.0 + 0.5) as u8).collect() }
}

/// A firefly's light from its middle (0) to its edge (1): full, 75% at 0.3, 25% at 0.65, none.
fn falloff(t: f32) -> f32 {
    const STOPS: [(f32, f32); 4] = [(0.0, 1.0), (0.3, 0.75), (0.65, 0.25), (1.0, 0.0)];
    for k in 1..STOPS.len() {
        let ((t0, a0), (t1, a1)) = (STOPS[k - 1], STOPS[k]);
        if t <= t1 {
            return a0 + (a1 - a0) * (t - t0) / (t1 - t0);
        }
    }
    0.0
}

/// Fireflies: light `dst` through the light map `map`, enlarged bilinearly as it goes, in bands on several threads (cairo's enlarged mask
/// costs far more than a frame). With `lit` (the wallpaper as the lights show it) each pixel moves
/// toward it as far as the light reaches; without (a video plays underneath) the night opens
/// there instead, a little warm.
unsafe fn raster_lights(dst: *mut CairoSurface, map: &LightMap, lit: Option<*mut CairoSurface>) {
    let size = |s: *mut CairoSurface| unsafe { (cb::cairo_image_surface_get_width(s) as usize, cb::cairo_image_surface_get_height(s) as usize, cb::cairo_image_surface_get_stride(s) as usize / 4) };
    let (w, h, stride) = size(dst);
    let (mw, mh, mstride, lights) = (map.w, map.h, map.w, &map.a);
    unsafe { cb::cairo_surface_flush(dst) };
    let out = unsafe { std::slice::from_raw_parts_mut(cb::cairo_image_surface_get_data(dst) as *mut u32, stride * h) };
    let lit = lit.filter(|s| size(*s).0 == w && size(*s).1 == h).map(|s| unsafe {
        cb::cairo_surface_flush(s);
        (std::slice::from_raw_parts(cb::cairo_image_surface_get_data(s) as *const u32, size(s).2 * h), size(s).2)
    });
    // Where each column and row samples the map: the two neighbours and the weight (of 256).
    let axis = |n: usize, m: usize| -> Vec<(usize, usize, u32)> {
        (0..n)
            .map(|i| {
                let p = ((i as f64 + 0.5) / LIGHT_SCALE - 0.5).clamp(0.0, (m - 1) as f64);
                let i0 = p.floor() as usize;
                (i0, (i0 + 1).min(m - 1), ((p - i0 as f64) * 256.0) as u32)
            })
            .collect()
    };
    let (cols, rows) = (axis(w, mw), axis(h, mh));
    // The warm tint over a video: (1, 0.86, 0.45) at 6% where fully lit, premultiplied.
    let warm = [255.0 * 0.06, 219.0 * 0.06, 115.0 * 0.06];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (k, chunk) in out.chunks_mut(band * stride).enumerate() {
            let (cols, rows, lit) = (&cols, &rows, &lit);
            scope.spawn(move || {
                for (j, line) in chunk.chunks_mut(stride).enumerate() {
                    let y = k * band + j;
                    if y >= h {
                        break;
                    }
                    let (y0, y1, fy) = rows[y];
                    let (r0, r1) = (&lights[y0 * mstride..y0 * mstride + mw], &lights[y1 * mstride..y1 * mstride + mw]);
                    for (x, p) in line[..w].iter_mut().enumerate() {
                        let (x0, x1, fx) = cols[x];
                        let top = r0[x0] as u32 * (256 - fx) + r0[x1] as u32 * fx;
                        let bottom = r1[x0] as u32 * (256 - fx) + r1[x1] as u32 * fx;
                        // 0..=255 (from 0..=255 * 65536).
                        let a = (top * (256 - fy) + bottom * fy) >> 16;
                        if a == 0 {
                            continue;
                        }
                        match lit {
                            Some((src, sstride)) => *p = 0xFF00_0000 | mix_argb(*p, src[y * sstride + x], a + (a >> 7)),
                            None => {
                                let keep = 255 - a;
                                let t = a as f32 / 255.0;
                                let ch = |v: u32, sh: u32, add: f32| ((((v >> sh) & 255) * keep / 255) as f32 + add * t).min(255.0) as u32;
                                let alpha = ((*p >> 24) * keep / 255 + (255.0 * 0.06 * t) as u32).min(255);
                                *p = alpha << 24 | ch(*p, 16, warm[0]) << 16 | ch(*p, 8, warm[1]) << 8 | ch(*p, 0, warm[2]);
                            }
                        }
                    }
                }
            });
        }
    });
    unsafe { cb::cairo_surface_mark_dirty(dst) };
}

unsafe fn fireflies(c: &mut Ctx, layers: &Layers) {
    let (cr, s) = (c.cr, c.s);
    let (w, h) = (c.w, c.h);
    let unit = w.min(h);
    let tau = std::f64::consts::TAU;
    let closing = c.closing_t();
    let flies = c.fx.splashes.len().saturating_sub(MAX_FIREFLIES);
    unsafe {
        if let Some(b) = layers.base {
            c.paint_surface(b, 1.0);
        }
        let mut glows = Vec::new();
        for (i, sp) in c.fx.splashes.iter().enumerate().skip(flies) {
            let (age, g) = (sp.age(s.now), sp.grow.value(s.now).clamp(0.0, 1.2));
            // A slow wander around where it was born, rising in as it appears.
            let f = |k: u64| 0.07 + 0.09 * noise(sp.seed, k);
            let mut x = sp.x + 0.045 * (age * tau * f(1) + tau * noise(sp.seed, 2)).sin();
            let mut y = sp.y + 0.035 * (age * tau * f(3) + tau * noise(sp.seed, 4)).sin() + 0.08 * (1.0 - g.min(1.0));
            if sp.dying {
                // Scattered away from the middle of the screen.
                let (dx, dy) = (x - 0.5, y - 0.5);
                let d = (dx * dx + dy * dy).sqrt().max(0.05);
                x += dx / d * 0.25 * (1.0 - g);
                y += dy / d * 0.25 * (1.0 - g);
            }
            let flicker = 0.78 + 0.22 * (age * tau * (0.5 + 0.6 * noise(sp.seed, 5))).sin();
            let mut light = unit * FIREFLY_LIGHT * (0.8 + 0.2 * noise(sp.seed, 6));
            let mut bright = g.min(1.0) * flicker;
            if let Some(t) = closing {
                let gather = progress((0.7, 1.0), t);
                x += (0.5 - x) * gather;
                y += (0.45 - y) * gather;
                light *= 1.0 + 0.8 * progress((0.8, 1.0), t - 0.2 - (i % 8) as f64 * 0.03);
                bright = bright.max(gather);
            }
            if bright > 0.01 {
                glows.push((x * w, y * h, light, bright));
            }
        }
        // Daylight comes back once the swarm has gathered.
        let day = closing.map_or(0.0, |t| ((t - 0.5) / 0.5).clamp(0.0, 1.0));
        // Each firefly lights the wallpaper around it: every light (a soft disc) goes into one
        // small light map, which then lights the wallpaper (or opens the night over a video) in a
        // single pass.
        if day < 1.0 && !glows.is_empty() {
            let map = light_map(w, h, &glows);
            let video = c.l.video.is_some();
            raster_lights(cb::cairo_get_target(cr), &map, if video { None } else { Some(layers.vivid.unwrap_or(layers.wall)) });
        }
        if day > 0.0 {
            c.reveal_wallpaper(layers, day);
        }
        // And glows itself: a soft halo round a bright core.
        let fly = (0.86, 1.0, 0.55);
        for &(x, y, _, b) in &glows {
            let b = b * (1.0 - day);
            let halo = c.px(30.0);
            let p = cb::cairo_pattern_create_radial(x, y, 0.0, x, y, halo);
            cb::cairo_pattern_add_color_stop_rgba(p, 0.0, fly.0, fly.1, fly.2, 0.75 * b);
            cb::cairo_pattern_add_color_stop_rgba(p, 1.0, fly.0, fly.1, fly.2, 0.0);
            cb::cairo_new_path(cr);
            cb::cairo_arc(cr, x, y, halo, 0.0, tau);
            cb::cairo_set_source(cr, p);
            cb::cairo_fill(cr);
            cb::cairo_pattern_destroy(p);
            cb::fill_circle(cr, x, y, c.px(3.0), (1.0, 1.0, 0.85, b));
        }
    }
    let ink = alpha(WHITE, text_fade(closing));
    if ink.3 > 0.0 {
        center_block(c, h * 0.3, ink, true);
    }
}

// ── Halftone ─────────────────────────────────────────────────────────────

/// Halftone: every dot `merge` (0 to 1) of the way from its own size to one that overlaps its
/// neighbours, rasterised straight into `s` (anti-aliased, in bands on several threads: thousands
/// of cairo arcs per frame would be far too slow).
unsafe fn raster_dots(s: *mut CairoSurface, d: &Dots, merge: f64) {
    let (w, h) = unsafe { (cb::cairo_image_surface_get_width(s) as usize, cb::cairo_image_surface_get_height(s) as usize) };
    let stride = unsafe { cb::cairo_image_surface_get_stride(s) as usize } / 4;
    unsafe { cb::cairo_surface_flush(s) };
    let data = unsafe { std::slice::from_raw_parts_mut(cb::cairo_image_surface_get_data(s) as *mut u32, stride * h) };
    let full = d.cell * 0.75;
    let bg = |c: f64| (c * 255.0).round() as u32;
    let bg = 0xFF00_0000 | bg(DOT_BG.0) << 16 | bg(DOT_BG.1) << 8 | bg(DOT_BG.2);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let band = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (k, rows) in data.chunks_mut(band * stride).enumerate() {
            scope.spawn(move || {
                let (y0, y1) = (k * band, (k * band + rows.len() / stride).min(h));
                rows.fill(bg);
                let reach = full + 1.0;
                let first = (((y0 as f64 - reach) / d.cell).floor().max(0.0)) as usize;
                let last = ((((y1 as f64 + reach) / d.cell).ceil()) as usize).min(d.rows);
                for row in first..last {
                    let cy = (row as f64 + 0.5) * d.cell;
                    for col in 0..d.cols {
                        let (c, r0) = d.dot[row * d.cols + col];
                        let r = r0 + (full - r0) * merge;
                        let cx = (col as f64 + 0.5) * d.cell;
                        let colour = [c.0, c.1, c.2].map(|v| v * 255.0);
                        let solid = 0xFF00_0000 | (colour[0].round() as u32) << 16 | (colour[1].round() as u32) << 8 | colour[2].round() as u32;
                        // Pixels this far in are fully covered; only the rim needs blending.
                        let (inner, outer) = ((r - 0.5).max(0.0).powi(2), (r + 0.5).powi(2));
                        let py0 = ((cy - r - 1.0).floor().max(y0 as f64)) as usize;
                        let py1 = ((cy + r + 1.0).ceil() as usize).min(y1);
                        let px0 = (cx - r - 1.0).floor().max(0.0) as usize;
                        let px1 = ((cx + r + 1.0).ceil() as usize).min(w);
                        // The last column of cells can lie (partly) past the right edge.
                        if px0 >= px1 {
                            continue;
                        }
                        for py in py0..py1 {
                            let dy = py as f64 + 0.5 - cy;
                            let line = &mut rows[(py - y0) * stride + px0..(py - y0) * stride + px1];
                            for (px, p) in (px0..).zip(line.iter_mut()) {
                                let dx = px as f64 + 0.5 - cx;
                                let d2 = dx * dx + dy * dy;
                                if d2 >= outer {
                                    continue;
                                }
                                if d2 <= inner {
                                    *p = solid;
                                    continue;
                                }
                                let cover = (r - d2.sqrt() + 0.5).clamp(0.0, 1.0);
                                let old = [(*p >> 16) & 255, (*p >> 8) & 255, *p & 255].map(|v| v as f64);
                                let mix = |i: usize| (old[i] + (colour[i] - old[i]) * cover).round() as u32;
                                *p = 0xFF00_0000 | mix(0) << 16 | mix(1) << 8 | mix(2);
                            }
                        }
                    }
                }
            });
        }
    });
    unsafe { cb::cairo_surface_mark_dirty(s) };
}

unsafe fn halftone(c: &mut Ctx, layers: &Layers) {
    let (cr, s) = (c.cr, c.s);
    let closing = c.closing_t();
    let (Some(base), Some(d)) = (layers.base, layers.dots.as_ref()) else { return };
    // Typing swells the dots until they merge into a mosaic, then the picture itself comes through.
    let level = c.fx.level(s.now);
    let ease = |x: f64| { let x = x.clamp(0.0, 1.0); x * x * (3.0 - 2.0 * x) };
    let (mut merge, mut clear) = (ease(level / 8.0), 0.8 * ease((level - 3.0) / 9.0));
    // Unlocking finishes the same motion: the dots swell until they merge, and the mosaic
    // settles into the wallpaper.
    if let Some(t) = closing {
        merge += (1.0 - merge) * progress((0.45, 1.0), t);
        clear += (1.0 - clear) * progress((0.55, 1.0), t - 0.15);
    }
    // After a failure the whole screen shudders sideways.
    let a = c.fx.shudder(s.now);
    let dx = if a > 0.0 { c.px(16.0) * (a * std::f64::consts::TAU * 7.0).sin() * (-a * 7.0).exp() } else { 0.0 };
    unsafe {
        let shown = match layers.merged {
            Some(m) if merge >= 0.002 => {
                if (layers.merged_at.get() - merge).abs() > 0.001 || layers.merged_at.get().is_nan() {
                    raster_dots(m, d, merge);
                    layers.merged_at.set(merge);
                }
                m
            }
            _ => base,
        };
        let dots = |dx: f64| {
            cb::cairo_set_source_surface(cr, shown, dx, 0.0);
            cb::cairo_paint(cr);
        };
        if clear >= 0.999 {
            c.paint_surface(layers.wall, 1.0);
        } else {
            if dx != 0.0 {
                cb::cairo_set_source_rgba(cr, DOT_BG.0, DOT_BG.1, DOT_BG.2, 1.0);
                cb::cairo_paint(cr);
            }
            dots(dx);
            if clear > 0.0 {
                cb::cairo_set_source_surface(cr, layers.wall, dx, 0.0);
                cb::cairo_paint_with_alpha(cr, clear);
            }
        }
    }
    let ink = alpha(WHITE, text_fade(closing));
    if ink.3 > 0.0 {
        corner_block(c, ink, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const THEMES: [(LockTheme, &str); 8] = [
        (LockTheme::Stencil, "stencil"),
        (LockTheme::Sentence, "sentence"),
        (LockTheme::Aperture, "aperture"),
        (LockTheme::Tide, "tide"),
        (LockTheme::Ink, "ink"),
        (LockTheme::Rings, "rings"),
        (LockTheme::Fireflies, "fireflies"),
        (LockTheme::Halftone, "halftone"),
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
            video: None,
            video_smooth: false,
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
            let frames: [(&str, Instant, Option<Instant>, usize); 6] = [
                ("idle", settled, None, 0),
                ("typing", settled, None, 7),
                // A moment after the last key, while its mark is still moving.
                ("typing-live", t0 + Duration::from_millis(1000 + 7 * 200 + 350), None, 7),
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

    /// The swollen dots reach past the screen edges at any size (a dot off the right edge once
    /// panicked the lock screen).
    /// Over a video only the design is drawn: it must leave see-through places while locked
    /// and nothing at all once unlocked.
    #[test]
    fn video_designs_cut_holes_and_clear_on_unlock() {
        let dir = std::env::var("PRISM_LOCK_RENDER_DIR").ok();
        let (w, h) = (1280, 720);
        for (theme, name) in [(LockTheme::Fireflies, "fireflies")] {
            assert!(!theme.needs_picture(), "{name}");
            let mut l = look(theme);
            l.video = Some("video.mp4".into());
            let layers = Layers::render(&l, w, h);
            let t0 = Instant::now();
            let close_at = t0 + Duration::from_secs(5);
            for (state, now, closing) in [("typing", close_at, None), ("unlock-end", close_at + Duration::from_millis(theme.closing_ms() as u64), Some(close_at))] {
                let mut f = Fx::new(theme, t0);
                for n in 1..=6 {
                    f.sync(n, false, None, false, t0 + Duration::from_millis(1000 + n as u64 * 200));
                }
                if closing.is_some() {
                    f.sync(6, false, None, true, close_at);
                }
                let sc = scene(now, t0, closing, 6, Mode::Lock);
                let img = unsafe { cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h) };
                let cr = unsafe { cb::cairo_create(img) };
                unsafe { paint(cr, &l, &sc, &f, &layers, w, h) };
                unsafe { cb::cairo_destroy(cr) };
                unsafe { cb::cairo_surface_flush(img) };
                let stride = unsafe { cb::cairo_image_surface_get_stride(img) } as usize / 4;
                let px = unsafe { std::slice::from_raw_parts(cb::cairo_image_surface_get_data(img) as *const u32, stride * h as usize) };
                let (clear, dark) = px.iter().fold((0usize, 0usize), |(c, d), p| (c + (p >> 24 < 128) as usize, d + (p >> 24 > 200) as usize));
                if let Some(d) = &dir {
                    let path = std::ffi::CString::new(format!("{d}/{name}-video-{state}.png")).unwrap();
                    unsafe { cb::cairo_surface_write_to_png(img, path.as_ptr()) };
                }
                unsafe { cb::cairo_surface_destroy(img) };
                let total = (w * h) as usize;
                if closing.is_none() {
                    assert!(clear > total / 100 && dark > total / 4, "{name} {state}: {clear} clear, {dark} dark");
                } else {
                    assert_eq!(clear, total, "{name} {state}");
                }
            }
        }
    }

    #[test]
    fn halftone_merges_at_any_screen_size() {
        let l = look(LockTheme::Halftone);
        for (w, h) in [(3840, 2160), (1920, 1080), (2560, 1440), (1366, 768), (1707, 1067), (3440, 1440), (1001, 613), (64, 37)] {
            let layers = Layers::render(&l, w, h);
            let d = layers.dots.as_ref().unwrap();
            for merge in [0.0, 0.3, 0.7, 1.0] {
                unsafe { raster_dots(layers.merged.unwrap(), d, merge) };
            }
        }
    }

    /// Rings start out of line, keys turn them, a failure sends them back, and the unlock lines
    /// every ring up.
    #[test]
    fn rings_scramble_turn_and_line_up() {
        let tau = std::f64::consts::TAU;
        let off = |a: f64| { let r = a.rem_euclid(tau); r.min(tau - r) };
        let t0 = Instant::now();
        let mut fx = Fx::new(LockTheme::Rings, t0);
        let start = fx.ring_turns(t0);
        assert!(start.iter().all(|a| off(*a) > 0.4));
        fx.sync(3, false, None, false, t0);
        let later = t0 + Duration::from_secs(3);
        let moved = fx.ring_turns(later).iter().zip(&start).filter(|(a, b)| (*a - *b).abs() > 0.5).count();
        assert_eq!(moved, 3, "one ring per key");
        fx.sync(2, false, None, false, later);
        let back = later + Duration::from_secs(3);
        let moved = fx.ring_turns(back).iter().zip(&start).filter(|(a, b)| (*a - *b).abs() > 0.5).count();
        assert_eq!(moved, 2, "backspace turns the last ring back");
        let later = back;
        fx.sync(0, false, Some(later), false, later);
        let home = later + Duration::from_secs(4);
        assert!(fx.ring_turns(home).iter().zip(&start).all(|(a, b)| off(a - b) < 0.01));
        fx.sync(5, false, None, false, home);
        fx.sync(5, false, None, true, home);
        let done = home + Duration::from_millis(LockTheme::Rings.closing_ms() as u64);
        assert!(fx.ring_turns(done).iter().all(|a| off(*a) < 0.01), "{:?}", fx.ring_turns(done));
    }

    #[test]
    fn rings_turn_at_any_screen_size() {
        let l = look(LockTheme::Rings);
        for (w, h) in [(2560, 1440), (1920, 1080), (1366, 768), (1001, 613), (64, 37)] {
            let layers = Layers::render(&l, w, h);
            let turns: Vec<f64> = (0..RINGS).map(|i| i as f64 * 0.7 - 2.0).collect();
            let t = Instant::now();
            unsafe { raster_rings(layers.turned.unwrap(), layers.wall, layers.rings.as_ref().unwrap(), &turns) };
            if std::env::var("PRISM_LOCK_RENDER_DIR").is_ok() {
                println!("rings raster {w}x{h} {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    #[test]
    fn every_idle_line_can_be_chosen() {
        let seen: std::collections::HashSet<&str> = (0..2000).map(idle_line).collect();
        assert_eq!(seen.len(), IDLE_LINES.len());
        assert!(IDLE_LINES.contains(&"You don't have any pending notifications."));
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
