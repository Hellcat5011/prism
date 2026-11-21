//! Notes: notes in `~/.config/prism/notes.json` (each with an optional title, its text, and a pin),
//! shown by the desktop Notes widget (one note's text, or a list of them) and edited in a card
//! (`prism-ctl notes toggle`, or a click on the widget). The card saves as you type; the widget
//! reads from memory (re-read when the file changes on disk). Lines starting with `○ ` / `● `
//! are a checklist, shown as round toggles. The single `notes.txt` of earlier versions becomes
//! the first note.

use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::time::{Duration, SystemTime};

use prism_common::Theme;
use serde::{Deserialize, Serialize};

use crate::cairo_backend::{self as cb, Cairo, Span};
use crate::desktop::layout::Rect;
use crate::launcher::{BACKSPACE, DELETE, DOWN, END, ENTER, ESC, HOME, LEFT, RIGHT, UP};
use crate::ui::Style;
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 600.0;
pub const CARD_H: f64 = 480.0;
const PAD: f64 = 24.0;
const FONT: &str = "Inter, Sans 15px";
const TAB: u32 = 0xff09;

/// A checklist line starts with one of these and a space.
pub const UNCHECKED: char = '\u{25CB}';
pub const CHECKED: char = '\u{25CF}';

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
    /// Unix ms of the last edit.
    #[serde(default)]
    pub updated: u64,
}

impl Note {
    /// What the list shows: the title, else the first line with words in it.
    pub fn label(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        self.body.lines().map(strip_marker).map(str::trim).find(|l| !l.is_empty()).unwrap_or("Empty note").to_string()
    }

    fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.body.trim().is_empty()
    }
}

/// A line without its checklist marker.
pub fn strip_marker(line: &str) -> &str {
    line.strip_prefix(UNCHECKED).or_else(|| line.strip_prefix(CHECKED)).map(|r| r.strip_prefix(' ').unwrap_or(r)).unwrap_or(line)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn path() -> PathBuf {
    prism_common::config::expand_home("~/.config/prism/notes.json")
}

fn old_path() -> PathBuf {
    prism_common::config::expand_home("~/.config/prism/notes.txt")
}

fn mtime() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// (generation, notes, file time last seen).
static STATE: Mutex<(u64, Option<Vec<Note>>, Option<SystemTime>)> = Mutex::new((0, None, None));
static WATCH: Once = Once::new();
/// A note to show when the card opens (or at once when it is open): the widget's list.
static OPEN: Mutex<Option<u64>> = Mutex::new(None);

fn read() -> Vec<Note> {
    if let Some(list) = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice::<Vec<Note>>(&b).ok()) {
        return list;
    }
    // The single note of earlier versions.
    match std::fs::read_to_string(old_path()) {
        Ok(text) if !text.trim().is_empty() => vec![Note { id: 1, title: String::new(), body: text, pinned: false, updated: now_ms() }],
        _ => Vec::new(),
    }
}

/// The notes, pinned ones first, then the most recently edited.
pub fn notes() -> Vec<Note> {
    let mut s = STATE.lock().unwrap();
    if s.1.is_none() {
        s.1 = Some(read());
        s.2 = mtime();
    }
    let mut list = s.1.clone().unwrap_or_default();
    list.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated.cmp(&a.updated)));
    list
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

/// Change the stored notes with `f` and write them (empty notes are not kept).
fn update(f: impl FnOnce(&mut Vec<Note>)) {
    let mut list = notes();
    f(&mut list);
    list.retain(|n| !n.is_empty());
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&list) {
        let _ = std::fs::write(&p, json);
    }
    let mut s = STATE.lock().unwrap();
    s.0 += 1;
    s.1 = Some(list);
    s.2 = mtime();
}

/// Store `note` (an empty one is removed).
fn put(note: &Note) {
    update(|list| match list.iter_mut().find(|n| n.id == note.id) {
        Some(n) => *n = note.clone(),
        None => list.push(note.clone()),
    });
}

fn remove(id: u64) {
    update(|list| list.retain(|n| n.id != id));
}

fn set_pinned(id: u64, pinned: bool) {
    update(|list| {
        if let Some(n) = list.iter_mut().find(|n| n.id == id) {
            n.pinned = pinned;
        }
    });
}

/// Show note `id` in the card (it opens with it, or switches to it when open).
pub fn request_open(id: u64) {
    *OPEN.lock().unwrap() = Some(id);
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
                if s.1.is_some() && now.is_some() && now != s.2 {
                    s.1 = Some(read());
                    s.2 = now;
                    s.0 += 1;
                    drop(s);
                    waker.wake();
                }
            })
            .ok();
    });
}

/// What a press placed in a field does once the layout says where it landed.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ClickKind {
    /// A new caret there (a drag grows a selection from it).
    Plain,
    /// The caret moves there, keeping the other end (Shift+click, dragging).
    Extend,
    Word,
    Line,
}

/// One editable text (the title or the body): caret, selection and what is waiting for the
/// next layout (a click, Up/Down).
#[derive(Default)]
struct Field {
    text: String,
    /// Caret, a byte index on a char boundary.
    caret: usize,
    /// The other end of the selection; None or equal to the caret: nothing selected.
    anchor: Option<usize>,
    /// Caret x kept while moving up and down.
    goal_x: Option<f64>,
    /// Up / Down pending: the line above or below the caret.
    vmove: Option<i32>,
    /// A press or drag waiting to be placed (screen point) and what it does.
    click: Option<(f64, f64, ClickKind)>,
    /// How far the text is scrolled (down for the body, right for the title); MAX: to the end.
    scroll: f64,
}

/// What a key did to a field.
#[derive(PartialEq)]
enum Edit {
    None,
    Moved,
    Changed,
}

impl Field {
    fn new(text: &str) -> Field {
        Field { text: text.to_string(), caret: text.len(), scroll: f64::MAX, ..Default::default() }
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

    /// The start of the word before `i` (skipping spaces first), as Ctrl+Left goes.
    fn word_left(&self, i: usize) -> usize {
        let mut j = i;
        while j > 0 && !is_word(self.text[..j].chars().next_back().unwrap_or(' ')) {
            j = self.prev(j);
        }
        while j > 0 && is_word(self.text[..j].chars().next_back().unwrap_or(' ')) {
            j = self.prev(j);
        }
        j
    }

    /// The end of the word after `i` (skipping spaces first), as Ctrl+Right goes.
    fn word_right(&self, i: usize) -> usize {
        let mut j = i;
        while j < self.text.len() && !is_word(self.text[j..].chars().next().unwrap_or(' ')) {
            j = self.next(j);
        }
        while j < self.text.len() && is_word(self.text[j..].chars().next().unwrap_or(' ')) {
            j = self.next(j);
        }
        j
    }

    /// The word (or run of other characters) around `i`.
    fn word_at(&self, i: usize) -> (usize, usize) {
        let kind = |c: char| if is_word(c) { 0 } else if c.is_whitespace() { 1 } else { 2 };
        let Some(here) = self.text[i..].chars().next().or_else(|| self.text[..i].chars().next_back()) else { return (i, i) };
        let k = kind(here);
        let (mut a, mut b) = (i, i);
        while a > 0 && self.text[..a].chars().next_back().is_some_and(|c| kind(c) == k && c != '\n') {
            a = self.prev(a);
        }
        while b < self.text.len() && self.text[b..].chars().next().is_some_and(|c| kind(c) == k && c != '\n') {
            b = self.next(b);
        }
        (a, b)
    }

    /// The selected byte range, when something is selected.
    fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.filter(|a| *a != self.caret).map(|a| (a.min(self.caret), a.max(self.caret)))
    }

    /// Remove the selection; true when there was one.
    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = None;
        true
    }

    fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
        self.goal_x = None;
    }

    /// Move the caret to `to`: with Shift the selection grows from where it was, without it any
    /// selection goes.
    fn move_to(&mut self, to: usize, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
        self.goal_x = None;
    }

    fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
        self.goal_x = None;
    }

    /// Place a waiting click at byte `i`.
    fn place(&mut self, i: usize, kind: ClickKind) {
        match kind {
            ClickKind::Plain => {
                self.caret = i;
                self.anchor = Some(i);
            }
            ClickKind::Extend => {
                self.anchor.get_or_insert(self.caret);
                self.caret = i;
            }
            ClickKind::Word => {
                let (a, b) = self.word_at(i);
                self.anchor = Some(a);
                self.caret = b;
            }
            ClickKind::Line => {
                let (a, b) = (self.line_start(i), self.line_end(i));
                self.anchor = Some(a);
                self.caret = if b < self.text.len() { b + 1 } else { b };
            }
        }
        self.goal_x = None;
    }

    /// Ordinary editing keys (the same for the title and the body).
    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Edit {
        match special {
            BACKSPACE => {
                if !self.delete_selection() {
                    if self.caret == 0 {
                        return Edit::None;
                    }
                    let p = if ctrl { self.word_left(self.caret) } else { self.prev(self.caret) };
                    self.text.replace_range(p..self.caret, "");
                    self.caret = p;
                }
                self.anchor = None;
                Edit::Changed
            }
            DELETE => {
                if !self.delete_selection() {
                    if self.caret >= self.text.len() {
                        return Edit::None;
                    }
                    let n = if ctrl { self.word_right(self.caret) } else { self.next(self.caret) };
                    self.text.replace_range(self.caret..n, "");
                }
                self.anchor = None;
                Edit::Changed
            }
            LEFT => {
                let to = match self.selection() {
                    // Left without Shift on a selection goes to its start.
                    Some((a, _)) if !shift && !ctrl => a,
                    _ if ctrl => self.word_left(self.caret),
                    _ => self.prev(self.caret),
                };
                self.move_to(to, shift);
                Edit::Moved
            }
            RIGHT => {
                let to = match self.selection() {
                    Some((_, b)) if !shift && !ctrl => b,
                    _ if ctrl => self.word_right(self.caret),
                    _ => self.next(self.caret),
                };
                self.move_to(to, shift);
                Edit::Moved
            }
            HOME => {
                let to = if ctrl { 0 } else { self.line_start(self.caret) };
                self.move_to(to, shift);
                Edit::Moved
            }
            END => {
                let to = if ctrl { self.text.len() } else { self.line_end(self.caret) };
                self.move_to(to, shift);
                Edit::Moved
            }
            UP | DOWN => {
                if shift {
                    self.anchor.get_or_insert(self.caret);
                } else {
                    self.anchor = None;
                }
                self.vmove = Some(if special == UP { -1 } else { 1 });
                Edit::Moved
            }
            0 => match typed {
                Some('a') | Some('A') if ctrl => {
                    self.select_all();
                    Edit::Moved
                }
                Some('c') | Some('C') if ctrl => {
                    if let Some((a, b)) = self.selection() {
                        prism_common::wlclip::copy_text(&self.text[a..b], true);
                    }
                    Edit::None
                }
                Some('x') | Some('X') if ctrl => {
                    if let Some((a, b)) = self.selection() {
                        prism_common::wlclip::copy_text(&self.text[a..b], true);
                    }
                    if self.delete_selection() { Edit::Changed } else { Edit::None }
                }
                Some('v') | Some('V') if ctrl => match prism_common::wlclip::paste_text() {
                    Some(t) => {
                        self.insert(&t.replace('\r', ""));
                        Edit::Changed
                    }
                    None => Edit::None,
                },
                Some(c) if !ctrl && !c.is_control() => {
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b));
                    Edit::Changed
                }
                _ => Edit::None,
            },
            _ => Edit::None,
        }
    }

    /// Lay the text out `width` wide (at `origin`, screen, already scrolled) and carry out what is
    /// waiting: a click, a drag, Up/Down. Returns the text's height and the caret (x, y, height)
    /// relative to the text's top-left.
    fn layout(&mut self, cr: *mut Cairo, font: &str, fg: crate::ui::Rgba, width: f64, origin: (f64, f64), area: Rect) -> (f64, (f64, f64, f64)) {
        let (h, caret, _) = cb::edit_text(cr, 0.0, 0.0, &self.text, font, fg, width, self.caret, None, false);
        if let Some(dir) = self.vmove.take() {
            let gx = *self.goal_x.get_or_insert(caret.0);
            let y = if dir < 0 { caret.1 - 2.0 } else { caret.1 + caret.2 + 2.0 };
            if y < 0.0 {
                self.caret = 0;
            } else if y > h {
                self.caret = self.text.len();
            } else if let (_, _, Some(i)) = cb::edit_text(cr, 0.0, 0.0, &self.text, font, fg, width, self.caret, Some((gx, y)), false) {
                self.caret = i;
            }
        }
        if let Some((px, py, kind)) = self.click.take() {
            // Dragging past the edges keeps selecting along them (and scrolls there).
            let (px, py) = (px.clamp(area.x, area.x + area.w), py.clamp(area.y - 1.0, area.y + area.h + 1.0));
            if let (_, _, Some(i)) = cb::edit_text(cr, 0.0, 0.0, &self.text, font, fg, width, self.caret, Some((px - origin.0, py - origin.1)), false) {
                self.place(i, kind);
            }
        }
        let (h, caret, _) = cb::edit_text(cr, 0.0, 0.0, &self.text, font, fg, width, self.caret, None, false);
        (h, caret)
    }
}

/// Part of a word, for word-wise moves and double-click selection.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The checklist marker that line `start..` of `text` begins with.
fn marker_at(text: &str, start: usize) -> Option<char> {
    text[start..].chars().next().filter(|c| *c == UNCHECKED || *c == CHECKED).filter(|_| text[start..].chars().nth(1) == Some(' '))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Button {
    Menu,
    New,
    Checklist,
    Open(u64),
    Pin(u64),
    Delete(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Title,
    Body,
}

pub struct NotesView {
    /// The note being edited (new ones get an id when first shown).
    id: u64,
    pinned: bool,
    title: Field,
    body: Field,
    focus: Focus,
    /// Showing the list of notes instead of one.
    listing: bool,
    list_scroll: f64,
    /// The field a press started in, while the button is down (dragging selects).
    dragging: Option<Focus>,
    /// The last press (time, place), for double and triple clicks, and how many in a row.
    last_press: Option<(std::time::Instant, f64, f64)>,
    presses: u32,
    pointer: (f64, f64),
    /// As last drawn: the title and body areas, the buttons, and the checklist toggles (with the
    /// byte where each marker is).
    title_area: Rect,
    body_area: Rect,
    buttons: Vec<(Rect, Button)>,
    markers: Vec<(Rect, usize)>,
    fades: crate::ui::Fades<Button>,
}

impl NotesView {
    pub fn new() -> Self {
        let mut v = NotesView {
            id: 0,
            pinned: false,
            title: Field::default(),
            body: Field::default(),
            focus: Focus::Body,
            listing: false,
            list_scroll: 0.0,
            dragging: None,
            last_press: None,
            presses: 0,
            pointer: (-1.0, -1.0),
            title_area: Rect::default(),
            body_area: Rect::default(),
            buttons: Vec::new(),
            markers: Vec::new(),
            fades: Default::default(),
        };
        let wanted = OPEN.lock().unwrap().take();
        let list = notes();
        match wanted.and_then(|id| list.iter().find(|n| n.id == id)).or(list.first()) {
            Some(n) => v.show(n.clone()),
            None => v.show_new(),
        }
        v
    }

    fn show(&mut self, n: Note) {
        self.id = n.id;
        self.pinned = n.pinned;
        self.title = Field::new(&n.title);
        self.body = Field::new(&n.body);
        self.focus = if n.title.is_empty() && n.body.is_empty() { Focus::Title } else { Focus::Body };
        self.listing = false;
    }

    fn show_new(&mut self) {
        let id = now_ms().max(notes().iter().map(|n| n.id + 1).max().unwrap_or(0));
        self.show(Note { id, ..Default::default() });
        self.focus = Focus::Title;
    }

    fn field(&mut self) -> &mut Field {
        match self.focus {
            Focus::Title => &mut self.title,
            Focus::Body => &mut self.body,
        }
    }

    /// Store the note as it is now (an empty one is not kept).
    fn save(&mut self) {
        put(&Note { id: self.id, title: self.title.text.replace('\n', " "), body: self.body.text.clone(), pinned: self.pinned, updated: now_ms() });
    }

    /// Turn the lines the caret or selection is on into checklist items, or back when they all are.
    fn toggle_checklist(&mut self) {
        let f = &mut self.body;
        let (a, b) = f.selection().unwrap_or((f.caret, f.caret));
        let first = f.line_start(a);
        let mut starts = vec![first];
        let mut i = first;
        while let Some(j) = f.text[i..].find('\n').map(|j| i + j + 1).filter(|j| *j <= b && *j <= f.text.len()) {
            if j > b || (j == b && b != a) {
                break;
            }
            starts.push(j);
            i = j;
        }
        let all = starts.iter().all(|&s| marker_at(&f.text, s).is_some());
        let (mut caret, mut anchor) = (f.caret as isize, f.anchor.map(|x| x as isize));
        // From the last line up, so earlier byte positions stay put.
        for &s in starts.iter().rev() {
            let delta: isize = if all {
                let len = marker_at(&f.text, s).map_or(0, |c| c.len_utf8() + 1);
                f.text.replace_range(s..s + len, "");
                -(len as isize)
            } else if marker_at(&f.text, s).is_none() {
                let m = format!("{UNCHECKED} ");
                f.text.insert_str(s, &m);
                m.len() as isize
            } else {
                0
            };
            let shift = |p: isize| if p >= s as isize { (p + delta).max(s as isize) } else { p };
            caret = shift(caret);
            anchor = anchor.map(shift);
        }
        f.caret = caret.max(0) as usize;
        f.anchor = anchor.map(|x| x.max(0) as usize);
        self.focus = Focus::Body;
        self.save();
    }

    /// Enter in the body: a new line, continuing a checklist (an empty item ends it instead).
    fn enter(&mut self) {
        let f = &mut self.body;
        f.delete_selection();
        let start = f.line_start(f.caret);
        match marker_at(&f.text, start) {
            Some(m) if f.text[start + m.len_utf8() + 1..f.line_end(f.caret)].trim().is_empty() => {
                // An empty item: the list ends here.
                f.text.replace_range(start..start + m.len_utf8() + 1, "");
                f.caret = start;
            }
            Some(_) => f.insert(&format!("\n{UNCHECKED} ")),
            None => f.insert("\n"),
        }
    }

    /// Tick or untick the checklist item whose marker is at byte `i`.
    fn toggle_item(&mut self, i: usize) {
        let Some(m) = marker_at(&self.body.text, i) else { return };
        let other = if m == UNCHECKED { CHECKED } else { UNCHECKED };
        // Both markers are three bytes: positions after it do not move.
        self.body.text.replace_range(i..i + m.len_utf8(), other.encode_utf8(&mut [0u8; 4]));
        self.save();
    }

    fn button_at(&self, x: f64, y: f64) -> Option<Button> {
        // Later buttons lie on top (a row's pin and delete over the row itself).
        self.buttons.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, b)| *b)
    }

    fn press(&mut self, b: Button) {
        match b {
            Button::Menu => {
                self.listing = !self.listing;
                self.list_scroll = 0.0;
            }
            Button::New => self.show_new(),
            Button::Checklist => self.toggle_checklist(),
            Button::Open(id) => {
                if let Some(n) = notes().into_iter().find(|n| n.id == id) {
                    self.show(n);
                }
            }
            Button::Pin(id) => {
                let pin = !notes().iter().any(|n| n.id == id && n.pinned);
                set_pinned(id, pin);
                if id == self.id {
                    self.pinned = pin;
                }
            }
            Button::Delete(id) => {
                remove(id);
                if id == self.id {
                    // The note shown is gone: show what is left, or a fresh one.
                    match notes().into_iter().next() {
                        Some(n) => {
                            self.show(n);
                            self.listing = true;
                        }
                        None => {
                            self.show_new();
                            self.listing = true;
                        }
                    }
                }
            }
        }
    }

    /// A round button with `icon`, hover-lit, recorded for clicks.
    fn icon_button(&mut self, cr: *mut Cairo, r: Rect, b: Button, icon: &str, color: crate::ui::Rgba, style: &Style) {
        let hover = self.fades.level(b, r.contains(self.pointer.0, self.pointer.1), style);
        let fg = style.fg;
        if hover > 0.01 {
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, (fg.0, fg.1, fg.2, 0.10 * hover));
        }
        let s = 18.0;
        crate::svg::icon(cr, icon, r.x + (r.w - s) / 2.0, r.y + (r.h - s) / 2.0, s, color, false);
        self.buttons.push((r, b));
    }

    fn draw_editor(&mut self, cr: *mut Cairo, card: Rect, style: &Style) {
        let fg = style.fg;
        let accent = style.accent;
        let select = (accent.0, accent.1, accent.2, 0.38);
        let family = &style.font_family;

        // Title: one line, scrolled sideways to keep the caret in view.
        let title_font = format!("{family} Bold 20px");
        let ta = Rect { x: card.x + PAD, y: card.y + PAD + 44.0, w: card.w - 2.0 * PAD, h: 30.0 };
        self.title_area = ta;
        let tscroll = if self.title.scroll == f64::MAX { 0.0 } else { self.title.scroll };
        let (_, tcaret) = self.title.layout(cr, &title_font, fg, 100_000.0, (ta.x - tscroll, ta.y), ta);
        let mut tscroll = tscroll;
        if tcaret.0 - tscroll > ta.w - 4.0 {
            tscroll = tcaret.0 - ta.w + 4.0;
        } else if tcaret.0 < tscroll {
            tscroll = tcaret.0;
        }
        self.title.scroll = tscroll.max(0.0);
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, ta.x - 2.0, ta.y - 2.0, ta.w + 4.0, ta.h + 4.0);
            cb::cairo_clip(cr);
        }
        let tx = ta.x - self.title.scroll;
        if self.title.text.is_empty() {
            cb::draw_pango_text(cr, ta.x, ta.y, "Title", &title_font, (fg.0, fg.1, fg.2, 0.35), None);
        }
        let spans: Vec<(usize, usize, Span)> = self.title.selection().filter(|_| self.focus == Focus::Title).map(|(a, b)| (a, b, Span::Selected(select))).into_iter().collect();
        cb::edit_text_styled(cr, tx, ta.y, &self.title.text, &title_font, fg, 100_000.0, self.title.caret, None, true, &spans);
        if self.focus == Focus::Title && crate::ui::caret_on() {
            cb::fill_rounded_rect(cr, tx + tcaret.0, ta.y + tcaret.1, 1.5, tcaret.2.max(18.0), 0.0, accent);
        }
        unsafe { cb::cairo_restore(cr) };
        cb::fill_rounded_rect(cr, ta.x, ta.y + ta.h + 6.0, ta.w, 1.0, 0.0, (fg.0, fg.1, fg.2, 0.12));

        // Body.
        let font = FONT.replace("Inter, Sans", family);
        let area = Rect { x: card.x + PAD, y: ta.y + ta.h + 20.0, w: card.w - 2.0 * PAD, h: card.y + card.h - PAD - (ta.y + ta.h + 20.0) };
        self.body_area = area;
        let scroll = if self.body.scroll == f64::MAX { 0.0 } else { self.body.scroll };
        let (h, caret) = self.body.layout(cr, &font, fg, area.w, (area.x, area.y - scroll), area);
        // Keep the caret in view (when it is the body being edited).
        let max = (h - area.h).max(0.0);
        let mut scroll = if self.body.scroll == f64::MAX { max } else { self.body.scroll };
        if self.focus == Focus::Body {
            if caret.1 < scroll {
                scroll = caret.1;
            } else if caret.1 + caret.2 > scroll + area.h {
                scroll = caret.1 + caret.2 - area.h;
            }
        }
        self.body.scroll = scroll.clamp(0.0, max);
        let top = area.y - self.body.scroll;

        // Checklist items: the marker is hidden and a round toggle drawn over it; done items fade.
        let mut spans: Vec<(usize, usize, Span)> = Vec::new();
        let mut marks: Vec<(usize, bool)> = Vec::new();
        let mut start = 0;
        loop {
            let end = self.body.text[start..].find('\n').map_or(self.body.text.len(), |j| start + j);
            if let Some(m) = marker_at(&self.body.text, start) {
                spans.push((start, start + m.len_utf8(), Span::Hidden));
                if m == CHECKED {
                    spans.push((start + m.len_utf8() + 1, end, Span::Done(0.45)));
                }
                marks.push((start, m == CHECKED));
            }
            if end >= self.body.text.len() {
                break;
            }
            start = end + 1;
        }
        if let Some((a, b)) = self.body.selection().filter(|_| self.focus == Focus::Body) {
            spans.push((a, b, Span::Selected(select)));
        }
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, area.x - 2.0, area.y, area.w + 4.0, area.h);
            cb::cairo_clip(cr);
        }
        if self.body.text.is_empty() {
            cb::draw_pango_text(cr, area.x, top, "Write something\u{2026}", &font, (fg.0, fg.1, fg.2, 0.4), None);
        }
        cb::edit_text_styled(cr, area.x, top, &self.body.text, &font, fg, area.w, self.body.caret, None, true, &spans);
        self.markers.clear();
        let positions = cb::text_positions(cr, &self.body.text, &font, area.w, &marks.iter().map(|m| m.0).collect::<Vec<_>>());
        for (&(i, done), &(x, y, lh)) in marks.iter().zip(&positions) {
            let (cx, cy, r) = (area.x + x + 7.0, top + y + lh / 2.0, 6.5);
            if cy + r < area.y || cy - r > area.y + area.h {
                continue;
            }
            unsafe {
                cb::cairo_new_path(cr);
                cb::cairo_arc(cr, cx, cy, r, 0.0, std::f64::consts::TAU);
                let c = if done { accent } else { (fg.0, fg.1, fg.2, 0.65) };
                cb::cairo_set_source_rgba(cr, c.0, c.1, c.2, c.3);
                cb::cairo_set_line_width(cr, 1.6);
                cb::cairo_stroke(cr);
            }
            if done {
                cb::fill_circle(cr, cx, cy, 3.4, accent);
            }
            self.markers.push((Rect { x: cx - 10.0, y: cy - 10.0, w: 20.0, h: 20.0 }, i));
        }
        if self.focus == Focus::Body && crate::ui::caret_on() {
            cb::fill_rounded_rect(cr, area.x + caret.0, top + caret.1, 1.5, caret.2.max(16.0), 0.0, accent);
        }
        unsafe { cb::cairo_restore(cr) };
    }

    fn draw_list(&mut self, cr: *mut Cairo, card: Rect, style: &Style) {
        let fg = style.fg;
        let family = &style.font_family;
        let area = Rect { x: card.x + PAD - 8.0, y: card.y + PAD + 44.0, w: card.w - 2.0 * PAD + 16.0, h: card.h - 2.0 * PAD - 44.0 };
        let list = notes();
        if list.is_empty() {
            cb::draw_pango_text(cr, area.x + 8.0, area.y + 8.0, "No notes yet. + starts one.", &format!("{family} 14px"), (fg.0, fg.1, fg.2, 0.55), None);
            return;
        }
        const ROW: f64 = 56.0;
        let max = (list.len() as f64 * ROW - area.h).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, max);
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, area.x, area.y, area.w, area.h);
            cb::cairo_clip(cr);
        }
        for (k, n) in list.iter().enumerate() {
            let r = Rect { x: area.x, y: area.y + k as f64 * ROW - self.list_scroll, w: area.w, h: ROW - 6.0 };
            if r.y + r.h < area.y || r.y > area.y + area.h {
                continue;
            }
            let open = Button::Open(n.id);
            let hover = self.fades.level(open, r.contains(self.pointer.0, self.pointer.1) && area.contains(self.pointer.0, self.pointer.1), style);
            let current = n.id == self.id;
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 12.0, (fg.0, fg.1, fg.2, 0.05 + 0.06 * hover + if current { 0.06 } else { 0.0 }));
            self.buttons.push((r, open));
            let text_w = (r.w - 110.0) as i32;
            cb::draw_pango_text(cr, r.x + 14.0, r.y + 8.0, &n.label(), &format!("{family} Bold 14px"), fg, Some(text_w));
            // The second line: the text after the label's line, or when it was written.
            let preview: String = if n.title.trim().is_empty() {
                n.body.lines().map(strip_marker).map(str::trim).filter(|l| !l.is_empty()).nth(1).unwrap_or("").to_string()
            } else {
                n.body.lines().map(strip_marker).map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
            };
            cb::draw_pango_text(cr, r.x + 14.0, r.y + 28.0, &preview, &format!("{family} 12px"), (fg.0, fg.1, fg.2, 0.55), Some(text_w));
            let pin = if n.pinned { style.accent } else { (fg.0, fg.1, fg.2, 0.5) };
            self.icon_button(cr, Rect { x: r.x + r.w - 84.0, y: r.y + (r.h - 34.0) / 2.0, w: 34.0, h: 34.0 }, Button::Pin(n.id), "pin", pin, style);
            self.icon_button(cr, Rect { x: r.x + r.w - 44.0, y: r.y + (r.h - 34.0) / 2.0, w: 34.0, h: 34.0 }, Button::Delete(n.id), "trash", (fg.0, fg.1, fg.2, 0.6), style);
        }
        unsafe { cb::cairo_restore(cr) };
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
        self.buttons.clear();
        // Header: the list button, "Notes", and on the right a checklist button and a new note.
        let y = card.y + PAD - 6.0;
        let menu_color = if self.listing { style.accent } else { fg };
        self.icon_button(cr, Rect { x: card.x + PAD - 8.0, y, w: 34.0, h: 34.0 }, Button::Menu, "menu", menu_color, style);
        let head = if self.listing { "All Notes" } else { "Notes" };
        cb::draw_pango_text(cr, card.x + PAD + 34.0, y + 5.0, head, &format!("{} Bold 18px", style.font_family), fg, None);
        cb::draw_pango_text(cr, card.x + PAD + 34.0 + if self.listing { 96.0 } else { 64.0 }, y + 10.0, "Saved as you type \u{b7} Esc closes", &format!("{} 11px", style.font_family), (fg.0, fg.1, fg.2, 0.5), None);
        let right = card.x + card.w - PAD + 8.0;
        self.icon_button(cr, Rect { x: right - 34.0, y, w: 34.0, h: 34.0 }, Button::New, "plus", fg, style);
        if !self.listing {
            self.icon_button(cr, Rect { x: right - 74.0, y, w: 34.0, h: 34.0 }, Button::Checklist, "checklist", fg, style);
            self.draw_editor(cr, card, style);
        } else {
            self.draw_list(cr, card, style);
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        if let Some(f) = self.dragging {
            match f {
                Focus::Title => self.title.click = Some((x, y, ClickKind::Extend)),
                Focus::Body => self.body.click = Some((x, y, ClickKind::Extend)),
            }
        }
        // Redraw for hover changes and drags.
        true
    }

    fn click(&mut self, x: f64, y: f64, shift: bool) -> Effect {
        if let Some(b) = self.button_at(x, y) {
            self.press(b);
            return Effect::Redraw;
        }
        if self.listing {
            return Effect::None;
        }
        if let Some(&(_, i)) = self.markers.iter().find(|(r, _)| r.contains(x, y)) {
            self.toggle_item(i);
            return Effect::Redraw;
        }
        let target = if self.title_area.contains(x, y) {
            Focus::Title
        } else if self.body_area.contains(x, y) {
            Focus::Body
        } else {
            return Effect::None;
        };
        if target != self.focus {
            self.field().anchor = None;
            self.focus = target;
        }
        // A second or third press close by, soon after the last: a word, then the whole line.
        let now = std::time::Instant::now();
        let again = self.last_press.is_some_and(|(t, px, py)| now.duration_since(t) < Duration::from_millis(400) && (px - x).abs() < 6.0 && (py - y).abs() < 6.0);
        self.presses = if again { self.presses % 3 + 1 } else { 1 };
        self.last_press = Some((now, x, y));
        let kind = match (shift, self.presses) {
            (true, _) => ClickKind::Extend,
            (_, 2) => ClickKind::Word,
            (_, 3) => ClickKind::Line,
            _ => ClickKind::Plain,
        };
        self.field().click = Some((x, y, kind));
        self.dragging = (self.presses == 1).then_some(target);
        Effect::Redraw
    }

    fn release(&mut self) -> Effect {
        self.dragging = None;
        Effect::None
    }

    fn scroll(&mut self, delta: f64) -> bool {
        if self.listing {
            self.list_scroll = (self.list_scroll + delta * 40.0).max(0.0);
        } else {
            self.body.scroll = (if self.body.scroll == f64::MAX { 0.0 } else { self.body.scroll } + delta * 40.0).max(0.0);
        }
        true
    }

    fn poll(&mut self) -> bool {
        // The widget asked for a note while the card is open.
        if let Some(id) = OPEN.lock().unwrap().take() {
            if let Some(n) = notes().into_iter().find(|n| n.id == id) {
                self.show(n);
                return true;
            }
        }
        false
    }

    fn clickable(&self) -> bool {
        self.button_at(self.pointer.0, self.pointer.1).is_some() || self.markers.iter().any(|(r, _)| r.contains(self.pointer.0, self.pointer.1))
    }

    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        if special == ESC {
            return Effect::Close;
        }
        if ctrl && matches!(typed, Some('n') | Some('N')) {
            self.show_new();
            return Effect::Redraw;
        }
        if self.listing {
            return Effect::None;
        }
        if ctrl && matches!(typed, Some('l') | Some('L')) && self.focus == Focus::Body {
            self.toggle_checklist();
            return Effect::Redraw;
        }
        match (special, self.focus) {
            (TAB, _) => {
                self.field().anchor = None;
                self.focus = if self.focus == Focus::Title { Focus::Body } else { Focus::Title };
                return Effect::Redraw;
            }
            (ENTER, Focus::Title) | (DOWN, Focus::Title) if !shift => {
                self.title.anchor = None;
                self.focus = Focus::Body;
                return Effect::Redraw;
            }
            (ENTER, Focus::Body) => {
                self.enter();
                self.save();
                return Effect::Redraw;
            }
            (UP, Focus::Body) if !shift && self.body.line_start(self.body.caret) == 0 => {
                self.body.anchor = None;
                self.focus = Focus::Title;
                return Effect::Redraw;
            }
            _ => {}
        }
        match self.field().key(special, typed, shift, ctrl) {
            Edit::None => Effect::None,
            Edit::Moved => Effect::Redraw,
            Edit::Changed => {
                self.save();
                Effect::Redraw
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_moves_select_and_typing_replaces_the_selection() {
        let mut f = Field::new("hello world");
        f.caret = 0;
        f.move_to(f.word_right(0), true);
        assert_eq!(f.selection(), Some((0, 5)));
        f.move_to(f.next(f.caret), true);
        assert_eq!(f.selection(), Some((0, 6)));
        f.insert("big ");
        assert_eq!(f.text, "big world");
        assert_eq!(f.selection(), None);
    }

    #[test]
    fn words_and_lines_are_found_around_a_point() {
        let f = Field::new("one two_three, four\nnext line");
        assert_eq!(f.word_at(6), (4, 13));
        assert_eq!(f.word_left(13), 4);
        assert_eq!(f.word_right(4), 13);
        assert_eq!((f.line_start(22), f.line_end(22)), (20, 29));
    }

    #[test]
    fn a_note_is_listed_by_its_title_or_first_line() {
        let n = |title: &str, body: &str| Note { id: 1, title: title.into(), body: body.into(), ..Default::default() };
        assert_eq!(n("Shopping", "milk").label(), "Shopping");
        assert_eq!(n(" ", "\n\u{25CB} milk\n\u{25CF} eggs").label(), "milk");
        assert_eq!(n("", "").label(), "Empty note");
        assert!(n(" ", "\n ").is_empty());
    }

    fn view(body: &str) -> NotesView {
        let mut v = NotesView { id: 1, pinned: false, title: Field::default(), body: Field::new(body), focus: Focus::Body, listing: false, list_scroll: 0.0, dragging: None, last_press: None, presses: 0, pointer: (0.0, 0.0), title_area: Rect::default(), body_area: Rect::default(), buttons: Vec::new(), markers: Vec::new(), fades: Default::default() };
        v.body.caret = v.body.text.len();
        v
    }

    #[test]
    fn checklist_lines_are_marked_continued_and_ended() {
        // Only the body editing is checked: `enter` and the marker scan, not saving.
        let mut v = view("milk");
        let f = &mut v.body;
        f.text.insert_str(0, &format!("{UNCHECKED} "));
        f.caret = f.text.len();
        v.enter();
        assert_eq!(v.body.text, format!("{UNCHECKED} milk\n{UNCHECKED} "));
        // Enter on the empty item ends the list.
        v.enter();
        assert_eq!(v.body.text, format!("{UNCHECKED} milk\n"));
        assert_eq!(marker_at(&v.body.text, 0), Some(UNCHECKED));
        assert_eq!(strip_marker(&format!("{CHECKED} eggs")), "eggs");
    }
}
