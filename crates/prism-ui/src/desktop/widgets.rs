// ─────────────────────────────────────────────────────────────────────────
// desktop/widgets.rs — drawing for the desktop widgets
//
// Ports of modules/Desktop*Content.qml: same sizes, scale factors, margins,
// fonts and colours (cards are inversePrimary at background_opacity with a
// 10 px radius; text is onPrimaryContainer). Every function draws in local
// widget coordinates (0,0)-(w,h) and returns hit regions for clicks, so the
// live desktop and edit mode share one implementation.
// ─────────────────────────────────────────────────────────────────────────

use std::f64::consts::PI;

use prism_common::config::WidgetPosition;
use prism_common::Theme;

use super::dbus::{MediaState, TrayItem};
use super::layout::{tray_icon_size, tray_spacing, Rect};
use super::text::{draw_in_box, fit_px, Align, TextStyle, ICON_FONT};
use crate::cairo_backend::*;
use crate::ui::Style;

pub type Rgba = (f64, f64, f64, f64);

/// Colours and font shared by all widgets (Theme + global Style).
#[derive(Clone)]
pub struct Look {
    pub card: Rgba,
    pub fg: Rgba,
    pub primary: Rgba,
    pub secondary: Rgba,
    pub background: Rgba,
    pub error: Rgba,
    pub family: String,
}

impl Look {
    pub fn new(style: &Style, theme: &Theme) -> Self {
        Look {
            card: style.card_bg,
            fg: style.fg,
            primary: hex_to_rgba(&theme.primary, 1.0),
            secondary: hex_to_rgba(&theme.secondary, 1.0),
            background: hex_to_rgba(&theme.background, 1.0),
            error: hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        }
    }

    /// Theme.widgetFont(customFont): the widget's own font if set, else the global one.
    pub fn family_for(&self, custom: &str) -> String {
        if custom.trim().is_empty() {
            self.family.clone()
        } else {
            format!("{}, {}", custom.trim(), self.family)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    CalPrev,
    CalNext,
    CalMonthTitle,
    CalYearTitle,
    CalPickMonth(u32),
    CalPickYear(i32),
    MediaPrev,
    MediaPlayPause,
    MediaNext,
    Tray(usize),
    Shortcut(usize),
    PhoneCard,
    PhoneRing,
    PhonePlayPause,
    PhoneNext,
    /// The Notes widget: open the editor.
    NotesOpen,
    /// The Weather widget: fetch again.
    WeatherRefresh,
    /// Timer widget buttons.
    Timer(TimerButton),
}

pub type Hits = Vec<(Hit, Rect)>;

fn set_rgba(cr: *mut Cairo, c: Rgba) {
    unsafe { cairo_set_source_rgba(cr, c.0, c.1, c.2, c.3) }
}

/// DesktopWidgetBackground.qml: 10 px radius card unless the widget is transparent.
pub fn draw_background(cr: *mut Cairo, w: f64, h: f64, look: &Look, transparent: bool) {
    if !transparent {
        fill_rounded_rect(cr, 0.0, 0.0, w, h, 10.0, look.card);
    }
}

// ── Digital clock ─────────────────────────────────────────────────────────

pub fn draw_clock(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, now: &LocalTime) {
    draw_background(cr, w, h, look, p.transparent_bg);
    let default_h = if p.hide_date { 120.0 } else { 180.0 };
    let sf = (w / 360.0).min(h / default_h).max(0.45);
    let m = (16.0 * sf).round();
    let (cx, cy, cw, ch) = (m, m, (w - 2.0 * m).max(1.0), (h - 2.0 * m).max(1.0));
    let spacing = (4.0 * sf).round();
    let family = look.family_for(&p.font_family);

    let date_h = if p.hide_date { 0.0 } else { (ch * 0.18).clamp(16.0, 36.0).round() };
    let time_h = if p.hide_date { ch } else { (ch - date_h - spacing).max(1.0) };

    let hour = if now.hour.is_multiple_of(12) { 12 } else { now.hour % 12 };
    let time = format!("{}:{:02}", hour, now.minute);
    let base_px = (cw * 0.6).min(ch * if p.hide_date { 0.95 } else { 0.8 }).max(24.0);
    let st = TextStyle { family: &family, px: base_px, bold: true, color: look.fg };
    let px = fit_px(cr, &time, &st, cw, time_h, 18.0);
    draw_in_box(cr, &time, &TextStyle { px, ..st }, cx, cy, cw, time_h, Align::Center);

    if !p.hide_date {
        let date = now.format("%A, %B %d");
        let st = TextStyle { family: &family, px: (ch * 0.14).clamp(10.0, 24.0).round(), bold: false, color: look.fg };
        let px = fit_px(cr, &date, &st, cw, date_h, 9.0);
        draw_in_box(cr, &date, &TextStyle { px, ..st }, cx, cy + time_h + spacing, cw, date_h, Align::Center);
    }
}

// ── Analog clock ──────────────────────────────────────────────────────────

/// Hand angles in degrees. `second` is the animated (eased) dot angle.
pub struct ClockHands {
    pub hour: f64,
    pub minute: f64,
    pub second: f64,
}

pub fn draw_analog_clock(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, hands: &ClockHands) {
    let size = w.min(h).max(1.0);
    let r = size / 2.0;
    let (cx, cy) = (w / 2.0, h / 2.0);
    unsafe {
        // Scalloped face: 12 scallops, depth 4.5% of the radius.
        if !p.transparent_bg {
            let (n, depth, steps) = (12.0, 0.045, 288);
            cairo_new_path(cr);
            for i in 0..=steps {
                let a = i as f64 / steps as f64 * PI * 2.0;
                let rr = r * (1.0 - depth + depth * (n * a).cos());
                let (x, y) = (cx + rr * a.sin(), cy - rr * a.cos());
                if i == 0 {
                    cairo_move_to(cr, x, y);
                } else {
                    cairo_line_to(cr, x, y);
                }
            }
            cairo_close_path(cr);
            set_rgba(cr, look.card);
            cairo_fill(cr);
        }

        let hw = r * 0.135;
        for (len, angle) in [(r * 0.46, hands.hour), (r * 0.70, hands.minute)] {
            cairo_save(cr);
            cairo_translate(cr, cx, cy);
            cairo_rotate(cr, angle.to_radians());
            fill_rounded_rect(cr, -hw / 2.0, -len, hw, len, hw / 2.0, look.primary);
            cairo_restore(cr);
        }

        let dot = size * 0.12;
        let orbit = r * 0.78;
        let a = hands.second.to_radians();
        cairo_new_path(cr);
        cairo_arc(cr, cx + orbit * a.sin(), cy - orbit * a.cos(), dot / 2.0, 0.0, PI * 2.0);
        set_rgba(cr, look.primary);
        cairo_fill(cr);
    }
}

// ── Calendar ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CalView {
    Days,
    Months,
    Years,
}

#[derive(Clone, Copy, Debug)]
pub struct CalState {
    pub year: i32,
    /// 0-based month
    pub month: u32,
    pub view: CalView,
}

const MONTHS_FULL: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const MONTHS_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub fn draw_calendar(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, cal: &CalState, today: &LocalTime) -> Hits {
    let mut hits = Hits::new();
    draw_background(cr, w, h, look, p.transparent_bg);
    let sf = (w / 360.0).min(h / 320.0).max(0.65);
    let m = (16.0 * sf).round();
    let (ix, iy, iw, ih) = (m, m, w - 2.0 * m, h - 2.0 * m);
    let family = look.family_for(&p.font_family);

    // Header: [<]  Month  Year  [>]
    let header_h = (30.0 * sf).round().max(22.0);
    let chev_px = (16.0 * sf).round().max(11.0);
    let title_px = (16.0 * sf).round().max(12.0);
    if cal.view == CalView::Days {
        let st = TextStyle { family: ICON_FONT, px: chev_px, bold: false, color: look.fg };
        let (x, y, tw, th) = draw_in_box(cr, "\u{f053}", &st, ix, iy, iw, header_h, Align::Start);
        hits.push((Hit::CalPrev, pad(Rect { x, y, w: tw, h: th }, 6.0)));
        let (x, y, tw, th) = draw_in_box(cr, "\u{f054}", &st, ix, iy, iw, header_h, Align::End);
        hits.push((Hit::CalNext, pad(Rect { x, y, w: tw, h: th }, 6.0)));
    }
    let tst = TextStyle { family: &family, px: title_px, bold: true, color: look.fg };
    let month = MONTHS_FULL[cal.month as usize % 12];
    let year = cal.year.to_string();
    let spacing = (10.0 * sf).round().max(4.0);
    let (mw, _) = super::text::measure(cr, month, &tst);
    let (yw, _) = super::text::measure(cr, &year, &tst);
    let total = mw + spacing + yw;
    let mx = ix + (iw - total) / 2.0;
    let (x, y, tw, th) = draw_in_box(cr, month, &tst, mx, iy, mw + 1.0, header_h, Align::Start);
    hits.push((Hit::CalMonthTitle, pad(Rect { x, y, w: tw, h: th }, 4.0)));
    let (x, y, tw, th) = draw_in_box(cr, &year, &tst, mx + mw + spacing, iy, yw + 1.0, header_h, Align::Start);
    hits.push((Hit::CalYearTitle, pad(Rect { x, y, w: tw, h: th }, 4.0)));

    let gy = iy + header_h + (8.0 * sf).round();
    match cal.view {
        CalView::Days => {
            let gh = (iy + ih - (4.0 * sf).round()) - gy;
            let first_wday = weekday_of(cal.year, cal.month + 1, 1);
            let dim = days_in_month(cal.year, cal.month + 1);
            let cells = if first_wday + dim > 35 { 42 } else { 35 };
            let rows = 1 + cells / 7;
            let csp = (4.0 * sf).round().max(2.0);
            let cell_w = (iw - 6.0 * csp) / 7.0;
            let cell_h = (gh - (rows as f64 - 1.0) * csp) / rows as f64;
            let px = (13.0 * sf).round().max(10.0);
            for (i, d) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
                let x = ix + i as f64 * (cell_w + csp);
                draw_in_box(cr, d, &TextStyle { family: &family, px, bold: true, color: look.fg }, x, gy, cell_w, cell_h, Align::Center);
            }
            for c in 0..cells {
                let day = c as i64 - first_wday as i64 + 1;
                if day < 1 || day > dim as i64 {
                    continue;
                }
                let (col, row) = (c % 7, 1 + c / 7);
                let x = ix + col as f64 * (cell_w + csp);
                let y = gy + row as f64 * (cell_h + csp);
                let is_today = today.year == cal.year && today.month0 == cal.month && today.mday as i64 == day;
                if is_today {
                    let d = (cell_w.min(cell_h) - 4.0).max(16.0);
                    unsafe {
                        cairo_new_path(cr);
                        cairo_arc(cr, x + cell_w / 2.0, y + cell_h / 2.0, d / 2.0, 0.0, PI * 2.0);
                    }
                    set_rgba(cr, look.primary);
                    unsafe { cairo_fill(cr) };
                }
                let color = if is_today { look.background } else { look.fg };
                draw_in_box(cr, &day.to_string(), &TextStyle { family: &family, px, bold: is_today, color }, x, y, cell_w, cell_h, Align::Center);
            }
        }
        CalView::Months | CalView::Years => {
            let gh = (iy + ih) - gy;
            let sp = (8.0 * sf).round().max(4.0);
            let cw = (iw - 2.0 * sp) / 3.0;
            let rh = (gh - 3.0 * sp) / 4.0;
            let px = (15.0 * sf).round().max(11.0);
            #[allow(clippy::needless_range_loop)] // i also gives the grid cell
            for i in 0..12 {
                let (col, row) = (i % 3, i / 3);
                let r = Rect { x: ix + col as f64 * (cw + sp), y: gy + row as f64 * (rh + sp), w: cw, h: rh };
                let (label, selected, hit) = if cal.view == CalView::Months {
                    (MONTHS_SHORT[i].to_string(), i as u32 == cal.month, Hit::CalPickMonth(i as u32))
                } else {
                    let y = cal.year - 4 + i as i32;
                    (y.to_string(), y == cal.year, Hit::CalPickYear(y))
                };
                if selected {
                    fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 3.0, look.primary);
                }
                let color = if selected { look.background } else { look.fg };
                draw_in_box(cr, &label, &TextStyle { family: &family, px, bold: selected, color }, r.x, r.y, r.w, r.h, Align::Center);
                hits.push((hit, r));
            }
        }
    }
    hits
}

// ── Media player ──────────────────────────────────────────────────────────

/// Album-art derived colours (DesktopMprisContent.setPalette), or the theme fallback.
#[derive(Clone, Copy, Debug)]
pub struct MediaPalette {
    pub accent: Rgba,
    pub back: Rgba,
    pub mid: Rgba,
    pub front: Rgba,
}

impl MediaPalette {
    pub fn fallback(look: &Look) -> Self {
        MediaPalette { accent: look.primary, back: look.secondary, mid: look.primary, front: look.fg }
    }

    pub fn from_hues(h: f64, s: f64, h2: f64, s2: f64) -> Self {
        MediaPalette {
            accent: hsla(h, s, 0.62),
            back: hsla(h2, s2 * 0.9, 0.50),
            mid: hsla(h, s, 0.66),
            front: hsla(h, s * 0.45, 0.90),
        }
    }
}

pub struct MediaView<'a> {
    pub media: Option<&'a MediaState>,
    /// Decoded album art (cairo image surface), if any.
    pub art: Option<*mut CairoSurface>,
    pub palette: MediaPalette,
    /// Seconds of wave animation time.
    pub wave_time: f64,
    /// 0..1, eased: wave height (grows while playing).
    pub wave_height: f64,
    pub position: f64,
}

/// Everything except the animated wave strip; returns hits and the strip's rectangle.
pub fn draw_mpris_base(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, v: &MediaView) -> (Hits, Rect) {
    let mut hits = Hits::new();
    draw_background(cr, w, h, look, p.transparent_bg);
    let sf = (w / 400.0).min(h / 170.0).max(0.55);
    let m = (16.0 * sf).round();
    let (cx, cy, cw, ch) = (m, m, w - 2.0 * m, h - 2.0 * m);
    let family = look.family_for(&p.font_family);

    // Album art (or the vinyl fallback) on the left
    let art_w = ch.min((cw * 0.42).round());
    let art_r = (10.0 * sf).round().max(4.0);
    unsafe {
        cairo_save(cr);
        rounded_rect_path(cr, cx, cy, art_w, ch, art_r);
        cairo_clip(cr);
        match v.art {
            Some(img) => {
                let (iw, ih) = image_size(img);
                let s = (art_w / iw).max(ch / ih);
                cairo_translate(cr, cx + (art_w - iw * s) / 2.0, cy + (ch - ih * s) / 2.0);
                cairo_scale(cr, s, s);
                cairo_set_source_surface(cr, img, 0.0, 0.0);
                cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
                cairo_paint(cr);
            }
            None => {
                set_rgba(cr, look.primary);
                cairo_paint(cr);
                let inset = (4.0 * sf).round();
                draw_vinyl(cr, cx + inset, cy + inset, art_w - 2.0 * inset, ch - 2.0 * inset, look.background, look.primary);
            }
        }
        cairo_restore(cr);
    }

    // Right column: title, artist, wave progress, controls
    let rx = cx + art_w + (14.0 * sf).round();
    let rw = (cx + cw - rx).max(1.0);
    let title = v.media.map(|m| m.title.as_str()).filter(|t| !t.is_empty()).unwrap_or("No Media");
    let artist = v.media.map(|m| m.artist.as_str()).unwrap_or("");
    let tpx = (16.0 * sf).round().max(11.0);
    let apx = (14.0 * sf).round().max(9.0);
    let tst = TextStyle { family: &family, px: tpx, bold: true, color: look.fg };
    let (_, th) = super::text::measure(cr, "Ag", &tst);
    draw_in_box(cr, title, &tst, rx, cy, rw, th, Align::Start);
    if !artist.is_empty() {
        let ast = TextStyle { family: &family, px: apx, bold: false, color: look.fg };
        let (_, ah) = super::text::measure(cr, "Ag", &ast);
        draw_in_box(cr, artist, &ast, rx, cy + th + (3.0 * sf).round(), rw, ah, Align::Start);
    }

    let ctrl_h = (36.0 * sf).round().max(28.0);
    let ctrl_y = cy + ch - ctrl_h;
    let gap = (18.0 * sf).round();
    let playing = v.media.map(|m| m.playing).unwrap_or(false);
    let glyphs = [
        ("\u{f04ae}", (24.0 * sf).round().max(16.0), Hit::MediaPrev),
        (if playing { "\u{f03e4}" } else { "\u{f040a}" }, (32.0 * sf).round().max(20.0), Hit::MediaPlayPause),
        ("\u{f04ad}", (24.0 * sf).round().max(16.0), Hit::MediaNext),
    ];
    let widths: Vec<f64> = glyphs
        .iter()
        .map(|(g, px, _)| super::text::measure(cr, g, &TextStyle { family: ICON_FONT, px: *px, bold: false, color: look.fg }).0)
        .collect();
    let total: f64 = widths.iter().sum::<f64>() + 2.0 * gap;
    let mut x = rx + (rw - total) / 2.0;
    for ((g, px, hit), gw) in glyphs.iter().zip(widths) {
        let st = TextStyle { family: ICON_FONT, px: *px, bold: false, color: look.fg };
        let (bx, by, bw, bh) = draw_in_box(cr, g, &st, x, ctrl_y, gw + 1.0, ctrl_h, Align::Start);
        hits.push((*hit, pad(Rect { x: bx, y: by, w: bw, h: bh }, 6.0)));
        x += gw + gap;
    }

    let canvas_h = (46.0 * sf).round().max(34.0);
    let canvas_y = ctrl_y - (4.0 * sf).round().max(2.0) - canvas_h;
    (hits, Rect { x: rx, y: canvas_y, w: rw, h: canvas_h })
}

/// Full media widget in one pass (used by edit mode previews).
pub fn draw_mpris(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, v: &MediaView) -> Hits {
    let (hits, c) = draw_mpris_base(cr, w, h, p, look, v);
    let sf = (w / 400.0).min(h / 170.0).max(0.55);
    draw_wave_progress(cr, c.x, c.y, c.w, c.h, sf, look, v);
    hits
}

/// The progress canvas: layered sine waves over the played part, then bar + thumb.
pub fn draw_wave_progress(cr: *mut Cairo, x0: f64, y0: f64, w: f64, h: f64, sf: f64, look: &Look, v: &MediaView) {
    let length = v.media.map(|m| m.length_s).unwrap_or(0.0);
    let progress = if length > 0.0 { (v.position / length).clamp(0.0, 1.0) } else { 0.0 };
    let bar = (4.0 * sf).round().max(3.0);
    let thumb_r = (7.0 * sf).round().max(5.0);
    let ring_w = (2.0 * sf).max(1.5);
    let pad_x = thumb_r;
    let bar_y = y0 + h - thumb_r - 1.0;
    let track_w = (w - 2.0 * pad_x).max(1.0);
    let progress_x = x0 + pad_x + track_w * progress;
    let played_w = progress_x - (x0 + pad_x);
    let pal = &v.palette;

    unsafe {
        if v.wave_height > 0.001 && played_w > 2.0 {
            let t = v.wave_time;
            let max_amp = (bar_y - y0 - 2.0) * 0.5 * v.wave_height;
            let mix = |a: Rgba, b: Rgba, k: f64| (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k, 1.0);
            // color, alpha, amp, l1, v1, o1, l2, v2, o2
            let layers = [
                (pal.back, 0.50, 1.00, 170.0, 55.0, 0.0, 260.0, 30.0, 1.3),
                (mix(pal.back, pal.mid, 0.5), 0.55, 0.85, 130.0, 70.0, 2.1, 210.0, 40.0, 0.4),
                (pal.mid, 0.62, 0.72, 105.0, 85.0, 4.0, 180.0, 25.0, 2.6),
                (mix(pal.mid, pal.front, 0.5), 0.72, 0.58, 145.0, 65.0, 5.2, 230.0, 45.0, 0.9),
                (pal.front, 0.88, 0.45, 120.0, 95.0, 3.3, 200.0, 35.0, 4.4),
            ];
            let start = x0 + pad_x;
            for (color, alpha, amp, l1, v1, o1, l2, v2, o2) in layers {
                cairo_new_path(cr);
                cairo_move_to(cr, start, bar_y);
                let mut x = start;
                while x < progress_x {
                    let px = (x - start) / sf;
                    let w1 = (PI * 2.0 * (px - v1 * t) / l1 + o1).sin();
                    let w2 = (PI * 2.0 * (px - v2 * t) / l2 + o2).sin();
                    let a = 0.5 + 0.5 * (0.7 * w1 + 0.3 * w2);
                    let env = (PI * (x - start) / played_w).sin().max(0.0).powf(1.4);
                    cairo_line_to(cr, x, bar_y - max_amp * amp * a * env);
                    x += 2.0;
                }
                cairo_line_to(cr, progress_x, bar_y);
                cairo_close_path(cr);
                let g = cairo_pattern_create_linear(0.0, bar_y - max_amp, 0.0, bar_y);
                cairo_pattern_add_color_stop_rgba(g, 0.0, color.0, color.1, color.2, alpha * 0.45);
                cairo_pattern_add_color_stop_rgba(g, 1.0, color.0, color.1, color.2, alpha);
                cairo_set_source(cr, g);
                cairo_fill(cr);
                cairo_pattern_destroy(g);
            }
        }

        cairo_set_line_cap(cr, 1); // round
        cairo_set_line_width(cr, bar);
        let un_start = progress_x + thumb_r + (3.0 * sf).max(2.0);
        let un_end = x0 + w - pad_x;
        if un_start < un_end {
            cairo_new_path(cr);
            cairo_move_to(cr, un_start, bar_y);
            cairo_line_to(cr, un_end, bar_y);
            cairo_set_source_rgba(cr, look.fg.0, look.fg.1, look.fg.2, 0.2);
            cairo_stroke(cr);
        }
        cairo_new_path(cr);
        cairo_move_to(cr, x0 + pad_x, bar_y);
        cairo_line_to(cr, progress_x, bar_y);
        set_rgba(cr, pal.accent);
        cairo_stroke(cr);

        cairo_new_path(cr);
        cairo_arc(cr, progress_x, bar_y, thumb_r - ring_w / 2.0, 0.0, PI * 2.0);
        set_rgba(cr, pal.accent);
        cairo_fill_preserve(cr);
        cairo_set_line_width(cr, ring_w);
        set_rgba(cr, look.fg);
        cairo_stroke(cr);
        cairo_set_line_cap(cr, 0);
    }
}

/// The QML fallback art (a vinyl glyph) drawn natively instead of a data: SVG.
fn draw_vinyl(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, fg: Rgba, hole: Rgba) {
    let s = w.min(h) / 24.0;
    let (ox, oy) = (x + (w - 24.0 * s) / 2.0, y + (h - 24.0 * s) / 2.0);
    unsafe {
        cairo_save(cr);
        cairo_translate(cr, ox, oy);
        cairo_scale(cr, s, s);
        cairo_set_line_width(cr, 2.0);
        set_rgba(cr, fg);
        let a1 = (6.55f64 - 12.0).atan2(19.78 - 12.0);
        let a2 = (4.22f64 - 12.0).atan2(17.45 - 12.0) + 2.0 * PI;
        cairo_new_path(cr);
        cairo_move_to(cr, 16.50, 8.85);
        cairo_line_to(cr, 19.78, 6.55);
        cairo_arc(cr, 12.0, 12.0, 9.5, a1, a2);
        cairo_stroke(cr);
        cairo_new_path(cr);
        cairo_arc(cr, 12.0, 12.0, 4.5, 0.0, 2.0 * PI);
        cairo_fill(cr);
        set_rgba(cr, hole);
        cairo_new_path(cr);
        cairo_arc(cr, 12.0, 12.0, 1.5, 0.0, 2.0 * PI);
        cairo_fill(cr);
        cairo_restore(cr);
    }
}

// ── System tray ───────────────────────────────────────────────────────────

pub struct TrayIcon {
    pub surface: Option<*mut CairoSurface>,
}

pub fn draw_tray(
    cr: *mut Cairo,
    w: f64,
    h: f64,
    p: &WidgetPosition,
    look: &Look,
    items: &[TrayItem],
    icons: &[TrayIcon],
    vertical: bool,
    hovered: Option<usize>,
    edit_mode: bool,
) -> Hits {
    let mut hits = Hits::new();
    draw_background(cr, w, h, look, p.transparent_bg);
    let cross = if vertical { w } else { h };
    let sf = (cross / 50.0).max(0.6);
    if items.is_empty() {
        if edit_mode {
            let st = TextStyle { family: ICON_FONT, px: (18.0 * sf).round().max(14.0), bold: false, color: look.fg };
            draw_in_box(cr, "\u{f003b}", &st, 0.0, 0.0, w, h, Align::Center);
        }
        return hits;
    }
    let size = tray_icon_size(cross);
    let sp = tray_spacing(cross);
    let n = items.len() as f64;
    let len = n * size + (n - 1.0) * sp;
    let (mut x, mut y) = if vertical { ((w - size) / 2.0, (h - len) / 2.0) } else { ((w - len) / 2.0, (h - size) / 2.0) };
    for (i, item) in items.iter().enumerate() {
        let r = Rect { x, y, w: size, h: size };
        let alpha = if hovered == Some(i) { 0.7 } else { 1.0 };
        let img = icons.get(i).and_then(|ic| ic.surface).or_else(|| {
            item.icon_path.as_ref().and_then(|p| cached_image(&p.to_string_lossy(), size as i32, size as i32, true))
        });
        if let Some(img) = img {
            let (iw, ih) = image_size(img);
            let s = (size / iw).min(size / ih);
            unsafe {
                cairo_save(cr);
                cairo_translate(cr, x + (size - iw * s) / 2.0, y + (size - ih * s) / 2.0);
                cairo_scale(cr, s, s);
                cairo_set_source_surface(cr, img, 0.0, 0.0);
                cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
                cairo_paint_with_alpha(cr, alpha);
                cairo_restore(cr);
            }
        } else {
            // No icon at all: first letter of the title in a circle.
            let letter: String = item.title.chars().next().unwrap_or('?').to_uppercase().collect();
            fill_rounded_rect(cr, x, y, size, size, size / 2.0, (look.primary.0, look.primary.1, look.primary.2, 0.35 * alpha));
            draw_in_box(cr, &letter, &TextStyle { family: &look.family, px: size * 0.55, bold: true, color: look.fg }, x, y, size, size, Align::Center);
        }
        hits.push((Hit::Tray(i), r));
        if vertical {
            y += size + sp;
        } else {
            x += size + sp;
        }
    }
    hits
}

// ── Shortcuts ─────────────────────────────────────────────────────────────

pub const SHORTCUTS: [(&str, &str, &str); 5] = [
    ("launcher", "App launcher", "\u{f003b}"),
    ("phone", "Phone", "\u{f011c}"),
    ("notifications", "Notifications", "\u{f009a}"),
    ("settings", "Settings", "\u{f0493}"),
    ("power", "Power", "\u{f0425}"),
];

/// Index of the Phone shortcut; it is only shown while phone sync is on.
pub const SHORTCUT_PHONE: usize = 1;

/// Phone sync is on (config `phone_enabled`), set by the desktop when it loads the config.
pub static PHONE_SYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn shortcut_enabled(p: &WidgetPosition, idx: usize) -> bool {
    match idx {
        0 => p.shortcuts.launcher,
        1 => p.shortcuts.phone,
        2 => p.shortcuts.notifications,
        3 => p.shortcuts.settings,
        _ => p.shortcuts.power,
    }
}

pub fn draw_shortcuts(
    cr: *mut Cairo,
    w: f64,
    h: f64,
    p: &WidgetPosition,
    look: &Look,
    vertical: bool,
    hovered: Option<usize>,
    pressed: bool,
    edit_mode: bool,
) -> Hits {
    let mut hits = Hits::new();
    draw_background(cr, w, h, look, p.transparent_bg);
    let phone_sync = PHONE_SYNC.load(std::sync::atomic::Ordering::Relaxed);
    let enabled: Vec<usize> = (0..SHORTCUTS.len()).filter(|i| shortcut_enabled(p, *i) && (*i != SHORTCUT_PHONE || phone_sync)).collect();
    if enabled.is_empty() {
        if edit_mode {
            draw_in_box(cr, "Shortcuts: no icons enabled", &TextStyle { family: &look.family, px: 12.0, bold: true, color: look.fg }, 0.0, 0.0, w, h, Align::Center);
        }
        return hits;
    }
    let (m, sp) = (6.0, 4.0);
    let n = enabled.len() as f64;
    let (cell_w, cell_h) = if vertical {
        (w - 2.0 * m, (h - 2.0 * m - (n - 1.0) * sp) / n)
    } else {
        ((w - 2.0 * m - (n - 1.0) * sp) / n, h - 2.0 * m)
    };
    for (k, &idx) in enabled.iter().enumerate() {
        let (x, y) = if vertical { (m, m + k as f64 * (cell_h + sp)) } else { (m + k as f64 * (cell_w + sp), m) };
        let size = cell_w.min(cell_h);
        let (ccx, ccy) = (x + cell_w / 2.0, y + cell_h / 2.0);
        if hovered == Some(idx) {
            let a = if pressed { 0.35 } else { 0.22 };
            unsafe {
                cairo_new_path(cr);
                cairo_arc(cr, ccx, ccy, size / 2.0, 0.0, PI * 2.0);
                cairo_set_source_rgba(cr, look.primary.0, look.primary.1, look.primary.2, a);
                cairo_fill(cr);
            }
        }
        let st = TextStyle { family: ICON_FONT, px: (size * 0.5).round().max(12.0), bold: false, color: look.fg };
        draw_in_box(cr, SHORTCUTS[idx].2, &st, x, y, cell_w, cell_h, Align::Center);
        hits.push((Hit::Shortcut(idx), Rect { x, y, w: cell_w, h: cell_h }));
    }
    hits
}

// ── helpers ───────────────────────────────────────────────────────────────

fn pad(r: Rect, p: f64) -> Rect {
    Rect { x: r.x - p, y: r.y - p, w: r.w + 2.0 * p, h: r.h + 2.0 * p }
}

pub fn hsla(h: f64, s: f64, l: f64) -> Rgba {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    if s == 0.0 {
        (l, l, l, 1.0)
    } else {
        (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0), 1.0)
    }
}

pub fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

/// Dominant-hue palette from 32x32 RGBA samples (port of the QML paletteSampler).
pub fn palette_from_pixels(px: &[(u8, u8, u8, u8)]) -> Option<MediaPalette> {
    #[derive(Default, Clone, Copy)]
    struct B {
        w: f64,
        r: f64,
        g: f64,
        b: f64,
        n: f64,
    }
    let mut buckets = [B::default(); 13];
    for &(r, g, b, a) in px {
        if a < 128 {
            continue;
        }
        let (h, s, l) = rgb_to_hsl(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        let (idx, wt) = if s < 0.18 || l < 0.10 || l > 0.93 {
            (12, 0.02)
        } else {
            (((h * 12.0).floor() as usize).min(11), s * (1.0 - (2.0 * l - 1.0).abs()) + 0.02)
        };
        let bk = &mut buckets[idx];
        bk.w += wt;
        bk.r += r as f64;
        bk.g += g as f64;
        bk.b += b as f64;
        bk.n += 1.0;
    }
    let mut order: Vec<B> = buckets[..12].iter().copied().filter(|b| b.n > 0.0).collect();
    order.sort_by(|a, b| b.w.partial_cmp(&a.w).unwrap_or(std::cmp::Ordering::Equal));
    if order.is_empty() || order[0].w <= buckets[12].w {
        return None;
    }
    let hsl = |b: &B| rgb_to_hsl(b.r / b.n / 255.0, b.g / b.n / 255.0, b.b / b.n / 255.0);
    let c1 = hsl(&order[0]);
    let h = c1.0;
    let s = c1.1.clamp(0.45, 0.95);
    let (mut h2, mut s2) = ((h + 0.06) % 1.0, s);
    if order.len() > 1 && order[1].w >= order[0].w * 0.35 {
        let c2 = hsl(&order[1]);
        h2 = c2.0;
        s2 = c2.1.clamp(0.40, 0.95);
    }
    Some(MediaPalette::from_hues(h, s, h2, s2))
}

// ── Local time ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalTime {
    pub year: i32,
    pub month0: u32,
    pub mday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
    tm: TmCopy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TmCopy {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
}

impl LocalTime {
    pub fn now() -> Self {
        let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
        unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) };
        let t: libc::time_t = tv.tv_sec;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&t, &mut tm) };
        LocalTime {
            year: tm.tm_year + 1900,
            month0: tm.tm_mon as u32,
            mday: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            millis: (tv.tv_usec / 1000) as u32,
            tm: TmCopy {
                sec: tm.tm_sec,
                min: tm.tm_min,
                hour: tm.tm_hour,
                mday: tm.tm_mday,
                mon: tm.tm_mon,
                year: tm.tm_year,
                wday: tm.tm_wday,
                yday: tm.tm_yday,
            },
        }
    }

    /// strftime in the user's locale.
    pub fn format(&self, fmt: &str) -> String {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_sec = self.tm.sec;
        tm.tm_min = self.tm.min;
        tm.tm_hour = self.tm.hour;
        tm.tm_mday = self.tm.mday;
        tm.tm_mon = self.tm.mon;
        tm.tm_year = self.tm.year;
        tm.tm_wday = self.tm.wday;
        tm.tm_yday = self.tm.yday;
        let mut buf = [0u8; 128];
        let cfmt = std::ffi::CString::new(fmt).unwrap_or_default();
        let n = unsafe { libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), cfmt.as_ptr(), &tm) };
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

pub fn days_in_month(year: i32, month1: u32) -> u32 {
    match month1 {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// 0 = Sunday (Zeller-style computation, proleptic Gregorian).
pub fn weekday_of(year: i32, month1: u32, day: u32) -> u32 {
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month1 < 3 { year - 1 } else { year };
    ((y + y / 4 - y / 100 + y / 400 + t[(month1 - 1) as usize] + day as i32).rem_euclid(7)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_and_days() {
        assert_eq!(weekday_of(2026, 11, 16), 1); // Monday
        assert_eq!(weekday_of(2000, 1, 1), 6); // Saturday
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
    }
}

// ── Phone ─────────────────────────────────────────────────────────────────

/// Phone widget: battery ring, name and status, what plays on the phone, and buttons for
/// find-my-phone, play/pause and next, and one that opens the Phone card.
pub fn draw_phone(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, state: Option<&prism_common::phone::PhoneState>, hovered: Option<Hit>) -> Hits {
    use crate::phone::{draw_battery_ring, media_position, status_line, ICON_NEXT, ICON_OPEN, ICON_PAUSE, ICON_PLAY, ICON_RING};
    let mut hits = Hits::new();
    draw_background(cr, w, h, look, p.transparent_bg);
    // Round icon buttons from the bottom-right corner leftwards.
    let b = 28.0;
    let by = h - b - 10.0;
    let mut bx = w - 12.0 - b;
    let mut button = |cr: *mut Cairo, hits: &mut Hits, hit: Hit, icon: &str, filled: bool| {
        if hovered == Some(hit) {
            crate::cairo_backend::fill_circle(cr, bx + b / 2.0, by + b / 2.0, b / 2.0, (look.primary.0, look.primary.1, look.primary.2, 0.22));
        }
        let i = b * 0.55;
        crate::svg::draw_path(cr, icon, bx + (b - i) / 2.0, by + (b - i) / 2.0, i, look.fg, if filled { None } else { Some(2.0) });
        hits.push((hit, Rect { x: bx, y: by, w: b, h: b }));
        bx -= b + 6.0;
    };
    let muted = (look.fg.0, look.fg.1, look.fg.2, 0.65);
    let font = |weight: &str, px: f64| format!("{} {weight} {px:.0}px", look.family);
    let Some(d) = state.and_then(|s| s.primary()) else {
        let msg = match state {
            None => "Phone sync is off",
            Some(_) => "No phone paired",
        };
        draw_in_box(cr, msg, &TextStyle { family: &look.family, px: 14.0, bold: true, color: muted }, 0.0, 0.0, w, h, Align::Center);
        button(cr, &mut hits, Hit::PhoneCard, ICON_OPEN, false);
        return hits;
    };
    let r = (h * 0.30).clamp(18.0, 44.0);
    let cx = 14.0 + r + 4.0;
    draw_battery_ring(cr, cx, h / 2.0, r, d, look.fg, look.primary);
    let tx = cx + r + 16.0;
    let tw = (w - tx - 12.0).max(10.0);
    crate::cairo_backend::draw_pango_text(cr, tx, h * 0.14, &d.name, &font("Bold", 16.0), look.fg, Some(tw as i32));
    crate::cairo_backend::draw_pango_text(cr, tx, h * 0.14 + 24.0, &status_line(d), &font("Regular", 12.0), muted, Some(tw as i32));
    let media = d.media.as_ref().filter(|m| !m.title.is_empty());
    if let Some(m) = media {
        let line = if m.artist.is_empty() { m.title.clone() } else { format!("{} · {}", m.title, m.artist) };
        let extra = if m.length_ms > 0 { format!("  {}", crate::phone::fmt_time(media_position(m))) } else { String::new() };
        crate::cairo_backend::draw_pango_text(cr, tx, h * 0.14 + 44.0, &format!("♪ {line}{extra}"), &font("Regular", 12.0), look.fg, Some(tw as i32));
    } else if !d.notifications.is_empty() {
        let n = d.notifications.len();
        crate::cairo_backend::draw_pango_text(cr, tx, h * 0.14 + 44.0, &format!("{n} notification{}", if n == 1 { "" } else { "s" }), &font("Regular", 12.0), look.fg, Some(tw as i32));
    }
    // Buttons, bottom right: always the open-card one; the rest when there is room.
    button(cr, &mut hits, Hit::PhoneCard, ICON_OPEN, false);
    if d.reachable && by > h * 0.14 + 60.0 {
        button(cr, &mut hits, Hit::PhoneRing, ICON_RING, false);
        if let Some(m) = media {
            button(cr, &mut hits, Hit::PhoneNext, ICON_NEXT, true);
            button(cr, &mut hits, Hit::PhonePlayPause, if m.playing { ICON_PAUSE } else { ICON_PLAY }, true);
        }
    }
    hits
}

// ── Batteries ─────────────────────────────────────────────────────────────

/// One device: icon, name, level and a battery. Sized to the cell it is given.
fn draw_battery_cell(cr: *mut Cairo, cell: Rect, family: &str, look: &Look, d: &crate::device_battery::DeviceBattery) {
    let size = cell.h.min(36.0);
    let cy = cell.y + cell.h / 2.0;
    // Low shows in the shape of the badge too: the error colour can be close to the card's.
    let (badge, glyph) = if d.low() {
        (look.fg, (look.card.0, look.card.1, look.card.2, 1.0))
    } else {
        ((look.primary.0, look.primary.1, look.primary.2, 0.18), look.fg)
    };
    fill_circle(cr, cell.x + size / 2.0, cy, size / 2.0, badge);
    let i = size * 0.58;
    crate::svg::draw_path(cr, d.icon, cell.x + (size - i) / 2.0, cy - i / 2.0, i, glyph, Some(2.0));

    // The battery, at the right end of the cell.
    let (gw, gh) = (42.0, 22.0);
    let gx = cell.x + cell.w - gw - 4.0;
    draw_battery_gauge(cr, gx, cy - gh / 2.0, gw, gh, look, d);

    let tx = cell.x + size + 10.0;
    let tw = (gx - tx - 10.0).max(10.0);
    let top = cy - 18.0;
    draw_in_box(cr, &d.name, &TextStyle { family, px: 13.0, bold: false, color: look.fg }, tx, top, tw, 18.0, Align::Start);
    let percent = TextStyle { family, px: 13.0, bold: true, color: look.fg };
    draw_in_box(cr, &format!("{}%", d.percent), &percent, tx, top + 18.0, tw, 18.0, Align::Start);
}

/// A battery filled as far as the device's level, with a bolt across it while charging. `w` is
/// the body; the terminal sticks out to its right.
fn draw_battery_gauge(cr: *mut Cairo, x: f64, y: f64, w: f64, h: f64, look: &Look, d: &crate::device_battery::DeviceBattery) {
    const LINE: f64 = 1.5;
    const INSET: f64 = 3.0;
    let outline = (look.fg.0, look.fg.1, look.fg.2, 0.75);
    unsafe {
        rounded_rect_path(cr, x, y, w, h, 5.0);
        cairo_set_source_rgba(cr, outline.0, outline.1, outline.2, outline.3);
        cairo_set_line_width(cr, LINE);
        cairo_stroke(cr);
    }
    fill_rounded_rect(cr, x + w + 1.5, y + h * 0.3, 2.5, h * 0.4, 1.0, outline);

    let inner = w - 2.0 * INSET;
    if d.percent > 0 {
        // Never thinner than its own corners, so a nearly empty battery still shows a sliver.
        let filled = (inner * d.percent as f64 / 100.0).max(4.0);
        let colour = if d.low() { look.error } else { look.primary };
        fill_rounded_rect(cr, x + INSET, y + INSET, filled, h - 2.0 * INSET, 2.5, colour);
    }
    if d.charging {
        // Outlined in the card's colour so it reads over the fill and over the empty part alike.
        let b = h - 4.0;
        let (bx, by) = (x + (w - b) / 2.0, y + 2.0);
        let halo = (look.card.0, look.card.1, look.card.2, 1.0);
        crate::svg::draw_path(cr, crate::device_battery::ICON_CHARGING, bx, by, b, halo, Some(5.0));
        crate::svg::draw_path(cr, crate::device_battery::ICON_CHARGING, bx, by, b, look.fg, None);
    }
}

/// Batteries widget: every connected device that reports a level, in as many columns and rows
/// as the widget has room for. `devices` is None while UPower cannot be reached.
pub fn draw_batteries(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, devices: Option<&[crate::device_battery::DeviceBattery]>) -> Hits {
    const PAD: f64 = 12.0;
    const GAP: f64 = 8.0;
    const CELL_W: f64 = 180.0;
    const CELL_H: f64 = 40.0;
    draw_background(cr, w, h, look, p.transparent_bg);
    let family = look.family_for(&p.font_family);
    let muted = (look.fg.0, look.fg.1, look.fg.2, 0.65);
    let reachable = devices.is_some();
    let devices = devices.unwrap_or_default();
    if devices.is_empty() {
        let msg = if reachable { "No devices reporting battery" } else { "UPower is not available" };
        draw_in_box(cr, msg, &TextStyle { family: &family, px: 14.0, bold: true, color: muted }, PAD, 0.0, w - 2.0 * PAD, h, Align::Center);
        return Hits::new();
    }
    let (iw, ih) = (w - 2.0 * PAD, h - 2.0 * PAD);
    let cols = (((iw + GAP) / (CELL_W + GAP)).floor() as usize).max(1);
    let rows = (((ih + GAP) / (CELL_H + GAP)).floor() as usize).max(1);
    let capacity = cols * rows;
    // The last place says how many did not fit.
    let shown = if devices.len() > capacity { capacity - 1 } else { devices.len() };
    let slots = if devices.len() > capacity { capacity } else { shown };
    let cols = cols.min(slots.max(1));
    let used_rows = slots.div_ceil(cols);
    let cell_w = (iw - GAP * (cols - 1) as f64) / cols as f64;
    let total = used_rows as f64 * CELL_H + (used_rows - 1) as f64 * GAP;
    let top = (h - total) / 2.0;
    let cell = |i: usize| Rect {
        x: PAD + (i % cols) as f64 * (cell_w + GAP),
        y: top + (i / cols) as f64 * (CELL_H + GAP),
        w: cell_w,
        h: CELL_H,
    };
    for (i, d) in crate::device_battery::fitting(devices, shown).into_iter().enumerate() {
        draw_battery_cell(cr, cell(i), &family, look, d);
    }
    if shown < devices.len() {
        let r = cell(shown);
        let more = format!("+{} more", devices.len() - shown);
        draw_in_box(cr, &more, &TextStyle { family: &family, px: 13.0, bold: true, color: muted }, r.x, r.y, r.w, r.h, Align::Center);
    }
    Hits::new()
}

/// Weather widget: glyph, temperature and conditions, the place, and three days ahead when tall
/// enough. Click to fetch again.
pub fn draw_weather(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, data: &(Option<crate::weather::Weather>, Option<String>)) -> Hits {
    const PAD: f64 = 16.0;
    draw_background(cr, w, h, look, p.transparent_bg);
    let family = look.family_for(&p.font_family);
    let muted = (look.fg.0, look.fg.1, look.fg.2, 0.65);
    let hits = vec![(Hit::WeatherRefresh, Rect { x: 0.0, y: 0.0, w, h })];
    let Some(wx) = &data.0 else {
        let msg = data.1.clone().unwrap_or_else(|| "Loading weather\u{2026}".into());
        draw_in_box(cr, &msg, &TextStyle { family: &family, px: 13.0, bold: true, color: muted }, PAD, 0.0, w - 2.0 * PAD, h, Align::Center);
        return hits;
    };
    let (desc, glyph) = crate::weather::describe(wx.code);
    let show_days = h >= 150.0 && !wx.days.is_empty();
    let top_h = if show_days { h - 64.0 } else { h };
    let g = (top_h - 2.0 * PAD).clamp(32.0, 64.0);
    crate::svg::draw_path(cr, crate::weather::glyph_path(glyph, wx.is_day), PAD, (top_h - g) / 2.0, g, look.fg, Some(1.8));
    let tx = PAD + g + 14.0;
    let temp = format!("{:.0}{}", wx.temp, wx.unit);
    draw_in_box(cr, &temp, &TextStyle { family: &family, px: 34.0, bold: true, color: look.fg }, tx, top_h / 2.0 - 40.0, w - tx - PAD, 44.0, Align::Start);
    let line = if wx.place.is_empty() { format!("{desc} \u{b7} feels {:.0}\u{b0}", wx.feels) } else { format!("{desc} \u{b7} {}", wx.place) };
    draw_in_box(cr, &line, &TextStyle { family: &family, px: 13.0, bold: false, color: muted }, tx, top_h / 2.0 + 6.0, w - tx - PAD, 20.0, Align::Start);
    if show_days {
        let n = wx.days.len().min(3) as f64;
        let cw = (w - 2.0 * PAD) / n;
        let y = h - 64.0;
        fill_rounded_rect(cr, PAD, y - 2.0, w - 2.0 * PAD, 1.0, 0.0, (look.fg.0, look.fg.1, look.fg.2, 0.12));
        for (i, d) in wx.days.iter().take(3).enumerate() {
            let x = PAD + i as f64 * cw;
            draw_in_box(cr, &d.name, &TextStyle { family: &family, px: 12.0, bold: true, color: muted }, x, y + 6.0, cw * 0.4, 20.0, Align::Start);
            let (_, gl) = crate::weather::describe(d.code);
            crate::svg::draw_path(cr, crate::weather::glyph_path(gl, true), x, y + 30.0, 20.0, look.fg, Some(1.8));
            let range = format!("{:.0}\u{b0} / {:.0}\u{b0}", d.max, d.min);
            draw_in_box(cr, &range, &TextStyle { family: &family, px: 12.0, bold: false, color: look.fg }, x + 26.0, y + 30.0, cw - 30.0, 20.0, Align::Start);
        }
    }
    hits
}

/// Notes widget: the note's text, as much as fits. Click to edit.
pub fn draw_notes(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, text: &str) -> Hits {
    const PAD: f64 = 16.0;
    draw_background(cr, w, h, look, p.transparent_bg);
    let family = look.family_for(&p.font_family);
    let muted = (look.fg.0, look.fg.1, look.fg.2, 0.6);
    draw_in_box(cr, "Notes", &TextStyle { family: &family, px: 12.0, bold: true, color: muted }, PAD, PAD - 4.0, w - 2.0 * PAD, 18.0, Align::Start);
    let body = if text.trim().is_empty() { "Click to write a note." } else { text };
    let color = if text.trim().is_empty() { muted } else { look.fg };
    let lines = (((h - PAD - 22.0 - PAD) / 19.0).floor() as i32).max(1);
    draw_pango_text_wrapped(cr, PAD, PAD + 20.0, body, &format!("{family} 14px"), color, (w - 2.0 * PAD) as i32, Some(lines));
    vec![(Hit::NotesOpen, Rect { x: 0.0, y: 0.0, w, h })]
}

/// A button on the Timer widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerButton {
    Preset(u32),
    Add,
    PauseResume,
    Cancel,
    Stopwatch,
    StopwatchReset,
}

impl TimerButton {
    pub fn action(self, t: &prism_common::ipc::TimerState) -> prism_common::ipc::TimerAction {
        use prism_common::ipc::TimerAction as A;
        match self {
            TimerButton::Preset(s) => A::Start(s as u64),
            TimerButton::Add => A::Add(60),
            TimerButton::PauseResume if t.end_ms.is_some() => A::Pause,
            TimerButton::PauseResume => A::Resume,
            TimerButton::Cancel => A::Cancel,
            TimerButton::Stopwatch => A::StopwatchToggle,
            TimerButton::StopwatchReset => A::StopwatchReset,
        }
    }
}

/// Timer widget: the countdown in a ring that empties, its buttons (or presets when none is
/// set), and the stopwatch underneath when there is room.
pub fn draw_timer(cr: *mut Cairo, w: f64, h: f64, p: &WidgetPosition, look: &Look, t: &prism_common::ipc::TimerState, hovered: Option<Hit>) -> Hits {
    const PAD: f64 = 14.0;
    draw_background(cr, w, h, look, p.transparent_bg);
    let family = look.family_for(&p.font_family);
    let muted = (look.fg.0, look.fg.1, look.fg.2, 0.6);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let left = t.remaining(now);
    let show_sw = h >= 130.0;
    let top_h = if show_sw { h - 44.0 } else { h };
    let ring = ((top_h - 2.0 * PAD) / 2.0).clamp(24.0, 56.0);
    let (cx, cy) = (PAD + ring, top_h / 2.0);
    let frac = match left {
        Some(l) if t.total_ms > 0 => (l as f64 / t.total_ms as f64).clamp(0.0, 1.0),
        _ => 0.0,
    };
    stroke_circle(cr, cx, cy, ring, (look.fg.0, look.fg.1, look.fg.2, 0.15), 5.0);
    if frac > 0.0 {
        unsafe {
            cairo_save(cr);
            cairo_set_line_width(cr, 5.0);
            let c = look.primary;
            cairo_set_source_rgba(cr, c.0, c.1, c.2, 1.0);
            let a0 = -std::f64::consts::FRAC_PI_2;
            cairo_new_path(cr);
            cairo_arc(cr, cx, cy, ring, a0, a0 + frac * std::f64::consts::TAU);
            cairo_stroke(cr);
            cairo_restore(cr);
        }
    }
    let text = left.map(prism_common::ipc::clock_text).unwrap_or_else(|| "00:00".into());
    let px = (ring * 0.42).clamp(13.0, 24.0);
    draw_in_box(cr, &text, &TextStyle { family: &family, px, bold: true, color: if left.is_some() { look.fg } else { muted } }, cx - ring, cy - px, ring * 2.0, px * 2.0, Align::Center);
    let mut hits = Hits::new();
    let bx = cx + ring + 14.0;
    let bw_total = w - bx - PAD;
    let buttons: Vec<(String, TimerButton)> = if left.is_some() {
        let pr = if t.end_ms.is_some() { "Pause" } else { "Resume" };
        vec![(pr.into(), TimerButton::PauseResume), ("+1 min".into(), TimerButton::Add), ("Cancel".into(), TimerButton::Cancel)]
    } else {
        [5u32, 10, 25].iter().map(|m| (format!("{m} min"), TimerButton::Preset(m * 60))).collect()
    };
    if bw_total > 60.0 {
        let n = buttons.len() as f64;
        let bh = 28.0;
        let gap = 6.0;
        let total_h = n * bh + (n - 1.0) * gap;
        let mut y = cy - total_h / 2.0;
        for (label, b) in buttons {
            let r = Rect { x: bx, y, w: bw_total, h: bh };
            let hov = hovered == Some(Hit::Timer(b));
            fill_rounded_rect(cr, r.x, r.y, r.w, r.h, bh / 2.0, (look.fg.0, look.fg.1, look.fg.2, if hov { 0.24 } else { 0.12 }));
            draw_in_box(cr, &label, &TextStyle { family: &family, px: 12.0, bold: true, color: look.fg }, r.x, r.y, r.w, r.h, Align::Center);
            hits.push((Hit::Timer(b), r));
            y += bh + gap;
        }
    }
    if show_sw {
        let y = h - 40.0;
        fill_rounded_rect(cr, PAD, y - 4.0, w - 2.0 * PAD, 1.0, 0.0, (look.fg.0, look.fg.1, look.fg.2, 0.12));
        let ms = t.stopwatch(now);
        draw_in_box(cr, "Stopwatch", &TextStyle { family: &family, px: 12.0, bold: true, color: muted }, PAD, y, 90.0, 28.0, Align::Start);
        let sw = prism_common::ipc::clock_text(ms);
        draw_in_box(cr, &sw, &TextStyle { family: &family, px: 16.0, bold: true, color: if ms > 0 { look.fg } else { muted } }, PAD + 86.0, y, 90.0, 28.0, Align::Start);
        let go = if t.sw_start_ms.is_some() { "Pause" } else if ms > 0 { "Resume" } else { "Start" };
        let mut x = w - PAD;
        for (label, b) in [("Reset", TimerButton::StopwatchReset), (go, TimerButton::Stopwatch)] {
            if b == TimerButton::StopwatchReset && ms == 0 {
                continue;
            }
            let bw = 62.0;
            x -= bw;
            let r = Rect { x, y, w: bw, h: 26.0 };
            if r.x < PAD + 170.0 {
                break;
            }
            let hov = hovered == Some(Hit::Timer(b));
            fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 13.0, (look.fg.0, look.fg.1, look.fg.2, if hov { 0.24 } else { 0.12 }));
            draw_in_box(cr, label, &TextStyle { family: &family, px: 12.0, bold: true, color: look.fg }, r.x, r.y, r.w, r.h, Align::Center);
            hits.push((Hit::Timer(b), r));
            x -= 6.0;
        }
    }
    hits
}
