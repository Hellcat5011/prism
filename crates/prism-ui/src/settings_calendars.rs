//! Settings > Desktop > Calendars: the calendar feeds kept in the keyring
//! (`prism_common::keyring`). The list is hidden until the user unlocks it: the keyring's own
//! password prompt when the keyring is locked, else their password (checked with PAM by
//! `prism-lock --check-auth`, which reads it from a pipe). Addresses are typed into a masked
//! field of a prompt card and never shown again; only names are listed.
//!
//! Keyring and PAM calls run on threads; results come back through a channel that
//! `SettingsView::tick` reads (`poll`).

use std::sync::mpsc::{channel, Receiver, Sender};

use prism_common::keyring::{self, Feed};

use crate::prompt::{self, Field, Request, Verdict};
use crate::waker::Waker;

enum Msg {
    /// How many calendars there are (known without unlocking).
    Count(usize),
    /// Unlocked: the list.
    Shown(Vec<Feed>),
    /// Added or removed one: the new list and what to say.
    Changed(Vec<Feed>, String),
    /// Something failed (shown as the Settings message).
    Error(String),
    /// The user closed a prompt.
    Idle,
}

pub struct Calendars {
    /// Unlocked in this Settings window.
    pub shown: bool,
    pub list: Vec<Feed>,
    pub count: Option<usize>,
    /// A prompt or keyring call is under way.
    pub busy: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    waker: Waker,
}

/// Button indices in the Calendars row: 0 is "Show calendars" / "Add calendar" (hidden list)
/// or "+ Add new" (shown list); 1 + k removes calendar k.
pub const MAIN: usize = 0;

impl Calendars {
    pub fn new(waker: Waker) -> Self {
        let (tx, rx) = channel();
        let c = Calendars { shown: false, list: Vec::new(), count: None, busy: false, tx, rx, waker };
        c.spawn(|| keyring::count().map(Msg::Count).unwrap_or(Msg::Count(0)));
        c
    }

    fn spawn(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let _ = tx.send(f());
            waker.wake();
        });
    }

    /// Apply finished work. Returns (redraw, a message for Settings: (text, ok)).
    pub fn poll(&mut self) -> (bool, Option<(String, bool)>) {
        let mut redraw = false;
        let mut message = None;
        for m in self.rx.try_iter() {
            redraw = true;
            match m {
                Msg::Count(n) => self.count = Some(n),
                Msg::Shown(list) => {
                    self.busy = false;
                    self.shown = true;
                    self.count = Some(list.len());
                    self.list = list;
                }
                Msg::Changed(list, text) => {
                    self.busy = false;
                    self.count = Some(list.len());
                    self.list = list;
                    message = Some((text, true));
                }
                Msg::Error(e) => {
                    self.busy = false;
                    message = Some((e, false));
                }
                Msg::Idle => self.busy = false,
            }
        }
        (redraw, message)
    }

    /// Button labels in order (see [`MAIN`]).
    pub fn labels(&self) -> Vec<String> {
        if !self.shown {
            let label = if self.busy {
                "Waiting for your password\u{2026}".to_string()
            } else {
                match self.count {
                    Some(n) if n > 0 => format!("Show calendars ({n})"),
                    _ => "Add calendar".to_string(),
                }
            };
            return vec![label];
        }
        let mut v = vec!["+ Add new".to_string()];
        v.extend(self.list.iter().map(|_| "Remove".to_string()));
        v
    }

    pub fn press(&mut self, b: usize) {
        if self.busy {
            return;
        }
        self.busy = true;
        if !self.shown {
            // Unlock first; with no calendars yet, go straight on to adding one.
            let add_after = self.count.unwrap_or(0) == 0;
            self.spawn(move || match unlock() {
                Ok(true) if add_after => match add() {
                    Ok(Some(name)) => keyring::feeds().map(|l| Msg::Changed(l, format!("Added {name}"))).unwrap_or_else(Msg::Error),
                    Ok(None) => keyring::feeds().map(Msg::Shown).unwrap_or_else(Msg::Error),
                    Err(e) => Msg::Error(e),
                },
                Ok(true) => keyring::feeds().map(Msg::Shown).unwrap_or_else(Msg::Error),
                Ok(false) => Msg::Idle,
                Err(e) => Msg::Error(e),
            });
            // Shown once unlocked, even when the add prompt is then closed.
            return;
        }
        if b == MAIN {
            self.spawn(|| match add() {
                Ok(Some(name)) => keyring::feeds().map(|l| Msg::Changed(l, format!("Added {name}"))).unwrap_or_else(Msg::Error),
                Ok(None) => Msg::Idle,
                Err(e) => Msg::Error(e),
            });
        } else if let Some(feed) = self.list.get(b - 1).cloned() {
            self.spawn(move || match remove(&feed) {
                Ok(true) => keyring::feeds().map(|l| Msg::Changed(l, format!("Removed {}", feed.name))).unwrap_or_else(Msg::Error),
                Ok(false) => Msg::Idle,
                Err(e) => Msg::Error(e),
            });
        } else {
            self.busy = false;
        }
    }
}

/// Tell prismd to read the keyring again.
fn bump() {
    let mut c = prism_common::Config::load();
    c.calendars_changed = c.calendars_changed.wrapping_add(1);
    if let Err(e) = c.save() {
        eprintln!("[prism-ui] calendars: {e}");
    }
}

/// The keyring's password when it is locked, else the user's password. True when unlocked.
fn unlock() -> Result<bool, String> {
    if keyring::locked()? {
        return keyring::unlock();
    }
    let Some(session) = prompt::ask(Request {
        title: "Calendars".into(),
        message: "Enter your password to see and change your calendars.".into(),
        detail: "Their addresses are kept in your keyring.".into(),
        icon: "lock",
        fields: vec![Field { label: "Password".into(), secret: true, value: String::new() }],
        ok_label: "Unlock".into(),
        choices: Vec::new(),
    }) else {
        return Err("cannot ask for a password".into());
    };
    loop {
        match session.answers.recv() {
            Ok(Some(answers)) => {
                let pw = answers.into_iter().next().unwrap_or_default();
                if check_password(&pw) {
                    session.verdict(Verdict::Done);
                    return Ok(true);
                }
                session.verdict(Verdict::Retry("Wrong password. Try again.".into()));
            }
            _ => return Ok(false),
        }
    }
}

/// PAM check of the user's password through `prism-lock --check-auth` (password on its stdin).
fn check_password(pw: &str) -> bool {
    use std::io::Write;
    if pw.is_empty() {
        return false;
    }
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("prism-lock"))).filter(|p| p.exists()).unwrap_or_else(|| "prism-lock".into());
    let Ok(mut child) = std::process::Command::new(exe)
        .arg("--check-auth")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{pw}");
    }
    child.wait().is_ok_and(|s| s.success())
}

/// Ask for a name and an address and store them. The added name, or None when closed.
fn add() -> Result<Option<String>, String> {
    let Some(session) = prompt::ask(Request {
        title: "Add calendar".into(),
        message: "Paste the calendar's iCalendar (.ics) address. It is saved in your keyring and not shown again.".into(),
        detail: "Google Calendar: Settings > your calendar > Integrate calendar > Secret address in iCal format. Also Nextcloud, iCloud and Outlook share links, or a .ics file.".into(),
        icon: "key",
        fields: vec![Field { label: "Name (optional)".into(), secret: false, value: String::new() }, Field { label: "Address".into(), secret: true, value: String::new() }],
        ok_label: "Add".into(),
        choices: Vec::new(),
    }) else {
        return Err("cannot open the add card".into());
    };
    loop {
        match session.answers.recv() {
            Ok(Some(answers)) => {
                let name = answers.first().map(|s| s.trim().to_string()).unwrap_or_default();
                let raw = answers.get(1).cloned().unwrap_or_default();
                let Some(address) = keyring::normalize_address(&raw) else {
                    session.verdict(Verdict::Retry("That is not a calendar address: paste an https:// or webcal:// link, or the path of a .ics file.".into()));
                    continue;
                };
                let name = if name.is_empty() { keyring::suggested_name(&address) } else { name };
                match keyring::add(&name, &address) {
                    Ok(_) => {
                        session.verdict(Verdict::Done);
                        bump();
                        return Ok(Some(name));
                    }
                    Err(e) => {
                        session.verdict(Verdict::Cancel);
                        return Err(e);
                    }
                }
            }
            _ => return Ok(None),
        }
    }
}

/// Confirm and delete. True when deleted.
fn remove(feed: &Feed) -> Result<bool, String> {
    let Some(session) = prompt::ask(Request {
        title: "Remove calendar".into(),
        message: format!("Remove \u{201c}{}\u{201d}? Its address is deleted from your keyring and its events leave the Calendar widget.", feed.name),
        detail: String::new(),
        icon: "key",
        fields: Vec::new(),
        ok_label: "Remove".into(),
        choices: Vec::new(),
    }) else {
        return Err("cannot ask".into());
    };
    match session.answers.recv() {
        Ok(Some(_)) => match keyring::remove(&feed.id) {
            Ok(()) => {
                session.verdict(Verdict::Done);
                bump();
                Ok(true)
            }
            Err(e) => {
                session.verdict(Verdict::Cancel);
                Err(e)
            }
        },
        _ => Ok(false),
    }
}
