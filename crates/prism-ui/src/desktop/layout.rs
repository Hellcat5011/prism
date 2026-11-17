// ─────────────────────────────────────────────────────────────────────────
// desktop/layout.rs — widget geometry (port of services/DesktopLayout.qml)
//
// Stored values: fractions of the screen in (0, 1] (what edit mode saves, so a
// layout survives resolution changes), or legacy absolute pixels / negative
// offsets from the right/bottom edge (defaults and older configs). Geometry
// is clamped on screen and to per-widget minimum sizes exactly like QML.
// The tray's length is derived from its icon count (computeTrayRect).
// ─────────────────────────────────────────────────────────────────────────

use prism_common::config::{DesktopLayoutConfig, WidgetPosition};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WidgetId {
    Mpris,
    Tray,
    Clock,
    Calendar,
    AnalogClock,
    Shortcuts,
    Phone,
}

impl WidgetId {
    pub const ALL: [WidgetId; 7] =
        [WidgetId::Mpris, WidgetId::Tray, WidgetId::Clock, WidgetId::Calendar, WidgetId::AnalogClock, WidgetId::Shortcuts, WidgetId::Phone];

    pub fn label(self) -> &'static str {
        match self {
            WidgetId::Mpris => "Media Player",
            WidgetId::Tray => "System Tray",
            WidgetId::Clock => "Digital Clock",
            WidgetId::Calendar => "Calendar",
            WidgetId::AnalogClock => "Analog Clock",
            WidgetId::Shortcuts => "Shortcuts",
            WidgetId::Phone => "Phone",
        }
    }

    pub fn min_size(self) -> (f64, f64) {
        match self {
            WidgetId::Mpris => (260.0, 120.0),
            WidgetId::Tray => (40.0, 40.0),
            WidgetId::Clock => (180.0, 90.0),
            WidgetId::Calendar => (240.0, 220.0),
            WidgetId::AnalogClock => (120.0, 120.0),
            WidgetId::Shortcuts => (48.0, 48.0),
            WidgetId::Phone => (240.0, 100.0),
        }
    }

    pub fn cfg(self, l: &DesktopLayoutConfig) -> &WidgetPosition {
        match self {
            WidgetId::Mpris => &l.mpris,
            WidgetId::Tray => &l.tray,
            WidgetId::Clock => &l.clock,
            WidgetId::Calendar => &l.calendar,
            WidgetId::AnalogClock => &l.analogclock,
            WidgetId::Shortcuts => &l.shortcuts,
            WidgetId::Phone => &l.phone,
        }
    }

    pub fn cfg_mut(self, l: &mut DesktopLayoutConfig) -> &mut WidgetPosition {
        match self {
            WidgetId::Mpris => &mut l.mpris,
            WidgetId::Tray => &mut l.tray,
            WidgetId::Clock => &mut l.clock,
            WidgetId::Calendar => &mut l.calendar,
            WidgetId::AnalogClock => &mut l.analogclock,
            WidgetId::Shortcuts => &mut l.shortcuts,
            WidgetId::Phone => &mut l.phone,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// On-screen geometry of a widget (pixels), clamped like DesktopLayout.getWidgetGeometry.
pub fn widget_rect(id: WidgetId, p: &WidgetPosition, sw: f64, sh: f64, tray_count: usize) -> Rect {
    if id == WidgetId::Tray {
        return tray_rect(p, tray_count, sw, sh).0;
    }
    let (x, y, w, h) = p.resolve(sw, sh);
    let (mw, mh) = id.min_size();
    let w = w.round().min(sw).max(mw);
    let h = h.round().min(sh).max(mh);
    let x = x.round().min(sw - w).max(0.0);
    let y = y.round().min(sh - h).max(0.0);
    Rect { x, y, w, h }
}

pub fn shortcuts_vertical(p: &WidgetPosition, r: &Rect) -> bool {
    p.vertical || r.h > r.w * 1.2
}

// ── Tray sizing (computeTrayRect) ─────────────────────────────────────────

pub fn tray_icon_size(t: f64) -> f64 {
    (t * 0.48).round().max(16.0)
}
pub fn tray_spacing(t: f64) -> f64 {
    (t * 0.24).round().max(4.0)
}
pub fn tray_padding(t: f64) -> f64 {
    (t * 0.30).round().max(8.0)
}
pub fn tray_length(count: usize, t: f64) -> f64 {
    let n = count.max(1) as f64;
    n * tray_icon_size(t) + (n - 1.0) * tray_spacing(t) + 2.0 * tray_padding(t)
}

const TRAY_EDGE_SNAP: f64 = 12.0;

/// Returns (rect, vertical, thickness). The tray's length follows its icon count; the stored
/// rectangle supplies thickness, orientation, position and which edge it is pinned to.
pub fn tray_rect(p: &WidgetPosition, count: usize, sw: f64, sh: f64) -> (Rect, bool, f64) {
    let (px, py, pw, ph) = p.resolve(sw, sh);
    let vertical = p.vertical || ph > pw * 1.2;
    let thickness = if vertical { pw } else { ph };
    let (s_len, c_len) = if vertical { (sh, sw) } else { (sw, sh) };
    let min_t = WidgetId::Tray.min_size().1;
    let mut t = thickness.round().min(c_len).max(min_t);
    while t > 16.0 && tray_length(count, t) > s_len {
        t -= 1.0;
    }
    let len = tray_length(count, t);

    let start0 = if vertical { py } else { px };
    let len0 = if vertical { ph } else { pw };
    let cross0 = if vertical { px } else { py };
    let start = if start0 <= TRAY_EDGE_SNAP {
        start0
    } else if s_len - (start0 + len0) <= TRAY_EDGE_SNAP {
        let gap_end = s_len - (start0 + len0);
        s_len - gap_end - len
    } else {
        start0 + len0 / 2.0 - len / 2.0
    };
    let start = start.min(s_len - len).max(0.0).round();
    let cross = cross0.min(c_len - t).max(0.0).round();
    let rect = if vertical {
        Rect { x: cross, y: start, w: t, h: len }
    } else {
        Rect { x: start, y: cross, w: len, h: t }
    };
    (rect, vertical, t)
}

/// Store a rectangle as screen fractions (resolution independent), as QML did.
pub fn store_rect(p: &mut WidgetPosition, r: &Rect, sw: f64, sh: f64) {
    // Keep fractions strictly positive so they are never mistaken for "0 px" / legacy pixels.
    p.x = (r.x / sw).max(1e-6);
    p.y = (r.y / sh).max(1e-6);
    p.w = (r.w / sw).clamp(1e-6, 1.0);
    p.h = (r.h / sh).clamp(1e-6, 1.0);
}
