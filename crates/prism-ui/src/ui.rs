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
/// Collapse to origin: a card opened from a button grows from, and every card collapses to,
/// this fraction of its size. The surface itself is scaled, so the compositor's blur
/// contracts with the card instead of staying behind while it fades.
pub const ORIGIN_SCALE: f64 = 0.3;
/// Collapsing cards shrink all the way to their origin...
pub const COLLAPSE_SCALE: f64 = 0.0;
/// ...and stay opaque until they are this small: the blur under a card can only be shrunk,
/// not faded, so the content must not fade out ahead of it.
const COLLAPSE_FADE_BELOW: f64 = 0.3;
/// Opening cards are opaque by this fraction of the way in (the blur appears with them).
const ENTER_OPAQUE_AT: f64 = 0.35;

thread_local! {
    /// Screen point a card is being opened from (a desktop shortcut button) and when.
    static OPEN_ORIGIN: std::cell::Cell<Option<(f64, f64, Instant)>> = const { std::cell::Cell::new(None) };
}

/// Record where the next card opens from (logical screen px).
pub fn set_open_origin(x: f64, y: f64) {
    OPEN_ORIGIN.with(|o| o.set(Some((x, y, Instant::now()))));
}

/// The recorded origin if it is fresh (the open request round-trips through prismd).
pub fn take_open_origin() -> Option<(f64, f64)> {
    OPEN_ORIGIN.with(|o| o.take()).filter(|(_, _, t)| t.elapsed().as_millis() < 1500).map(|(x, y, _)| (x, y))
}

/// Card motion springs: (response in seconds, damping ratio), as in SwiftUI / Android. A
/// damping ratio below 1 settles with a slight, natural overshoot; 1 never overshoots.
const SPRING_ENTER_SCALE: (f64, f64) = (0.38, 0.86);
const SPRING_ENTER_SLIDE: (f64, f64) = (0.34, 0.86);
const SPRING_EXIT: (f64, f64) = (0.26, 1.0);
/// Slides out at about the pace they came in (a faster exit read as a mismatch).
const SPRING_EXIT_SLIDE: (f64, f64) = (0.30, 1.0);

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
        let (v, dv) = self.state();
        *self = SpringValue { target, from: v, from_v: dv, start: Instant::now(), spring: Spring::new(spring), time_scale: style.time_scale() };
    }

    /// Jump to `value` without animating.
    pub fn snap(&mut self, value: f64) {
        *self = SpringValue::new(value);
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
                alpha: if self.min_scale < ORIGIN_SCALE + 1e-9 { (shown / COLLAPSE_FADE_BELOW).clamp(0.0, 1.0) } else { shown },
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
const STAGGER_STEP_MS: f64 = 22.0;
const STAGGER_ROWS: usize = 10;
/// Fade/rise of one row, and how far it rises.
const STAGGER_ROW_MS: f64 = 260.0;
const STAGGER_RISE: f64 = 12.0;

thread_local! {
    /// When the opening card's first frame was shown, and the animation speed.
    static ENTRANCE: std::cell::Cell<Option<(Instant, f64)>> = const { std::cell::Cell::new(None) };
    /// Set by `stagger` when a row has not finished; read by the frame loop.
    static STAGGER_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
        STAGGER_PENDING.with(|p| p.set(true));
    }
    let e = 1.0 - (1.0 - t).powi(4);
    (e, STAGGER_RISE * (1.0 - e))
}

/// True if a row drawn since the last call is still entering (the card must keep rendering).
pub fn take_stagger_pending() -> bool {
    STAGGER_PENDING.with(|p| p.replace(false))
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
