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
/// QML slide transitions use animMed + 50 with OutBack(overshoot 2.0).
pub const ANIM_SLIDE_MS: f64 = ANIM_MED_MS + 50.0;
/// Starting scale of the "scale" enter animation (OverlayWindow.qml).
pub const ENTER_SCALE: f64 = 0.92;

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
    /// Fade 0 to 1 and scale 0.92 to 1 about the card centre (OverlayWindow "scale").
    Scale,
    /// Slide in from the left screen edge with OutBack overshoot (OverlayWindow "slideLeft").
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

#[derive(Clone, Debug)]
pub struct Transition {
    pub motion: Motion,
    pub entering: bool,
    duration_ms: f64,
    /// Starts when the first frame is actually presented (frame callback), so a slow first
    /// frame (image decoding) does not eat the animation.
    start: Option<Instant>,
}

impl Transition {
    pub fn enter(motion: Motion, style: &Style) -> Self {
        let base = match motion {
            Motion::Scale => ANIM_MED_MS,
            Motion::SlideLeft { .. } | Motion::SlideRight { .. } => ANIM_SLIDE_MS,
        };
        Self { motion, entering: true, duration_ms: style.duration_ms(base), start: None }
    }

    pub fn exit(motion: Motion, style: &Style) -> Self {
        Self { motion, entering: false, duration_ms: style.duration_ms(ANIM_MED_MS), start: None }
    }

    pub fn start_clock(&mut self) {
        if self.start.is_none() {
            self.start = Some(Instant::now());
        }
    }

    pub fn started(&self) -> bool {
        self.start.is_some()
    }

    pub fn sample(&self) -> Pose {
        let t = match self.start {
            _ if self.duration_ms <= 0.0 => 1.0,
            None => 0.0,
            Some(s) => (s.elapsed().as_secs_f64() * 1000.0 / self.duration_ms).clamp(0.0, 1.0),
        };
        let done = t >= 1.0;
        // Exit runs the enter curve backwards with an ease-in, like QML's reversed Behaviors.
        let (shown, slide_p) = if self.entering {
            (out_cubic(t), out_back(t, 2.0))
        } else {
            (1.0 - in_cubic(t), 1.0 - in_cubic(t))
        };
        match self.motion {
            Motion::Scale => Pose {
                tx: 0.0,
                ty: 0.0,
                scale: ENTER_SCALE + (1.0 - ENTER_SCALE) * shown,
                alpha: shown,
                shown,
                done,
            },
            Motion::SlideLeft { .. } | Motion::SlideRight { .. } => Pose {
                tx: match self.motion {
                    Motion::SlideRight { distance } => distance * (1.0 - slide_p),
                    Motion::SlideLeft { distance } => -distance * (1.0 - slide_p),
                    Motion::Scale => 0.0,
                },
                ty: 0.0,
                scale: 1.0,
                alpha: 1.0,
                shown,
                done,
            },
        }
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
}
