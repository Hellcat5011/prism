//! Notes: one plain-text note in `~/.config/prism/notes.txt`, shown by the desktop Notes widget
//! and edited in a card (`prism-ctl notes toggle`, or a click on the widget). The card saves as
//! you type; the widget shows the text from memory (re-read when the file changes on disk).

use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::{Duration, SystemTime};

use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{BACKSPACE, DELETE, DOWN, END, ENTER, ESC, HOME, LEFT, RIGHT, UP};
use crate::ui::Style;
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 560.0;
pub const CARD_H: f64 = 440.0;
const PAD: f64 = 24.0;
const FONT: &str = "Inter, Sans 15px";

/// (generation, text, file time last seen).
static STATE: Mutex<(u64, Option<String>, Option<SystemTime>)> = Mutex::new((0, None, None));
static WATCH: Once = Once::new();

fn path() -> PathBuf {
    prism_common::config::expand_home("~/.config/prism/notes.txt")
}

fn mtime() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// The note (empty when there is none).
pub fn text() -> String {
    let mut s = STATE.lock().unwrap();
    if s.1.is_none() {
        s.1 = Some(std::fs::read_to_string(path()).unwrap_or_default());
        s.2 = mtime();
    }
    s.1.clone().unwrap_or_default()
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

fn save(text: &str) {
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, text);
    let mut s = STATE.lock().unwrap();
    s.0 += 1;
    s.1 = Some(text.to_string());
    s.2 = mtime();
}

/// Notice edits made outside Prism (another editor) every few seconds.
pub fn watch(waker: Waker) {
    WATCH.call_once(|| {
        std::thread::Builder::new()
            .name("prism-notes".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(3));
                let now = mtime();
                let mut s = STATE.lock().unwrap();
                if s.1.is_some() && now != s.2 {
                    s.1 = Some(std::fs::read_to_string(path()).unwrap_or_default());
                    s.2 = now;
                    s.0 += 1;
                    drop(s);
                    waker.wake();
                }
            })
            .ok();
    });
}

pub struct NotesView {
    text: String,
    /// Caret, a byte index on a char boundary.
    caret: usize,
    scroll: f64,
    /// The text area as last drawn, and a click waiting to place the caret.
    area: Rect,
    click_at: Option<(f64, f64)>,
    /// Caret x kept while moving up and down.
    goal_x: Option<f64>,
    /// Up / Down pending: the line above or below the caret.
    vmove: Option<i32>,
    caret_rect: (f64, f64, f64),
}

impl NotesView {
    pub fn new() -> Self {
        let text = text();
        let caret = text.len();
        NotesView { text, caret, scroll: f64::MAX, area: Rect::default(), click_at: None, goal_x: None, vmove: None, caret_rect: (0.0, 0.0, 0.0) }
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map(|(j, _)| j).unwrap_or(0)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }

    fn line_start(&self, i: usize) -> usize {
        self.text[..i].rfind('\n').map(|j| j + 1).unwrap_or(0)
    }

    fn line_end(&self, i: usize) -> usize {
        self.text[i..].find('\n').map(|j| i + j).unwrap_or(self.text.len())
    }

    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.changed();
    }

    fn changed(&mut self) {
        self.goal_x = None;
        save(&self.text);
    }
}

impl Default for NotesView {
    fn default() -> Self {
        Self::new()
    }
}

impl CardView for NotesView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, _theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let fg = style.fg;
        let title = format!("{} Bold 18px", style.font_family);
        cb::draw_pango_text(cr, card.x + PAD, card.y + PAD - 2.0, "Notes", &title, fg, None);
        cb::draw_pango_text(cr, card.x + PAD + 70.0, card.y + PAD + 3.0, "Saved as you type \u{b7} Esc closes", &format!("{} 11px", style.font_family), (fg.0, fg.1, fg.2, 0.55), None);
        let area = Rect { x: card.x + PAD, y: card.y + PAD + 40.0, w: card.w - 2.0 * PAD, h: card.h - 2.0 * PAD - 40.0 };
        self.area = area;
        cb::fill_rounded_rect(cr, area.x - 8.0, area.y - 8.0, area.w + 16.0, area.h + 16.0, 12.0, (fg.0, fg.1, fg.2, 0.06));

        // Layout pass: where the caret is, and what a click or Up/Down points at.
        let font = FONT.replace("Inter, Sans", &style.font_family);
        let (h, caret, _) = cb::edit_text(cr, 0.0, 0.0, &self.text, &font, fg, area.w, self.caret, None, false);
        if let Some(dir) = self.vmove.take() {
            let gx = *self.goal_x.get_or_insert(caret.0);
            let y = if dir < 0 { caret.1 - 2.0 } else { caret.1 + caret.2 + 2.0 };
            if y < 0.0 {
                self.caret = 0;
            } else if y > h {
                self.caret = self.text.len();
            } else if let (_, _, Some(i)) = cb::edit_text(cr, 0.0, 0.0, &self.text, &font, fg, area.w, self.caret, Some((gx, y)), false) {
                self.caret = i;
            }
        }
        if let Some((px, py)) = self.click_at.take() {
            let scroll = if self.scroll == f64::MAX { 0.0 } else { self.scroll };
            if let (_, _, Some(i)) = cb::edit_text(cr, 0.0, 0.0, &self.text, &font, fg, area.w, self.caret, Some((px - area.x, py - area.y + scroll)), false) {
                self.caret = i;
                self.goal_x = None;
            }
        }
        let (_, caret, _) = cb::edit_text(cr, 0.0, 0.0, &self.text, &font, fg, area.w, self.caret, None, false);
        // Keep the caret in view.
        let max = (h - area.h).max(0.0);
        if self.scroll == f64::MAX {
            self.scroll = max;
        }
        if caret.1 < self.scroll {
            self.scroll = caret.1;
        } else if caret.1 + caret.2 > self.scroll + area.h {
            self.scroll = caret.1 + caret.2 - area.h;
        }
        self.scroll = self.scroll.clamp(0.0, max);
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, area.x, area.y, area.w, area.h);
            cb::cairo_clip(cr);
        }
        let top = area.y - self.scroll;
        if self.text.is_empty() {
            cb::draw_pango_text(cr, area.x, top, "Write something\u{2026}", &font, (fg.0, fg.1, fg.2, 0.4), None);
        }
        cb::edit_text(cr, area.x, top, &self.text, &font, fg, area.w, self.caret, None, true);
        if crate::ui::caret_on() {
            cb::fill_rounded_rect(cr, area.x + caret.0, top + caret.1, 1.5, caret.2.max(16.0), 0.0, style.accent);
        }
        unsafe { cb::cairo_restore(cr) };
        self.caret_rect = caret;
    }

    fn motion(&mut self, _x: f64, _y: f64) -> bool {
        false
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        if self.area.contains(x, y) {
            self.click_at = Some((x, y));
            return Effect::Redraw;
        }
        Effect::None
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.scroll = (self.scroll + delta * 40.0).max(0.0);
        true
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        match special {
            ESC => return Effect::Close,
            ENTER => self.insert("\n"),
            BACKSPACE if self.caret > 0 => {
                let p = self.prev(self.caret);
                self.text.replace_range(p..self.caret, "");
                self.caret = p;
                self.changed();
            }
            DELETE if self.caret < self.text.len() => {
                let n = self.next(self.caret);
                self.text.replace_range(self.caret..n, "");
                self.changed();
            }
            LEFT => {
                self.caret = self.prev(self.caret);
                self.goal_x = None;
            }
            RIGHT => {
                self.caret = self.next(self.caret);
                self.goal_x = None;
            }
            HOME => self.caret = if ctrl { 0 } else { self.line_start(self.caret) },
            END => self.caret = if ctrl { self.text.len() } else { self.line_end(self.caret) },
            UP => self.vmove = Some(-1),
            DOWN => self.vmove = Some(1),
            0 => match typed {
                Some('v') | Some('V') if ctrl => {
                    if let Some(t) = prism_common::wlclip::paste_text() {
                        self.insert(&t.replace('\r', ""));
                    }
                }
                Some(c) if !ctrl && !c.is_control() => {
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b));
                }
                _ => return Effect::None,
            },
            _ => return Effect::None,
        }
        Effect::Redraw
    }
}
