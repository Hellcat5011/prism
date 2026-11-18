// ─────────────────────────────────────────────────────────────────────────
// ui.rs — global look & motion for every Prism surface
//
// One place decides how cards look (colours, opacity, corner radius, border,
// font) and how they move (enter/exit transitions, durations, easing,
// animation on/off, speed). Every overlay reads these instead of hard-coding
// its own values, so changing a setting changes all cards uniformly.
//
// Values come from ~/.config/prism/config.json (Config) and the matugen
// palette (Theme). They mirror the QML shell: cards are inversePrimary at
// `background_opacity`, text is onPrimaryContainer, timings follow
// Theme.animMed / OverlayWindow.qml.
// ─────────────────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use prism_common::{Config, Theme};

use crate::cairo_backend::hex_to_rgba;

pub type Rgba = (f64, f64, f64, f64);

// ── Resolution scale ───────────────────────────────────────────────────────

/// Every size in the UI code is in "design pixels" for a 1440 px tall screen (the QML layout);
/// drawing multiplies them by the interface scale so cards keep their share of the screen.
pub const DESIGN_HEIGHT: f64 = 1440.0;

static SCREEN_HEIGHT: AtomicU64 = AtomicU64::new(0);

/// Record the logical screen height (from the first output / a full-screen configure).
pub fn set_screen_height(h: f64) {
    if h > 0.0 {
        SCREEN_HEIGHT.store(h.to_bits(), Ordering::Relaxed);
    }
}

pub fn screen_height() -> f64 {
    let v = f64::from_bits(SCREEN_HEIGHT.load(Ordering::Relaxed));
    if v > 0.0 { v } else { DESIGN_HEIGHT }
}

/// Interface scale for a configured value (0 = automatic) and screen height.
pub fn scale_for(configured: f32, screen_h: f64) -> f64 {
    let s = if configured > 0.0 { configured as f64 } else { screen_h / DESIGN_HEIGHT };
    s.clamp(0.5, 3.0)
}

/// QML Theme.animMed (ms): base duration for fades and scale transitions.
pub const ANIM_MED_MS: f64 = 98.0;
/// Starting scale of the "scale" enter animation.
pub const ENTER_SCALE: f64 = 0.94;
/// Scale a card shrinks to while it fades out (without a surface that can shrink).
pub const EXIT_SCALE: f64 = 0.96;
/// Closing cards shrink all the way into their centre. The surface itself is scaled, so the
/// compositor's blur contracts with the card instead of staying behind while it fades...
pub const COLLAPSE_SCALE: f64 = 0.0;
/// ...and stay opaque until they are this small: the blur under a card can only be shrunk,
/// not faded, so the content must not fade out ahead of it.
const COLLAPSE_FADE_BELOW: f64 = 0.3;
/// Opening cards are opaque by this fraction of the way in (the blur appears with them).
const ENTER_OPAQUE_AT: f64 = 0.35;

/// Card motion springs: (response in seconds, damping ratio), as in SwiftUI / Android. A
/// damping ratio below 1 settles with a slight, natural overshoot; 1 never overshoots.
const SPRING_ENTER_SCALE: (f64, f64) = (0.22, 0.9);
const SPRING_ENTER_SLIDE: (f64, f64) = (0.22, 0.9);
const SPRING_EXIT: (f64, f64) = (0.16, 1.0);
/// Slides out at about the pace they came in (a faster exit read as a mismatch).
const SPRING_EXIT_SLIDE: (f64, f64) = (0.20, 1.0);

#[derive(Clone, Debug)]
pub struct Style {
    /// Card background (inversePrimary at background_opacity).
    pub card_bg: Rgba,
    pub card_border: Rgba,
    pub radius: f64,
    /// Primary text colour on cards (onPrimaryContainer).
    pub fg: Rgba,
    pub accent: Rgba,
    /// Font family list substituted for the default "Inter, Sans".
    pub font_family: String,
    pub animations: bool,
    /// Multiplier for every animation duration (1.0 = QML timings).
    pub animation_speed: f64,
    pub blur: bool,
    /// Drop shadow under modal cards (shadow.rs).
    pub shadows: bool,
    pub dim_enabled: bool,
    pub dim_opacity: f64,
    /// Interface scale (design pixels to screen pixels), see `scale_for`.
    pub scale: f64,
}

impl Style {
    pub fn new(cfg: &Config, theme: &Theme) -> Self {
        let opacity = (cfg.background_opacity as f64).clamp(0.2, 1.0);
        let font_family = if cfg.global_font.trim().is_empty() || cfg.global_font == "System Default" {
            "Inter, Sans".to_string()
        } else {
            format!("{}, Sans", cfg.global_font.trim())
        };
        Self {
            card_bg: hex_to_rgba(&theme.inverse_primary, opacity),
            card_border: hex_to_rgba(&theme.outline_variant, 0.40),
            radius: (cfg.window_radius as f64).clamp(0.0, 36.0),
            fg: hex_to_rgba(&theme.on_primary_container, 1.0),
            accent: hex_to_rgba(&theme.primary, 1.0),
            font_family,
            animations: cfg.animations_enabled,
            animation_speed: (cfg.animation_speed as f64).clamp(0.25, 4.0),
            blur: cfg.blur_enabled,
            shadows: cfg.shadows_enabled,
            dim_enabled: cfg.dim_enabled,
            dim_opacity: (cfg.dim_opacity as f64).clamp(0.0, 0.9),
            scale: scale_for(cfg.ui_scale, screen_height()),
        }
    }

    /// Factor applied to animation time (the global speed), or None when animations are off.
    pub fn time_scale(&self) -> Option<f64> {
        self.animations.then_some(self.animation_speed)
    }

    /// Scale a nominal duration by the global speed; 0 when animations are off.
    pub fn duration_ms(&self, base_ms: f64) -> f64 {
        if self.animations {
            base_ms / self.animation_speed
        } else {
            0.0
        }
    }

    /// Layer-shell namespace for a blurred card surface; the "-solid" variant is excluded from
    /// compositor blur by a layer rule, which is how the blur toggle takes effect.
    pub fn namespace(&self, base: &str) -> String {
        if self.blur {
            base.to_string()
        } else {
            format!("{}-solid", base)
        }
    }
}

// ── Card surface ───────────────────────────────────────────────────────────

/// Card background shared by the modal cards: the tinted fill, a soft sheen at the top (light
/// falling on glass) and a hairline border that is lighter on the top edge.
pub fn card_surface(cr: *mut crate::cairo_backend::Cairo, x: f64, y: f64, w: f64, h: f64, style: &Style) {
    use crate::cairo_backend as cb;
    let r = style.radius;
    cb::fill_rounded_rect(cr, x, y, w, h, r, style.card_bg);
    let sheen = (1.0, 1.0, 1.0, 0.055);
    cb::fill_rounded_rect_gradient(cr, x, y, w, h, r, sheen, (1.0, 1.0, 1.0, 0.0), 0.0, (160.0 / h.max(1.0)).min(0.45));
    card_edge(cr, x, y, w, h, style);
}

/// The card's hairline border alone (for cards that paint over their background, like the
/// launcher's wallpaper panel).
pub fn card_edge(cr: *mut crate::cairo_backend::Cairo, x: f64, y: f64, w: f64, h: f64, style: &Style) {
    let b = style.card_border;
    crate::cairo_backend::stroke_rounded_rect_gradient(cr, x + 0.5, y + 0.5, w - 1.0, h - 1.0, style.radius, (1.0, 1.0, 1.0, 0.16), (b.0, b.1, b.2, b.3 * 0.6), 1.0);
}

// ── Transitions ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// Fade in while growing from ENTER_SCALE about the card centre.
    Scale,
    /// Slide in from the left screen edge.
    SlideLeft { distance: f64 },
    /// Mirror of SlideLeft, from the right screen edge.
    SlideRight { distance: f64 },
}

/// Where the card is in its enter/exit transition, ready to apply when compositing.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub tx: f64,
    pub ty: f64,
    pub scale: f64,
    pub alpha: f64,
    /// 0 = fully hidden, 1 = fully shown (eased). Used for the scrim fade.
    pub shown: f64,
    pub done: bool,
}

impl Pose {
    pub const REST: Pose = Pose { tx: 0.0, ty: 0.0, scale: 1.0, alpha: 1.0, shown: 1.0, done: true };
}

/// A damped harmonic spring with unit mass, evaluated in closed form so a transition is a pure
/// function of wall-clock time (no per-frame integration, see CLAUDE.md).
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    omega: f64,
    zeta: f64,
}

impl Spring {
    /// `response`: seconds per (undamped) oscillation; `damping`: damping ratio.
    pub fn new((response, damping): (f64, f64)) -> Spring {
        Spring { omega: std::f64::consts::TAU / response.max(1e-3), zeta: damping.max(0.05) }
    }

    /// Displacement from the target and velocity after `t` seconds, starting at displacement
    /// `d0` with velocity `v0`.
    pub fn eval(&self, d0: f64, v0: f64, t: f64) -> (f64, f64) {
        let w = self.omega;
        if self.zeta < 1.0 {
            let a = self.zeta * w;
            let wd = w * (1.0 - self.zeta * self.zeta).sqrt();
            let b = (v0 + a * d0) / wd;
            let (s, c) = (wd * t).sin_cos();
            let e = (-a * t).exp();
            let d = e * (d0 * c + b * s);
            let v = e * (-a * (d0 * c + b * s) + (-d0 * wd * s + b * wd * c));
            (d, v)
        } else {
            let b = v0 + w * d0;
            let e = (-w * t).exp();
            (e * (d0 + b * t), e * (b - w * (d0 + b * t)))
        }
    }
}

/// A value that follows its target on a spring and keeps its velocity when the target changes
/// mid-flight (OSD slide and level bar). Wall-clock based like Transition.
#[derive(Clone, Copy, Debug)]
pub struct SpringValue {
    target: f64,
    from: f64,
    from_v: f64,
    start: Instant,
    spring: Spring,
    time_scale: Option<f64>,
}

impl SpringValue {
    pub fn new(value: f64) -> SpringValue {
        SpringValue { target: value, from: value, from_v: 0.0, start: Instant::now(), spring: Spring::new((1.0, 1.0)), time_scale: None }
    }

    /// Value and velocity (units per second) now.
    pub fn state(&self) -> (f64, f64) {
        let Some(k) = self.time_scale else { return (self.target, 0.0) };
        let t = self.start.elapsed().as_secs_f64() * k;
        let (d, v) = self.spring.eval(self.from - self.target, self.from_v / k, t);
        (self.target + d, v * k)
    }

    pub fn value(&self) -> f64 {
        self.state().0
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    /// Head for `target` with `spring` (response s, damping), from the current value and speed.
    pub fn set(&mut self, target: f64, spring: (f64, f64), style: &Style) {
        self.set_scaled(target, spring, style.time_scale());
    }

    /// `set` with an explicit time scale (None: animations off).
    pub fn set_scaled(&mut self, target: f64, spring: (f64, f64), time_scale: Option<f64>) {
        let (v, dv) = self.state();
        *self = SpringValue { target, from: v, from_v: dv, start: Instant::now(), spring: Spring::new(spring), time_scale };
    }

    /// Jump to `value` without animating.
    pub fn snap(&mut self, value: f64) {
        *self = SpringValue::new(value);
    }

    /// Head for `target` starting at the current value with velocity `v` (units per second).
    pub fn launch(&mut self, target: f64, v: f64, spring: (f64, f64), time_scale: Option<f64>) {
        let x = self.state().0;
        *self = SpringValue { target, from: x, from_v: v, start: Instant::now(), spring: Spring::new(spring), time_scale };
    }

    /// Value for drawing: the target once settled within `eps`, otherwise the current value
    /// (and the frame loop is told to draw another frame).
    pub fn animated(&self, eps: f64) -> f64 {
        if self.settled(eps) {
            self.target
        } else {
            keep_animating();
            self.value()
        }
    }

    /// Within `eps` of the target and nearly still (eps per 1/10 s).
    pub fn settled(&self, eps: f64) -> bool {
        let (v, dv) = self.state();
        (v - self.target).abs() < eps && dv.abs() < eps * 10.0
    }
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub motion: Motion,
    pub entering: bool,
    spring: Spring,
    /// Progress (0 hidden, 1 shown) and its velocity when the transition began. An exit that
    /// interrupts an enter starts from where the card is, moving as fast as it was.
    from: f64,
    from_v: f64,
    /// Animation speed factor; None = animations off (finish at once).
    time_scale: Option<f64>,
    /// Scale at progress 0 (Motion::Scale): where an enter starts and an exit ends.
    min_scale: f64,
    /// Starts when the first frame is actually presented (frame callback), so a slow first
    /// frame (image decoding) does not eat the animation.
    start: Option<Instant>,
}

impl Transition {
    pub fn enter(motion: Motion, style: &Style) -> Self {
        let spring = match motion {
            Motion::Scale => SPRING_ENTER_SCALE,
            Motion::SlideLeft { .. } | Motion::SlideRight { .. } => SPRING_ENTER_SLIDE,
        };
        Self { motion, entering: true, spring: Spring::new(spring), from: 0.0, from_v: 0.0, time_scale: style.time_scale(), min_scale: ENTER_SCALE, start: None }
    }

    /// Exit transition; `current` is the transition still running (an enter that is cut short).
    pub fn exit(motion: Motion, style: &Style, current: Option<&Transition>) -> Self {
        let (from, from_v) = current.map(|t| t.progress()).unwrap_or((1.0, 0.0));
        let spring = match motion {
            Motion::Scale => SPRING_EXIT,
            Motion::SlideLeft { .. } | Motion::SlideRight { .. } => SPRING_EXIT_SLIDE,
        };
        Self { motion, entering: false, spring: Spring::new(spring), from, from_v, time_scale: style.time_scale(), min_scale: EXIT_SCALE, start: None }
    }

    /// Scale the card has at progress 0 (Motion::Scale).
    pub fn with_min_scale(mut self, scale: f64) -> Self {
        self.min_scale = scale;
        self
    }

    pub fn start_clock(&mut self) {
        if self.start.is_none() {
            self.start = Some(Instant::now());
        }
    }

    pub fn started(&self) -> bool {
        self.start.is_some()
    }

    fn target(&self) -> f64 {
        if self.entering { 1.0 } else { 0.0 }
    }

    /// Progress (overshoots 1 slightly on enter), its velocity per second, and whether the
    /// motion has settled.
    fn state(&self) -> (f64, f64, bool) {
        let target = self.target();
        let Some(k) = self.time_scale else { return (target, 0.0, true) };
        let t = self.start.map(|s| s.elapsed().as_secs_f64() * k).unwrap_or(0.0);
        let (d, v) = self.spring.eval(self.from - target, self.from_v / k, t);
        let p = target + d;
        // Settled: close enough to rest that nothing visible changes; an exit is done as soon
        // as the card is practically invisible.
        let done = if self.entering { d.abs() < 0.002 && v.abs() < 0.02 } else { p < 0.01 } || t > 3.0;
        (if done { target } else { p }, v * k, done)
    }

    fn progress(&self) -> (f64, f64) {
        let (p, v, _) = self.state();
        (p, v)
    }

    pub fn sample(&self) -> Pose {
        let (p, _, done) = self.state();
        let shown = p.clamp(0.0, 1.0);
        match (self.motion, self.entering) {
            (Motion::Scale, true) => Pose {
                tx: 0.0,
                ty: 0.0,
                scale: self.min_scale + (1.0 - self.min_scale) * p,
                // Opaque almost at once: a transparent card would show its blur alone.
                alpha: (p / ENTER_OPAQUE_AT).clamp(0.0, 1.0),
                shown,
                done,
            },
            (Motion::Scale, false) => Pose {
                tx: 0.0,
                ty: 0.0,
                scale: self.min_scale + (1.0 - self.min_scale) * shown,
                // A collapse keeps the card opaque until it is small; a plain shrink fades.
                alpha: if self.min_scale <= COLLAPSE_SCALE + 1e-9 { (shown / COLLAPSE_FADE_BELOW).clamp(0.0, 1.0) } else { shown },
                // The dim trails the card so the screen does not flash back.
                shown: shown.sqrt(),
                done,
            },
            (Motion::SlideLeft { distance } | Motion::SlideRight { distance }, entering) => Pose {
                tx: match self.motion {
                    Motion::SlideLeft { .. } => -distance * (1.0 - p),
                    _ => distance * (1.0 - p),
                },
                ty: 0.0,
                scale: 1.0,
                alpha: 1.0,
                shown: if entering { shown } else { shown.sqrt() },
                done,
            },
        }
    }
}

// ── Staggered content entrance ─────────────────────────────────────────────
//
// Rows of a card that is opening fade in and rise a few px one after another. Views wrap each
// row in `staggered`; the card re-renders its content while any row is still moving.

/// Delay between rows and how many rows get their own delay (later rows share the last).
const STAGGER_STEP_MS: f64 = 12.0;
const STAGGER_ROWS: usize = 10;
/// Fade/rise of one row, and how far it rises.
const STAGGER_ROW_MS: f64 = 140.0;
const STAGGER_RISE: f64 = 12.0;

thread_local! {
    /// When the opening card's first frame was shown, and the animation speed.
    static ENTRANCE: std::cell::Cell<Option<(Instant, f64)>> = const { std::cell::Cell::new(None) };
    /// Set by anything drawn this frame that is still moving (stagger rows, scrollers, glides,
    /// fades); read by the frame loop so the card keeps re-rendering its content.
    static ANIM_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Start the row entrance of the card that is opening (call when its first frame is shown).
pub fn begin_entrance(style: &Style) {
    ENTRANCE.with(|e| e.set(style.time_scale().map(|k| (Instant::now(), k))));
}

/// Opacity and downward offset of row `index` of the opening card ((1, 0) once settled).
pub fn stagger(index: usize) -> (f64, f64) {
    let Some((start, k)) = ENTRANCE.with(|e| e.get()) else { return (1.0, 0.0) };
    let ms = start.elapsed().as_secs_f64() * 1000.0 * k - index.min(STAGGER_ROWS) as f64 * STAGGER_STEP_MS;
    let t = (ms / STAGGER_ROW_MS).clamp(0.0, 1.0);
    if t < 1.0 {
        keep_animating();
    }
    let e = 1.0 - (1.0 - t).powi(4);
    (e, STAGGER_RISE * (1.0 - e))
}

/// Something drawn this frame is still moving: the card must render another frame.
pub fn keep_animating() {
    ANIM_PENDING.with(|p| p.set(true));
}

/// True if anything drawn since the last call is still moving (the card must keep rendering).
pub fn take_anim_pending() -> bool {
    ANIM_PENDING.with(|p| p.replace(false))
}

/// Draw row `index` (inside `bounds`) with its entrance fade and rise. `draw` always runs, so
/// views still record their hit regions at the row's resting position.
pub fn staggered(cr: *mut crate::cairo_backend::Cairo, index: usize, bounds: crate::desktop::layout::Rect, draw: impl FnOnce()) {
    use crate::cairo_backend as cb;
    let (alpha, dy) = stagger(index);
    if alpha >= 0.999 {
        draw();
        return;
    }
    unsafe {
        cb::cairo_save(cr);
        // Keep the group small: only the row (plus its rise) is composited.
        cb::cairo_rectangle(cr, bounds.x - 2.0, bounds.y - 2.0, bounds.w + 4.0, bounds.h + dy + 4.0);
        cb::cairo_clip(cr);
        cb::cairo_push_group(cr);
        cb::cairo_translate(cr, 0.0, dy);
    }
    draw();
    unsafe {
        cb::cairo_pop_group_to_source(cr);
        cb::cairo_paint_with_alpha(cr, alpha);
        cb::cairo_restore(cr);
    }
}

// ── In-card motion: scrolling, gliding highlights, fades ───────────────────

thread_local! {
    /// Source of the scroll events being delivered (true: touchpad finger, 1:1 with momentum).
    static SCROLL_FINGER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Record the source of the following scroll deltas (wl_pointer.axis_source).
pub fn set_scroll_finger(finger: bool) {
    SCROLL_FINGER.with(|f| f.set(finger));
}

/// Wheel scrolling eases to the new offset; it keeps its speed when the wheel turns again.
const SPRING_SCROLL: (f64, f64) = (0.24, 1.0);
/// Keyboard-driven scrolling (keeping the selection visible).
const SPRING_SCROLL_KEY: (f64, f64) = (0.28, 1.0);
/// Momentum: a fling coasts with an exponential decay of this time constant (seconds).
const FLING_TAU: f64 = 0.33;
/// A finger gesture has ended when no delta arrived for this long.
const FINGER_IDLE: f64 = 0.05;
/// Past an edge the content follows the finger with this resistance, and a fling that hits the
/// edge overshoots by at most about this many px before it springs back.
const RUBBER: f64 = 0.35;
const EDGE_OVERSHOOT: f64 = 28.0;

/// A scroll offset that animates: wheel steps ease, touchpad scrolling follows the fingers
/// 1:1 with rubber-banded edges and coasts with momentum when they lift.
#[derive(Clone, Debug)]
pub struct Scroller {
    pos: SpringValue,
    max: f64,
    time_scale: Option<f64>,
    /// Finger gesture in progress: recent (time, delta) samples for the release velocity.
    finger: Option<Vec<(Instant, f64)>>,
}

impl Default for Scroller {
    fn default() -> Self {
        Scroller { pos: SpringValue::new(0.0), max: f64::MAX / 4.0, time_scale: Some(1.0), finger: None }
    }
}

impl Scroller {
    /// Offset to draw with (and hit-test against) now.
    pub fn pos(&mut self) -> f64 {
        self.release_if_idle();
        if self.finger.is_some() {
            keep_animating();
            return self.pos.target();
        }
        self.pos.animated(0.3)
    }

    /// Offset now, without advancing gesture state (hit testing between frames).
    pub fn current(&self) -> f64 {
        if self.finger.is_some() { self.pos.target() } else { self.pos.value() }
    }

    /// Where the offset is heading (for keyboard paging and visibility checks).
    pub fn target(&self) -> f64 {
        self.pos.target()
    }

    /// Jump to `v` (new results, reopening).
    pub fn reset(&mut self, v: f64) {
        self.finger = None;
        self.pos.snap(v.clamp(0.0, self.max));
    }

    /// Wheel or touchpad delta (design px). True when the offset changed.
    pub fn by(&mut self, delta: f64) -> bool {
        let finger = SCROLL_FINGER.with(|f| f.get()) && self.time_scale.is_some();
        if finger {
            let cur = self.pos.target();
            let over = cur < 0.0 || cur > self.max;
            let next = cur + if over { delta * RUBBER } else { delta };
            let samples = self.finger.get_or_insert_with(Vec::new);
            let now = Instant::now();
            samples.retain(|(t, _)| now.duration_since(*t).as_secs_f64() < 0.1);
            samples.push((now, delta));
            self.pos.snap(next);
            return true;
        }
        let old = self.pos.target();
        let next = (old + delta).clamp(0.0, self.max);
        if (next - old).abs() < 1e-6 {
            return false;
        }
        self.pos.set_scaled(next, SPRING_SCROLL, self.time_scale);
        true
    }

    /// Ease to `v` (keyboard navigation).
    pub fn to(&mut self, v: f64) {
        let v = v.clamp(0.0, self.max);
        if (v - self.pos.target()).abs() < 1e-6 {
            return;
        }
        self.finger = None;
        self.pos.set_scaled(v, SPRING_SCROLL_KEY, self.time_scale);
    }

    /// Ease just far enough that `top..top + h` (content px) shows in a viewport of `view_h`.
    pub fn ensure_visible(&mut self, top: f64, h: f64, view_h: f64) {
        let s = self.pos.target();
        if top < s {
            self.to(top);
        } else if top + h > s + view_h {
            self.to(top + h - view_h);
        }
    }

    /// Set the scrollable range (call while drawing, before `pos`).
    pub fn set_max(&mut self, max: f64, style: &Style) {
        self.set_max_scaled(max, style.time_scale());
    }

    /// `set_max` with an explicit animation time scale (None: animations off).
    pub fn set_max_scaled(&mut self, max: f64, time_scale: Option<f64>) {
        self.max = max.max(0.0);
        self.time_scale = time_scale;
        if self.finger.is_some() {
            return;
        }
        let t = self.pos.target();
        if t < 0.0 || t > self.max {
            self.pos.set_scaled(t.clamp(0.0, self.max), SPRING_SCROLL, self.time_scale);
        }
    }

    /// Fingers lifted (no deltas for a moment): coast with the release velocity.
    fn release_if_idle(&mut self) {
        let Some(samples) = self.finger.as_ref() else { return };
        let Some(&(last, _)) = samples.last() else {
            self.finger = None;
            return;
        };
        if last.elapsed().as_secs_f64() < FINGER_IDLE {
            return;
        }
        let first = samples[0].0;
        let span = last.duration_since(first).as_secs_f64().max(1.0 / 120.0);
        let sum: f64 = samples.iter().skip(1).map(|(_, d)| d).sum::<f64>();
        // Velocity from the recent samples, zero if the fingers rested before lifting.
        let v = if samples.len() > 1 && last.elapsed().as_secs_f64() < 0.1 { sum / span } else { 0.0 };
        self.finger = None;
        let cur = self.pos.target();
        let free = cur + v * FLING_TAU;
        let target = free.clamp(0.0, self.max);
        if cur < 0.0 || cur > self.max {
            self.pos.set_scaled(target, SPRING_SCROLL, self.time_scale);
            return;
        }
        // Critically damped with omega = 1/tau and initial velocity v decays exponentially to
        // cur + v * tau; a stiffer spring when the edge cuts the coast short bounds the overshoot.
        let w = (1.0 / FLING_TAU).max(v.abs() / (EDGE_OVERSHOOT * std::f64::consts::E));
        let w = if (target - free).abs() > 1e-6 { w } else { 1.0 / FLING_TAU };
        self.pos.launch(target, v, (std::f64::consts::TAU / w, 1.0), self.time_scale);
    }
}

/// A rectangle (y and height) that glides between positions: the selection highlight of lists.
#[derive(Clone, Debug, Default)]
pub struct Glide {
    y: Option<SpringValue>,
    h: Option<SpringValue>,
}

/// Highlight glide: quick with a trace of overshoot.
const SPRING_GLIDE: (f64, f64) = (0.26, 0.86);

impl Glide {
    /// Head for `y`, `h` (content coordinates); returns where to draw now. The first call, and
    /// any call after `reset`, jumps there.
    pub fn to(&mut self, y: f64, h: f64, style: &Style) -> (f64, f64) {
        (Self::axis(&mut self.y, y, style), Self::axis(&mut self.h, h, style))
    }

    fn axis(v: &mut Option<SpringValue>, to: f64, style: &Style) -> f64 {
        let s = v.get_or_insert_with(|| SpringValue::new(to));
        if (s.target() - to).abs() > 1e-6 {
            s.set(to, SPRING_GLIDE, style);
        }
        s.animated(0.2)
    }

    /// Jump on the next `to` (the list was replaced).
    pub fn reset(&mut self) {
        self.y = None;
        self.h = None;
    }
}

/// Per-key 0..1 levels that ease towards on/off: hover fades, switch knobs, pressed states.
#[derive(Clone, Debug)]
pub struct Fades<K> {
    map: std::collections::HashMap<K, SpringValue>,
}

impl<K> Default for Fades<K> {
    fn default() -> Self {
        Fades { map: std::collections::HashMap::new() }
    }
}

/// Hover fades in and out over about a tenth of a second.
pub const SPRING_HOVER: (f64, f64) = (0.16, 1.0);
/// Switch knobs travel with a slight bounce.
pub const SPRING_KNOB: (f64, f64) = (0.30, 0.72);

impl<K: std::hash::Hash + Eq + Clone> Fades<K> {
    /// Level of `key` heading for `on`, starting from 0 the first time (hover, press).
    pub fn level(&mut self, key: K, on: bool, style: &Style) -> f64 {
        self.level_with(key, on, 0.0, SPRING_HOVER, style.time_scale())
    }

    /// `level` with an explicit animation time scale (None: animations off).
    pub fn level_scaled(&mut self, key: K, on: bool, time_scale: Option<f64>) -> f64 {
        self.level_with(key, on, 0.0, SPRING_HOVER, time_scale)
    }

    /// Level of `key` heading for `on`, starting at rest the first time (switch positions, so
    /// opening a card does not animate every switch).
    pub fn settled_level(&mut self, key: K, on: bool, spring: (f64, f64), style: &Style) -> f64 {
        let start = if on { 1.0 } else { 0.0 };
        self.level_with(key, on, start, spring, style.time_scale())
    }

    fn level_with(&mut self, key: K, on: bool, start: f64, spring: (f64, f64), time_scale: Option<f64>) -> f64 {
        let to = if on { 1.0 } else { 0.0 };
        if !on && start == 0.0 && !self.map.contains_key(&key) {
            return 0.0;
        }
        let s = self.map.entry(key.clone()).or_insert_with(|| SpringValue::new(start));
        if (s.target() - to).abs() > 1e-6 {
            s.set_scaled(to, spring, time_scale);
        }
        let v = s.animated(0.004);
        // Faded out hover entries are dropped; positions (`start` 1 or 0 at rest) are kept.
        if !on && start == 0.0 && s.settled(0.004) {
            self.map.remove(&key);
        }
        v
    }
}

/// How much of a row (`y`, `h`) a gliding highlight (`hy`, `hh`) covers, 0..1: rows mix their
/// text towards the highlighted colour by this, so the colour follows the highlight.
pub fn coverage(hy: f64, hh: f64, y: f64, h: f64) -> f64 {
    let overlap = (hy + hh).min(y + h) - hy.max(y);
    (overlap / h.max(1.0)).clamp(0.0, 1.0)
}

/// Widths for a row of buttons that fills `room`: each gets its natural width (label plus
/// padding) and the spare space is shared in proportion, so long labels keep their room. When
/// the row does not fit, every button shrinks by the same factor.
pub fn fit_widths(natural: &[f64], room: f64, gap: f64) -> Vec<f64> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    let sum: f64 = natural.iter().sum();
    let avail = (room - gap * (n as f64 - 1.0)).max(0.0);
    if sum <= 0.0 {
        return vec![avail / n as f64; n];
    }
    let k = avail / sum;
    natural.iter().map(|w| w * k).collect()
}

/// True when buttons of these natural widths fit side by side in `room`.
pub fn widths_fit(natural: &[f64], room: f64, gap: f64) -> bool {
    natural.iter().sum::<f64>() + gap * (natural.len().max(1) as f64 - 1.0) <= room + 0.5
}

/// Mix two colours (`t` 0: a, 1: b).
pub fn mix(a: Rgba, b: Rgba, t: f64) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t, a.3 + (b.3 - a.3) * t)
}

pub fn out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

pub fn in_cubic(t: f64) -> f64 {
    t * t * t
}

/// Qt's Easing.OutBack with the given overshoot.
pub fn out_back(t: f64, s: f64) -> f64 {
    let u = t - 1.0;
    1.0 + (s + 1.0) * u.powi(3) + s * u.powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_endpoints() {
        assert!((out_cubic(1.0) - 1.0).abs() < 1e-9);
        assert!((out_back(1.0, 2.0) - 1.0).abs() < 1e-9);
        assert!(out_back(0.0, 2.0).abs() < 1e-9);
        assert!(in_cubic(0.0).abs() < 1e-9);
    }

    #[test]
    fn scale_follows_screen_unless_configured() {
        assert!((scale_for(0.0, 1440.0) - 1.0).abs() < 1e-9);
        assert!((scale_for(0.0, 1080.0) - 0.75).abs() < 1e-9);
        assert!((scale_for(0.0, 2160.0) - 1.5).abs() < 1e-9);
        assert!((scale_for(1.25, 1080.0) - 1.25).abs() < 1e-9);
    }

    #[test]
    fn disabled_animations_finish_immediately() {
        let mut cfg = Config::default();
        cfg.animations_enabled = false;
        let style = Style::new(&cfg, &Theme::default());
        let tr = Transition::enter(Motion::Scale, &style);
        let p = tr.sample();
        assert!(p.done);
        assert!((p.scale - 1.0).abs() < 1e-9);
    }

    #[test]
    fn springs_settle_and_overshoot_only_when_underdamped() {
        let under = Spring::new((0.4, 0.8));
        let (d, v) = under.eval(-1.0, 0.0, 3.0);
        assert!(d.abs() < 1e-4 && v.abs() < 1e-3);
        // Underdamped: passes the target (d > 0) at some point; critical: never does.
        assert!((0..300).any(|i| under.eval(-1.0, 0.0, i as f64 / 300.0).0 > 0.0));
        let crit = Spring::new((0.4, 1.0));
        assert!((0..300).all(|i| crit.eval(-1.0, 0.0, i as f64 / 300.0).0 <= 1e-12));
        // Velocity matches the derivative of displacement.
        let (d1, v1) = under.eval(-1.0, 0.0, 0.1);
        let (d2, _) = under.eval(-1.0, 0.0, 0.1 + 1e-6);
        assert!(((d2 - d1) / 1e-6 - v1).abs() < 1e-3);
    }

    #[test]
    fn spring_value_keeps_velocity_when_retargeted() {
        let style = Style::new(&Config::default(), &Theme::default());
        let mut v = SpringValue::new(0.0);
        v.set(100.0, (0.3, 1.0), &style);
        v.start -= std::time::Duration::from_millis(50);
        let (x, dx) = v.state();
        assert!(x > 0.0 && dx > 0.0);
        v.set(0.0, (0.3, 1.0), &style);
        let (x2, dx2) = v.state();
        assert!((x2 - x).abs() < 1.0 && (dx2 - dx).abs() / dx < 0.05, "continuous: {x} {dx} -> {x2} {dx2}");
    }

    #[test]
    fn scroller_keeps_selection_visible_and_clamps() {
        let mut s = Scroller::default();
        s.set_max(1000.0, &Style::new(&Config::default(), &Theme::default()));
        s.ensure_visible(0.0, 54.0, 200.0);
        assert_eq!(s.target(), 0.0);
        s.ensure_visible(5.0 * 58.0, 54.0, 200.0);
        assert_eq!(s.target(), 5.0 * 58.0 + 54.0 - 200.0);
        s.ensure_visible(58.0, 54.0, 200.0);
        assert_eq!(s.target(), 58.0);
        s.to(5000.0);
        assert_eq!(s.target(), 1000.0);
        set_scroll_finger(false);
        assert!(!s.by(10.0));
        assert!(s.by(-10.0));
        assert_eq!(s.target(), 990.0);
    }

    #[test]
    fn fades_start_at_rest_for_positions_and_drop_finished_hovers() {
        let style = Style::new(&Config::default(), &Theme::default());
        let mut f: Fades<u32> = Fades::default();
        assert_eq!(f.settled_level(1, true, SPRING_KNOB, &style), 1.0);
        assert_eq!(f.level(2, false, &style), 0.0);
        assert!(f.level(2, true, &style) < 0.5);
        assert!((coverage(0.0, 10.0, 5.0, 10.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn button_row_shares_spare_room_by_label_length() {
        let w = fit_widths(&[50.0, 110.0, 60.0], 250.0, 6.0);
        assert!((w.iter().sum::<f64>() + 12.0 - 250.0).abs() < 1e-9);
        assert!(w[1] >= 110.0 && w[0] >= 50.0 && w[2] >= 60.0);
        assert!(widths_fit(&[50.0, 110.0, 60.0], 250.0, 6.0));
        assert!(!widths_fit(&[100.0, 110.0, 60.0], 250.0, 6.0));
    }

    #[test]
    fn cards_arrive_quickly() {
        let style = Style::new(&Config::default(), &Theme::default());
        let settled = |motion, ms| {
            let mut enter = Transition::enter(motion, &style);
            enter.start = Some(Instant::now() - std::time::Duration::from_millis(ms));
            enter.sample().done
        };
        for motion in [Motion::Scale, Motion::SlideLeft { distance: 500.0 }] {
            assert!(!settled(motion, 60));
            assert!(settled(motion, 300));
        }
    }

    #[test]
    fn exit_continues_from_an_interrupted_enter() {
        let style = Style::new(&Config::default(), &Theme::default());
        let mut enter = Transition::enter(Motion::Scale, &style);
        enter.start = Some(Instant::now() - std::time::Duration::from_millis(60));
        let (p, _) = enter.progress();
        assert!(p > 0.05 && p < 0.95, "mid-enter progress {p}");
        let exit = Transition::exit(Motion::Scale, &style, Some(&enter));
        // Not started yet: the card stays where the enter left it.
        assert!((exit.progress().0 - p).abs() < 1e-3);
    }
}
