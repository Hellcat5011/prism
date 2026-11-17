//! Phone card (look of the Unison popover): battery ring, name and status with find-my-phone
//! and send-clipboard buttons; Send Files and Notifications tiles; the phone's media with
//! controls; transfers; settings and refresh. Sub-pages: phone notifications (with reply) and
//! pairing. State comes from `phone_client` (prism-phoned); commands go back through it.

use prism_common::phone::{MediaCommand, PairStatus, PhoneCommand, PhoneDevice, PhoneMedia, PhoneState, TransferDirection};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{draw_query, Query, ENTER, ESC};
use crate::phone_client;
use crate::svg;
use crate::ui::Style;
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 480.0;
pub const CARD_H: f64 = 600.0;
const PAD: f64 = 20.0;
const TILE_R: f64 = 18.0;

pub const ICON_PHONE: &str = "M7 2h10a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2zM11 18h2";
pub const ICON_RING: &str = "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0M4 2C2.8 3.7 2 5.7 2 8M22 8c0-2.3-.8-4.3-2-6";
const ICON_CLIP: &str = "M9 2h6a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1H9a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1zM16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2";
const ICON_UPLOAD: &str = "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6M12 18v-6M9 15l3-3 3 3";
const ICON_BELL: &str = "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0";
pub const ICON_PREV: &str = "M19 20 9 12l10-8zM5 5h2.5v14H5z";
pub const ICON_PLAY: &str = "M6 3l14 9-14 9z";
pub const ICON_PAUSE: &str = "M6 4h4v16H6zM14 4h4v16h-4z";
pub const ICON_NEXT: &str = "M5 4l10 8-10 8zM16.5 5H19v14h-2.5z";
const ICON_GEAR: &str = "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z";
const ICON_REFRESH: &str = "M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8M21 3v5h-5";
const ICON_X: &str = "M18 6 6 18M6 6l12 12";
const ICON_BACK: &str = "M19 12H5M12 19l-7-7 7-7";
/// External link: opens the Phone card from the desktop widget.
pub const ICON_OPEN: &str = "M15 3h6v6M10 14 21 3M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6";
const ICON_SEND: &str = "M22 2 11 13M22 2l-7 20-4-9-9-4z";

type Rgba = (f64, f64, f64, f64);

fn alpha(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, c.3 * a)
}

#[derive(Clone, Debug, PartialEq)]
enum Act {
    Ring,
    Clip,
    SendFiles,
    NotifPage,
    Back,
    Media(MediaCommand),
    Seek(Rect),
    Settings,
    Refresh,
    EnableSync,
    Pair(String),
    Accept(String),
    Reject(String),
    NotifAction(String, String),
    Reply(String),
    SendReply,
    Dismiss(String),
    CancelTransfer(u64),
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Main,
    Notifications,
}

pub struct PhoneView {
    waker: Waker,
    seen: u64,
    state: Option<PhoneState>,
    page: Page,
    hits: Vec<(Rect, Act)>,
    hover: Option<Act>,
    pointer: (f64, f64),
    /// Notification id being replied to, and the text.
    reply: Option<(String, Query)>,
    scroll: crate::ui::Scroller,
}

/// Unix milliseconds now.
fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Playback position now (advances while playing).
pub fn media_position(m: &PhoneMedia) -> i64 {
    let p = if m.playing { m.position_ms + (now_ms() - m.position_at_ms).max(0) } else { m.position_ms };
    if m.length_ms > 0 { p.min(m.length_ms) } else { p }
}

pub fn fmt_time(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn fmt_bytes(b: u64) -> String {
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1048575 => format!("{:.0} KB", b as f64 / 1024.0),
        1048576..=1073741823 => format!("{:.1} MB", b as f64 / 1048576.0),
        _ => format!("{:.2} GB", b as f64 / 1073741824.0),
    }
}

/// "● Connected · 76%" and similar.
pub fn status_line(d: &PhoneDevice) -> String {
    if !d.reachable {
        let ago = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_secs()).unwrap_or(0)).saturating_sub(d.last_seen);
        return if d.last_seen == 0 {
            "Not connected".into()
        } else if ago < 120 {
            "Not connected · just now".into()
        } else if ago < 7200 {
            format!("Not connected · {} min ago", ago / 60)
        } else {
            "Not connected".into()
        };
    }
    let mut s = String::from("Connected");
    if let Some(b) = d.battery {
        s.push_str(&format!(" · {}%{}", b.percent, if b.charging { " ⚡" } else { "" }));
    }
    if let Some(n) = d.network.as_ref().filter(|n| !n.kind.is_empty()) {
        s.push_str(&format!(" · {}", n.kind));
    }
    s
}

/// Battery ring with a phone glyph and the percentage inside.
pub fn draw_battery_ring(cr: *mut Cairo, cx: f64, cy: f64, r: f64, d: &PhoneDevice, fg: Rgba, accent: Rgba) {
    let pct = d.battery.map(|b| b.percent).unwrap_or(0) as f64 / 100.0;
    let track = alpha(fg, 0.12);
    let fill = if !d.reachable { alpha(fg, 0.35) } else if d.battery.is_some_and(|b| b.percent <= 15 && !b.charging) { (0.9, 0.3, 0.3, 1.0) } else { accent };
    let w = (r * 0.18).max(4.0);
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_set_line_width(cr, w);
        cb::cairo_set_line_cap(cr, 1);
        cb::cairo_set_source_rgba(cr, track.0, track.1, track.2, track.3);
        cb::cairo_new_path(cr);
        cb::cairo_arc(cr, cx, cy, r, 0.0, std::f64::consts::TAU);
        cb::cairo_stroke(cr);
        if pct > 0.0 {
            cb::cairo_set_source_rgba(cr, fill.0, fill.1, fill.2, fill.3);
            cb::cairo_new_path(cr);
            let start = -std::f64::consts::FRAC_PI_2;
            cb::cairo_arc(cr, cx, cy, r, start, start + std::f64::consts::TAU * pct);
            cb::cairo_stroke(cr);
        }
        cb::cairo_restore(cr);
    }
    let icon = r * 0.62;
    svg::draw_path(cr, ICON_PHONE, cx - icon / 2.0, cy - icon * 0.78, icon, fg, Some(1.8));
    let label = d.battery.map(|b| format!("{}%", b.percent)).unwrap_or_else(|| "–".into());
    let font = format!("Inter, Sans Bold {:.0}px", (r * 0.36).max(10.0));
    let (tw, _) = cb::measure_pango_text(cr, &label, &font, 1000, Some(1));
    cb::draw_pango_text(cr, cx - tw as f64 / 2.0, cy + r * 0.12, &label, &font, fg, None);
}

impl PhoneView {
    pub fn new(waker: Waker) -> Self {
        phone_client::start(waker.clone());
        phone_client::clear_error();
        let (seen, state) = phone_client::snapshot();
        PhoneView { waker, seen, state, page: Page::Main, hits: Vec::new(), hover: None, pointer: (0.0, 0.0), reply: None, scroll: crate::ui::Scroller::default() }
    }

    fn cmd(&self, c: PhoneCommand) {
        phone_client::command(c, &self.waker);
    }

    fn device(&self) -> Option<&PhoneDevice> {
        self.state.as_ref().and_then(|s| s.primary())
    }

    fn hit(&mut self, r: Rect, a: Act) {
        self.hits.push((r, a));
    }

    fn hovered(&self, a: &Act) -> bool {
        self.hover.as_ref() == Some(a)
    }

    /// Round icon button.
    fn icon_button(&mut self, cr: *mut Cairo, x: f64, y: f64, size: f64, icon: &str, act: Act, style: &Style, enabled: bool) {
        let hov = enabled && self.hovered(&act);
        cb::fill_circle(cr, x + size / 2.0, y + size / 2.0, size / 2.0, alpha(style.fg, if hov { 0.16 } else { 0.07 }));
        let i = size * 0.5;
        svg::draw_path(cr, icon, x + (size - i) / 2.0, y + (size - i) / 2.0, i, alpha(style.fg, if enabled { 1.0 } else { 0.35 }), Some(2.0));
        if enabled {
            self.hit(Rect { x, y, w: size, h: size }, act);
        }
    }

    /// Text button (pill).
    fn pill(&mut self, cr: *mut Cairo, x: f64, y: f64, label: &str, act: Act, style: &Style, strong: bool) -> f64 {
        let font = "Inter, Sans Semi-Bold 13px";
        let (tw, th) = cb::measure_pango_text(cr, label, font, 1000, Some(1));
        let (w, h) = (tw as f64 + 24.0, 30.0);
        let hov = self.hovered(&act);
        let bg = if strong { alpha(style.accent, if hov { 1.0 } else { 0.85 }) } else { alpha(style.fg, if hov { 0.16 } else { 0.08 }) };
        cb::fill_rounded_rect(cr, x, y, w, h, h / 2.0, bg);
        let fg = if strong { (0.08, 0.08, 0.1, 1.0) } else { style.fg };
        cb::draw_pango_text(cr, x + 12.0, y + (h - th as f64) / 2.0, label, font, fg, None);
        self.hit(Rect { x, y, w, h }, act);
        w
    }

    fn draw_main(&mut self, cr: *mut Cairo, c: Rect, style: &Style, d: PhoneDevice) {
        let (x, w) = (c.x + PAD, c.w - 2.0 * PAD);
        let mut y = c.y + PAD;
        let muted = alpha(style.fg, 0.62);
        let tile_bg = alpha(style.fg, 0.06);
        let live = d.reachable;

        // Header.
        let head = Rect { x, y, w, h: 96.0 };
        cb::fill_rounded_rect(cr, head.x, head.y, head.w, head.h, TILE_R, tile_bg);
        draw_battery_ring(cr, x + 52.0, y + 48.0, 32.0, &d, style.fg, style.accent);
        let name_x = x + 100.0;
        let bw = 40.0;
        let text_w = (w - 100.0 - 2.0 * bw - 28.0) as i32;
        cb::draw_pango_text(cr, name_x, y + 22.0, &d.name, "Inter, Sans Bold 19px", style.fg, Some(text_w));
        let dot = if live { (0.30, 0.78, 0.45, 1.0) } else { alpha(style.fg, 0.35) };
        cb::fill_circle(cr, name_x + 5.0, y + 60.0, 4.5, dot);
        cb::draw_pango_text(cr, name_x + 16.0, y + 50.0, &status_line(&d), "Inter, Sans 14px", muted, Some(text_w - 16));
        let by = y + (96.0 - bw) / 2.0;
        self.icon_button(cr, x + w - 16.0 - bw, by, bw, ICON_CLIP, Act::Clip, style, live && d.supports("kdeconnect.clipboard"));
        self.icon_button(cr, x + w - 24.0 - 2.0 * bw, by, bw, ICON_RING, Act::Ring, style, live);
        y += 96.0 + 12.0;

        // Tiles.
        let tw = (w - 12.0) / 2.0;
        let unread = d.notifications.len();
        let tiles = [
            (ICON_UPLOAD, "Send Files".to_string(), "Pick files to send".to_string(), Act::SendFiles, live),
            (ICON_BELL, "Notifications".to_string(), if unread == 0 { "None".into() } else { format!("{unread} from the phone") }, Act::NotifPage, true),
        ];
        for (i, (icon, title, sub, act, en)) in tiles.into_iter().enumerate() {
            let r = Rect { x: x + i as f64 * (tw + 12.0), y, w: tw, h: 92.0 };
            let hov = en && self.hovered(&act);
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, TILE_R, alpha(style.fg, if hov { 0.12 } else { 0.06 }));
            let a = if en { 1.0 } else { 0.4 };
            svg::draw_path(cr, icon, r.x + 18.0, r.y + 16.0, 22.0, alpha(style.accent, a), Some(2.0));
            cb::draw_pango_text(cr, r.x + 18.0, r.y + 46.0, &title, "Inter, Sans Bold 15px", alpha(style.fg, a), Some((r.w - 30.0) as i32));
            cb::draw_pango_text(cr, r.x + 18.0, r.y + 66.0, &sub, "Inter, Sans 12px", alpha(muted, a), Some((r.w - 30.0) as i32));
            if en {
                self.hit(r, act);
            }
        }
        y += 92.0 + 12.0;

        // Media.
        match d.media.clone().filter(|m| !m.title.is_empty() || m.playing) {
            Some(m) => {
                let r = Rect { x, y, w, h: 170.0 };
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, TILE_R, tile_bg);
                let art = 64.0;
                let drew = m.art_path.as_deref().is_some_and(|p| cb::draw_image_cover(cr, p, r.x + 16.0, r.y + 16.0, art, art, 12.0));
                if !drew {
                    cb::fill_rounded_rect(cr, r.x + 16.0, r.y + 16.0, art, art, 12.0, alpha(style.accent, 0.35));
                    svg::draw_path(cr, ICON_PLAY, r.x + 16.0 + 20.0, r.y + 16.0 + 20.0, 24.0, style.fg, None);
                }
                let tx = r.x + 16.0 + art + 14.0;
                let tmax = (r.w - (tx - r.x) - 16.0) as i32;
                let title = if m.title.is_empty() { "Unknown title".to_string() } else { m.title.clone() };
                cb::draw_pango_text(cr, tx, r.y + 24.0, &title, "Inter, Sans Bold 16px", style.fg, Some(tmax));
                let sub = [m.artist.as_str(), m.player.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
                cb::draw_pango_text(cr, tx, r.y + 48.0, &sub, "Inter, Sans 13px", muted, Some(tmax));
                // Progress.
                let bar = Rect { x: r.x + 16.0, y: r.y + 96.0, w: r.w - 32.0, h: 5.0 };
                let pos = media_position(&m);
                let frac = if m.length_ms > 0 { pos as f64 / m.length_ms as f64 } else { 0.0 };
                cb::fill_rounded_rect(cr, bar.x, bar.y, bar.w, bar.h, 2.5, alpha(style.fg, 0.15));
                cb::fill_rounded_rect(cr, bar.x, bar.y, (bar.w * frac.clamp(0.0, 1.0)).max(5.0), bar.h, 2.5, alpha(style.fg, 0.75));
                if m.can_seek && m.length_ms > 0 {
                    self.hit(Rect { x: bar.x, y: bar.y - 8.0, w: bar.w, h: bar.h + 16.0 }, Act::Seek(bar));
                }
                let tf = "Inter, Sans 11px";
                cb::draw_pango_text(cr, bar.x, bar.y + 9.0, &fmt_time(pos), tf, muted, None);
                if m.length_ms > 0 {
                    let total = fmt_time(m.length_ms);
                    let (lw, _) = cb::measure_pango_text(cr, &total, tf, 1000, Some(1));
                    cb::draw_pango_text(cr, bar.x + bar.w - lw as f64, bar.y + 9.0, &total, tf, muted, None);
                }
                // Controls.
                let cy = r.y + 124.0;
                let mid = r.x + r.w / 2.0;
                let btn = 38.0;
                let controls = [
                    (mid - 76.0 - btn / 2.0, ICON_PREV, MediaCommand::Previous, m.can_previous),
                    (mid - btn / 2.0, if m.playing { ICON_PAUSE } else { ICON_PLAY }, MediaCommand::PlayPause, m.can_play || m.can_pause),
                    (mid + 76.0 - btn / 2.0, ICON_NEXT, MediaCommand::Next, m.can_next),
                ];
                for (bx, icon, act, en) in controls {
                    let a = Act::Media(act);
                    let hov = en && self.hovered(&a);
                    if hov {
                        cb::fill_circle(cr, bx + btn / 2.0, cy + btn / 2.0, btn / 2.0, alpha(style.fg, 0.12));
                    }
                    let i = 20.0;
                    svg::draw_path(cr, icon, bx + (btn - i) / 2.0, cy + (btn - i) / 2.0, i, alpha(style.fg, if en { 1.0 } else { 0.35 }), None);
                    if en && live {
                        self.hit(Rect { x: bx, y: cy, w: btn, h: btn }, a);
                    }
                }
                y += r.h + 12.0;
            }
            None => {
                let r = Rect { x, y, w, h: 56.0 };
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, TILE_R, tile_bg);
                cb::draw_pango_text(cr, r.x + 18.0, r.y + 18.0, if live { "Nothing playing on the phone" } else { "Media appears here when the phone is connected" }, "Inter, Sans 14px", muted, Some((r.w - 36.0) as i32));
                y += r.h + 12.0;
            }
        }

        // Transfers (running first, then the latest finished ones).
        let mut transfers = d.transfers.clone();
        transfers.sort_by_key(|t| (t.done || t.error.is_some(), std::cmp::Reverse(t.id)));
        let footer_y = c.y + c.h - PAD - 36.0;
        for t in transfers.iter().take(3) {
            if y + 52.0 > footer_y - 6.0 {
                break;
            }
            let r = Rect { x, y, w, h: 52.0 };
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 14.0, tile_bg);
            let arrow = if t.direction == TransferDirection::Incoming { "↓" } else { "↑" };
            cb::draw_pango_text(cr, r.x + 14.0, r.y + 8.0, &format!("{arrow}  {}", t.name), "Inter, Sans Semi-Bold 13px", style.fg, Some((r.w - 60.0) as i32));
            let detail = match (&t.error, t.done) {
                (Some(e), _) => format!("Failed: {e}"),
                (None, true) => format!("Done · {}", fmt_bytes(t.total.max(t.bytes))),
                (None, false) => format!("{} of {} · {}/s", fmt_bytes(t.bytes), fmt_bytes(t.total), fmt_bytes(t.speed)),
            };
            cb::draw_pango_text(cr, r.x + 14.0, r.y + 28.0, &detail, "Inter, Sans 11px", muted, Some((r.w - 60.0) as i32));
            if !t.done && t.error.is_none() {
                let frac = if t.total > 0 { t.bytes as f64 / t.total as f64 } else { 0.0 };
                cb::fill_rounded_rect(cr, r.x + 14.0, r.y + 44.0, r.w - 28.0, 3.0, 1.5, alpha(style.fg, 0.15));
                cb::fill_rounded_rect(cr, r.x + 14.0, r.y + 44.0, ((r.w - 28.0) * frac).max(3.0), 3.0, 1.5, style.accent);
                self.icon_button(cr, r.x + r.w - 40.0, r.y + 10.0, 28.0, ICON_X, Act::CancelTransfer(t.id), style, true);
            }
            y += r.h + 8.0;
        }
        self.footer(cr, c, style);
    }

    fn footer(&mut self, cr: *mut Cairo, c: Rect, style: &Style) {
        let fy = c.y + c.h - PAD - 36.0;
        self.icon_button(cr, c.x + PAD, fy, 36.0, ICON_GEAR, Act::Settings, style, true);
        self.icon_button(cr, c.x + c.w - PAD - 36.0, fy, 36.0, ICON_REFRESH, Act::Refresh, style, true);
        if let Some(e) = phone_client::last_error() {
            let font = "Inter, Sans 12px";
            cb::draw_pango_text(cr, c.x + PAD + 48.0, fy + 9.0, &e, font, (0.95, 0.45, 0.45, 1.0), Some((c.w - 2.0 * PAD - 96.0) as i32));
        }
    }

    fn draw_notifications(&mut self, cr: *mut Cairo, c: Rect, style: &Style, d: PhoneDevice) {
        let (x, w) = (c.x + PAD, c.w - 2.0 * PAD);
        let muted = alpha(style.fg, 0.62);
        self.icon_button(cr, x, c.y + PAD, 36.0, ICON_BACK, Act::Back, style, true);
        cb::draw_pango_text(cr, x + 48.0, c.y + PAD + 6.0, "Notifications", "Inter, Sans Bold 18px", style.fg, None);
        let top = c.y + PAD + 52.0;
        let bottom = c.y + c.h - PAD - 48.0;
        if d.notifications.is_empty() {
            let msg = if d.reachable {
                "No notifications from the phone. If this stays empty, allow notification access for KDE Connect on the phone."
            } else {
                "The phone is not connected."
            };
            cb::draw_pango_text(cr, x, top + 10.0, msg, "Inter, Sans 14px", muted, Some(w as i32));
            self.footer(cr, c, style);
            return;
        }
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, c.x, top, c.w, bottom - top);
            cb::cairo_clip(cr);
        }
        let scroll = self.scroll.pos();
        let mut y = top - scroll;
        let start_y = y;
        for n in d.notifications.iter() {
            let replying = self.reply.as_ref().is_some_and(|(id, _)| *id == n.id);
            let has_buttons = !n.actions.is_empty() || !n.reply_id.is_empty();
            let h = 92.0 + if has_buttons { 38.0 } else { 0.0 } + if replying { 46.0 } else { 0.0 };
            let r = Rect { x, y, w, h };
            if r.y + r.h >= top && r.y <= bottom {
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 16.0, alpha(style.fg, 0.06));
                let icon_drawn = n.icon_path.as_deref().is_some_and(|p| cb::draw_image_cover(cr, p, r.x + 14.0, r.y + 14.0, 28.0, 28.0, 8.0));
                if !icon_drawn {
                    svg::draw_path(cr, ICON_BELL, r.x + 18.0, r.y + 18.0, 20.0, style.accent, Some(2.0));
                }
                cb::draw_pango_text(cr, r.x + 52.0, r.y + 12.0, &n.app, "Inter, Sans 12px", muted, Some((r.w - 100.0) as i32));
                cb::draw_pango_text(cr, r.x + 52.0, r.y + 30.0, &n.title, "Inter, Sans Bold 14px", style.fg, Some((r.w - 70.0) as i32));
                cb::draw_pango_text_wrapped(cr, r.x + 52.0, r.y + 50.0, &n.text, "Inter, Sans 13px", alpha(style.fg, 0.85), (r.w - 70.0) as i32, Some(2));
                if n.dismissable && r.y + 10.0 >= top {
                    self.icon_button(cr, r.x + r.w - 38.0, r.y + 8.0, 28.0, ICON_X, Act::Dismiss(n.id.clone()), style, d.reachable);
                }
                let mut bx = r.x + 52.0;
                let by = r.y + 92.0;
                if has_buttons && by + 30.0 <= bottom && by >= top {
                    if !n.reply_id.is_empty() {
                        bx += self.pill(cr, bx, by, "Reply", Act::Reply(n.id.clone()), style, replying) + 8.0;
                    }
                    for a in &n.actions {
                        if bx > r.x + r.w - 80.0 {
                            break;
                        }
                        bx += self.pill(cr, bx, by, a, Act::NotifAction(n.id.clone(), a.clone()), style, false) + 8.0;
                    }
                }
                if replying {
                    let fy = r.y + h - 46.0;
                    let field = Rect { x: r.x + 52.0, y: fy, w: r.w - 52.0 - 56.0, h: 36.0 };
                    cb::fill_rounded_rect(cr, field.x, field.y, field.w, field.h, 10.0, alpha(style.fg, 0.10));
                    if let Some((_, q)) = &self.reply {
                        draw_query(cr, q, "Write a reply…", field.x + 10.0, field.y, field.w - 20.0, field.h, "Inter, Sans 14px", style.fg, muted);
                    }
                    self.icon_button(cr, r.x + r.w - 46.0, fy, 36.0, ICON_SEND, Act::SendReply, style, true);
                }
            }
            y += h + 10.0;
        }
        self.scroll.set_max(y - start_y - (bottom - top), style);
        unsafe { cb::cairo_restore(cr) };
        self.footer(cr, c, style);
    }

    fn draw_pairing(&mut self, cr: *mut Cairo, c: Rect, style: &Style, st: PhoneState) {
        let (x, w) = (c.x + PAD, c.w - 2.0 * PAD);
        let muted = alpha(style.fg, 0.62);
        let mut y = c.y + PAD;
        cb::draw_pango_text(cr, x, y, "Pair a Phone", "Inter, Sans Bold 22px", style.fg, None);
        y += 40.0;
        if let Some(e) = &st.error {
            let (_, h) = cb::measure_pango_text(cr, e, "Inter, Sans 13px", w as i32, Some(4));
            cb::draw_pango_text_wrapped(cr, x, y, e, "Inter, Sans 13px", (0.95, 0.45, 0.45, 1.0), w as i32, Some(4));
            y += h as f64 + 12.0;
        }
        // A request waiting for the user, or ours waiting for the phone.
        if let Some(d) = st.devices.iter().find(|d| matches!(d.pair, PairStatus::RequestedByPeer | PairStatus::Requested)).cloned() {
            let r = Rect { x, y, w, h: 170.0 };
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, TILE_R, alpha(style.fg, 0.06));
            let head = if d.pair == PairStatus::RequestedByPeer { format!("{} wants to pair", d.name) } else { format!("Waiting for {}…", d.name) };
            cb::draw_pango_text(cr, r.x + 18.0, r.y + 16.0, &head, "Inter, Sans Bold 16px", style.fg, Some((r.w - 36.0) as i32));
            cb::draw_pango_text(cr, r.x + 18.0, r.y + 42.0, "Check that the phone shows the same key:", "Inter, Sans 13px", muted, None);
            let key = d.verification_key.clone().unwrap_or_else(|| "…".into());
            cb::draw_pango_text(cr, r.x + 18.0, r.y + 66.0, &key, "Inter, Sans Bold 34px", style.accent, None);
            let by = r.y + r.h - 46.0;
            if d.pair == PairStatus::RequestedByPeer {
                let aw = self.pill(cr, r.x + 18.0, by, "Accept", Act::Accept(d.id.clone()), style, true);
                self.pill(cr, r.x + 18.0 + aw + 10.0, by, "Reject", Act::Reject(d.id.clone()), style, false);
            } else {
                self.pill(cr, r.x + 18.0, by, "Cancel", Act::Reject(d.id.clone()), style, false);
            }
            y += r.h + 16.0;
        }
        let found: Vec<PhoneDevice> = st.devices.iter().filter(|d| !d.paired && d.reachable && d.pair == PairStatus::NotPaired).cloned().collect();
        if !found.is_empty() {
            cb::draw_pango_text(cr, x, y, "Devices on this network", "Inter, Sans Semi-Bold 13px", muted, None);
            y += 24.0;
            for d in found {
                let r = Rect { x, y, w, h: 56.0 };
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 14.0, alpha(style.fg, 0.06));
                svg::draw_path(cr, ICON_PHONE, r.x + 16.0, r.y + 16.0, 24.0, style.fg, Some(1.8));
                cb::draw_pango_text(cr, r.x + 52.0, r.y + 17.0, &d.name, "Inter, Sans Semi-Bold 15px", style.fg, Some((r.w - 150.0) as i32));
                self.pill(cr, r.x + r.w - 80.0, r.y + 13.0, "Pair", Act::Pair(d.id.clone()), style, true);
                y += r.h + 8.0;
            }
            y += 8.0;
        }
        let help = format!(
            "Install KDE Connect on your Android phone (F-Droid or Google Play) and join the same Wi-Fi. \
             Then pick \u{201c}{}\u{201d} in the app, or press Pair above.\n\nNothing showing up? Open the firewall ports: \
             sudo ufw allow KDEConnect",
            st.name
        );
        cb::draw_pango_text_wrapped(cr, x, y, &help, "Inter, Sans 13px", muted, w as i32, Some(8));
        self.footer(cr, c, style);
    }

    fn draw_off(&mut self, cr: *mut Cairo, c: Rect, style: &Style) {
        let (x, w) = (c.x + PAD, c.w - 2.0 * PAD);
        cb::draw_pango_text(cr, x, c.y + PAD, "Phone", "Inter, Sans Bold 22px", style.fg, None);
        let msg = "Phone sync is off. Turn it on to connect an Android phone running KDE Connect: battery, media, \
                   notifications with reply, clipboard, files and remote input.";
        cb::draw_pango_text_wrapped(cr, x, c.y + PAD + 44.0, msg, "Inter, Sans 14px", alpha(style.fg, 0.7), w as i32, Some(6));
        self.pill(cr, x, c.y + PAD + 140.0, "Turn on phone sync", Act::EnableSync, style, true);
    }

    fn run(&mut self, act: Act) -> Effect {
        let dev = self.device().map(|d| d.id.clone());
        match act {
            Act::Ring => self.cmd(PhoneCommand::Ring { device: dev }),
            Act::Clip => self.cmd(PhoneCommand::SendClipboard { device: dev, text: String::new() }),
            Act::SendFiles => {
                let waker = self.waker.clone();
                std::thread::spawn(move || match crate::portal::pick_files("Send to phone") {
                    Ok(files) if !files.is_empty() => {
                        let paths = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                        phone_client::command(PhoneCommand::SendFiles { device: None, paths }, &waker);
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("[prism-ui] file picker: {e}"),
                });
            }
            Act::NotifPage => {
                self.page = Page::Notifications;
                self.scroll.reset(0.0);
            }
            Act::Back => {
                self.page = Page::Main;
                self.reply = None;
            }
            Act::Media(a) => self.cmd(PhoneCommand::Media { device: dev, action: a }),
            Act::Seek(bar) => {
                if let Some(len) = self.device().and_then(|d| d.media.as_ref()).map(|m| m.length_ms) {
                    let frac = ((self.pointer.0 - bar.x) / bar.w).clamp(0.0, 1.0);
                    self.cmd(PhoneCommand::Seek { device: dev, position_ms: (frac * len as f64) as i64 });
                }
            }
            Act::Settings => {
                std::thread::spawn(|| {
                    let _ = prism_common::ipc::request(&prism_common::IpcCommand::Settings(prism_common::ipc::SettingsAction::Open));
                });
                return Effect::Close;
            }
            Act::Refresh => self.cmd(PhoneCommand::Refresh),
            Act::EnableSync => {
                let mut cfg = prism_common::Config::load();
                cfg.phone_enabled = true;
                if let Err(e) = cfg.save() {
                    eprintln!("[prism-ui] saving config: {e}");
                }
            }
            Act::Pair(id) => self.cmd(PhoneCommand::Pair { device: Some(id) }),
            Act::Accept(id) => self.cmd(PhoneCommand::Accept { device: Some(id) }),
            Act::Reject(id) => self.cmd(PhoneCommand::Reject { device: Some(id) }),
            Act::NotifAction(id, a) => self.cmd(PhoneCommand::NotificationAction { device: dev, id, action: a }),
            Act::Dismiss(id) => self.cmd(PhoneCommand::NotificationDismiss { device: dev, id }),
            Act::Reply(id) => {
                self.reply = match &self.reply {
                    Some((cur, _)) if *cur == id => None,
                    _ => Some((id, Query::default())),
                };
            }
            Act::SendReply => self.send_reply(),
            Act::CancelTransfer(id) => self.cmd(PhoneCommand::CancelTransfer { id }),
        }
        Effect::Redraw
    }

    fn send_reply(&mut self) {
        let Some((nid, q)) = self.reply.take() else { return };
        let reply_id = self.device().and_then(|d| d.notifications.iter().find(|n| n.id == nid)).map(|n| n.reply_id.clone()).unwrap_or_default();
        if !q.text.trim().is_empty() && !reply_id.is_empty() {
            let dev = self.device().map(|d| d.id.clone());
            self.cmd(PhoneCommand::NotificationReply { device: dev, reply_id, text: q.text.clone() });
        }
    }
}

impl CardView for PhoneView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, _theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        self.hits.clear();
        match self.state.clone() {
            None => self.draw_off(cr, card, style),
            Some(st) => match st.primary().cloned() {
                None => self.draw_pairing(cr, card, style, st),
                Some(d) if self.page == Page::Notifications => self.draw_notifications(cr, card, style, d),
                Some(d) => self.draw_main(cr, card, style, d),
            },
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        let h = self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone());
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        self.pointer = (x, y);
        match self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone()) {
            Some(a) => self.run(a),
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        if self.page != Page::Notifications {
            return false;
        }
        self.scroll.by(delta)
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        if let Some((_, q)) = self.reply.as_mut() {
            match special {
                ESC => {
                    self.reply = None;
                    return Effect::Redraw;
                }
                ENTER => {
                    self.send_reply();
                    return Effect::Redraw;
                }
                _ => {
                    return match q.key(special, typed, ctrl) {
                        Some(_) => Effect::Redraw,
                        None => Effect::None,
                    };
                }
            }
        }
        match special {
            ESC if self.page != Page::Main => {
                self.page = Page::Main;
                Effect::Redraw
            }
            ESC => Effect::Close,
            _ => Effect::None,
        }
    }

    fn poll(&mut self) -> bool {
        let seen = phone_client::generation();
        let playing = self.device().and_then(|d| d.media.as_ref()).is_some_and(|m| m.playing);
        if seen != self.seen {
            self.seen = seen;
            self.state = phone_client::snapshot().1;
            if self.reply.as_ref().is_some_and(|(id, _)| !self.device().is_some_and(|d| d.notifications.iter().any(|n| n.id == *id))) {
                self.reply = None;
            }
            return true;
        }
        playing
    }

    fn wake_in(&self) -> Option<std::time::Duration> {
        let playing = self.device().and_then(|d| d.media.as_ref()).is_some_and(|m| m.playing);
        playing.then(|| std::time::Duration::from_millis(500))
    }

    fn clickable(&self) -> bool {
        self.hover.is_some()
    }
}
