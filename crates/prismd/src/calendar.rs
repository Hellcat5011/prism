//! Calendar feeds (`calendar_feeds`): fetched every 15 minutes (curl for URLs), expanded with
//! `prism_common::calendar` and written to `~/.cache/prism/calendar.json` for the Calendar
//! widget and the notification centre. Reminders: a notification `calendar_reminder_minutes`
//! before each timed event.

use std::collections::HashSet;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use prism_common::calendar::{self, Occurrence};
use prism_common::Config;

const REFRESH: Duration = Duration::from_secs(15 * 60);
/// Occurrences kept: from a day ago to this far ahead.
const AHEAD: i64 = 62 * 86400;

/// (feeds, reminder minutes) from the config.
type Settings = (Vec<String>, u32);

static TX: OnceLock<Mutex<Sender<Settings>>> = OnceLock::new();

pub fn apply(cfg: &Config) {
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel();
        std::thread::Builder::new().name("prism-calendar".into()).spawn(move || run(rx)).ok();
        Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send((cfg.calendar_feeds.clone(), cfg.calendar_reminder_minutes));
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn fetch(feed: &str) -> Result<String, String> {
    let f = feed.trim();
    if f.starts_with('/') || f.starts_with("~/") {
        let path = f.strip_prefix("~/").map(|r| format!("{}/{r}", std::env::var("HOME").unwrap_or_default())).unwrap_or_else(|| f.to_string());
        return std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"));
    }
    let url = calendar::feed_url(f);
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("not an http(s) or webcal URL, or a file path".into());
    }
    let out = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "30", "--max-filesize", "20000000", "--proto", "=http,https", "--", &url])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("download failed (curl exit {})", out.status.code().unwrap_or(-1)));
    }
    String::from_utf8(out.stdout).map_err(|_| "not text".into())
}

fn run(rx: std::sync::mpsc::Receiver<Settings>) {
    let mut feeds: Vec<String> = Vec::new();
    let mut remind_min: u32 = 10;
    // Last good text of each feed (a failed fetch keeps the old events).
    let mut texts: Vec<Option<String>> = Vec::new();
    let mut last_fetch: Option<Instant> = None;
    let mut occ: Vec<Occurrence> = calendar::load_cache();
    let mut reminded: HashSet<(i64, String)> = HashSet::new();
    let mut first_check = true;
    loop {
        if last_fetch.is_none_or(|t| t.elapsed() >= REFRESH) {
            last_fetch = Some(Instant::now());
            texts.resize(feeds.len(), None);
            for (i, f) in feeds.iter().enumerate() {
                match fetch(f) {
                    Ok(t) => texts[i] = Some(t),
                    Err(e) => eprintln!("[prismd/calendar] feed {}: {e}", i + 1),
                }
            }
            let t = now();
            let mut all: Vec<Occurrence> = texts.iter().enumerate().filter_map(|(i, t)| t.as_ref().map(|t| (i, t))).flat_map(|(i, text)| calendar::occurrences(text, i, t - 86400, t + AHEAD)).collect();
            all.sort_by_key(|o| o.start);
            if all != occ || !calendar::cache_path().exists() {
                occ = all;
                write_cache(&occ);
            }
        }
        // Reminders (not for events that started before prismd did).
        if remind_min > 0 {
            let t = now();
            let lead = remind_min as i64 * 60;
            for o in occ.iter().filter(|o| !o.all_day && o.start > t && o.start - lead <= t) {
                let key = (o.start, o.summary.clone());
                if reminded.insert(key) && !first_check {
                    remind(o, ((o.start - t + 59) / 60).max(1));
                }
            }
            reminded.retain(|(s, _)| *s > t - 86400);
        }
        first_check = false;
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok((f, r)) => {
                if f != feeds {
                    feeds = f;
                    texts.clear();
                    last_fetch = None;
                    if feeds.is_empty() {
                        occ.clear();
                        write_cache(&occ);
                    }
                }
                remind_min = r;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn write_cache(occ: &[Occurrence]) {
    let path = calendar::cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(json) = serde_json::to_vec(occ) {
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn remind(o: &Occurrence, minutes: i64) {
    let summary = o.summary.clone();
    let mut body = if minutes >= 60 { format!("In {} h {} min", minutes / 60, minutes % 60) } else { format!("In {minutes} min") };
    if !o.location.is_empty() {
        body.push_str(" \u{b7} ");
        body.push_str(&o.location);
    }
    std::thread::spawn(move || {
        let Ok(session) = zbus::blocking::Connection::session() else { return };
        crate::obex::ask_as(&session, "Calendar", &summary, &body, "x-office-calendar", &[], None);
    });
}
