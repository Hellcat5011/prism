//! The Calendar card (`prism-ctl calendar toggle`, a click on the bar's clock, a hot corner):
//! a month with its events in the day cells, and the selected day's agenda beside it. New
//! events (+, Enter or N) and changes (a click on an event) go through the event card
//! (`event_card.rs`), which brings this card back, on the same day, when it closes.
//!
//! Keys: arrows move the selected day, Page Up/Down the month, Home or T goes to today, Enter
//! or N adds an event on the selected day, Esc closes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use prism_common::calendar::{civil_from_days, days_from_civil, Occurrence};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::cal_events;
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, Align, TextStyle};
use crate::launcher::{DOWN, END, ENTER, ESC, HOME, LEFT, PGDN, PGUP, RIGHT, UP};
use crate::ui::{Fades, Style};
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};

pub const CARD_W: f64 = 1000.0;
pub const CARD_H: f64 = 660.0;
const PAD: f64 = 22.0;
const HEADER_H: f64 = 44.0;
const AGENDA_W: f64 = 300.0;
const GAP: f64 = 18.0;
const CELL_GAP: f64 = 4.0;
const CHIP_H: f64 = 18.0;

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_FULL: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// The selected day (days since 1970) kept while the event card is open, so the calendar
/// comes back where it was.
static SELECTED: Mutex<Option<i64>> = Mutex::new(None);
static WANTED: AtomicBool = AtomicBool::new(false);

/// Asks the run loop to open the card again (after the event card it opened).
pub fn reopen() {
    WANTED.store(true, Ordering::SeqCst);
}

/// True once after `reopen`.
pub fn take_open() -> bool {
    WANTED.swap(false, Ordering::SeqCst)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Hit {
    Prev,
    Next,
    Today,
    Add,
    Day(i64),
    /// An occurrence: the index into `shown`.
    Event(usize),
}

/// Sunday = 0 of the day `d` (days since 1970, a Thursday).
fn weekday(d: i64) -> usize {
    (d + 4).rem_euclid(7) as usize
}

fn today() -> i64 {
    let (y, m, d, ..) = cal_events::local(now());
    days_from_civil(y as i64, m + 1, d)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Unix time of local midnight starting the day `d`.
fn day_start(d: i64) -> i64 {
    let (y, m, dd) = civil_from_days(d);
    cal_events::day_start(y as i32, m - 1, dd)
}

/// The occurrences that touch the day `d`, all-day ones first.
fn events_on(d: i64) -> Vec<Occurrence> {
    let (y, m, dd) = civil_from_days(d);
    let mut v = cal_events::on_day(y as i32, m - 1, dd);
    v.sort_by_key(|o| (!o.all_day, o.start));
    v
}

pub struct CalendarView {
    selected: i64,
    /// First day of the month shown.
    month: i64,
    generation: u64,
    pointer: (f64, f64),
    hits: Vec<(Hit, Rect)>,
    hover: Fades<Hit>,
    /// Occurrences drawn this frame (for `Hit::Event`).
    shown: Vec<Occurrence>,
    /// Wheel travel towards the next month.
    wheel: f64,
}

impl Default for CalendarView {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarView {
    pub fn new() -> Self {
        let selected = SELECTED.lock().unwrap().take().unwrap_or_else(today);
        let mut v = CalendarView { selected, month: 0, generation: cal_events::generation(), pointer: (-1.0, -1.0), hits: Vec::new(), hover: Fades::default(), shown: Vec::new(), wheel: 0.0 };
        v.month = Self::month_of(selected);
        v
    }

    fn month_of(d: i64) -> i64 {
        let (y, m, _) = civil_from_days(d);
        days_from_civil(y, m, 1)
    }

    fn select(&mut self, d: i64) {
        self.selected = d;
        self.month = Self::month_of(d);
    }

    /// The same day in the month `delta` months away (the last day when it is shorter).
    fn shift_month(&mut self, delta: i64) {
        let (y, m, d) = civil_from_days(self.selected);
        let total = y * 12 + (m as i64 - 1) + delta;
        let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
        let mut day = d;
        while day > 28 && civil_from_days(days_from_civil(ny, nm, day)).1 != nm {
            day -= 1;
        }
        self.select(days_from_civil(ny, nm, day));
    }

    fn add(&mut self) -> Effect {
        self.edit(crate::event_card::Request::New(day_start(self.selected)))
    }

    /// Opens the event card; this card comes back when it closes.
    fn edit(&mut self, req: crate::event_card::Request) -> Effect {
        *SELECTED.lock().unwrap() = Some(self.selected);
        crate::event_card::open_from_calendar(req);
        Effect::Redraw
    }

    fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits.iter().rev().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)
    }

    fn level(&mut self, h: Hit, style: &Style) -> f64 {
        let on = self.hit(self.pointer.0, self.pointer.1) == Some(h);
        self.hover.level(h, on, style)
    }

    fn palette(style: &Style, theme: &Theme) -> Palette {
        Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: cb::hex_to_rgba(&theme.on_primary, 1.0),
            outline: cb::hex_to_rgba(&theme.outline_variant, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        }
    }
}

fn colour_of(o: &Occurrence, accent: crate::ui::Rgba) -> crate::ui::Rgba {
    if o.color.len() == 7 && o.color.starts_with('#') {
        cb::hex_to_rgba(&o.color, 1.0)
    } else {
        accent
    }
}

fn time_label(o: &Occurrence, from: i64, to: i64) -> String {
    if o.all_day || (o.start <= from && o.end >= to) {
        "All day".into()
    } else if o.start < from {
        format!("Until {}", cal_events::clock(o.end))
    } else if o.end > to {
        format!("From {}", cal_events::clock(o.start))
    } else if o.end > o.start {
        format!("{} \u{2013} {}", cal_events::clock(o.start), cal_events::clock(o.end))
    } else {
        cal_events::clock(o.start)
    }
}

impl CardView for CalendarView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        self.hits.clear();
        self.shown.clear();
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let dim = (fg.0, fg.1, fg.2, 0.6);
        let fam = p.family.clone();
        let st = |px: f64, bold: bool, color: crate::ui::Rgba| TextStyle { family: &fam, px, bold, color };
        let today = today();

        // Header: < Month Year >   Today   + New event
        let (x0, y0) = (card.x + PAD, card.y + PAD);
        let (my, mm, _) = civil_from_days(self.month);
        let prev = Rect { x: x0, y: y0 + 4.0, w: 36.0, h: 36.0 };
        let hv = self.level(Hit::Prev, style);
        uikit::button(cr, prev, "\u{2039}", ButtonKind::Outline, hv, true, &p);
        self.hits.push((Hit::Prev, prev));
        let title = format!("{} {my}", MONTHS[(mm - 1) as usize]);
        let (_, _, tw, _) = draw_in_box(cr, &title, &st(22.0, true, fg), prev.x + prev.w + 14.0, y0, 400.0, HEADER_H, Align::Start);
        let next = Rect { x: prev.x + prev.w + 14.0 + tw + 14.0, ..prev };
        let hv = self.level(Hit::Next, style);
        uikit::button(cr, next, "\u{203a}", ButtonKind::Outline, hv, true, &p);
        self.hits.push((Hit::Next, next));
        let add = Rect { x: card.x + card.w - PAD - 150.0, y: y0 + 4.0, w: 150.0, h: 36.0 };
        let hv = self.level(Hit::Add, style);
        uikit::button(cr, add, "+  New event", ButtonKind::Primary, hv, true, &p);
        self.hits.push((Hit::Add, add));
        if self.selected != today || Self::month_of(today) != self.month {
            let t = Rect { x: add.x - 10.0 - 90.0, y: add.y, w: 90.0, h: 36.0 };
            let hv = self.level(Hit::Today, style);
            uikit::button(cr, t, "Today", ButtonKind::Outline, hv, true, &p);
            self.hits.push((Hit::Today, t));
        }

        // The month.
        let grid = Rect { x: x0, y: y0 + HEADER_H + 14.0, w: card.w - 2.0 * PAD - AGENDA_W - GAP, h: card.h - 2.0 * PAD - HEADER_H - 14.0 };
        let head_h = 22.0;
        for (i, wd) in WEEKDAYS.iter().enumerate() {
            let cw = (grid.w - 6.0 * CELL_GAP) / 7.0;
            draw_in_box(cr, wd, &st(12.0, true, dim), grid.x + i as f64 * (cw + CELL_GAP) + 8.0, grid.y, cw, head_h, Align::Start);
        }
        let first = self.month - weekday(self.month) as i64;
        let next_month = days_from_civil(if mm == 12 { my + 1 } else { my }, if mm == 12 { 1 } else { mm + 1 }, 1);
        let rows = ((next_month - first) as f64 / 7.0).ceil() as usize;
        let cw = (grid.w - 6.0 * CELL_GAP) / 7.0;
        let ch = (grid.h - head_h - 6.0 - (rows as f64 - 1.0) * CELL_GAP) / rows as f64;
        for k in 0..rows * 7 {
            let d = first + k as i64;
            let (col, row) = (k % 7, k / 7);
            let r = Rect { x: grid.x + col as f64 * (cw + CELL_GAP), y: grid.y + head_h + 6.0 + row as f64 * (ch + CELL_GAP), w: cw, h: ch };
            let inside = d >= self.month && d < next_month;
            let hv = self.level(Hit::Day(d), style);
            let base = if inside { 0.06 } else { 0.025 };
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 10.0, (fg.0, fg.1, fg.2, base + 0.06 * hv));
            if d == self.selected {
                cb::stroke_rounded_rect(cr, r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0, 9.0, p.accent, 2.0);
            }
            self.hits.push((Hit::Day(d), r));
            // The day number; today on an accent disc.
            let num = civil_from_days(d).2.to_string();
            let ncol = if inside { fg } else { dim };
            if d == today {
                cb::fill_circle(cr, r.x + 16.0, r.y + 15.0, 12.0, p.accent);
                draw_in_box(cr, &num, &st(12.0, true, p.on_accent), r.x + 4.0, r.y + 3.0, 24.0, 24.0, Align::Center);
            } else {
                draw_in_box(cr, &num, &st(12.0, d == self.selected, ncol), r.x + 4.0, r.y + 3.0, 24.0, 24.0, Align::Center);
            }
            // Event chips, as many as fit, then "+N".
            let evs = events_on(d);
            let room = ((r.h - 32.0) / (CHIP_H + 2.0)).floor().max(0.0) as usize;
            let fit = if evs.len() > room { room.saturating_sub(1) } else { evs.len() };
            for (j, o) in evs.iter().take(fit).enumerate() {
                let c = Rect { x: r.x + 4.0, y: r.y + 30.0 + j as f64 * (CHIP_H + 2.0), w: r.w - 8.0, h: CHIP_H };
                let col = colour_of(o, p.accent);
                let alpha = if inside { 1.0 } else { 0.5 };
                if o.all_day {
                    cb::fill_rounded_rect(cr, c.x, c.y, c.w, c.h, 5.0, (col.0, col.1, col.2, 0.32 * alpha));
                    draw_in_box(cr, &o.summary, &st(11.0, true, (fg.0, fg.1, fg.2, alpha)), c.x + 6.0, c.y, c.w - 10.0, c.h, Align::Start);
                } else {
                    cb::fill_circle(cr, c.x + 5.0, c.y + c.h / 2.0, 3.0, (col.0, col.1, col.2, alpha));
                    let label = format!("{} {}", cal_events::clock(o.start), o.summary);
                    draw_in_box(cr, &label, &st(11.0, false, (fg.0, fg.1, fg.2, alpha)), c.x + 12.0, c.y, c.w - 14.0, c.h, Align::Start);
                }
                self.shown.push(o.clone());
                self.hits.push((Hit::Event(self.shown.len() - 1), c));
            }
            if evs.len() > fit {
                let y = r.y + 30.0 + fit as f64 * (CHIP_H + 2.0);
                draw_in_box(cr, &format!("+{} more", evs.len() - fit), &st(11.0, false, dim), r.x + 8.0, y, r.w - 12.0, CHIP_H, Align::Start);
            }
        }

        // The agenda of the selected day.
        let a = Rect { x: card.x + card.w - PAD - AGENDA_W, y: grid.y, w: AGENDA_W, h: grid.h };
        cb::fill_rounded_rect(cr, a.x, a.y, a.w, a.h, 14.0, (fg.0, fg.1, fg.2, 0.05));
        let (sy, sm, sd) = civil_from_days(self.selected);
        let heading = if self.selected == today { "Today".to_string() } else if self.selected == today + 1 { "Tomorrow".to_string() } else { WEEKDAYS_FULL[weekday(self.selected)].to_string() };
        draw_in_box(cr, &heading, &st(18.0, true, fg), a.x + 16.0, a.y + 12.0, a.w - 32.0, 26.0, Align::Start);
        draw_in_box(cr, &format!("{sd} {} {sy}", MONTHS[(sm - 1) as usize]), &st(13.0, false, dim), a.x + 16.0, a.y + 38.0, a.w - 32.0, 20.0, Align::Start);
        let (from, to) = (day_start(self.selected), day_start(self.selected + 1));
        let evs = events_on(self.selected);
        let mut y = a.y + 72.0;
        let bottom = a.y + a.h - 60.0;
        if evs.is_empty() {
            draw_in_box(cr, "Nothing planned", &st(13.0, false, dim), a.x + 16.0, y, a.w - 32.0, 24.0, Align::Start);
        }
        for (i, o) in evs.iter().enumerate() {
            let row_h = if o.location.is_empty() { 50.0 } else { 66.0 };
            if y + row_h > bottom {
                draw_in_box(cr, &format!("+{} more", evs.len() - i), &st(12.0, false, dim), a.x + 16.0, y, a.w - 32.0, 20.0, Align::Start);
                break;
            }
            let r = Rect { x: a.x + 8.0, y, w: a.w - 16.0, h: row_h - 6.0 };
            self.shown.push(o.clone());
            let h = Hit::Event(self.shown.len() - 1);
            let hv = self.level(h, style);
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 10.0, (fg.0, fg.1, fg.2, 0.04 + 0.08 * hv));
            cb::fill_rounded_rect(cr, r.x + 8.0, r.y + 8.0, 4.0, r.h - 16.0, 2.0, colour_of(o, p.accent));
            draw_in_box(cr, &o.summary, &st(14.0, true, fg), r.x + 22.0, r.y + 6.0, r.w - 30.0, 20.0, Align::Start);
            draw_in_box(cr, &time_label(o, from, to), &st(12.0, false, dim), r.x + 22.0, r.y + 26.0, r.w - 30.0, 18.0, Align::Start);
            if !o.location.is_empty() {
                draw_in_box(cr, &o.location, &st(12.0, false, dim), r.x + 22.0, r.y + 42.0, r.w - 30.0, 18.0, Align::Start);
            }
            self.hits.push((h, r));
            y += row_h;
        }
        let ab = Rect { x: a.x + 16.0, y: a.y + a.h - 50.0, w: a.w - 32.0, h: 36.0 };
        let hv = self.level(Hit::Add, style);
        uikit::button(cr, ab, "+  Add event on this day", ButtonKind::Outline, hv, true, &p);
        self.hits.push((Hit::Add, ab));
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let before = self.hit(self.pointer.0, self.pointer.1);
        self.pointer = (x, y);
        before != self.hit(x, y)
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        let Some(h) = self.hit(x, y) else { return Effect::None };
        match h {
            Hit::Prev => self.shift_month(-1),
            Hit::Next => self.shift_month(1),
            Hit::Today => self.select(today()),
            Hit::Add => return self.add(),
            Hit::Day(d) => {
                // A second click on the selected day adds an event there.
                if d == self.selected {
                    return self.add();
                }
                self.selected = d;
                if Self::month_of(d) != self.month {
                    self.month = Self::month_of(d);
                }
            }
            Hit::Event(i) => {
                if let Some(o) = self.shown.get(i).cloned() {
                    return self.edit(crate::event_card::Request::Edit(o));
                }
            }
        }
        Effect::Redraw
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.wheel += delta;
        if self.wheel.abs() < 40.0 {
            return false;
        }
        self.shift_month(self.wheel.signum() as i64);
        self.wheel = 0.0;
        true
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, _ctrl: bool) -> Effect {
        match (special, typed) {
            (ESC, _) => return Effect::Close,
            (ENTER, _) | (_, Some('n' | 'N' | '+')) => return self.add(),
            (LEFT, _) => self.select(self.selected - 1),
            (RIGHT, _) => self.select(self.selected + 1),
            (UP, _) => self.select(self.selected - 7),
            (DOWN, _) => self.select(self.selected + 7),
            (PGUP, _) => self.shift_month(-1),
            (PGDN, _) => self.shift_month(1),
            (HOME, _) | (_, Some('t' | 'T')) => self.select(today()),
            (END, _) => {
                let (y, m, _) = civil_from_days(self.month);
                let next = days_from_civil(if m == 12 { y + 1 } else { y }, if m == 12 { 1 } else { m + 1 }, 1);
                self.select(next - 1);
            }
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// New events from prismd's cache.
    fn poll(&mut self) -> bool {
        let g = cal_events::generation();
        let changed = g != self.generation;
        self.generation = g;
        changed
    }

    fn wake_in(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }

    fn clickable(&self) -> bool {
        self.hit(self.pointer.0, self.pointer.1).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekdays_and_months() {
        // 2026-11-24 is a Tuesday.
        assert_eq!(WEEKDAYS[weekday(days_from_civil(2026, 11, 24))], "Tue");
        let mut v = CalendarView { selected: days_from_civil(2026, 1, 31), month: 0, generation: 0, pointer: (0.0, 0.0), hits: Vec::new(), hover: Fades::default(), shown: Vec::new(), wheel: 0.0 };
        v.shift_month(1);
        assert_eq!(civil_from_days(v.selected), (2026, 2, 28));
        v.shift_month(-2);
        assert_eq!(civil_from_days(v.selected), (2025, 12, 28));
    }

    /// `PRISM_CALCARD_RENDER_DIR=<dir> cargo test --release -p prism-ui render_calendar_card -- --nocapture`
    #[test]
    fn render_calendar_card() {
        let Ok(dir) = std::env::var("PRISM_CALCARD_RENDER_DIR") else { return };
        let theme = Theme::load();
        let style = Style::new(&prism_common::Config::load(), &theme);
        let mut v = CalendarView::new();
        if let Some(o) = cal_events::snapshot().iter().find(|o| o.start > now()) {
            let (y, m, d, ..) = cal_events::local(o.start);
            v.select(days_from_civil(y as i64, m + 1, d));
        }
        let card = Rect { x: 20.0, y: 20.0, w: CARD_W, h: CARD_H };
        unsafe {
            let surf = cb::cairo_image_surface_create(0, (CARD_W + 40.0) as i32, (CARD_H + 40.0) as i32);
            let cr = cb::cairo_create(surf);
            cb::cairo_set_source_rgba(cr, 0.1, 0.1, 0.12, 1.0);
            cb::cairo_paint(cr);
            v.draw(cr, card, &style, &theme);
            let path = std::ffi::CString::new(format!("{dir}/calendar-card.png")).unwrap();
            cb::cairo_surface_write_to_png(surf, path.as_ptr());
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
    }
}
