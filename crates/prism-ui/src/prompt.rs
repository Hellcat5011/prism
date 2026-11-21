//! Authentication / secret prompt card, shared by the polkit agent (`polkit_agent.rs`) and the
//! NetworkManager secret agent (`nm_agent.rs`), which replace polkit-gnome and nm-applet.
//!
//! An agent thread calls [`ask`]: the request is queued, the run loop opens a `UiMode::Prompt`
//! overlay for it, and the agent reads the answers. After checking them it sends a [`Verdict`]:
//! `Done` closes the card, `Retry` clears the secrets and shows the message (wrong password),
//! `Cancel` closes it from the agent's side. Closing the card (Esc, click outside) answers
//! `None`.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, Align, TextStyle};
use crate::launcher::{Query, DOWN, ENTER, ESC, UP};
use crate::ui::Style;
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 460.0;
const PAD: f64 = 28.0;
const FIELD_H: f64 = 40.0;
const LABEL_H: f64 = 20.0;
const ROW_GAP: f64 = 12.0;
const BUTTON_H: f64 = 38.0;
const TAB: u32 = 15;
const CHOICE_H: f64 = 44.0;
/// Rows shown at once; the list scrolls past this.
const CHOICE_ROWS: usize = 6;

pub struct Field {
    pub label: String,
    pub secret: bool,
    /// Initial value (e.g. a stored user name).
    pub value: String,
}

pub struct Request {
    pub title: String,
    pub message: String,
    /// Smaller text under the message (action id, network name, ...).
    pub detail: String,
    /// svg.rs icon name.
    pub icon: &'static str,
    pub fields: Vec<Field>,
    pub ok_label: String,
    /// A list to pick one entry from (the screen-share picker); the answer is its index.
    pub choices: Vec<Choice>,
}

pub struct Choice {
    pub title: String,
    pub detail: String,
    /// svg.rs icon name.
    pub icon: &'static str,
}

pub enum Verdict {
    Done,
    Retry(String),
    Cancel,
}

/// The agent's side of an open prompt.
pub struct Session {
    /// The user's answers (one per field), or None when the prompt was dismissed.
    pub answers: Receiver<Option<Vec<String>>>,
    verdict: Sender<Verdict>,
    waker: Waker,
}

impl Session {
    pub fn verdict(&self, v: Verdict) {
        let _ = self.verdict.send(v);
        self.waker.wake();
    }
}

/// Closes a prompt from another thread (the caller gave up).
#[derive(Clone)]
pub struct Canceller {
    verdict: Sender<Verdict>,
    waker: Waker,
}

impl Canceller {
    pub fn cancel(&self) {
        let _ = self.verdict.send(Verdict::Cancel);
        self.waker.wake();
    }
}

impl Session {
    pub fn canceller(&self) -> Canceller {
        Canceller { verdict: self.verdict.clone(), waker: self.waker.clone() }
    }
}

struct Pending {
    req: Request,
    answers: Sender<Option<Vec<String>>>,
    verdict: Receiver<Verdict>,
}

static QUEUE: Mutex<VecDeque<Pending>> = Mutex::new(VecDeque::new());
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// Called once by the daemon: prompts can be shown from now on.
pub fn init(waker: Waker) {
    *WAKER.lock().unwrap() = Some(waker);
}

/// Queue a prompt; None when no prism-ui daemon loop runs prompts.
pub fn ask(req: Request) -> Option<Session> {
    let waker = WAKER.lock().unwrap().clone()?;
    let (a_tx, a_rx) = channel();
    let (v_tx, v_rx) = channel();
    QUEUE.lock().unwrap().push_back(Pending { req, answers: a_tx, verdict: v_rx });
    waker.wake();
    let waker = waker.clone();
    Some(Session { answers: a_rx, verdict: v_tx, waker })
}

/// A prompt is waiting to be shown.
pub fn pending() -> bool {
    !QUEUE.lock().unwrap().is_empty()
}

/// The view for the next queued prompt.
pub fn next_view() -> Option<PromptView> {
    let p = QUEUE.lock().unwrap().pop_front()?;
    let values = p
        .req
        .fields
        .iter()
        .map(|f| {
            let mut q = Query::with_text(&f.value);
            if f.secret {
                // Room for any password up front, so typing does not reallocate (and leave copies).
                q.text.reserve(512);
            }
            q
        })
        .collect();
    let focus = p.req.fields.iter().position(|f| f.value.is_empty()).unwrap_or(0);
    Some(PromptView { req: p.req, values, focus, answers: Some(p.answers), verdict: p.verdict, busy: false, error: None, hover: None, fields: Vec::new(), buttons: [Rect::default(); 2], close: false, selected: 0, first: 0, rows: Vec::new(), palette: None })
}

pub struct PromptView {
    req: Request,
    values: Vec<Query>,
    focus: usize,
    answers: Option<Sender<Option<Vec<String>>>>,
    verdict: Receiver<Verdict>,
    /// Answers sent, waiting for the agent's verdict.
    busy: bool,
    error: Option<String>,
    hover: Option<usize>,
    fields: Vec<Rect>,
    /// Cancel, OK.
    buttons: [Rect; 2],
    close: bool,
    /// Choice list: the selected entry, the first one shown, and the shown rows.
    selected: usize,
    first: usize,
    rows: Vec<(usize, Rect)>,
    /// Palette of the last draw (clicks in a field measure its text).
    palette: Option<Palette>,
}

impl PromptView {
    /// Card height for its fields.
    pub fn card_h(&self) -> f64 {
        let n = self.req.fields.len() as f64;
        let list = self.req.choices.len().min(CHOICE_ROWS) as f64 * CHOICE_H;
        PAD + 40.0 + 12.0 + 44.0 + if self.req.detail.is_empty() { 0.0 } else { 22.0 } + 16.0 + n * (LABEL_H + FIELD_H + ROW_GAP) + list + 22.0 + 12.0 + BUTTON_H + PAD
    }

    /// Select choice `i` and keep it in view.
    fn select(&mut self, i: usize) {
        let n = self.req.choices.len();
        if n == 0 {
            return;
        }
        self.selected = i.min(n - 1);
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + CHOICE_ROWS {
            self.first = self.selected + 1 - CHOICE_ROWS;
        }
    }

    fn submit(&mut self) -> Effect {
        if self.busy {
            return Effect::None;
        }
        if let Some(tx) = &self.answers {
            let vals = if self.req.choices.is_empty() { self.values.iter().map(|q| q.text.clone()).collect() } else { vec![self.selected.to_string()] };
            if tx.send(Some(vals)).is_err() {
                return Effect::Close;
            }
            self.busy = true;
            self.error = None;
        }
        Effect::Redraw
    }

    fn cancel(&mut self) -> Effect {
        if let Some(tx) = self.answers.take() {
            let _ = tx.send(None);
        }
        Effect::Close
    }
}

impl PromptView {
    fn wipe_secrets(&mut self) {
        for (q, f) in self.values.iter_mut().zip(&self.req.fields) {
            if f.secret {
                prism_common::secret::wipe(&mut q.text);
                *q = Query::default();
            }
        }
    }
}

impl Drop for PromptView {
    fn drop(&mut self) {
        if let Some(tx) = self.answers.take() {
            let _ = tx.send(None);
        }
        self.wipe_secrets();
    }
}

impl CardView for PromptView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let p = Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: cb::hex_to_rgba(&theme.on_primary, 1.0),
            outline: cb::hex_to_rgba(&theme.outline_variant, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        };
        self.palette = Some(p.clone());
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;
        // Icon and title.
        cb::stroke_circle(cr, x + 20.0, y + 20.0, 19.5, p.fg, 1.0);
        crate::svg::icon(cr, self.req.icon, x + 9.0, y + 9.0, 22.0, p.fg, false);
        let bold = format!("{} Bold", style.font_family);
        draw_in_box(cr, &self.req.title, &TextStyle { family: &bold, px: 18.0, bold: false, color: p.fg }, x + 54.0, y, w - 54.0, 40.0, Align::Start);
        y += 40.0 + 12.0;
        let body = TextStyle { family: &p.family, px: 13.0, bold: false, color: p.fg };
        cb::draw_pango_text_wrapped(cr, x, y, &self.req.message, &format!("{} 13px", p.family), p.fg, w as i32, Some(2));
        y += 44.0;
        if !self.req.detail.is_empty() {
            draw_in_box(cr, &self.req.detail, &TextStyle { color: (p.fg.0, p.fg.1, p.fg.2, 0.6), px: 11.5, ..body }, x, y, w, 18.0, Align::Start);
            y += 22.0;
        }
        y += 16.0;
        self.fields.clear();
        for (i, f) in self.req.fields.iter().enumerate() {
            draw_in_box(cr, &f.label, &TextStyle { color: (p.fg.0, p.fg.1, p.fg.2, 0.75), px: 12.0, ..body }, x + 4.0, y, w, LABEL_H, Align::Start);
            y += LABEL_H;
            let r = Rect { x, y, w, h: FIELD_H };
            let text = if f.secret { "\u{2022}".repeat(self.values[i].text.chars().count()) } else { self.values[i].text.clone() };
            let hover = if self.hover == Some(10 + i) { 1.0 } else { 0.0 };
            uikit::text_field(cr, r, &text, "", (i == self.focus && !self.busy).then(|| self.values[i].caret()), hover, self.error.is_some() && f.secret, &p);
            self.fields.push(r);
            y += FIELD_H + ROW_GAP;
        }
        self.rows.clear();
        let shown = self.req.choices.len().min(CHOICE_ROWS);
        for (k, i) in (self.first..self.first + shown).enumerate() {
            let c = &self.req.choices[i];
            let r = Rect { x, y: y + k as f64 * CHOICE_H, w, h: CHOICE_H - 4.0 };
            let sel = i == self.selected;
            let hover = self.hover == Some(100 + i);
            if sel || hover {
                let a = if sel { 0.16 } else { 0.07 };
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 10.0, (p.fg.0, p.fg.1, p.fg.2, a));
            }
            crate::svg::icon(cr, c.icon, r.x + 12.0, r.y + (r.h - 20.0) / 2.0, 20.0, p.fg, false);
            let tx = r.x + 44.0;
            let tw = r.w - 52.0;
            if c.detail.is_empty() {
                draw_in_box(cr, &c.title, &TextStyle { px: 13.5, ..body }, tx, r.y, tw, r.h, Align::Start);
            } else {
                draw_in_box(cr, &c.title, &TextStyle { px: 13.5, ..body }, tx, r.y + 3.0, tw, 20.0, Align::Start);
                draw_in_box(cr, &c.detail, &TextStyle { color: (p.fg.0, p.fg.1, p.fg.2, 0.6), px: 11.5, ..body }, tx, r.y + 21.0, tw, 16.0, Align::Start);
            }
            self.rows.push((i, r));
        }
        y += shown as f64 * CHOICE_H;
        let status = if self.busy { Some(("Checking\u{2026}".to_string(), p.fg)) } else { self.error.clone().map(|e| (e, p.error)) };
        if let Some((t, c)) = status {
            draw_in_box(cr, &t, &TextStyle { color: c, px: 12.0, ..body }, x + 4.0, y, w, 18.0, Align::Start);
        }
        y += 22.0 + 12.0;
        let bw = 120.0;
        self.buttons = [Rect { x: x + w - 2.0 * bw - 10.0, y, w: bw, h: BUTTON_H }, Rect { x: x + w - bw, y, w: bw, h: BUTTON_H }];
        uikit::button(cr, self.buttons[0], "Cancel", ButtonKind::Outline, if self.hover == Some(0) { 1.0 } else { 0.0 }, true, &p);
        uikit::button(cr, self.buttons[1], &self.req.ok_label, ButtonKind::Primary, if self.hover == Some(1) { 1.0 } else { 0.0 }, !self.busy, &p);
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let h = self
            .buttons
            .iter()
            .position(|r| r.contains(x, y))
            .or_else(|| self.fields.iter().position(|r| r.contains(x, y)).map(|i| 10 + i))
            .or_else(|| self.rows.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| 100 + i));
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        if self.buttons[0].contains(x, y) {
            return self.cancel();
        }
        if self.buttons[1].contains(x, y) {
            return self.submit();
        }
        if let Some(i) = self.fields.iter().position(|r| r.contains(x, y)) {
            self.focus = i;
            // The caret goes where the field was clicked (secret fields show one dot per char).
            if let (Some(p), Some(q)) = (self.palette.clone(), self.values.get_mut(i)) {
                let shown = if self.req.fields[i].secret { "\u{2022}".repeat(q.text.chars().count()) } else { q.text.clone() };
                let ci = uikit::field_index_at(self.fields[i], &shown, q.caret(), &p, x);
                q.set_caret(ci);
            }
            return Effect::Redraw;
        }
        if let Some(&(i, _)) = self.rows.iter().find(|(_, r)| r.contains(x, y)) {
            // A second click on the selected entry takes it.
            if i == self.selected {
                return self.submit();
            }
            self.select(i);
            return Effect::Redraw;
        }
        Effect::None
    }

    fn scroll(&mut self, delta: f64) -> bool {
        let n = self.req.choices.len();
        if n <= CHOICE_ROWS || delta == 0.0 {
            return false;
        }
        let max = n - CHOICE_ROWS;
        self.first = if delta > 0.0 { (self.first + 1).min(max) } else { self.first.saturating_sub(1) };
        true
    }

    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        match special {
            ESC => return self.cancel(),
            DOWN | UP | TAB if !self.req.choices.is_empty() => {
                let n = self.req.choices.len();
                let next = if special == UP || shift { (self.selected + n - 1) % n } else { (self.selected + 1) % n };
                self.select(next);
                return Effect::Redraw;
            }
            ENTER if !self.req.choices.is_empty() => return self.submit(),
            ENTER => {
                // Enter moves to the next empty field, then submits.
                if let Some(i) = (self.focus + 1..self.values.len()).find(|&i| self.values[i].text.is_empty()) {
                    self.focus = i;
                    return Effect::Redraw;
                }
                return self.submit();
            }
            TAB | DOWN | UP if !self.values.is_empty() => {
                let n = self.values.len();
                self.focus = if shift || special == UP { (self.focus + n - 1) % n } else { (self.focus + 1) % n };
                return Effect::Redraw;
            }
            _ => {}
        }
        if self.busy {
            return Effect::None;
        }
        match self.values.get_mut(self.focus).and_then(|q| q.key(special, typed, ctrl)) {
            Some(_) => {
                self.error = None;
                Effect::Redraw
            }
            None => Effect::None,
        }
    }

    fn poll(&mut self) -> bool {
        let mut redraw = false;
        while let Ok(v) = self.verdict.try_recv() {
            redraw = true;
            match v {
                Verdict::Done | Verdict::Cancel => {
                    self.answers = None;
                    self.close = true;
                }
                Verdict::Retry(msg) => {
                    self.busy = false;
                    self.error = Some(msg);
                    self.wipe_secrets();
                    self.focus = self.req.fields.iter().position(|f| f.secret).unwrap_or(0);
                }
            }
        }
        redraw
    }

    fn clickable(&self) -> bool {
        matches!(self.hover, Some(0) | Some(1)) || self.hover.is_some_and(|h| h >= 100)
    }

    fn height(&self) -> Option<f64> {
        Some(self.card_h())
    }

    fn wants_close(&self) -> bool {
        self.close
    }
}
