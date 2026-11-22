//! The event card: add an event to a calendar, or change or delete one, from the Calendar
//! widget's day page (its + button, or a click on an event). Events go through
//! evolution-data-server (`prism_common::eds`), which syncs them with the account's server;
//! prismd is then told to read the calendars again (`IpcCommand::CalendarRefresh`).
//!
//! A repeating event is changed or deleted either all of it or only this occurrence. Only this
//! one: the occurrence is removed from the series and, when changed, added again as an event of
//! its own (EDS's own single-occurrence edits are not dependable across servers).
//! Events of subscribed .ics feeds, and of calendars that cannot be written, are shown read only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;

use prism_common::calendar::{self, civil_from_days, Draft, Occurrence, Zone};
use prism_common::{eds, Config, Theme};

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, Align, TextStyle};
use crate::launcher::{Query, ENTER, ESC, TAB};
use crate::ui::{Fades, Style};
use crate::uikit::{self, ButtonKind, Dropdown, Palette};
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 540.0;
pub const CARD_H: f64 = 540.0;
const PAD: f64 = 24.0;
const FIELD_H: f64 = 38.0;
const ICON: &str = "M8 2v4M16 2v4M3 10h18M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z";

/// What the card opens for.
#[derive(Clone, Debug)]
pub enum Request {
    /// A new event on the local day that starts at this instant.
    New(i64),
    /// An occurrence from the calendar cache.
    Edit(Occurrence),
}

static PENDING: Mutex<Option<Request>> = Mutex::new(None);
static WANTED: AtomicBool = AtomicBool::new(false);
/// Opened from the Calendar card: bring it back when this card closes.
static BACK: AtomicBool = AtomicBool::new(false);

/// Asks the run loop to open the card for `req`.
pub fn open(req: Request) {
    *PENDING.lock().unwrap() = Some(req);
    WANTED.store(true, Ordering::SeqCst);
}

/// `open`, from the Calendar card: it comes back when this card closes.
pub fn open_from_calendar(req: Request) {
    BACK.store(true, Ordering::SeqCst);
    open(req);
}

/// True once after `open`: the run loop opens the card.
pub fn take_open() -> bool {
    WANTED.swap(false, Ordering::SeqCst)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Hit {
    Title,
    Location,
    Start,
    End,
    DayPrev,
    DayNext,
    UntilPrev,
    UntilNext,
    AllDay,
    Calendar,
    This,
    All,
    Delete,
    Cancel,
    Save,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Title,
    Start,
    End,
    Location,
}

/// Local date (days since 1970) and seconds into it of the instant `t`.
fn local_parts(t: i64) -> (i64, i64) {
    let wall = t + Zone::local().offset_at(t) as i64;
    (wall.div_euclid(86400), wall.rem_euclid(86400))
}

/// The instant of a local date and time of day.
fn local_instant(day: i64, secs: i64) -> i64 {
    Zone::local().to_utc(day * 86400 + secs)
}

fn hhmm(secs: i64) -> String {
    format!("{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60)
}

/// "9", "930", "09:30", "9.30", "9:30 pm" as seconds into the day.
fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim().to_lowercase();
    let (s, pm, am) = match (s.strip_suffix("pm"), s.strip_suffix("am")) {
        (Some(r), _) => (r.trim().to_string(), true, false),
        (_, Some(r)) => (r.trim().to_string(), false, true),
        _ => (s, false, false),
    };
    let (h, m) = match s.split_once([':', '.']) {
        Some((h, m)) => (h.trim().parse::<i64>().ok()?, m.trim().parse::<i64>().ok()?),
        None if s.len() > 2 => (s[..s.len() - 2].parse().ok()?, s[s.len() - 2..].parse().ok()?),
        None => (s.parse().ok()?, 0),
    };
    let h = match (pm, am, h) {
        (true, _, 12) => 12,
        (true, _, h) if h < 12 => h + 12,
        (_, true, 12) => 0,
        _ => h,
    };
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 3600 + m * 60)
}

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "Tue 24 Nov" (with the year when it is not this one).
fn date_label(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    let this_year = civil_from_days(local_parts(now()).0).0;
    let wd = WEEKDAYS[day.rem_euclid(7) as usize];
    if y == this_year {
        format!("{wd} {d} {}", MONTHS[(m - 1) as usize])
    } else {
        format!("{wd} {d} {} {y}", MONTHS[(m - 1) as usize])
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// What the worker thread does.
enum Job {
    Create { cal: String, draft: Draft },
    /// Change all of the event (`occ` is the occurrence the card shows).
    Modify { occ: Occurrence, draft: Draft },
    /// Only this occurrence of a repeating event: remove it, add the changed one alone.
    Detach { occ: Occurrence, draft: Draft },
    /// `rid` empty: all of it.
    Delete { occ: Occurrence, rid: String },
}

fn run(job: Job) -> Result<(), String> {
    let mut c = eds::Client::new()?;
    let t = now();
    match job {
        Job::Create { cal, draft } => {
            c.create(&cal, &calendar::new_vevent(&calendar::new_uid(), &draft, t))?;
        }
        Job::Modify { occ, draft } => {
            let orig = c.get(&occ.calendar, &occ.uid)?;
            let text = calendar::edit_vevent(&orig, &draft, occ.start, t).ok_or("this event cannot be read")?;
            c.modify(&occ.calendar, &text)?;
        }
        Job::Detach { occ, draft } => {
            c.remove(&occ.calendar, &occ.uid, &occ.rid)?;
            c.create(&occ.calendar, &calendar::new_vevent(&calendar::new_uid(), &draft, t))?;
        }
        Job::Delete { occ, rid } => c.remove(&occ.calendar, &occ.uid, &rid)?,
    }
    let _ = prism_common::ipc::request(&prism_common::ipc::IpcCommand::CalendarRefresh);
    Ok(())
}

pub struct EventView {
    editing: Option<Occurrence>,
    read_only: bool,
    title: Query,
    location: Query,
    start: Query,
    end: Query,
    all_day: bool,
    day: i64,
    /// Last day of an all-day event (inclusive).
    until: i64,
    focus: Focus,
    /// Calendars that take new events, loading until Some.
    calendars: Option<Vec<eds::Calendar>>,
    cal_rx: Option<Receiver<Result<Vec<eds::Calendar>, String>>>,
    chosen: usize,
    dropdown: Option<Dropdown>,
    /// Repeating event: change only this occurrence.
    only_this: bool,
    busy: Option<Receiver<Result<(), String>>>,
    error: Option<String>,
    /// The second press of Delete deletes.
    confirm_delete: bool,
    done: bool,
    waker: Waker,
    pointer: (f64, f64),
    hits: Vec<(Hit, Rect)>,
    hover: Fades<Hit>,
    knob: Fades<Hit>,
    /// Opened from the Calendar card.
    back: bool,
    bounds: Rect,
    cal_rect: Rect,
}

impl EventView {
    pub fn new(waker: Waker) -> Self {
        let req = PENDING.lock().unwrap().take().unwrap_or_else(|| Request::New(local_instant(local_parts(now()).0, 0)));
        let (editing, read_only) = match &req {
            Request::Edit(o) => (Some(o.clone()), o.calendar.is_empty()),
            Request::New(_) => (None, false),
        };
        let mut v = EventView {
            editing: editing.clone(),
            read_only,
            title: Query::default(),
            location: Query::default(),
            start: Query::default(),
            end: Query::default(),
            all_day: false,
            day: 0,
            until: 0,
            focus: Focus::Title,
            calendars: None,
            cal_rx: None,
            chosen: 0,
            dropdown: None,
            only_this: true,
            busy: None,
            error: None,
            confirm_delete: false,
            done: false,
            waker,
            pointer: (-1.0, -1.0),
            hits: Vec::new(),
            hover: Fades::default(),
            knob: Fades::default(),
            back: BACK.swap(false, Ordering::SeqCst),
            bounds: Rect::default(),
            cal_rect: Rect::default(),
        };
        match req {
            Request::Edit(o) => {
                v.title = Query::with_text(&o.summary);
                v.location = Query::with_text(&o.location);
                v.all_day = o.all_day;
                let (d0, s0) = local_parts(o.start);
                let (d1, s1) = local_parts(o.end);
                v.day = d0;
                v.until = if o.all_day { local_parts(o.end - 1).0.max(d0) } else { d0 };
                v.start = Query::with_text(&hhmm(s0));
                v.end = Query::with_text(&hhmm(if d1 > d0 && s1 == 0 { 86400 - 60 } else { s1 }));
            }
            Request::New(day_start) => {
                let (day, _) = local_parts(day_start + 3600);
                let (today, secs) = local_parts(now());
                // Today: the next full hour; another day: 9:00.
                let s = if day == today { ((secs / 3600 + 1) * 3600).min(23 * 3600) } else { 9 * 3600 };
                v.day = day;
                v.until = day;
                v.start = Query::with_text(&hhmm(s));
                v.end = Query::with_text(&hhmm((s + 3600).min(86400 - 60)));
            }
        }
        if !v.read_only {
            v.load_calendars();
        }
        v
    }

    fn load_calendars(&mut self) {
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let only = self.editing.as_ref().map(|o| o.calendar.clone());
        std::thread::spawn(move || {
            let r = (|| {
                if !eds::available() {
                    return Err("Adding events needs evolution-data-server (Settings > Calendar > Calendar Accounts).".to_string());
                }
                let mut c = eds::Client::new()?;
                let all = c.calendars()?;
                let mut out = Vec::new();
                for cal in all {
                    // Editing: only the event's own calendar.
                    if only.as_ref().is_some_and(|u| *u != cal.uid) {
                        continue;
                    }
                    if c.writable(&cal.uid).unwrap_or(false) {
                        out.push(cal);
                    }
                }
                Ok(out)
            })();
            let _ = tx.send(r);
            waker.wake();
        });
        self.cal_rx = Some(rx);
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

    fn repeating(&self) -> bool {
        self.editing.as_ref().is_some_and(|o| !o.rid.is_empty())
    }

    fn editable(&self) -> bool {
        !self.read_only && self.busy.is_none() && self.calendars.as_ref().is_some_and(|c| !c.is_empty())
    }

    fn query(&mut self, f: Focus) -> &mut Query {
        match f {
            Focus::Title => &mut self.title,
            Focus::Start => &mut self.start,
            Focus::End => &mut self.end,
            Focus::Location => &mut self.location,
        }
    }

    /// The event as the card has it, or what is wrong with it.
    fn draft(&self) -> Result<Draft, String> {
        let summary = self.title.text.trim().to_string();
        if summary.is_empty() {
            return Err("The event needs a title".into());
        }
        let (start, end) = if self.all_day {
            (local_instant(self.day, 0), local_instant(self.until.max(self.day) + 1, 0))
        } else {
            let s = parse_time(&self.start.text).ok_or("The start time is not a time (like 9:30)")?;
            let e = parse_time(&self.end.text).ok_or("The end time is not a time (like 10:30)")?;
            // Ending at or before the start: it ends the next day.
            let end_day = if e <= s { self.day + 1 } else { self.day };
            (local_instant(self.day, s), local_instant(end_day, e))
        };
        Ok(Draft { summary, location: self.location.text.trim().to_string(), all_day: self.all_day, start, end })
    }

    fn start_job(&mut self, job: Job) {
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run(job));
            waker.wake();
        });
        self.busy = Some(rx);
        self.error = None;
    }

    fn save(&mut self) -> Effect {
        if !self.editable() {
            return Effect::None;
        }
        let draft = match self.draft() {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(e);
                return Effect::Redraw;
            }
        };
        let job = match self.editing.clone() {
            None => {
                let cals = self.calendars.as_deref().unwrap_or_default();
                let Some(cal) = cals.get(self.chosen) else { return Effect::None };
                Job::Create { cal: cal.uid.clone(), draft }
            }
            Some(occ) if self.repeating() && self.only_this => Job::Detach { occ, draft },
            Some(occ) => Job::Modify { occ, draft },
        };
        self.start_job(job);
        Effect::Redraw
    }

    fn delete(&mut self) -> Effect {
        let Some(occ) = self.editing.clone() else { return Effect::None };
        if !self.editable() {
            return Effect::None;
        }
        if !self.confirm_delete {
            self.confirm_delete = true;
            return Effect::Redraw;
        }
        let rid = if self.repeating() && self.only_this { occ.rid.clone() } else { String::new() };
        self.start_job(Job::Delete { occ, rid });
        Effect::Redraw
    }

    fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits.iter().rev().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)
    }

    fn hovered(&self) -> Option<Hit> {
        self.hit(self.pointer.0, self.pointer.1)
    }

    fn level(&mut self, h: Hit, style: &Style) -> f64 {
        let on = self.hovered() == Some(h);
        self.hover.level(h, on, style)
    }
}

impl Drop for EventView {
    fn drop(&mut self) {
        if self.back {
            crate::calendar_card::reopen();
        }
    }
}

impl CardView for EventView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        self.bounds = card;
        self.hits.clear();
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let dim = (fg.0, fg.1, fg.2, 0.62);
        let label_font = format!("{} Semi-Bold 12px", p.family);
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;
        let editable = self.editable();

        // Header.
        cb::fill_circle(cr, x + 20.0, y + 20.0, 20.0, (p.accent.0, p.accent.1, p.accent.2, 0.18));
        crate::svg::draw_path(cr, ICON, x + 9.0, y + 9.0, 22.0, p.accent, Some(2.0));
        let heading = match (&self.editing, self.read_only) {
            (None, _) => "New Event",
            (Some(_), true) => "Event",
            (Some(_), false) => "Edit Event",
        };
        cb::draw_pango_text(cr, x + 54.0, y, heading, &format!("{} Bold 18px", p.family), fg, None);
        let sub = if self.read_only {
            "From a subscribed feed: read only".to_string()
        } else if self.repeating() {
            "A repeating event".to_string()
        } else {
            match (&self.calendars, self.editing.as_ref()) {
                (Some(c), Some(_)) => c.first().map(|c| if c.account.is_empty() { c.name.clone() } else { format!("{} \u{b7} {}", c.name, c.account) }).unwrap_or_default(),
                _ => "Added to the calendar below, and synced with its account".to_string(),
            }
        };
        cb::draw_pango_text(cr, x + 54.0, y + 25.0, &sub, &format!("{} 12px", p.family), dim, Some((w - 54.0) as i32));
        y += 62.0;

        // Title.
        cb::draw_pango_text(cr, x, y, "Title", &label_font, dim, None);
        y += 20.0;
        let r = Rect { x, y, w, h: FIELD_H };
        let caret = (editable && self.focus == Focus::Title).then(|| self.title.caret());
        let hv = self.level(Hit::Title, style);
        uikit::text_field(cr, r, &self.title.text, "What is it?", caret, hv, false, &p);
        self.hits.push((Hit::Title, r));
        y += FIELD_H + 14.0;

        // Day, with All day on the right.
        cb::draw_pango_text(cr, x, y, if self.all_day { "From" } else { "Day" }, &label_font, dim, None);
        let sw = Rect { x: x + w - 50.0, y: y + 22.0, w: 50.0, h: 28.0 };
        cb::draw_pango_text(cr, sw.x - 70.0, sw.y + 5.0, "All day", &format!("{} 13px", p.family), fg, None);
        let knob = self.knob.settled_level(Hit::AllDay, self.all_day, crate::ui::SPRING_KNOB, style);
        let hv = self.level(Hit::AllDay, style);
        uikit::switch(cr, sw, knob, hv, &p);
        self.hits.push((Hit::AllDay, Rect { x: sw.x - 76.0, ..Rect { w: sw.w + 76.0, ..sw } }));
        y += 20.0;
        let stepper = |me: &mut Self, cr: *mut Cairo, y: f64, day: i64, prev: Hit, next: Hit| {
            let left = Rect { x, y, w: 38.0, h: FIELD_H };
            let right = Rect { x: x + 38.0 + 8.0 + 190.0 + 8.0, y, w: 38.0, h: FIELD_H };
            let mid = Rect { x: x + 46.0, y, w: 190.0, h: FIELD_H };
            let (hp, hn) = (me.level(prev, style), me.level(next, style));
            uikit::button(cr, left, "\u{2039}", ButtonKind::Outline, hp, editable, &p);
            uikit::button(cr, right, "\u{203a}", ButtonKind::Outline, hn, editable, &p);
            uikit::text_field(cr, mid, &date_label(day), "", None, 0.0, false, &p);
            me.hits.push((prev, left));
            me.hits.push((next, right));
        };
        stepper(self, cr, y, self.day, Hit::DayPrev, Hit::DayNext);
        y += FIELD_H + 14.0;

        // Until (all day), or the times.
        if self.all_day {
            cb::draw_pango_text(cr, x, y, "Until", &label_font, dim, None);
            y += 20.0;
            let until = self.until.max(self.day);
            stepper(self, cr, y, until, Hit::UntilPrev, Hit::UntilNext);
        } else {
            cb::draw_pango_text(cr, x, y, "Starts", &label_font, dim, None);
            cb::draw_pango_text(cr, x + 150.0, y, "Ends", &label_font, dim, None);
            y += 20.0;
            let sr = Rect { x, y, w: 120.0, h: FIELD_H };
            let er = Rect { x: x + 150.0, y, w: 120.0, h: FIELD_H };
            let bad_s = parse_time(&self.start.text).is_none();
            let bad_e = parse_time(&self.end.text).is_none();
            let (hs, he) = (self.level(Hit::Start, style), self.level(Hit::End, style));
            uikit::text_field(cr, sr, &self.start.text, "9:00", (editable && self.focus == Focus::Start).then(|| self.start.caret()), hs, bad_s, &p);
            uikit::text_field(cr, er, &self.end.text, "10:00", (editable && self.focus == Focus::End).then(|| self.end.caret()), he, bad_e, &p);
            self.hits.push((Hit::Start, sr));
            self.hits.push((Hit::End, er));
            if let (Some(s), Some(e)) = (parse_time(&self.start.text), parse_time(&self.end.text)) {
                if e <= s {
                    cb::draw_pango_text(cr, er.x + er.w + 12.0, y + 10.0, "next day", &format!("{} 12px", p.family), dim, None);
                }
            }
        }
        y += FIELD_H + 14.0;

        // Location.
        let r = Rect { x, y, w, h: FIELD_H };
        let caret = (editable && self.focus == Focus::Location).then(|| self.location.caret());
        let hv = self.level(Hit::Location, style);
        uikit::text_field(cr, r, &self.location.text, "Location (optional)", caret, hv, false, &p);
        self.hits.push((Hit::Location, r));
        y += FIELD_H + 14.0;

        // Calendar (new events), or This / All (a repeating event).
        if self.editing.is_none() {
            cb::draw_pango_text(cr, x, y, "Calendar", &label_font, dim, None);
            y += 20.0;
            let r = Rect { x, y, w, h: 34.0 };
            self.cal_rect = r;
            let label = match &self.calendars {
                None => "Loading calendars\u{2026}".to_string(),
                Some(c) if c.is_empty() => "No calendar takes new events".to_string(),
                Some(c) => c.get(self.chosen).map(|c| if c.account.is_empty() { c.name.clone() } else { format!("{} \u{b7} {}", c.name, c.account) }).unwrap_or_default(),
            };
            let hv = self.level(Hit::Calendar, style);
            uikit::combo(cr, r, &label, self.dropdown.is_some(), hv, &p, None);
            self.hits.push((Hit::Calendar, r));
        } else if self.repeating() && !self.read_only {
            cb::draw_pango_text(cr, x, y + 8.0, "Change", &format!("{} 13px", p.family), fg, None);
            let a = Rect { x: x + 80.0, y, w: 150.0, h: 34.0 };
            let b = Rect { x: a.x + a.w + 8.0, y, w: 150.0, h: 34.0 };
            let (ha, hb) = (self.level(Hit::This, style), self.level(Hit::All, style));
            uikit::button(cr, a, "Only this one", if self.only_this { ButtonKind::Primary } else { ButtonKind::Outline }, ha, editable, &p);
            uikit::button(cr, b, "All of them", if self.only_this { ButtonKind::Outline } else { ButtonKind::Primary }, hb, editable, &p);
            self.hits.push((Hit::This, a));
            self.hits.push((Hit::All, b));
        }

        // Buttons, the error on the left.
        let by = card.y + card.h - PAD - 40.0;
        let save = Rect { x: x + w - 120.0, y: by, w: 120.0, h: 40.0 };
        let cancel = Rect { x: save.x - 10.0 - 100.0, y: by, w: 100.0, h: 40.0 };
        let mut left_edge = x;
        if self.read_only {
            let hv = self.level(Hit::Cancel, style);
            uikit::button(cr, save, "Close", ButtonKind::Primary, hv, true, &p);
            self.hits.push((Hit::Cancel, save));
        } else {
            let (hc, hs) = (self.level(Hit::Cancel, style), self.level(Hit::Save, style));
            uikit::button(cr, cancel, "Cancel", ButtonKind::Outline, hc, self.busy.is_none(), &p);
            let label = if self.busy.is_some() { "Saving\u{2026}" } else { "Save" };
            uikit::button(cr, save, label, ButtonKind::Primary, hs, editable, &p);
            self.hits.push((Hit::Cancel, cancel));
            self.hits.push((Hit::Save, save));
            if self.editing.is_some() {
                let del = Rect { x, y: by, w: 110.0, h: 40.0 };
                let hd = self.level(Hit::Delete, style);
                uikit::button(cr, del, if self.confirm_delete { "Really delete?" } else { "Delete" }, ButtonKind::Danger, hd, editable, &p);
                self.hits.push((Hit::Delete, del));
                left_edge = del.x + del.w + 12.0;
            }
        }
        if let Some(e) = &self.error {
            let st = TextStyle { family: &p.family, px: 12.0, bold: false, color: p.error };
            let room = Rect { x: left_edge, y: by - 34.0, w: x + w - left_edge, h: 28.0 };
            draw_in_box(cr, e, &st, room.x, room.y, room.w, room.h, Align::Start);
        }

        // The calendar list, over everything.
        if let (Some(dd), Some(cals)) = (self.dropdown.as_mut(), self.calendars.as_ref()) {
            let names: Vec<String> = cals.iter().map(|c| if c.account.is_empty() { c.name.clone() } else { format!("{} \u{b7} {}", c.name, c.account) }).collect();
            dd.draw(cr, self.cal_rect, card, &names, Some(self.chosen), false, &p);
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let before = self.hovered();
        self.pointer = (x, y);
        if let Some(dd) = self.dropdown.as_mut() {
            let was = dd.hover;
            dd.hover = dd.rows.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i);
            if dd.hover != was {
                return true;
            }
        }
        before != self.hovered()
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        if let Some(dd) = self.dropdown.take() {
            if let Some(i) = dd.pick(x, y) {
                self.chosen = i;
            } else if dd.contains(x, y) {
                self.dropdown = Some(dd);
            }
            return Effect::Redraw;
        }
        let Some(h) = self.hit(x, y) else { return Effect::None };
        if h != Hit::Delete {
            self.confirm_delete = false;
        }
        if h == Hit::Cancel {
            return Effect::Close;
        }
        if !self.editable() {
            return Effect::None;
        }
        match h {
            Hit::Title => self.focus = Focus::Title,
            Hit::Location => self.focus = Focus::Location,
            Hit::Start => self.focus = Focus::Start,
            Hit::End => self.focus = Focus::End,
            Hit::DayPrev | Hit::DayNext => {
                let d = if h == Hit::DayPrev { -1 } else { 1 };
                let span = self.until.max(self.day) - self.day;
                self.day += d;
                self.until = self.day + span;
            }
            Hit::UntilPrev => self.until = (self.until.max(self.day) - 1).max(self.day),
            Hit::UntilNext => self.until = self.until.max(self.day) + 1,
            Hit::AllDay => {
                self.all_day = !self.all_day;
                if self.focus == Focus::Start || self.focus == Focus::End {
                    self.focus = Focus::Title;
                }
            }
            Hit::Calendar => {
                if self.calendars.as_ref().is_some_and(|c| c.len() > 1) {
                    self.dropdown = Some(Dropdown::default());
                }
            }
            Hit::This => self.only_this = true,
            Hit::All => self.only_this = false,
            Hit::Delete => return self.delete(),
            Hit::Save => return self.save(),
            Hit::Cancel => {}
        }
        self.error = None;
        Effect::Redraw
    }

    fn scroll(&mut self, delta: f64) -> bool {
        match self.dropdown.as_mut() {
            Some(dd) => {
                dd.scroll_by(delta);
                true
            }
            None => false,
        }
    }

    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        if let Some(dd) = self.dropdown.as_mut() {
            let names: Vec<String> = self.calendars.as_deref().unwrap_or_default().iter().map(|c| c.name.clone()).collect();
            match special {
                ESC => self.dropdown = None,
                ENTER => {
                    if let Some(i) = dd.current(&names) {
                        self.chosen = i;
                    }
                    self.dropdown = None;
                }
                _ => {
                    dd.key(special, typed, ctrl);
                }
            }
            return Effect::Redraw;
        }
        match special {
            ESC => return Effect::Close,
            ENTER if self.read_only => return Effect::Close,
            ENTER => return self.save(),
            TAB => {
                let order: Vec<Focus> = if self.all_day { vec![Focus::Title, Focus::Location] } else { vec![Focus::Title, Focus::Start, Focus::End, Focus::Location] };
                let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
                let n = order.len();
                self.focus = order[if shift { (i + n - 1) % n } else { (i + 1) % n }];
                return Effect::Redraw;
            }
            _ => {}
        }
        if !self.editable() {
            return Effect::None;
        }
        let f = self.focus;
        match self.query(f).key(special, typed, ctrl) {
            Some(_) => {
                self.error = None;
                self.confirm_delete = false;
                Effect::Redraw
            }
            None => Effect::None,
        }
    }

    fn poll(&mut self) -> bool {
        let mut redraw = false;
        if let Some(rx) = &self.cal_rx {
            if let Ok(r) = rx.try_recv() {
                self.cal_rx = None;
                redraw = true;
                match r {
                    Ok(list) => {
                        let cfg = Config::load();
                        self.chosen = list
                            .iter()
                            .position(|c| c.uid == cfg.calendar_default)
                            .or_else(|| list.iter().position(|c| !c.account.is_empty()))
                            .or_else(|| list.iter().position(|c| c.uid == eds::LOCAL))
                            .unwrap_or(0);
                        if list.is_empty() && self.editing.is_some() {
                            self.read_only = true;
                        }
                        self.calendars = Some(list);
                    }
                    Err(e) => {
                        self.calendars = Some(Vec::new());
                        self.error = Some(e);
                        if self.editing.is_some() {
                            self.read_only = true;
                        }
                    }
                }
            }
        }
        if let Some(rx) = &self.busy {
            if let Ok(r) = rx.try_recv() {
                self.busy = None;
                redraw = true;
                match r {
                    Ok(()) => self.done = true,
                    Err(e) => self.error = Some(e),
                }
            }
        }
        redraw
    }

    fn wants_close(&self) -> bool {
        self.done
    }

    fn clickable(&self) -> bool {
        self.hovered().is_some_and(|h| !matches!(h, Hit::Title | Hit::Location | Hit::Start | Hit::End))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("9"), Some(9 * 3600));
        assert_eq!(parse_time("09:30"), Some(9 * 3600 + 1800));
        assert_eq!(parse_time("930"), Some(9 * 3600 + 1800));
        assert_eq!(parse_time("9.15 pm"), Some(21 * 3600 + 900));
        assert_eq!(parse_time("12am"), Some(0));
        assert_eq!(parse_time("12pm"), Some(12 * 3600));
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(parse_time("x"), None);
    }

    /// `PRISM_EVENT_RENDER_DIR=<dir> cargo test --release -p prism-ui render_event_card -- --nocapture`
    #[test]
    fn render_event_card() {
        let Ok(dir) = std::env::var("PRISM_EVENT_RENDER_DIR") else { return };
        let theme = Theme::load();
        let style = Style::new(&Config::load(), &theme);
        let t = local_instant(local_parts(now()).0 + 1, 0);
        let rep = Occurrence { start: t + 9 * 3600, end: t + 10 * 3600, all_day: false, summary: "Standup".into(), location: "Room 2".into(), feed: 0, calendar: eds::LOCAL.into(), uid: "u".into(), rid: "20261124T090000Z".into(), color: String::new() };
        let feed = Occurrence { calendar: String::new(), rid: String::new(), ..rep.clone() };
        for (name, req, all_day) in [("new", Request::New(t), false), ("new-allday", Request::New(t), true), ("repeating", Request::Edit(rep), false), ("feed", Request::Edit(feed), false)] {
            open(req);
            let mut v = EventView::new(Waker::new().unwrap());
            v.all_day = all_day;
            let start = std::time::Instant::now();
            while (v.cal_rx.is_some()) && start.elapsed().as_secs() < 10 {
                v.poll();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let card = Rect { x: 20.0, y: 20.0, w: CARD_W, h: CARD_H };
            unsafe {
                let surf = cb::cairo_image_surface_create(0, (CARD_W + 40.0) as i32, (CARD_H + 40.0) as i32);
                let cr = cb::cairo_create(surf);
                cb::cairo_set_source_rgba(cr, 0.1, 0.1, 0.12, 1.0);
                cb::cairo_paint(cr);
                v.draw(cr, card, &style, &theme);
                let path = std::ffi::CString::new(format!("{dir}/event-{name}.png")).unwrap();
                cb::cairo_surface_write_to_png(surf, path.as_ptr());
                cb::cairo_destroy(cr);
                cb::cairo_surface_destroy(surf);
            }
        }
    }

    #[test]
    fn weekday_names() {
        // 1970-01-01 was a Thursday; 2026-11-24 is a Tuesday.
        assert_eq!(WEEKDAYS[prism_common::calendar::days_from_civil(2026, 11, 24).rem_euclid(7) as usize], "Tue");
    }
}
