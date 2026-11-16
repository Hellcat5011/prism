//! Minimal SVG path data support for icons drawn with cairo, plus the QML `NcIcon` glyph set.
//!
//! Paths are in a 24x24 viewBox (Material / Feather icons, as in the QML shell) and are
//! scaled to the requested size. Filled glyphs use the non-zero rule; stroked glyphs use a
//! 2-unit stroke with round caps and joins.

use std::f64::consts::PI;

use crate::cairo_backend::{self as cb, Cairo};
pub type Rgba = (f64, f64, f64, f64);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Move(f64, f64),
    Line(f64, f64),
    Cubic(f64, f64, f64, f64, f64, f64),
    /// Elliptical arc from the current point: rx, ry, x-rotation (deg), large, sweep, end.
    Arc(f64, f64, f64, bool, bool, f64, f64),
    Close,
}

/// Parse SVG path data into absolute segments (quadratics are raised to cubics).
fn parse(d: &str) -> Vec<Seg> {
    parse_full(d).0
}

/// Segments, and whether the whole string was understood.
fn parse_full(d: &str) -> (Vec<Seg>, bool) {
    let mut toks = Tokens { s: d.as_bytes(), i: 0 };
    let mut out = Vec::new();
    let (mut cx, mut cy) = (0.0, 0.0); // current point
    let (mut sx, mut sy) = (0.0, 0.0); // subpath start
    let mut last_ctrl: Option<(f64, f64)> = None; // for S
    let mut last_q: Option<(f64, f64)> = None; // for T
    let mut cmd = b'M';
    let mut ok = true;
    loop {
        match toks.command() {
            Some(c) => cmd = c,
            None if toks.at_end() => break,
            None => {} // implicit repeat of the previous command
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let mut ctrl = None;
        let mut q = None;
        match cmd.to_ascii_uppercase() {
            b'Z' => {
                out.push(Seg::Close);
                cx = sx;
                cy = sy;
                if toks.at_end() {
                    break;
                }
                // Z takes no arguments; the next token must be a command.
                if toks.peek_command().is_none() {
                    ok = false;
                    break;
                }
                last_ctrl = None;
                last_q = None;
                continue;
            }
            b'M' => {
                let (Some(x), Some(y)) = (toks.num(), toks.num()) else { ok = false; break };
                cx = ox + x;
                cy = oy + y;
                sx = cx;
                sy = cy;
                out.push(Seg::Move(cx, cy));
                // Further pairs after a moveto are implicit linetos.
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let (Some(x), Some(y)) = (toks.num(), toks.num()) else { ok = false; break };
                cx = ox + x;
                cy = oy + y;
                out.push(Seg::Line(cx, cy));
            }
            b'H' => {
                let Some(x) = toks.num() else { ok = false; break };
                cx = ox + x;
                out.push(Seg::Line(cx, cy));
            }
            b'V' => {
                let Some(y) = toks.num() else { ok = false; break };
                cy = oy + y;
                out.push(Seg::Line(cx, cy));
            }
            b'C' => {
                let v: Option<Vec<f64>> = (0..6).map(|_| toks.num()).collect();
                let Some(v) = v else { ok = false; break };
                let (x1, y1, x2, y2) = (ox + v[0], oy + v[1], ox + v[2], oy + v[3]);
                cx = ox + v[4];
                cy = oy + v[5];
                out.push(Seg::Cubic(x1, y1, x2, y2, cx, cy));
                ctrl = Some((x2, y2));
            }
            b'S' => {
                let v: Option<Vec<f64>> = (0..4).map(|_| toks.num()).collect();
                let Some(v) = v else { ok = false; break };
                let (x1, y1) = match last_ctrl {
                    Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                    None => (cx, cy),
                };
                let (x2, y2) = (ox + v[0], oy + v[1]);
                cx = ox + v[2];
                cy = oy + v[3];
                out.push(Seg::Cubic(x1, y1, x2, y2, cx, cy));
                ctrl = Some((x2, y2));
            }
            b'Q' | b'T' => {
                let (qx, qy) = if cmd.eq_ignore_ascii_case(&b'Q') {
                    let (Some(a), Some(b)) = (toks.num(), toks.num()) else { ok = false; break };
                    (ox + a, oy + b)
                } else {
                    match last_q {
                        Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        None => (cx, cy),
                    }
                };
                let (Some(x), Some(y)) = (toks.num(), toks.num()) else { ok = false; break };
                let (ex, ey) = (ox + x, oy + y);
                out.push(Seg::Cubic(
                    cx + 2.0 / 3.0 * (qx - cx),
                    cy + 2.0 / 3.0 * (qy - cy),
                    ex + 2.0 / 3.0 * (qx - ex),
                    ey + 2.0 / 3.0 * (qy - ey),
                    ex,
                    ey,
                ));
                cx = ex;
                cy = ey;
                q = Some((qx, qy));
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot)) = (toks.num(), toks.num(), toks.num()) else { ok = false; break };
                let (Some(large), Some(sweep)) = (toks.flag(), toks.flag()) else { ok = false; break };
                let (Some(x), Some(y)) = (toks.num(), toks.num()) else { ok = false; break };
                cx = ox + x;
                cy = oy + y;
                out.push(Seg::Arc(rx, ry, rot, large, sweep, cx, cy));
            }
            _ => {
                ok = false;
                break;
            }
        }
        last_ctrl = ctrl;
        last_q = q;
    }
    (out, ok)
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tokens<'_> {
    fn skip_sep(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_sep();
        self.i >= self.s.len()
    }

    fn peek_command(&mut self) -> Option<u8> {
        self.skip_sep();
        self.s.get(self.i).copied().filter(|c| c.is_ascii_alphabetic() && *c != b'e' && *c != b'E')
    }

    fn command(&mut self) -> Option<u8> {
        let c = self.peek_command()?;
        self.i += 1;
        Some(c)
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip_sep();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }

    /// One number; handles "1.65.33" (two numbers) and "-.06".
    fn num(&mut self) -> Option<f64> {
        self.skip_sep();
        let start = self.i;
        let s = self.s;
        let mut j = self.i;
        if j < s.len() && (s[j] == b'-' || s[j] == b'+') {
            j += 1;
        }
        let mut dot = false;
        let mut digits = false;
        while j < s.len() {
            match s[j] {
                b'0'..=b'9' => {
                    digits = true;
                    j += 1;
                }
                b'.' if !dot => {
                    dot = true;
                    j += 1;
                }
                b'e' | b'E' if digits => {
                    j += 1;
                    if j < s.len() && (s[j] == b'-' || s[j] == b'+') {
                        j += 1;
                    }
                    while j < s.len() && s[j].is_ascii_digit() {
                        j += 1;
                    }
                    break;
                }
                _ => break,
            }
        }
        if !digits {
            return None;
        }
        self.i = j;
        std::str::from_utf8(&s[start..j]).ok()?.parse().ok()
    }
}

/// Emit the path to cairo (in the current transform).
fn emit(cr: *mut Cairo, segs: &[Seg]) {
    let (mut cx, mut cy) = (0.0, 0.0);
    let (mut sx, mut sy) = (0.0, 0.0);
    unsafe {
        for s in segs {
            match *s {
                Seg::Move(x, y) => {
                    cb::cairo_move_to(cr, x, y);
                    (cx, cy, sx, sy) = (x, y, x, y);
                }
                Seg::Line(x, y) => {
                    cb::cairo_line_to(cr, x, y);
                    (cx, cy) = (x, y);
                }
                Seg::Cubic(a, b, c, d, x, y) => {
                    cb::cairo_curve_to(cr, a, b, c, d, x, y);
                    (cx, cy) = (x, y);
                }
                Seg::Arc(rx, ry, rot, large, sweep, x, y) => {
                    arc_to(cr, (cx, cy), rx, ry, rot, large, sweep, (x, y));
                    (cx, cy) = (x, y);
                }
                Seg::Close => {
                    cb::cairo_close_path(cr);
                    (cx, cy) = (sx, sy);
                }
            }
        }
    }
}

/// SVG endpoint arc to cairo (SVG 1.1 implementation notes, F.6.5).
#[allow(clippy::too_many_arguments)]
fn arc_to(cr: *mut Cairo, p0: (f64, f64), rx: f64, ry: f64, rot_deg: f64, large: bool, sweep: bool, p1: (f64, f64)) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-9 || ry < 1e-9 || (p0.0 - p1.0).abs() + (p0.1 - p1.1).abs() < 1e-9 {
        unsafe { cb::cairo_line_to(cr, p1.0, p1.1) };
        return;
    }
    let phi = rot_deg.to_radians();
    let (sin, cos) = phi.sin_cos();
    let dx = (p0.0 - p1.0) / 2.0;
    let dy = (p0.1 - p1.1) / 2.0;
    let x1 = cos * dx + sin * dy;
    let y1 = -sin * dx + cos * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1 / ry;
    let cyp = -coef * ry * x1 / rx;
    let ccx = cos * cxp - sin * cyp + (p0.0 + p1.0) / 2.0;
    let ccy = sin * cxp + cos * cyp + (p0.1 + p1.1) / 2.0;
    let angle = |ux: f64, uy: f64| uy.atan2(ux);
    let t1 = angle((x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dt = angle((-x1 - cxp) / rx, (-y1 - cyp) / ry) - t1;
    if sweep && dt < 0.0 {
        dt += 2.0 * PI;
    } else if !sweep && dt > 0.0 {
        dt -= 2.0 * PI;
    }
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_translate(cr, ccx, ccy);
        cb::cairo_rotate(cr, phi);
        cb::cairo_scale(cr, rx, ry);
        if sweep {
            cb::cairo_arc(cr, 0.0, 0.0, 1.0, t1, t1 + dt);
        } else {
            cb::cairo_arc_negative(cr, 0.0, 0.0, 1.0, t1, t1 + dt);
        }
        cb::cairo_restore(cr);
    }
}

/// Fill (or stroke, width in viewBox units) a 24x24 path at (x, y) scaled to `size`.
pub fn draw_path(cr: *mut Cairo, d: &str, x: f64, y: f64, size: f64, color: Rgba, stroke: Option<f64>) {
    let segs = parse(d);
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_translate(cr, x, y);
        cb::cairo_scale(cr, size / 24.0, size / 24.0);
        cb::cairo_new_path(cr);
        emit(cr, &segs);
        cb::cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        match stroke {
            Some(w) => {
                cb::cairo_set_line_width(cr, w);
                cb::cairo_set_line_cap(cr, 1);
                cb::cairo_set_line_join(cr, 1);
                cb::cairo_stroke(cr);
            }
            None => cb::cairo_fill(cr),
        }
        cb::cairo_restore(cr);
    }
}

/// QML NcIcon definitions: (stroked, path).
fn icon_def(name: &str) -> (bool, &'static str) {
    match name {
        "bell" => (false, "M12 22c1.1 0 2-.9 2-2h-4c0 1.1.89 2 2 2zm6-6v-5c0-3.07-1.64-5.64-4.5-6.32V4c0-.83-.67-1.5-1.5-1.5s-1.5.67-1.5 1.5v.68C7.63 5.36 6 7.92 6 11v5l-2 2v1h16v-1l-2-2z"),
        "wifi" => (false, "M1 9l2 2c4.97-4.97 13.03-4.97 18 0l2-2C16.93 2.93 7.08 2.93 1 9zm8 8l3 3 3-3c-1.65-1.66-4.34-1.66-6 0zm-4-4l2 2c2.76-2.76 7.24-2.76 10 0l2-2C15.14 9.14 8.87 9.14 5 13z"),
        "bluetooth" => (false, "M17.71 7.71L12 2h-1v7.59L6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 11 14.41V22h1l5.71-5.71-4.3-4.29 4.3-4.29zM13 5.83l1.88 1.88L13 9.59V5.83zm1.88 10.46L13 18.17v-3.76l1.88 1.88z"),
        "coffee" => (false, "M4,19h16v2H4V19z M20,3H4v10c0,2.21,1.79,4,4,4h6c2.21,0,4-1.79,4-4v-3h2c1.11,0,2-0.89,2-2V5C22,3.89,21.11,3,20,3z M18,8h-2V5h2V8z"),
        "lock" => (false, "M18 8h-1V6c0-2.76-2.24-5-5-5S7 3.24 7 6v2H6c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V10c0-1.1-.9-2-2-2zm-6 9c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2zm3.1-9H8.9V6c0-1.71 1.39-3.1 3.1-3.1 1.71 0 3.1 1.39 3.1 3.1v2z"),
        "refresh" => (false, "M17.65 6.35C16.2 4.9 14.21 4 12 4c-4.42 0-7.99 3.58-7.99 8s3.57 8 7.99 8c3.73 0 6.84-2.55 7.73-6h-2.08c-.82 2.33-3.04 4-5.65 4-3.31 0-6-2.69-6-6s2.69-6 6-6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35z"),
        "copy" => (false, "M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"),
        "headset" => (false, "M12 1c-4.97 0-9 4.03-9 9v7c0 1.66 1.34 3 3 3h3v-8H5v-2c0-3.87 3.13-7 7-7s7 3.13 7 7v2h-4v8h3c1.66 0 3-1.34 3-3v-7c0-4.97-4.03-9-9-9z"),
        "keyboard" => (false, "M20 5H4c-1.1 0-1.99.9-1.99 2L2 17c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm-9 3h2v2h-2V8zm0 3h2v2h-2v-2zM8 8h2v2H8V8zm0 3h2v2H8v-2zm-1 2H5v-2h2v2zm0-3H5V8h2v2zm9 7H8v-2h8v2zm0-4h-2v-2h2v2zm0-3h-2V8h2v2zm3 3h-2v-2h2v2zm0-3h-2V8h2v2z"),
        "phone" => (false, "M17 1.01L7 1c-1.1 0-2 .9-2 2v18c0 1.1.9 2 2 2h10c1.1 0 2-.9 2-2V3c0-1.1-.9-1.99-2-1.99zM17 19H7V5h10v14z"),
        "monitor" => (false, "M21 3H3c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h5v2h8v-2h5c1.1 0 1.99-.9 1.99-2L23 5c0-1.1-.9-2-2-2zm0 14H3V5h18v12z"),
        "speaker" => (false, "M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"),
        "ethernet" => (false, "M7.77 6.76L6.23 5.48.82 12l5.41 6.52 1.54-1.28L3.42 12l4.35-5.24zM7 13h2v-2H7v2zm10-2h-2v2h2v-2zm-6 2h2v-2h-2v2zm6.77-7.52l-1.54 1.28L20.58 12l-4.35 5.24 1.54 1.28L23.18 12l-5.41-6.52z"),
        "close" => (true, "M18 6 6 18M6 6l12 12"),
        "check" => (true, "M20 6 9 17l-5-5"),
        "chevron-down" => (true, "m6 9 6 6 6-6"),
        "chevron-up" => (true, "m18 15-6-6-6 6"),
        "volume" => (true, "M11 5L6 9H2v6h4l5 4V5zM15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14"),
        "sun" => (true, "M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8z"),
        "gear" => (true, "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1zM 12 9 A 3 3 0 0 1 12 15 A 3 3 0 0 1 12 9"),
        _ => (false, "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z"), // "dot"
    }
}

/// Draw a QML NcIcon glyph with its top-left at (x, y). `slashed` adds the diagonal strike.
pub fn icon(cr: *mut Cairo, name: &str, x: f64, y: f64, size: f64, color: Rgba, slashed: bool) {
    let (stroked, d) = icon_def(name);
    draw_path(cr, d, x, y, size, color, stroked.then_some(2.0));
    if slashed {
        draw_path(cr, "M4 4 L20 20", x, y, size, color, Some(2.0));
    }
}

/// `icon`, rotated by `deg` about its centre (chevrons).
pub fn icon_rotated(cr: *mut Cairo, name: &str, x: f64, y: f64, size: f64, color: Rgba, deg: f64) {
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_translate(cr, x + size / 2.0, y + size / 2.0);
        cb::cairo_rotate(cr, deg.to_radians());
        icon(cr, name, -size / 2.0, -size / 2.0, size, color, false);
        cb::cairo_restore(cr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_without_separators() {
        let segs = parse("M1.65.33l-.06.06");
        assert_eq!(segs[0], Seg::Move(1.65, 0.33));
        match segs[1] {
            Seg::Line(x, y) => {
                assert!((x - 1.59).abs() < 1e-9 && (y - 0.39).abs() < 1e-9)
            }
            ref s => panic!("{s:?}"),
        }
    }

    #[test]
    fn implicit_lineto_after_move_and_close() {
        let segs = parse("M2 2 4 4 6 2zm1 1h2v2");
        assert_eq!(segs[1], Seg::Line(4.0, 4.0));
        assert_eq!(segs[3], Seg::Close);
        // relative move after Z is from the subpath start (2, 2)
        assert_eq!(segs[4], Seg::Move(3.0, 3.0));
        assert_eq!(segs[5], Seg::Line(5.0, 3.0));
        assert_eq!(segs[6], Seg::Line(5.0, 5.0));
    }

    #[test]
    fn arc_flags_and_smooth_curves() {
        let segs = parse("M12 8a4 4 0 1 0 0 8s2-1 3 0");
        assert!(matches!(segs[1], Seg::Arc(_, _, _, true, false, x, y) if x == 12.0 && y == 16.0));
        assert!(matches!(segs[2], Seg::Cubic(..)));
    }

    #[test]
    fn every_icon_parses_fully() {
        for name in ["bell", "wifi", "bluetooth", "coffee", "lock", "refresh", "copy", "headset", "keyboard", "phone",
            "monitor", "speaker", "ethernet", "close", "check", "chevron-down", "chevron-up", "volume", "sun", "gear", "dot"]
        {
            let (_, d) = icon_def(name);
            let (segs, ok) = parse_full(d);
            assert!(ok && segs.len() > 1, "{name}");
        }
    }
}
