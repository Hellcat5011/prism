//! Power menu (port of QML PowerMenu.qml): four large buttons, Lock / Logout / Reboot /
//! Shutdown. Arrow keys move, Enter runs; 1-4 or L/E/R/S run directly.

use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{ENTER, ESC, LEFT, RIGHT};
use crate::svg;
use crate::ui::Style;
use crate::view::{CardView, Effect};

pub const CARD_H: f64 = 360.0;
/// Theme.radiusLarge.
const BUTTON_RADIUS: f64 = 24.0;
const ICON: f64 = 128.0;

const BUTTONS: [(&str, &str); 4] = [
    ("Lock", "M7 10h10a3 3 0 0 1 3 3v6a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3v-6a3 3 0 0 1 3-3zM7 10V6a5 5 0 0 1 10 0v4"),
    ("Logout", "M10 22H5a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h5M16 17l5-5-5-5M21 12H9"),
    ("Reboot", "M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8M21 3v5h-5"),
    ("Shutdown", "M18.36 6.64a9 9 0 1 1-12.73 0M12 2v10"),
];

/// Card width for a screen width (QML: 70 % of the screen).
pub fn card_w(sw: f64) -> f64 {
    (sw * 0.70).round()
}

pub struct PowerView {
    selected: Option<usize>,
    buttons: Vec<Rect>,
    hover: Option<usize>,
}

impl PowerView {
    pub fn new() -> Self {
        Self { selected: None, buttons: Vec::new(), hover: None }
    }

    fn run(&self, i: usize) -> Effect {
        std::thread::spawn(move || {
            use prism_common::PowerAction;
            let result = match i {
                0 => prism_common::ipc::request(&prism_common::IpcCommand::Lock(prism_common::ipc::LockAction::Lock)).map(|_| String::new()),
                1 => prism_common::power::execute_power(PowerAction::Logout),
                2 => prism_common::power::execute_power(PowerAction::Reboot),
                _ => prism_common::power::execute_power(PowerAction::Shutdown),
            };
            if let Err(e) = result {
                eprintln!("[prism-ui] power action failed: {e}");
            }
        });
        Effect::Close
    }
}

impl CardView for PowerView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let fg = style.fg;
        let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
        let outline = cb::hex_to_rgba(&theme.outline_variant, 1.0);
        let gap = 32.0;
        let inner = Rect { x: card.x + 36.0, y: card.y + 36.0, w: card.w - 72.0, h: card.h - 72.0 };
        let bw = (inner.w - 3.0 * gap) / 4.0;
        self.buttons.clear();
        for (i, (label, path)) in BUTTONS.iter().enumerate() {
            let r = Rect { x: inner.x + i as f64 * (bw + gap), y: inner.y, w: bw, h: inner.h };
            self.buttons.push(r);
            crate::ui::staggered(cr, i, r, || {
                let on = self.selected == Some(i) || self.hover == Some(i);
                if on {
                    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, BUTTON_RADIUS, (fg.0, fg.1, fg.2, 0.85));
                    cb::stroke_rounded_rect(cr, r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0, BUTTON_RADIUS, inv, 2.0);
                } else {
                    cb::stroke_rounded_rect(cr, r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, BUTTON_RADIUS, outline, 1.0);
                }
                let color = if on { inv } else { fg };
                let font = "Inter, Sans Semi-Bold 22px";
                let (lw, lh) = cb::measure_pango_text(cr, label, font, 1000, Some(1));
                let size = if on { ICON * 1.05 } else { ICON };
                let total = ICON + 16.0 + lh as f64;
                let top = r.y + ((r.h - total) / 2.0).round();
                let icon_x = r.x + (r.w - size) / 2.0;
                let icon_y = top + (ICON - size) / 2.0;
                // stroke-width 1.8 in the 24-unit viewBox
                svg::draw_path(cr, path, icon_x, icon_y, size, color, Some(1.8));
                cb::draw_pango_text(cr, r.x + ((r.w - lw as f64) / 2.0).round(), top + ICON + 16.0, label, font, color, None);
            });
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let h = self.buttons.iter().position(|r| r.contains(x, y));
        let changed = h != self.hover;
        self.hover = h;
        if let Some(i) = h {
            self.selected = Some(i);
        }
        changed
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        match self.buttons.iter().position(|r| r.contains(x, y)) {
            Some(i) => self.run(i),
            None => Effect::None,
        }
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, _ctrl: bool) -> Effect {
        match (special, typed.map(|c| c.to_ascii_lowercase())) {
            (ESC, _) => Effect::Close,
            (LEFT, _) => {
                self.selected = Some(self.selected.map(|i| i.saturating_sub(1)).unwrap_or(0));
                Effect::Redraw
            }
            (RIGHT, _) => {
                self.selected = Some(self.selected.map(|i| (i + 1).min(3)).unwrap_or(0));
                Effect::Redraw
            }
            (ENTER, _) => self.selected.map(|i| self.run(i)).unwrap_or(Effect::None),
            (_, Some('1' | 'l')) => self.run(0),
            (_, Some('2' | 'e')) => self.run(1),
            (_, Some('3' | 'r')) => self.run(2),
            (_, Some('4' | 's')) => self.run(3),
            _ => Effect::None,
        }
    }

    fn clickable(&self) -> bool {
        self.hover.is_some()
    }
}
