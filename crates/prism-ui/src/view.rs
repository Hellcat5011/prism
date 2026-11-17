//! Common interface of the self-contained modal card views (launcher, spotlight, power,
//! clipboard, emoji, keybinds), so main.rs routes input and drawing to them the same way.

use prism_common::Theme;

use crate::cairo_backend::Cairo;
use crate::desktop::layout::Rect;
use crate::ui::Style;

pub use crate::launcher::Effect;

pub trait CardView {
    /// Draw into `card` (screen coordinates).
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme);
    /// Pointer moved; true when a redraw is needed.
    fn motion(&mut self, x: f64, y: f64) -> bool;
    /// Left click inside the card.
    fn click(&mut self, x: f64, y: f64, shift: bool) -> Effect;
    /// Wheel; true when a redraw is needed.
    fn scroll(&mut self, _delta: f64) -> bool {
        false
    }
    /// Key press: `special` is a translated key code (0 when `typed` carries text).
    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect;
    /// Alt+key shortcut; `None` passes the key on to `key` as usual.
    fn alt_key(&mut self, _typed: char) -> Option<Effect> {
        None
    }
    /// Background results; true when a redraw is needed.
    fn poll(&mut self) -> bool {
        false
    }
    /// An in-card animation is running.
    fn animating(&self) -> bool {
        false
    }
    /// The pointer is over something clickable (pointer cursor).
    fn clickable(&self) -> bool {
        false
    }
    /// Time until `poll` wants to run again (periodic refresh).
    fn wake_in(&self) -> Option<std::time::Duration> {
        None
    }
    /// Card height when it is content-sized (spotlight); None for the fixed size.
    fn height(&self) -> Option<f64> {
        None
    }
}
