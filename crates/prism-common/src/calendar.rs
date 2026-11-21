//! Calendar events from iCalendar (.ics) feeds: an http(s)/webcal URL (Google's "secret
//! address in iCal format", Nextcloud/Radicale/iCloud share links, Outlook's ICS link) or a local
//! file. prismd fetches the feeds (`calendar_feeds`), expands repeating events and writes the
//! occurrences of the coming weeks to `~/.cache/prism/calendar.json`; the Calendar widget and
//! the notification centre read that file.
//!
//! Supported: VEVENT with DTSTART/DTEND/DURATION (UTC, TZID or floating times, all-day dates),
//! RRULE (DAILY, WEEKLY with BYDAY, MONTHLY with BYMONTHDAY or BYDAY like 2TU/-1FR, YEARLY;
//! INTERVAL, COUNT, UNTIL), EXDATE, RECURRENCE-ID overrides and STATUS:CANCELLED. Time zones
//! come from the system's zoneinfo files.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One occurrence of an event. Times are Unix seconds; all-day events run from local
/// midnight to local midnight.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Occurrence {
    pub start: i64,
    pub end: i64,
    #[serde(default)]
    pub all_day: bool,
    pub summary: String,
    #[serde(default)]
    pub location: String,
    /// Which feed (its position in `calendar_feeds`), for colours.
    #[serde(default)]
    pub feed: usize,
}

pub fn cache_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/prism/calendar.json")
}

/// The cached occurrences (sorted by start), empty without a cache.
pub fn load_cache() -> Vec<Occurrence> {
    std::fs::read(cache_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

// ── Civil dates ─────────────────────────────────────────────────────────

/// Days since 1970-01-01 of a proleptic Gregorian date (month 1..12).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month 1..12, day) of days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 0 = Monday .. 6 = Sunday.
fn weekday(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

fn days_in_month(y: i64, m: u32) -> u32 {
    let next = if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) };
    (next - days_from_civil(y, m, 1)) as u32
}

// ── Time zones ──────────────────────────────────────────────────────────

/// A zone: UTC offsets from the system's TZif file.
#[derive(Debug, Clone)]
pub struct Zone {
    /// (transition time, offset after it), sorted.
    transitions: Vec<(i64, i32)>,
    /// Offset before the first transition (or always, without transitions).
    initial: i32,
}

impl Zone {
    pub fn utc() -> Zone {
        Zone { transitions: Vec::new(), initial: 0 }
    }

    /// The zone named `name` ("Europe/Berlin"), None if unknown.
    pub fn named(name: &str) -> Option<Zone> {
        let name = name.trim().trim_matches('"').trim_start_matches('/');
        if name.is_empty() || name.contains("..") {
            return None;
        }
        if matches!(name, "UTC" | "Etc/UTC" | "GMT" | "Z") {
            return Some(Zone::utc());
        }
        let data = std::fs::read(PathBuf::from("/usr/share/zoneinfo").join(name)).ok()?;
        parse_tzif(&data)
    }

    /// The system's local zone (/etc/localtime).
    pub fn local() -> Zone {
        std::fs::read("/etc/localtime").ok().and_then(|d| parse_tzif(&d)).unwrap_or_else(Zone::utc)
    }

    /// UTC offset (seconds) at the instant `t`.
    pub fn offset_at(&self, t: i64) -> i32 {
        match self.transitions.partition_point(|(tt, _)| *tt <= t) {
            0 => self.initial,
            i => self.transitions[i - 1].1,
        }
    }

    /// The instant of a wall-clock time (seconds since 1970 as if it were UTC) in this zone.
    pub fn to_utc(&self, wall: i64) -> i64 {
        let guess = wall - self.offset_at(wall) as i64;
        wall - self.offset_at(guess) as i64
    }
}

/// TZif v1 or v2+ (the 64-bit block when present).
fn parse_tzif(d: &[u8]) -> Option<Zone> {
    if d.len() < 44 || &d[..4] != b"TZif" {
        return None;
    }
    let be32 = |o: usize| -> Option<u32> { d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])) };
    let counts = |o: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (i, v) in c.iter_mut().enumerate() {
            *v = be32(o + 20 + 4 * i)? as usize;
        }
        Some(c)
    };
    // counts: isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
    let c1 = counts(0)?;
    let v1_len = 44 + c1[3] * 5 + c1[4] * 6 + c1[5] + c1[2] * 8 + c1[1] + c1[0];
    let (base, c, tsize) = if d[4] >= b'2' && d.len() > v1_len + 44 && &d[v1_len..v1_len + 4] == b"TZif" {
        (v1_len, counts(v1_len)?, 8)
    } else {
        (0, c1, 4)
    };
    let (timecnt, typecnt) = (c[3], c[4]);
    let times_at = base + 44;
    let idx_at = times_at + timecnt * tsize;
    let types_at = idx_at + timecnt;
    if d.len() < types_at + typecnt * 6 || typecnt == 0 {
        return None;
    }
    let offset_of = |ty: usize| -> i32 { be32(types_at + ty * 6).map(|v| v as i32).unwrap_or(0) };
    let mut transitions = Vec::with_capacity(timecnt);
    for i in 0..timecnt {
        let t = if tsize == 8 {
            i64::from_be_bytes(d[times_at + i * 8..times_at + i * 8 + 8].try_into().ok()?)
        } else {
            be32(times_at + i * 4)? as i32 as i64
        };
        let ty = d[idx_at + i] as usize;
        transitions.push((t, offset_of(ty.min(typecnt - 1))));
    }
    // Times after the last transition follow the POSIX rule in the footer; repeating the last
    // year's pattern is close enough here, so extend with yearly copies of the last two.
    if transitions.len() >= 2 {
        let n = transitions.len();
        let (a, b) = (transitions[n - 2], transitions[n - 1]);
        if a.1 != b.1 && b.0 - a.0 < 366 * 86400 {
            const YEAR: i64 = 31_556_952; // average Gregorian year
            for k in 1..=40i64 {
                // Snap to the same weekday as the original transition (DST rules use "last Sunday").
                let shift = |t: i64| {
                    let s = t + k * YEAR;
                    s - (weekday(s.div_euclid(86400)) as i64 - weekday(t.div_euclid(86400)) as i64).rem_euclid(7) * 86400
                };
                transitions.push((shift(a.0), a.1));
                transitions.push((shift(b.0), b.1));
            }
            transitions.sort_by_key(|t| t.0);
        }
    }
    Some(Zone { initial: offset_of(0), transitions })
}

// ── Parsing ─────────────────────────────────────────────────────────────

/// A DTSTART-like value: wall-clock seconds and how to read them.
#[derive(Debug, Clone, Copy, PartialEq)]
enum When {
    Date(i64),
    Utc(i64),
    /// Wall clock in a zone (index into the zone cache; None = floating, i.e. local).
    Wall(i64, Option<usize>),
}

impl When {
    fn wall(self) -> i64 {
        match self {
            When::Date(w) | When::Utc(w) | When::Wall(w, _) => w,
        }
    }

    fn with_wall(self, w: i64) -> When {
        match self {
            When::Date(_) => When::Date(w),
            When::Utc(_) => When::Utc(w),
            When::Wall(_, z) => When::Wall(w, z),
        }
    }
}

struct Zones {
    names: Vec<String>,
    zones: Vec<Zone>,
    local: Zone,
}

impl Zones {
    fn new() -> Self {
        Zones { names: Vec::new(), zones: Vec::new(), local: Zone::local() }
    }

    fn index(&mut self, name: &str) -> Option<usize> {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return Some(i);
        }
        // Outlook names ("W. Europe Standard Time") are not in zoneinfo: those read as local time.
        let z = Zone::named(name)?;
        self.names.push(name.to_string());
        self.zones.push(z);
        Some(self.names.len() - 1)
    }

    fn instant(&self, w: When) -> i64 {
        match w {
            When::Utc(t) => t,
            When::Date(t) | When::Wall(t, None) => self.local.to_utc(t),
            When::Wall(t, Some(i)) => self.zones[i].to_utc(t),
        }
    }
}

/// "20261121" or "20261121T101500" (Z suffix stripped by the caller) as wall seconds.
fn parse_stamp(v: &str) -> Option<(i64, bool)> {
    let v = v.trim();
    let (date, time) = match v.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (v, None),
    };
    if date.len() != 8 {
        return None;
    }
    let y: i64 = date[..4].parse().ok()?;
    let m: u32 = date[4..6].parse().ok()?;
    let d: u32 = date[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y, m, d) * 86400;
    if let Some(t) = time {
        let t = t.trim_end_matches('Z');
        if t.len() < 6 {
            return None;
        }
        let h: i64 = t[..2].parse().ok()?;
        let mi: i64 = t[2..4].parse().ok()?;
        let s: i64 = t[4..6].parse().ok()?;
        secs += h * 3600 + mi * 60 + s;
    }
    Some((secs, time.is_none()))
}

fn parse_when(params: &HashMap<String, String>, value: &str, zones: &mut Zones) -> Option<When> {
    let (wall, date_only) = parse_stamp(value)?;
    if date_only || params.get("VALUE").is_some_and(|v| v == "DATE") {
        return Some(When::Date(wall - wall.rem_euclid(86400)));
    }
    if value.trim().ends_with('Z') {
        return Some(When::Utc(wall));
    }
    Some(When::Wall(wall, params.get("TZID").and_then(|z| zones.index(z))))
}

/// "PT1H30M", "P1D", "P2W" in seconds.
fn parse_duration(v: &str) -> Option<i64> {
    let v = v.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Default, Debug)]
struct Event {
    uid: String,
    summary: String,
    location: String,
    start: Option<When>,
    end: Option<When>,
    duration: Option<i64>,
    rrule: Option<String>,
    exdates: Vec<When>,
    recurrence_id: Option<When>,
    cancelled: bool,
}

/// Content lines of an .ics file: unfolded, split into (NAME, params, value).
fn content_lines(text: &str) -> Vec<(String, HashMap<String, String>, String)> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(cont) = raw.strip_prefix(' ').or_else(|| raw.strip_prefix('\t')) {
            if let Some(last) = lines.last_mut() {
                last.push_str(cont);
                continue;
            }
        }
        lines.push(raw.to_string());
    }
    lines
        .into_iter()
        .filter_map(|l| {
            // The value starts at the first ':' outside a quoted parameter.
            let mut quoted = false;
            let colon = l.char_indices().find(|(_, c)| {
                if *c == '"' {
                    quoted = !quoted;
                }
                *c == ':' && !quoted
            })?.0;
            let (head, value) = (&l[..colon], &l[colon + 1..]);
            let mut parts = head.split(';');
            let name = parts.next()?.trim().to_uppercase();
            let params = parts.filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_uppercase(), v.trim().trim_matches('"').to_string())).collect();
            Some((name, params, value.to_string()))
        })
        .collect()
}

fn parse_events(text: &str, zones: &mut Zones) -> Vec<Event> {
    let mut out = Vec::new();
    let mut cur: Option<Event> = None;
    let mut depth = 0; // nested components inside a VEVENT (VALARM)
    for (name, params, value) in content_lines(text) {
        match (name.as_str(), value.trim()) {
            ("BEGIN", "VEVENT") => cur = Some(Event::default()),
            ("END", "VEVENT") => {
                if let Some(e) = cur.take() {
                    out.push(e);
                }
            }
            ("BEGIN", _) if cur.is_some() => depth += 1,
            ("END", _) if cur.is_some() => depth -= 1,
            _ => {}
        }
        let Some(e) = cur.as_mut() else { continue };
        if depth > 0 {
            continue;
        }
        match name.as_str() {
            "UID" => e.uid = value.trim().to_string(),
            "SUMMARY" => e.summary = unescape(value.trim()),
            "LOCATION" => e.location = unescape(value.trim()),
            "DTSTART" => e.start = parse_when(&params, &value, zones),
            "DTEND" => e.end = parse_when(&params, &value, zones),
            "DURATION" => e.duration = parse_duration(&value),
            "RRULE" => e.rrule = Some(value.trim().to_uppercase()),
            "EXDATE" => {
                for v in value.split(',') {
                    if let Some(w) = parse_when(&params, v, zones) {
                        e.exdates.push(w);
                    }
                }
            }
            "RECURRENCE-ID" => e.recurrence_id = parse_when(&params, &value, zones),
            "STATUS" => e.cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
            _ => {}
        }
    }
    out
}

// ── Recurrence ──────────────────────────────────────────────────────────

const WEEKDAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// Wall-clock starts of the occurrences of `start` under `rule`, up to `until_wall` (and at
/// most a few thousand).
fn expand(start: When, rule: &str, zones: &Zones, until_instant: i64) -> Vec<When> {
    let parts: HashMap<&str, &str> = rule.split(';').filter_map(|p| p.split_once('=')).collect();
    let freq = parts.get("FREQ").copied().unwrap_or("");
    let interval = parts.get("INTERVAL").and_then(|v| v.parse::<i64>().ok()).unwrap_or(1).max(1);
    let count = parts.get("COUNT").and_then(|v| v.parse::<usize>().ok());
    let until = parts.get("UNTIL").and_then(|v| {
        let (w, date) = parse_stamp(v)?;
        Some(if v.trim().ends_with('Z') { w } else if date { zones.local.to_utc(w + 86399) } else { zones.local.to_utc(w) })
    });
    let byday: Vec<(i64, u32)> = parts
        .get("BYDAY")
        .map(|v| {
            v.split(',')
                .filter_map(|d| {
                    let d = d.trim();
                    let (n, wd) = d.split_at(d.len().saturating_sub(2));
                    let wd = WEEKDAYS.iter().position(|w| *w == wd)? as u32;
                    Some((n.parse::<i64>().unwrap_or(0), wd))
                })
                .collect()
        })
        .unwrap_or_default();
    let bymonthday: Vec<i64> = parts.get("BYMONTHDAY").map(|v| v.split(',').filter_map(|d| d.trim().parse().ok()).collect()).unwrap_or_default();

    let wall0 = start.wall();
    let day0 = wall0.div_euclid(86400);
    let tod = wall0.rem_euclid(86400);
    let (y0, m0, d0) = civil_from_days(day0);
    let mut out = Vec::new();
    let push = |day: i64, out: &mut Vec<When>| -> bool {
        if day < day0 {
            return true;
        }
        let w = start.with_wall(day * 86400 + tod);
        let t = zones.instant(w);
        if until.is_some_and(|u| t > u) || t > until_instant {
            return false;
        }
        out.push(w);
        count.is_none_or(|c| out.len() < c) && out.len() < 5000
    };
    let mut k: i64 = 0;
    loop {
        let more = match freq {
            "DAILY" => push(day0 + k * interval, &mut out),
            "WEEKLY" => {
                let week_monday = day0 - weekday(day0) as i64 + 7 * k * interval;
                let days: Vec<u32> = if byday.is_empty() { vec![weekday(day0)] } else { byday.iter().map(|b| b.1).collect() };
                let mut days = days;
                days.sort();
                days.into_iter().all(|wd| push(week_monday + wd as i64, &mut out))
            }
            "MONTHLY" => {
                let mi = (m0 as i64 - 1) + k * interval;
                let (y, m) = (y0 + mi.div_euclid(12), (mi.rem_euclid(12) + 1) as u32);
                let dim = days_in_month(y, m) as i64;
                let first = days_from_civil(y, m, 1);
                let mut days: Vec<i64> = Vec::new();
                if !byday.is_empty() {
                    for (n, wd) in &byday {
                        let all: Vec<i64> = (0..dim).map(|i| first + i).filter(|d| weekday(*d) == *wd).collect();
                        match *n {
                            0 => days.extend(all),
                            n if n > 0 => days.extend(all.get(n as usize - 1)),
                            n => days.extend(all.len().checked_sub((-n) as usize).and_then(|i| all.get(i))),
                        }
                    }
                } else {
                    let mds = if bymonthday.is_empty() { vec![d0 as i64] } else { bymonthday.clone() };
                    for md in mds {
                        let md = if md < 0 { dim + md + 1 } else { md };
                        if (1..=dim).contains(&md) {
                            days.push(first + md - 1);
                        }
                    }
                }
                days.sort();
                days.into_iter().all(|d| push(d, &mut out))
            }
            "YEARLY" => {
                let y = y0 + k * interval;
                if d0 <= days_in_month(y, m0) {
                    push(days_from_civil(y, m0, d0), &mut out)
                } else {
                    true
                }
            }
            _ => {
                push(day0, &mut out);
                false
            }
        };
        if !more || k > 20000 {
            break;
        }
        k += 1;
    }
    out
}

/// Occurrences of the events in `ics` that overlap [from, to) (Unix seconds), sorted.
pub fn occurrences(ics: &str, feed: usize, from: i64, to: i64) -> Vec<Occurrence> {
    let mut zones = Zones::new();
    let events = parse_events(ics, &mut zones);
    // Moved or cancelled single occurrences: (uid, instant of the original start).
    let overridden: Vec<(String, i64)> = events.iter().filter_map(|e| Some((e.uid.clone(), zones.instant(e.recurrence_id?)))).collect();
    let mut out = Vec::new();
    for e in &events {
        let Some(start) = e.start else { continue };
        let all_day = matches!(start, When::Date(_));
        let len = match (e.end, e.duration) {
            (Some(end), _) => zones.instant(end) - zones.instant(start),
            (None, Some(d)) => d,
            (None, None) => {
                if all_day {
                    86400
                } else {
                    0
                }
            }
        }
        .max(0);
        if e.cancelled {
            continue;
        }
        let starts = match (&e.rrule, e.recurrence_id) {
            (Some(rule), None) => expand(start, rule, &zones, to),
            _ => vec![start],
        };
        let ex: Vec<i64> = e.exdates.iter().map(|w| zones.instant(*w)).collect();
        for w in starts {
            let s = zones.instant(w);
            if ex.contains(&s) || (e.recurrence_id.is_none() && e.rrule.is_some() && overridden.iter().any(|(u, t)| *u == e.uid && *t == s)) {
                continue;
            }
            // All-day: the end is midnight of a later local date (DST-safe).
            let end = if all_day { zones.instant(w.with_wall(w.wall() + (len + 3600) / 86400 * 86400)) } else { s + len };
            if end <= from || s >= to {
                continue;
            }
            out.push(Occurrence { start: s, end: end.max(s), all_day, summary: if e.summary.is_empty() { "(No title)".into() } else { e.summary.clone() }, location: e.location.clone(), feed });
        }
    }
    out.sort_by_key(|o| o.start);
    out
}

/// webcal:// is https.
pub fn feed_url(feed: &str) -> String {
    match feed.trim().strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => feed.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i64, m: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60
    }

    #[test]
    fn civil_round_trip() {
        for z in [-800_000, -1, 0, 1, 20_000, 400_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(weekday(days_from_civil(2026, 11, 21)), 5); // Saturday
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_duration("P1D"), Some(86400));
        assert_eq!(parse_duration("P2W"), Some(14 * 86400));
    }

    #[test]
    fn weekly_byday_with_exdate_override_and_utc() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Stand\r\n up\r\nDTSTART:20261102T090000Z\r\nDTEND:20261102T091500Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=6\r\nEXDATE:20261104T090000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:a\r\nRECURRENCE-ID:20261109T090000Z\r\nSUMMARY:Moved\r\nDTSTART:20261109T130000Z\r\nDTEND:20261109T131500Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let o = occurrences(ics, 0, utc(2026, 1, 1, 0, 0), utc(2027, 1, 1, 0, 0));
        let starts: Vec<(i64, &str)> = o.iter().map(|o| (o.start, o.summary.as_str())).collect();
        assert_eq!(
            starts,
            vec![
                (utc(2026, 11, 2, 9, 0), "Standup"),
                (utc(2026, 11, 9, 13, 0), "Moved"),
                (utc(2026, 11, 11, 9, 0), "Standup"),
                (utc(2026, 11, 16, 9, 0), "Standup"),
                (utc(2026, 11, 18, 9, 0), "Standup"),
            ]
        );
        assert_eq!(o[0].end - o[0].start, 900);
    }

    #[test]
    fn monthly_last_friday_in_a_zone() {
        let ics = "BEGIN:VEVENT\nUID:b\nSUMMARY:Review\nDTSTART;TZID=Europe/Berlin:20260130T100000\nDURATION:PT1H\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20260801T000000Z\nEND:VEVENT\n";
        if Zone::named("Europe/Berlin").is_none() {
            return; // no zoneinfo on this machine
        }
        let o = occurrences(ics, 0, utc(2026, 1, 1, 0, 0), utc(2027, 1, 1, 0, 0));
        assert_eq!(o.len(), 7); // Jan..Jul
        assert_eq!(o[0].start, utc(2026, 1, 30, 9, 0)); // CET = UTC+1
        assert_eq!(o[5].start, utc(2026, 6, 26, 8, 0)); // CEST = UTC+2
    }

    #[test]
    fn all_day_and_yearly() {
        let ics = "BEGIN:VEVENT\nUID:c\nSUMMARY:Birthday\nDTSTART;VALUE=DATE:20200229\nRRULE:FREQ=YEARLY\nEND:VEVENT\nBEGIN:VEVENT\nUID:d\nSUMMARY:Off\nDTSTART;VALUE=DATE:20261123\nDTEND;VALUE=DATE:20261125\nEND:VEVENT\n";
        let o = occurrences(ics, 1, utc(2026, 1, 1, 0, 0), utc(2029, 1, 1, 0, 0));
        // Feb 29 only in leap years; the two-day event is one occurrence.
        assert_eq!(o.iter().filter(|o| o.summary == "Birthday").count(), 1);
        let off = o.iter().find(|o| o.summary == "Off").unwrap();
        assert!(off.all_day && off.feed == 1);
        assert!((off.end - off.start - 2 * 86400).abs() <= 3600);
    }
}
