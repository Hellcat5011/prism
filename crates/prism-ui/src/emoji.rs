//! Emoji picker (port of QML EmojiPicker.qml + scripts/paste-emoji.sh).
//!
//!   ┌──┬────────────────────────────────┐
//!   │🕒│ [🔍 Search emojis...        ✕] │
//!   │🌐│ 😀 😃 😄 😁 😆 😅 …           │  grid, 46 px rows
//!   │😀│ …                              │
//!   │..│ [😀 Grinning face · Smileys  hints] │
//!   └──┴────────────────────────────────┘
//!
//! Enter pastes into the window that was focused when the picker opened (copy, wait for that
//! window to get focus back, then Ctrl+V, or Ctrl+Shift+V in terminals); Shift+Enter only
//! copies. Recently used emojis are kept in $XDG_STATE_HOME/prism/emoji-recents.json.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prism_common::emoji::{score_emoji, EmojiItem};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{draw_query, Query, DOWN, ENTER, ESC, LEFT, PGDN, PGUP, RIGHT, UP};
use crate::ui::{Rgba, Style};
use crate::view::{CardView, Effect};

pub const CARD_W: f64 = 740.0;
pub const CARD_H: f64 = 520.0;
const RAIL_W: f64 = 56.0;
const CELL: f64 = 48.0;
const CELL_H: f64 = 46.0;
const GUTTER: f64 = 10.0;
const MAX_RECENTS: usize = 48;
const TAB: u32 = 15;
const EMOJI_FONT: &str = "Noto Color Emoji";

/// (category id, rail label, icon): QML picker.categories.
const CATEGORIES: [(&str, &str, &str); 11] = [
    ("recent", "Recent", "🕒"),
    ("all", "All", "🌐"),
    ("Smileys & Emotion", "Smileys", "😀"),
    ("People & Body", "People", "👋"),
    ("Animals & Nature", "Nature", "🐶"),
    ("Food & Drink", "Food", "🍔"),
    ("Travel & Places", "Travel", "✈️"),
    ("Activities", "Activities", "⚽"),
    ("Objects", "Objects", "💡"),
    ("Symbols", "Symbols", "🔣"),
    ("Flags", "Flags", "🏁"),
];

fn clean(g: &str) -> String {
    g.replace('\u{FE0F}', "")
}

fn recents_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"));
    base.join("prism/emoji-recents.json")
}

fn load_recents() -> Vec<String> {
    std::fs::read_to_string(recents_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn save_recents(list: &[String]) {
    let path = recents_path();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_string(list) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

// ── Paste target ─────────────────────────────────────────────────────────

/// Focused window: (id, app id).
type Target = (String, String);

fn focused_window() -> Option<Target> {
    let json = |cmd: &str, args: &[&str]| -> Option<serde_json::Value> {
        let out = Command::new(cmd).args(args).stderr(Stdio::null()).output().ok()?;
        serde_json::from_slice(&out.stdout).ok()
    };
    let field = |v: &serde_json::Value, keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| v.get(*k).filter(|x| !x.is_null()))
            .map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string()))
            .unwrap_or_default()
    };
    let v = if std::env::var_os("MANGO_INSTANCE_SIGNATURE").is_some() {
        let v = json("mmsg", &["get", "focusing-client"])?;
        (field(&v, &["id"]), field(&v, &["appid", "app_id", "class"]))
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let v = json("hyprctl", &["activewindow", "-j"])?;
        (field(&v, &["address"]), field(&v, &["class", "initialClass"]))
    } else {
        let v = json("niri", &["msg", "--json", "focused-window"])?;
        (field(&v, &["id"]), field(&v, &["app_id"]))
    };
    (!v.0.is_empty()).then_some(v)
}

fn is_terminal(app: &str) -> bool {
    let a = app.to_lowercase();
    ["ghostty", "kitty", "foot", "alacritty", "wezterm", "xterm", "rxvt", "konsole", "gnome-terminal", "ptyxis", "tilix", "terminator"]
        .iter()
        .any(|t| a.contains(t))
}

/// Copy `emoji`; when `paste`, wait (40 ms, then up to 300 ms) for `target` to have focus again
/// and send the paste shortcut to it. Runs on its own thread after the picker has closed.
fn copy_and_paste(emoji: String, target: Option<Target>, paste: bool) {
    std::thread::spawn(move || {
        prism_common::clipboard::touch_self_copy_lock();
        if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(emoji.as_bytes());
            }
            let _ = child.wait();
        }
        let Some((id, app)) = target.filter(|_| paste) else { return };
        let lower = app.to_lowercase();
        if ["nautilus", "thunar", "org.kde.dolphin", "pcmanfm", "nemo"].iter().any(|b| lower.contains(b)) {
            return; // file managers: copy only
        }
        std::thread::sleep(Duration::from_millis(40));
        let deadline = Instant::now() + Duration::from_millis(300);
        loop {
            if focused_window().map(|w| w.0 == id).unwrap_or(false) {
                break;
            }
            if Instant::now() >= deadline {
                eprintln!("[prism-ui] emoji: window {id} did not regain focus; copied only");
                return;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        let args: &[&str] = if is_terminal(&app) {
            &["-M", "ctrl", "-M", "shift", "v", "-m", "shift", "-m", "ctrl"]
        } else {
            &["-M", "ctrl", "v", "-m", "ctrl"]
        };
        if let Err(e) = Command::new("wtype").args(args).status() {
            eprintln!("[prism-ui] emoji: paste failed: {e}");
        }
    });
}

// ── View ─────────────────────────────────────────────────────────────────

pub struct EmojiView {
    all: Vec<EmojiItem>,
    recents: Vec<String>,
    tab: usize,
    query: Query,
    list: Vec<usize>,
    selected: usize,
    scroll: f64,
    cols: usize,
    grid: Rect,
    tabs: Vec<Rect>,
    clear_btn: Rect,
    pointer: (f64, f64),
    last_hover_pos: (f64, f64),
    target: Arc<Mutex<Option<Target>>>,
}

impl EmojiView {
    pub fn new(all: Vec<EmojiItem>) -> Self {
        let target = Arc::new(Mutex::new(None));
        let t = Arc::clone(&target);
        std::thread::spawn(move || *t.lock().unwrap() = focused_window());
        let recents = load_recents();
        let mut v = Self {
            all,
            recents,
            tab: 1,
            query: Query::default(),
            list: Vec::new(),
            selected: 0,
            scroll: 0.0,
            cols: 1,
            grid: Rect::default(),
            tabs: Vec::new(),
            clear_btn: Rect::default(),
            pointer: (-1.0, -1.0),
            last_hover_pos: (-100.0, -100.0),
            target,
        };
        v.tab = if v.recent_items().is_empty() { 1 } else { 0 };
        v.refresh();
        v
    }

    fn recent_items(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for g in &self.recents {
            if let Some(i) = self.all.iter().position(|e| clean(&e.emoji) == *g) {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
            if out.len() >= MAX_RECENTS {
                break;
            }
        }
        out
    }

    fn refresh(&mut self) {
        let q = self.query.text.trim().to_string();
        self.list = if !q.is_empty() {
            let mut scored: Vec<(f64, usize)> = self.all.iter().enumerate().filter_map(|(i, e)| score_emoji(&q, e).filter(|s| *s > 0.0).map(|s| (s, i))).collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.into_iter().map(|(_, i)| i).collect()
        } else {
            match self.tab {
                0 => self.recent_items(),
                1 => (0..self.all.len()).collect(),
                t => (0..self.all.len()).filter(|&i| self.all[i].category == CATEGORIES[t].0).collect(),
            }
        };
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
    }

    fn switch_tab(&mut self, t: usize) {
        self.query = Query::default();
        self.tab = t % CATEGORIES.len();
        self.selected = 0;
        self.scroll = 0.0;
        self.refresh();
    }

    fn move_selection(&mut self, delta: i32) {
        if self.list.is_empty() {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, self.list.len() as i32 - 1) as usize;
        let top = (self.selected / self.cols) as f64 * CELL_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + CELL_H > self.scroll + self.grid.h {
            self.scroll = top + CELL_H - self.grid.h;
        }
    }

    fn select(&mut self, i: usize, paste: bool) -> Effect {
        let Some(&k) = self.list.get(i) else { return Effect::None };
        let emoji = self.all[k].emoji.clone();
        let g = clean(&emoji);
        self.recents.retain(|r| *r != g);
        self.recents.insert(0, g);
        self.recents.truncate(MAX_RECENTS);
        save_recents(&self.recents);
        copy_and_paste(emoji, self.target.lock().unwrap().clone(), paste);
        Effect::Close
    }

    fn cell_at(&self, x: f64, y: f64) -> Option<usize> {
        if !self.grid.contains(x, y) {
            return None;
        }
        let cw = ((self.grid.w) / self.cols as f64).floor();
        let col = ((x - self.grid.x) / cw) as usize;
        let row = ((y - self.grid.y + self.scroll) / CELL_H) as usize;
        let i = row * self.cols + col.min(self.cols - 1);
        (i < self.list.len()).then_some(i)
    }

    fn tab_at(&self, x: f64, y: f64) -> Option<usize> {
        self.tabs.iter().position(|r| r.contains(x, y))
    }
}

fn centered_text(cr: *mut Cairo, text: &str, font: &str, cx: f64, cy: f64, color: Rgba) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 2000, Some(1));
    cb::draw_pango_text(cr, (cx - w as f64 / 2.0).round(), (cy - h as f64 / 2.0).round(), text, font, color, None);
}

impl CardView for EmojiView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let primary = cb::hex_to_rgba(&theme.primary, 1.0);
        let sv = cb::hex_to_rgba(&theme.surface_variant, 1.0);
        let sv_text = cb::hex_to_rgba(&theme.on_surface_variant, 1.0);
        let on_surface = cb::hex_to_rgba(&theme.on_surface, 1.0);
        let outline = cb::hex_to_rgba(&theme.outline_variant, 1.0);
        let a = |c: Rgba, v: f64| (c.0, c.1, c.2, v);
        cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, style.radius, style.card_bg);
        let searching = !self.query.text.trim().is_empty();

        // Category rail (clipped by the card's rounded corners).
        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, style.radius);
            cb::cairo_clip(cr);
        }
        cb::fill_rounded_rect(cr, card.x, card.y, RAIL_W, card.h, 0.0, a(sv, 0.45));
        cb::stroke_rounded_rect(cr, card.x + 0.5, card.y + 0.5, RAIL_W - 1.0, card.h - 1.0, 0.0, a(outline, 0.3), 1.0);
        unsafe { cb::cairo_restore(cr) };
        self.tabs.clear();
        let hovered_tab = self.tab_at(self.pointer.0, self.pointer.1);
        for (i, (_, _, icon)) in CATEGORIES.iter().enumerate() {
            let r = Rect { x: card.x + (RAIL_W - 44.0) / 2.0, y: card.y + 12.0 + i as f64 * 44.0, w: 44.0, h: 40.0 };
            self.tabs.push(r);
            let active = i == self.tab;
            let bg = if active {
                Some(if searching { a(primary, 0.25) } else { primary })
            } else if hovered_tab == Some(i) {
                Some(a(primary, 0.15))
            } else {
                None
            };
            if let Some(bg) = bg {
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 12.0, bg);
            }
            let op = if active { if searching { 0.6 } else { 1.0 } } else { 0.85 };
            unsafe { cb::cairo_push_group(cr) };
            centered_text(cr, icon, &format!("{EMOJI_FONT} 18px"), r.x + r.w / 2.0, r.y + r.h / 2.0, (1.0, 1.0, 1.0, 1.0));
            unsafe {
                cb::cairo_pop_group_to_source(cr);
                cb::cairo_paint_with_alpha(cr, op);
            }
        }

        // Right column.
        let x = card.x + RAIL_W + 14.0;
        let w = card.x + card.w - 14.0 - x;
        let mut y = card.y + 14.0;
        cb::fill_rounded_rect(cr, x, y, w, 46.0, 12.0, sv);
        cb::stroke_rounded_rect(cr, x + 1.0, y + 1.0, w - 2.0, 44.0, 11.0, primary, 2.0);
        let lf = "Inter, Sans 14px";
        let (lw, lh) = cb::measure_pango_text(cr, "🔍", lf, 100, Some(1));
        cb::draw_pango_text(cr, x + 12.0, y + ((46.0 - lh as f64) / 2.0).round(), "🔍", lf, sv_text, None);
        let qx = x + 12.0 + lw as f64 + 8.0;
        let mut qw = x + w - 10.0 - qx;
        self.clear_btn = Rect::default();
        if !self.query.text.is_empty() {
            self.clear_btn = Rect { x: x + w - 10.0 - 26.0, y: y + 10.0, w: 26.0, h: 26.0 };
            if self.clear_btn.contains(self.pointer.0, self.pointer.1) {
                cb::fill_circle(cr, self.clear_btn.x + 13.0, self.clear_btn.y + 13.0, 13.0, cb::hex_to_rgba(&theme.surface, 1.0));
            }
            centered_text(cr, "✕", "Inter, Sans 12px", self.clear_btn.x + 13.0, self.clear_btn.y + 13.0, sv_text);
            qw -= 26.0 + 8.0;
        }
        draw_query(cr, &self.query, "Search emojis...", qx, y + 11.0, qw, 24.0, "Inter, Sans 15px", on_surface, sv_text);
        y += 46.0 + 10.0;

        // Grid.
        let strip_h = 48.0;
        let area = Rect { x, y, w, h: card.y + card.h - 14.0 - strip_h - 10.0 - y };
        let avail = (area.w - GUTTER).max(100.0);
        self.cols = ((avail / CELL) as usize).max(1);
        let cw = (avail / self.cols as f64).floor();
        self.grid = Rect { x: area.x, y: area.y, w: cw * self.cols as f64, h: area.h };
        let rows = self.list.len().div_ceil(self.cols);
        let content = rows as f64 * CELL_H;
        let max = (content - area.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        if self.list.is_empty() {
            let (icon, title, sub) = if searching {
                ("🔍", "No emojis found", "Try searching with broader terms or synonyms.")
            } else if self.tab == 0 {
                ("🕒", "No recent emojis yet", "Selected emojis will appear here for quick access.")
            } else {
                ("", "", "")
            };
            if !title.is_empty() {
                let cx = area.x + area.w / 2.0;
                let cy = area.y + area.h / 2.0;
                centered_text(cr, icon, &format!("{EMOJI_FONT} 34px"), cx, cy - 40.0, sv_text);
                centered_text(cr, title, "Inter, Sans Bold 15px", cx, cy, sv_text);
                centered_text(cr, sub, "Inter, Sans 13px", cx, cy + 24.0, a(sv_text, 0.8));
            }
        } else {
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, area.x, area.y, area.w, area.h);
                cb::cairo_clip(cr);
            }
            let first_row = (self.scroll / CELL_H) as usize;
            let font = format!("{EMOJI_FONT} 24px");
            for row in first_row..rows {
                let ry = area.y + row as f64 * CELL_H - self.scroll;
                if ry > area.y + area.h {
                    break;
                }
                for col in 0..self.cols {
                    let i = row * self.cols + col;
                    let Some(&k) = self.list.get(i) else { break };
                    let cx = area.x + col as f64 * cw;
                    if i == self.selected {
                        cb::fill_rounded_rect(cr, cx + 2.0, ry + 2.0, cw - 4.0, CELL_H - 4.0, 12.0, a(primary, 0.3));
                        cb::stroke_rounded_rect(cr, cx + 2.5, ry + 2.5, cw - 5.0, CELL_H - 5.0, 12.0, primary, 1.0);
                    }
                    centered_text(cr, &self.all[k].emoji, &font, cx + cw / 2.0, ry + CELL_H / 2.0, (1.0, 1.0, 1.0, 1.0));
                }
            }
            unsafe { cb::cairo_restore(cr) };
            if max > 0.0 {
                let track_h = area.h;
                let th = (track_h * area.h / content).max(24.0);
                let ty = area.y + (self.scroll / max) * (track_h - th);
                cb::fill_rounded_rect(cr, area.x + area.w - 6.0, ty, 4.0, th, 2.0, a(style.fg, 0.35));
            }
        }

        // Detail strip.
        let sy = area.y + area.h + 10.0;
        cb::fill_rounded_rect(cr, x, sy, w, strip_h, 12.0, a(sv, 0.5));
        cb::stroke_rounded_rect(cr, x + 0.5, sy + 0.5, w - 1.0, strip_h - 1.0, 12.0, a(outline, 0.3), 1.0);
        // Hint chips, right aligned.
        let hf = "Inter, Sans 10px";
        let mut hx = x + w - 12.0;
        unsafe { cb::cairo_push_group(cr) };
        for hint in ["Esc close", "Shift+Enter copy", "Enter ↵ paste"] {
            let (tw, th) = cb::measure_pango_text(cr, hint, hf, 1000, Some(1));
            let cwid = tw as f64 + 10.0;
            hx -= cwid;
            let cy = sy + (strip_h - 18.0) / 2.0;
            cb::fill_rounded_rect(cr, hx, cy, cwid, 18.0, 4.0, sv);
            cb::draw_pango_text(cr, hx + 5.0, cy + ((18.0 - th as f64) / 2.0).round(), hint, hf, sv_text, None);
            hx -= 8.0;
        }
        unsafe {
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, 0.7);
        }
        if let Some(&k) = self.list.get(self.selected) {
            let e = &self.all[k];
            let ef = format!("{EMOJI_FONT} 28px");
            let (ew, eh) = cb::measure_pango_text(cr, &e.emoji, &ef, 200, Some(1));
            cb::draw_pango_text(cr, x + 12.0, sy + ((strip_h - eh as f64) / 2.0).round(), &e.emoji, &ef, (1.0, 1.0, 1.0, 1.0), None);
            let tx = x + 12.0 + ew as f64 + 12.0;
            let tw = (hx - 4.0 - tx).max(20.0);
            cb::draw_pango_text(cr, tx, sy + 7.0, &e.name, "Inter, Sans Bold 13px", on_surface, Some(tw as i32));
            cb::draw_pango_text(cr, tx, sy + 26.0, &e.category, "Inter, Sans Bold 11px", primary, Some(tw as i32));
        }

        // Tooltip for the hovered tab.
        if let Some(i) = hovered_tab {
            let t = CATEGORIES[i].1;
            let tf = "Inter, Sans Bold 12px";
            let (tw, th) = cb::measure_pango_text(cr, t, tf, 1000, Some(1));
            let r = self.tabs[i];
            let tip = Rect { x: card.x + RAIL_W + 8.0, y: r.y + r.h / 2.0 - (th as f64 + 8.0) / 2.0, w: tw as f64 + 16.0, h: th as f64 + 8.0 };
            cb::fill_rounded_rect(cr, tip.x, tip.y, tip.w, tip.h, 6.0, cb::hex_to_rgba(&theme.surface, 1.0));
            cb::stroke_rounded_rect(cr, tip.x + 0.5, tip.y + 0.5, tip.w - 1.0, tip.h - 1.0, 6.0, outline, 1.0);
            cb::draw_pango_text(cr, tip.x + 8.0, tip.y + 4.0, t, tf, on_surface, None);
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let tab_before = self.tab_at(self.pointer.0, self.pointer.1);
        let clear_before = self.clear_btn.contains(self.pointer.0, self.pointer.1);
        self.pointer = (x, y);
        let mut redraw = tab_before != self.tab_at(x, y) || clear_before != self.clear_btn.contains(x, y);
        // Hover selects after moving at least 3 px (QML), so scrolling under a still pointer
        // does not change the selection.
        let (lx, ly) = self.last_hover_pos;
        if (x - lx).abs() >= 3.0 || (y - ly).abs() >= 3.0 {
            self.last_hover_pos = (x, y);
            if let Some(i) = self.cell_at(x, y) {
                if i != self.selected {
                    self.selected = i;
                    redraw = true;
                }
            }
        }
        redraw
    }

    fn click(&mut self, x: f64, y: f64, shift: bool) -> Effect {
        if let Some(t) = self.tab_at(x, y) {
            self.switch_tab(t);
            return Effect::Redraw;
        }
        if self.clear_btn.contains(x, y) {
            self.query = Query::default();
            self.refresh();
            return Effect::Redraw;
        }
        match self.cell_at(x, y) {
            Some(i) => {
                self.selected = i;
                self.select(i, !shift)
            }
            None => Effect::None,
        }
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.scroll = (self.scroll + delta).max(0.0);
        true
    }

    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        let cols = self.cols as i32;
        match special {
            ESC => {
                if self.query.text.is_empty() {
                    return Effect::Close;
                }
                self.query = Query::default();
                self.refresh();
            }
            ENTER if ctrl => return Effect::None,
            ENTER => return self.select(self.selected, !shift),
            LEFT => self.move_selection(-1),
            RIGHT => self.move_selection(1),
            UP => self.move_selection(-cols),
            DOWN => self.move_selection(cols),
            PGUP => self.move_selection(-cols * 4),
            PGDN => self.move_selection(cols * 4),
            TAB if ctrl && shift => self.switch_tab(self.tab + CATEGORIES.len() - 1),
            TAB if ctrl => self.switch_tab(self.tab + 1),
            _ => match self.query.key(special, typed, ctrl) {
                Some(true) => {
                    self.selected = 0;
                    self.scroll = 0.0;
                    self.refresh();
                }
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }

    /// Alt+1..9 jumps to the first nine category tabs (QML).
    fn alt_key(&mut self, typed: char) -> Option<Effect> {
        let n = typed.to_digit(10).filter(|n| (1..=9).contains(n))?;
        self.switch_tab(n as usize - 1);
        Some(Effect::Redraw)
    }

    fn clickable(&self) -> bool {
        let (x, y) = self.pointer;
        self.tab_at(x, y).is_some() || self.clear_btn.contains(x, y) || self.cell_at(x, y).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_and_variation_selectors() {
        assert!(is_terminal("com.mitchellh.ghostty"));
        assert!(!is_terminal("firefox"));
        assert_eq!(clean("✈️"), "✈");
    }
}
