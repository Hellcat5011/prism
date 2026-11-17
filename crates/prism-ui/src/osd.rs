//! Volume / brightness OSD (port of QML Osd.qml).
//!
//! A 320x64 card at the bottom centre of the screen. It slides up from below the screen edge on
//! a spring (slight overshoot), stays 2 s after the last update and slides back down on a
//! critically damped spring; the level bar follows key presses on a spring too, keeping its
//! speed when the value changes again. The surface is exactly the card and the slide moves the
//! layer surface's
//! bottom margin, so compositor blur (which covers the whole layer surface on Mango) stays
//! behind the card during the motion too.

use std::time::{Duration, Instant};

use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::ui::{SpringValue, Style};

pub const CARD_W: u32 = 320;
pub const CARD_H: u32 = 64;
/// QML: 224 px window anchored to the bottom, card at y 60 when shown, 260 when hidden.
const SHOWN_MARGIN: f64 = 100.0;
const HIDDEN_MARGIN: f64 = -100.0;
/// Slide springs (response s, damping ratio) and the level bar's.
const SPRING_IN: (f64, f64) = (0.36, 0.8);
const SPRING_OUT: (f64, f64) = (0.28, 1.0);
const SPRING_BAR: (f64, f64) = (0.18, 1.0);
const HIDE_AFTER: Duration = Duration::from_millis(2000);
const RADIUS: f64 = 12.0; // Theme.radiusSmall

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    In,
    Out,
    Done,
}

pub struct OsdView {
    kind: String,
    /// Percent (volume is clamped to 100, brightness is shown as reported).
    value: i32,
    phase: Phase,
    /// Bottom margin of the layer surface (the slide).
    slide: SpringValue,
    hide_at: Instant,
    /// Shown level of the bar (percent).
    bar: SpringValue,
}

impl OsdView {
    pub fn new(kind: &str, value_pct: f32, style: &Style) -> Self {
        let now = Instant::now();
        let value = clamp_value(kind, value_pct);
        let mut slide = SpringValue::new(HIDDEN_MARGIN);
        slide.set(SHOWN_MARGIN, SPRING_IN, style);
        Self {
            kind: kind.to_string(),
            value,
            phase: Phase::In,
            slide,
            hide_at: now + HIDE_AFTER,
            bar: SpringValue::new(value as f64),
        }
    }

    /// A new volume/brightness value: show it, slide back in if leaving, restart the hide timer.
    pub fn update(&mut self, kind: &str, value_pct: f32, style: &Style) {
        let now = Instant::now();
        let value = clamp_value(kind, value_pct);
        if kind != self.kind {
            self.kind = kind.to_string();
            self.bar.snap(value as f64);
        } else if value as f64 != self.bar.target() {
            self.bar.set(value as f64, SPRING_BAR, style);
        }
        self.value = value;
        self.hide_at = now + HIDE_AFTER;
        if self.phase != Phase::In {
            // Reverses from wherever the exit slide is, keeping its speed.
            self.phase = Phase::In;
            self.slide.set(SHOWN_MARGIN, SPRING_IN, style);
        }
    }

    /// Run the hide timer. Returns the time until the next timer event, if any.
    pub fn tick(&mut self, style: &Style) -> Option<Duration> {
        let now = Instant::now();
        match self.phase {
            Phase::In if now >= self.hide_at => {
                self.phase = Phase::Out;
                self.slide.set(HIDDEN_MARGIN, SPRING_OUT, style);
                None
            }
            Phase::In => Some(self.hide_at - now),
            _ => None,
        }
    }

    /// Bottom margin of the layer surface for this frame.
    pub fn margin(&self) -> i32 {
        self.slide.value().round() as i32
    }

    /// Another frame is wanted (slide or bar animation running).
    pub fn animating(&self) -> bool {
        let sliding = self.phase != Phase::Done && !self.slide.settled(0.5);
        sliding || !self.bar.settled(0.05)
    }

    /// The exit slide has finished; the overlay can be torn down.
    pub fn finished(&mut self) -> bool {
        // Done once the card is fully below the screen edge (bottom margin past its height).
        if self.phase == Phase::Out && (self.slide.value() <= -(CARD_H as f64) || self.slide.settled(0.5)) {
            self.phase = Phase::Done;
        }
        self.phase == Phase::Done
    }

    /// Draw the card at (0, 0); the surface is exactly CARD_W x CARD_H.
    pub fn draw(&self, cr: *mut Cairo, style: &Style, theme: &Theme) {
        let (w, h) = (CARD_W as f64, CARD_H as f64);
        let primary = cb::hex_to_rgba(&theme.primary, 1.0);
        cb::fill_rounded_rect(cr, 0.0, 0.0, w, h, RADIUS, style.card_bg);
        cb::stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, RADIUS, cb::hex_to_rgba(&theme.inverse_primary, 1.0), 1.0);

        // RowLayout, margins 16, spacing 16: icon 24 | bar (fill) | label (min 40, right aligned).
        let icon_x = 16.0;
        let icon_y = (h - 24.0) / 2.0;
        draw_icon(cr, icon_x, icon_y, &self.kind, self.value, primary);

        let label = format!("{}%", self.value);
        let font = "Inter, Sans Bold 14px";
        let (tw, th) = cb::measure_pango_text(cr, &label, font, 200, None);
        let label_w = (tw as f64).max(40.0);
        let label_right = w - 16.0;
        cb::draw_pango_text(cr, label_right - tw as f64, (h - th as f64) / 2.0, &label, font, primary, None);

        let bar_x = icon_x + 24.0 + 16.0;
        let bar_w = (label_right - label_w - 16.0 - bar_x).max(1.0);
        let bar_y = (h - 8.0) / 2.0;
        cb::fill_rounded_rect(cr, bar_x, bar_y, bar_w, 8.0, 4.0, cb::hex_to_rgba(&theme.surface, 1.0));
        let frac = (self.bar.value() / 100.0).clamp(0.0, 1.0);
        if frac > 0.0 {
            let fill_w = (bar_w * frac).max(8.0);
            cb::fill_rounded_rect(cr, bar_x, bar_y, fill_w, 8.0, 4.0, primary);
        }
    }
}

fn clamp_value(kind: &str, pct: f32) -> i32 {
    let v = pct.round().max(0.0) as i32;
    if kind.contains("bright") { v } else { v.min(100) }
}

/// The Feather-style icons from Osd.qml (24x24 viewBox, stroke 2, round caps and joins).
fn draw_icon(cr: *mut Cairo, x: f64, y: f64, kind: &str, value: i32, color: crate::ui::Rgba) {
    use std::f64::consts::PI;
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_translate(cr, x, y);
        cb::cairo_new_path(cr);
        cb::cairo_set_line_width(cr, 2.0);
        cb::cairo_set_line_cap(cr, 1); // ROUND
        cb::cairo_set_line_join(cr, 1); // ROUND
        cb::cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        let line = |x0: f64, y0: f64, x1: f64, y1: f64| {
            cb::cairo_move_to(cr, x0, y0);
            cb::cairo_line_to(cr, x1, y1);
        };
        if kind.contains("bright") {
            line(12.0, 2.0, 12.0, 4.0);
            line(12.0, 20.0, 12.0, 22.0);
            line(4.93, 4.93, 6.34, 6.34);
            line(17.66, 17.66, 19.07, 19.07);
            line(2.0, 12.0, 4.0, 12.0);
            line(20.0, 12.0, 22.0, 12.0);
            line(4.93, 19.07, 6.34, 17.66);
            line(17.66, 6.34, 19.07, 4.93);
            cb::cairo_new_sub_path(cr);
            cb::cairo_arc(cr, 12.0, 12.0, 4.0, 0.0, 2.0 * PI);
        } else {
            // Speaker: M11 5 L6 9 H2 v6 h4 l5 4 V5 z
            cb::cairo_move_to(cr, 11.0, 5.0);
            cb::cairo_line_to(cr, 6.0, 9.0);
            cb::cairo_line_to(cr, 2.0, 9.0);
            cb::cairo_line_to(cr, 2.0, 15.0);
            cb::cairo_line_to(cr, 6.0, 15.0);
            cb::cairo_line_to(cr, 11.0, 19.0);
            cb::cairo_close_path(cr);
            if value == 0 {
                line(23.0, 9.0, 17.0, 15.0);
                line(17.0, 9.0, 23.0, 15.0);
            } else {
                if value >= 30 {
                    // a5 5 0 0 1 0 7.07 from (15.54, 8.46): arc about (12, 12)
                    cb::cairo_new_sub_path(cr);
                    cb::cairo_arc(cr, 12.0, 12.0, 5.0, -PI / 4.0, PI / 4.0);
                }
                if value >= 70 {
                    cb::cairo_new_sub_path(cr);
                    cb::cairo_arc(cr, 12.0, 12.0, 10.0, -PI / 4.0, PI / 4.0);
                }
            }
        }
        cb::cairo_stroke(cr);
        cb::cairo_restore(cr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_common::Config;

    fn style(anim: bool) -> Style {
        let mut cfg = Config::default();
        cfg.animations_enabled = anim;
        Style::new(&cfg, &Theme::default())
    }

    #[test]
    fn volume_is_clamped_brightness_is_not() {
        assert_eq!(clamp_value("volume", 130.0), 100);
        assert_eq!(clamp_value("brightness", 130.0), 130);
        assert_eq!(clamp_value("volume", -3.0), 0);
    }

    #[test]
    fn without_animations_it_snaps_in_and_out() {
        let s = style(false);
        let mut v = OsdView::new("volume", 40.0, &s);
        assert_eq!(v.margin(), SHOWN_MARGIN as i32);
        assert!(!v.animating());
        v.hide_at = Instant::now();
        assert!(v.tick(&s).is_none());
        assert!(v.finished());
    }

    #[test]
    fn update_while_leaving_slides_back_in() {
        let s = style(false);
        let mut v = OsdView::new("volume", 40.0, &s);
        v.hide_at = Instant::now();
        v.tick(&s);
        v.update("volume", 50.0, &s);
        assert!(!v.finished());
        assert_eq!(v.margin(), SHOWN_MARGIN as i32);
        assert_eq!(v.value, 50);
    }
}
