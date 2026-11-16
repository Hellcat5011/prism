// ─────────────────────────────────────────────────────────────────────────
// anim.rs — Prism animation physics engine
//
// Implements Quickshell-compatible animation curves:
// - OutBack with configurable overshoot (default 2.0)
// - OutCubic (fast enter, gentle deceleration)
// - InCubic (smooth acceleration for dismissals)
// - InOutCubic / Linear
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimationCurve {
    OutBack { overshoot: f64 },
    OutCubic,
    InCubic,
    InOutCubic,
    Linear,
}

#[derive(Debug, Clone)]
pub struct SpringAnimation {
    pub start_val: f64,
    pub target_val: f64,
    pub current_val: f64,
    pub duration_ms: u64,
    pub elapsed_ms: u64,
    pub curve: AnimationCurve,
    /// Wall-clock start, set on the first `tick()` (i.e. the first presented frame), so time
    /// spent loading before the surface is mapped does not eat into the animation.
    pub started: Option<std::time::Instant>,
}

impl SpringAnimation {
    pub fn new(start_val: f64, target_val: f64, duration_ms: u64, curve: AnimationCurve) -> Self {
        Self {
            start_val,
            target_val,
            current_val: start_val,
            duration_ms: duration_ms.max(1),
            elapsed_ms: 0,
            curve,
            started: None,
        }
    }

    pub fn out_back(start_val: f64, target_val: f64, duration_ms: u64, overshoot: f64) -> Self {
        Self::new(start_val, target_val, duration_ms, AnimationCurve::OutBack { overshoot })
    }

    pub fn out_cubic(start_val: f64, target_val: f64, duration_ms: u64) -> Self {
        Self::new(start_val, target_val, duration_ms, AnimationCurve::OutCubic)
    }

    pub fn in_cubic(start_val: f64, target_val: f64, duration_ms: u64) -> Self {
        Self::new(start_val, target_val, duration_ms, AnimationCurve::InCubic)
    }

    /// Advance to the current wall-clock time. Call once per presented frame (frame callback);
    /// progress depends only on elapsed real time, so the animation has the same duration and
    /// curve at 60 Hz, 144 Hz or any other refresh rate. Returns true while still running.
    pub fn tick(&mut self) -> bool {
        let now = std::time::Instant::now();
        let start = *self.started.get_or_insert(now);
        let elapsed = now.duration_since(start).as_millis() as u64;
        self.elapsed_ms = 0;
        self.step(elapsed)
    }

    /// Advance animation by delta_ms. Returns true if animation is still running, false if finished.
    pub fn step(&mut self, delta_ms: u64) -> bool {
        self.elapsed_ms += delta_ms;
        let t = (self.elapsed_ms as f64 / self.duration_ms as f64).clamp(0.0, 1.0);

        let progress = match self.curve {
            AnimationCurve::OutBack { overshoot } => {
                let s = overshoot;
                let c3 = s + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + s * (t - 1.0).powi(2)
            }
            AnimationCurve::OutCubic => 1.0 - (1.0 - t).powi(3),
            AnimationCurve::InCubic => t.powi(3),
            AnimationCurve::InOutCubic => {
                if t < 0.5 {
                    4.0 * t.powi(3)
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            AnimationCurve::Linear => t,
        };

        self.current_val = self.start_val + (self.target_val - self.start_val) * progress;
        self.elapsed_ms < self.duration_ms
    }

    pub fn value(&self) -> f64 {
        self.current_val
    }

    pub fn is_finished(&self) -> bool {
        self.elapsed_ms >= self.duration_ms
    }
}
