//! Calendar events for the UI: the occurrences prismd writes to ~/.cache/prism/calendar.json
//! (see `prism_common::calendar`), re-read when the file changes (checked at most every two
//! seconds, so drawing never waits on it).

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use prism_common::calendar::{self, Occurrence};

struct Cache {
    list: Arc<Vec<Occurrence>>,
    mtime: Option<SystemTime>,
    checked: Option<Instant>,
    generation: u64,
}

static CACHE: std::sync::LazyLock<Mutex<Cache>> = std::sync::LazyLock::new(|| Mutex::new(Cache { list: Arc::new(Vec::new()), mtime: None, checked: None, generation: 0 }));

fn refresh(c: &mut Cache) {
    if c.checked.is_some_and(|t| t.elapsed().as_secs() < 2) {
        return;
    }
    c.checked = Some(Instant::now());
    let mtime = std::fs::metadata(calendar::cache_path()).and_then(|m| m.modified()).ok();
    if mtime != c.mtime {
        c.mtime = mtime;
        c.list = Arc::new(calendar::load_cache());
        c.generation += 1;
    }
}

/// The cached occurrences, sorted by start.
pub fn snapshot() -> Arc<Vec<Occurrence>> {
    let mut c = CACHE.lock().unwrap();
    refresh(&mut c);
    c.list.clone()
}

/// Changes when the events changed.
pub fn generation() -> u64 {
    let mut c = CACHE.lock().unwrap();
    refresh(&mut c);
    c.generation
}

/// Local (year, month 0..11, day, hour, minute) of a Unix time.
pub fn local(t: i64) -> (i32, u32, u32, u32, u32) {
    let tt = t as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&tt, &mut tm) };
    (tm.tm_year + 1900, tm.tm_mon as u32, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32)
}

/// Days (1..31) of a month that have an event, as a bit set.
pub fn days_with_events(year: i32, month0: u32) -> u64 {
    let mut bits = 0u64;
    for o in snapshot().iter() {
        // Every local day the occurrence touches (the end is exclusive).
        let mut t = o.start;
        let last = (o.end - 1).max(o.start);
        let mut guard = 0;
        loop {
            let (y, m, d, ..) = local(t);
            if y == year && m == month0 {
                bits |= 1 << d;
            }
            let (ly, lm, ld, ..) = local(last);
            if (y, m, d) == (ly, lm, ld) || guard > 62 {
                break;
            }
            t += 86400;
            guard += 1;
        }
    }
    bits
}

/// "10:30" in local time.
pub fn clock(t: i64) -> String {
    let (.., h, m) = local(t);
    format!("{h:02}:{m:02}")
}
