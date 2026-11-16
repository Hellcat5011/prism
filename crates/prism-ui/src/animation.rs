#![allow(dead_code)]

use std::time::Instant;

/// Quintic ease-out curve: f(t) = 1 - (1 - t)^5
/// Produces a rapid initial movement that smoothly and imperceptibly glides to rest.
pub fn ease_out_quint(t: f32) -> f32 {
    let clamped = t.clamp(0.0, 1.0);
    1.0 - (1.0 - clamped).powi(5)
}

/// Quartic ease-out curve: f(t) = 1 - (1 - t)^4
pub fn ease_out_quart(t: f32) -> f32 {
    let clamped = t.clamp(0.0, 1.0);
    1.0 - (1.0 - clamped).powi(4)
}

/// Hermite smoothstep curve: f(t) = 3t^2 - 2t^3
pub fn smoothstep(t: f32) -> f32 {
    let clamped = t.clamp(0.0, 1.0);
    clamped * clamped * (3.0 - 2.0 * clamped)
}

/// State of a slide-and-fade animation driven by compositor frame events
#[derive(Debug, Clone)]
pub struct SlideAnimation {
    pub start_time: Instant,
    pub duration_ms: u64,
    pub from_offset: i32,
    pub to_offset: i32,
    pub is_leaving: bool,
}

impl SlideAnimation {
    pub fn new_enter(duration_ms: u64, from_offset: i32) -> Self {
        Self {
            start_time: Instant::now(),
            duration_ms,
            from_offset,
            to_offset: 0,
            is_leaving: false,
        }
    }

    pub fn new_leave(duration_ms: u64, to_offset: i32) -> Self {
        Self {
            start_time: Instant::now(),
            duration_ms,
            from_offset: 0,
            to_offset,
            is_leaving: true,
        }
    }

    /// Evaluates current (offset_x, alpha, is_done)
    pub fn evaluate(&self) -> (i32, f32, bool) {
        let elapsed = self.start_time.elapsed().as_millis() as f32;
        let t = (elapsed / (self.duration_ms as f32)).clamp(0.0, 1.0);
        let eased = ease_out_quint(t);

        let is_done = t >= 1.0;

        if !self.is_leaving {
            // Entering: slide from from_offset to to_offset (0), fade from 0.0 to 1.0
            let offset = self.from_offset + ((self.to_offset - self.from_offset) as f32 * eased).round() as i32;
            let alpha = eased;
            (offset, alpha, is_done)
        } else {
            // Leaving: slide from 0 to to_offset, fade from 1.0 to 0.0
            let offset = (self.to_offset as f32 * eased).round() as i32;
            let alpha = 1.0 - eased;
            (offset, alpha, is_done)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_easing_bounds() {
        assert_eq!(ease_out_quint(0.0), 0.0);
        assert!((ease_out_quint(1.0) - 1.0).abs() < 0.0001);
        assert_eq!(ease_out_quart(0.0), 0.0);
        assert!((ease_out_quart(1.0) - 1.0).abs() < 0.0001);
        assert_eq!(smoothstep(0.0), 0.0);
        assert!((smoothstep(1.0) - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_easing_monotonic() {
        let mut prev = 0.0;
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_quint(t);
            assert!(val >= prev);
            prev = val;
        }
    }
}
