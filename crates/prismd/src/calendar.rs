//! Calendar feeds: their addresses are in the user's keyring (`prism_common::keyring`), added
//! and removed in Settings. Fetched every 15 minutes (curl for URLs, which reads the address
//! from its standard input so it never shows in the process list), expanded with
//! `prism_common::calendar` and written to `~/.cache/prism/calendar.json` (owner only) for the
//! Calendar widget and the notification centre. Reminders: a notification
//! `calendar_reminder_minutes` before each timed event.
//!
//! Calendars of online accounts (and the local one) come from evolution-data-server
//! (`prism_common::eds`), read every minute and right after Prism changed an event
//! (`IpcCommand::CalendarRefresh`); every 15 minutes EDS is asked to sync them with their
//! servers. `calendar_hidden` leaves calendars out.
//!
//! prismd never makes the keyring ask for its password. While it is locked, the events last
//! fetched stay, and one notification offers to unlock it. Old `calendar_feeds` entries in
//! config.json move into the keyring once it is unlocked, and are then removed from config.json
//! and from the Settings snapshots.

use std::collections::HashSet;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use prism_common::calendar::{self, Occurrence};
use prism_common::{eds, keyring, Config};

const REFRESH: Duration = Duration::from_secs(15 * 60);
/// While the keyring is locked (or not there yet at login), look again this often.
const RETRY: Duration = Duration::from_secs(60);
/// Occurrences kept: this far back and ahead (the Calendar widget pages through months).
const AHEAD: i64 = 366 * 86400;
const BACK: i64 = 366 * 86400;
/// evolution-data-server is local: read it this often.
const EDS_POLL: Duration = Duration::from_secs(60);

enum Msg {
    /// (old config feeds, reminder minutes, calendars_changed, hidden calendars) from the config.
    Settings(Vec<String>, u32, u64, Vec<String>),
    /// Prism changed an event: read evolution-data-server now.
    Eds,
    /// The keyring was unlocked from our notification: read it now.
    Refresh,
}

static TX: OnceLock<Mutex<Sender<Msg>>> = OnceLock::new();

pub fn apply(cfg: &Config) {
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel();
        std::thread::Builder::new().name("prism-calendar".into()).spawn(move || run(rx)).ok();
        Mutex::new(tx)
    });
    let _ = tx.lock().unwrap().send(Msg::Settings(cfg.calendar_feeds.clone(), cfg.calendar_reminder_minutes, cfg.calendars_changed, cfg.calendar_hidden.clone()));
}

/// `IpcCommand::CalendarRefresh`.
pub fn refresh_eds() {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(Msg::Eds);
    }
}

/// evolution-data-server's side: a connection kept open (EDS closes calendars whose client
/// left), the events last read, and when it was last asked to sync with the servers.
struct Eds {
    client: Option<eds::Client>,
    occ: Vec<Occurrence>,
    read: Option<Instant>,
    synced: Option<Instant>,
    logged: Option<String>,
}

impl Eds {
    /// Reads every shown calendar; true when the events changed. Feeds take colour slots
    /// 0..first, these the ones after.
    fn read(&mut self, hidden: &[String], first: usize) -> bool {
        self.read = Some(Instant::now());
        if !eds::available() {
            return false;
        }
        if self.client.is_none() {
            self.client = eds::Client::new().ok();
        }
        let Some(client) = self.client.as_mut() else { return false };
        let cals = match client.calendars() {
            Ok(c) => c,
            Err(e) => {
                if self.logged.as_ref() != Some(&e) {
                    eprintln!("[prismd/calendar] {e}");
                    self.logged = Some(e);
                }
                // A new connection next time (EDS may have restarted).
                self.client = None;
                return false;
            }
        };
        let sync = self.synced.is_none_or(|t| t.elapsed() >= REFRESH);
        if sync {
            self.synced = Some(Instant::now());
        }
        let t = now();
        let mut all = Vec::new();
        for (i, cal) in cals.iter().filter(|c| !hidden.contains(&c.uid)).enumerate() {
            if sync {
                client.refresh(&cal.uid);
            }
            match client.events(&cal.uid, t - BACK, t + AHEAD) {
                Ok(list) => {
                    let mut occ = calendar::occurrences(&list.concat(), first + i, t - BACK, t + AHEAD);
                    for o in &mut occ {
                        o.calendar = cal.uid.clone();
                        o.color = cal.color.clone();
                    }
                    all.extend(occ);
                }
                Err(e) => {
                    // Keep what was read before from this calendar.
                    let msg = format!("{}: {e}", cal.name);
                    if self.logged.as_ref() != Some(&msg) {
                        eprintln!("[prismd/calendar] {msg}");
                        self.logged = Some(msg);
                    }
                    all.extend(self.occ.iter().filter(|o| o.calendar == cal.uid).cloned());
                }
            }
        }
        all.sort_by_key(|o| o.start);
        let changed = all != self.occ;
        self.occ = all;
        changed
    }
}

fn refresh_now() {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(Msg::Refresh);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// curl's config syntax for one quoted value.
fn curl_quote(s: &str) -> String {
    let mut q = String::with_capacity(s.len() + 2);
    q.push('"');
    for c in s.chars() {
        match c {
            '\\' => q.push_str("\\\\"),
            '"' => q.push_str("\\\""),
            '\n' | '\r' => {}
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

fn fetch(feed: &str) -> Result<String, String> {
    use std::io::Write;
    let f = feed.trim();
    if f.starts_with('/') || f.starts_with("~/") {
        let path = f.strip_prefix("~/").map(|r| format!("{}/{r}", std::env::var("HOME").unwrap_or_default())).unwrap_or_else(|| f.to_string());
        return std::fs::read_to_string(&path).map_err(|e| format!("reading the file: {}", e.kind()));
    }
    let url = calendar::feed_url(f);
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("not an http(s) or webcal URL, or a file path".into());
    }
    // The address goes in through curl's config on stdin (-K -), not its arguments: a
    // secret address would otherwise be visible to every process on the machine.
    let mut child = std::process::Command::new("curl")
        .args(["-sfL", "--max-time", "30", "--max-filesize", "20000000", "--proto", "=http,https", "--proto-redir", "=https", "-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "url = {}", curl_quote(&url));
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("download failed (curl exit {})", out.status.code().unwrap_or(-1)));
    }
    String::from_utf8(out.stdout).map_err(|_| "not text".into())
}

/// Move old `calendar_feeds` from config.json into the keyring (only while it is unlocked),
/// then forget them in config.json and in the Settings snapshots. True when done.
fn migrate(old: &[String]) -> bool {
    if old.is_empty() || keyring::locked().unwrap_or(true) {
        return old.is_empty();
    }
    let have: Vec<String> = keyring::addresses().ok().flatten().map(|v| v.into_iter().map(|(_, a)| a).collect()).unwrap_or_default();
    for f in old {
        if have.iter().any(|a| a == f) {
            continue;
        }
        if let Err(e) = keyring::add(&keyring::suggested_name(f), f) {
            eprintln!("[prismd/calendar] moving a calendar into the keyring: {e}");
            return false;
        }
    }
    let mut cfg = Config::load();
    cfg.calendar_feeds.clear();
    cfg.calendars_changed = cfg.calendars_changed.wrapping_add(1);
    if let Err(e) = cfg.save() {
        eprintln!("[prismd/calendar] {e}");
        return false;
    }
    prism_common::config_history::forget_key("calendar_feeds");
    eprintln!("[prismd/calendar] moved {} calendar address(es) from config.json into the keyring", old.len());
    true
}

/// "Unlock the keyring to load your calendars", once per prismd run.
fn ask_unlock() {
    std::thread::spawn(|| {
        let Ok(session) = zbus::blocking::Connection::session() else { return };
        let answer = crate::obex::ask_as(&session, "Calendar", "Calendars are waiting for the keyring", "Unlock your keyring to load your calendars. The events loaded before stay meanwhile.", "x-office-calendar", &["unlock", "Unlock"], None);
        if matches!(answer.as_deref(), Some("unlock") | Some("default")) && keyring::unlock().unwrap_or(false) {
            refresh_now();
        }
    });
}

fn run(rx: std::sync::mpsc::Receiver<Msg>) {
    let mut old_feeds: Vec<String> = Vec::new();
    let mut changed_seen: Option<u64> = None;
    // The addresses to fetch; None while the keyring cannot be read.
    let mut feeds: Option<Vec<String>> = None;
    let mut remind_min: u32 = 10;
    // Last good text of each feed (a failed fetch keeps the old events).
    let mut texts: Vec<Option<String>> = Vec::new();
    let mut last_fetch: Option<Instant> = None;
    let mut last_read: Option<Instant> = None;
    let mut asked_unlock = false;
    let mut logged: Option<String> = None;
    let mut occ: Vec<Occurrence> = calendar::load_cache();
    let mut feed_occ: Vec<Occurrence> = occ.iter().filter(|o| o.calendar.is_empty()).cloned().collect();
    let mut eds = Eds { client: None, occ: occ.iter().filter(|o| !o.calendar.is_empty()).cloned().collect(), read: None, synced: None, logged: None };
    let mut hidden: Vec<String> = Vec::new();
    let mut reminded: HashSet<(i64, String)> = HashSet::new();
    let mut first_check = true;
    loop {
        let mut changed = false;
        // Read the keyring: at start, when Settings changed the calendars, every refresh, and
        // every minute while it is locked.
        let due = match feeds {
            None => last_read.is_none_or(|t| t.elapsed() >= RETRY),
            Some(_) => last_fetch.is_none_or(|t| t.elapsed() >= REFRESH),
        };
        if due {
            last_read = Some(Instant::now());
            // Locked: offer to unlock once (calendars to read, or old ones to move in).
            if !asked_unlock && keyring::locked().unwrap_or(false) && (!old_feeds.is_empty() || keyring::count().unwrap_or(0) > 0) {
                asked_unlock = true;
                ask_unlock();
            }
            let migrated = migrate(&old_feeds);
            if migrated {
                old_feeds.clear();
            }
            match keyring::addresses() {
                Ok(Some(list)) => {
                    let mut addrs: Vec<String> = list.into_iter().map(|(_, a)| a).collect();
                    // Not moved yet: keep using the old ones meanwhile.
                    let missing: Vec<String> = old_feeds.iter().filter(|f| !addrs.contains(f)).cloned().collect();
                    addrs.extend(missing);
                    if feeds.as_ref() != Some(&addrs) {
                        texts.clear();
                    }
                    feeds = Some(addrs);
                    logged = None;
                }
                Ok(None) => {
                    if !old_feeds.is_empty() {
                        feeds = Some(old_feeds.clone());
                    }
                }
                Err(e) => {
                    if logged.as_ref() != Some(&e) {
                        eprintln!("[prismd/calendar] {e}");
                        logged = Some(e);
                    }
                    if !old_feeds.is_empty() {
                        feeds = Some(old_feeds.clone());
                    }
                }
            }
            if let Some(list) = feeds.as_ref() {
                last_fetch = Some(Instant::now());
                texts.resize(list.len(), None);
                for (i, f) in list.iter().enumerate() {
                    match fetch(f) {
                        Ok(t) => texts[i] = Some(t),
                        Err(e) => eprintln!("[prismd/calendar] feed {}: {e}", i + 1),
                    }
                }
                let t = now();
                let mut all: Vec<Occurrence> = texts.iter().enumerate().filter_map(|(i, t)| t.as_ref().map(|t| (i, t))).flat_map(|(i, text)| calendar::occurrences(text, i, t - BACK, t + AHEAD)).collect();
                all.sort_by_key(|o| o.start);
                if all != feed_occ {
                    feed_occ = all;
                    changed = true;
                }
            }
        }
        if eds.read.is_none_or(|t| t.elapsed() >= EDS_POLL) {
            let first = feeds.as_ref().map_or(0, Vec::len);
            changed |= eds.read(&hidden, first);
        }
        if changed || !calendar::cache_path().exists() {
            let mut all: Vec<Occurrence> = feed_occ.iter().chain(eds.occ.iter()).cloned().collect();
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
            Ok(Msg::Eds) => eds.read = None,
            Ok(Msg::Settings(old, r, changed, h)) => {
                if h != hidden {
                    hidden = h;
                    eds.read = None;
                }
                if old != old_feeds {
                    old_feeds = old;
                    feeds = None;
                    last_read = None;
                }
                if changed_seen != Some(changed) {
                    // Settings added or removed a calendar: read the keyring again.
                    if changed_seen.is_some() {
                        feeds = None;
                        last_read = None;
                    }
                    changed_seen = Some(changed);
                }
                remind_min = r;
            }
            Ok(Msg::Refresh) => {
                feeds = None;
                last_read = None;
                asked_unlock = false;
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
        // Owner only: event titles and places are private.
        if prism_common::clipboard::write_private(&tmp, &json).is_ok() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_config_quoting() {
        assert_eq!(curl_quote("https://a.test/x?y=1&z=\"q\""), "\"https://a.test/x?y=1&z=\\\"q\\\"\"");
        assert_eq!(curl_quote("a\\b\nc"), "\"a\\\\bc\"");
    }
}
