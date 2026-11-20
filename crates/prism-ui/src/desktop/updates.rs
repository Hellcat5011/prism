//! Package updates for the bar's Updates module and the "updates available" notification.
//! A worker thread runs `checkupdates` (pacman-contrib: a private copy of the sync databases, so
//! no root and no partial upgrade) and the AUR helper's `-Qua`, every few hours and again after
//! an update run. Arch-based systems only; elsewhere the module stays empty.

use crate::waker::Waker;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, Once};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Updates {
    pub repo: u32,
    pub aur: u32,
}

impl Updates {
    pub fn total(&self) -> u32 {
        self.repo + self.aur
    }
}

static STATE: Mutex<(u64, Option<Updates>)> = Mutex::new((0, None));
static WANTED: AtomicBool = AtomicBool::new(false);
static NOTIFY: AtomicBool = AtomicBool::new(false);
static HOURS: AtomicU32 = AtomicU32::new(3);
static START: Once = Once::new();
/// Wakes the worker for a check now.
static RECHECK: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

fn installed(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

fn aur_helper() -> Option<&'static str> {
    ["yay", "paru"].into_iter().find(|h| installed(h))
}

/// Lines of output, or None when the command failed. `checkupdates` exits 2 for "none".
fn count(cmd: &str, args: &[&str]) -> Option<u32> {
    let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    match out.status.code() {
        Some(0) | Some(2) => Some(String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()).count() as u32),
        // yay/paru -Qua exit 1 when nothing is out of date.
        Some(1) if cmd != "checkupdates" => Some(0),
        _ => None,
    }
}

fn check() -> Option<Updates> {
    let repo = count("checkupdates", &[])?;
    let aur = aur_helper().and_then(|h| count(h, &["-Qua"])).unwrap_or(0);
    Some(Updates { repo, aur })
}

/// Whether anything wants the numbers (bar module, tray icon, notification), and whether to
/// notify.
pub fn configure(wanted: bool, notify: bool, hours: u32, waker: &Waker) {
    WANTED.store(wanted, Ordering::Relaxed);
    NOTIFY.store(notify, Ordering::Relaxed);
    HOURS.store(hours.clamp(1, 48), Ordering::Relaxed);
    if wanted && installed("checkupdates") {
        let waker = waker.clone();
        START.call_once(move || {
            std::thread::Builder::new().name("prism-updates".into()).spawn(move || run(waker)).ok();
        });
    }
}

fn run(waker: Waker) {
    // Let the session settle (network, mirrors) before the first check.
    std::thread::sleep(Duration::from_secs(60));
    let mut notified = 0;
    loop {
        if WANTED.load(Ordering::Relaxed) {
            if let Some(u) = check() {
                let mut st = STATE.lock().unwrap();
                if st.1 != Some(u) {
                    st.0 += 1;
                    st.1 = Some(u);
                    drop(st);
                    waker.wake();
                }
                // Tell once per new batch (not again for the same or fewer).
                if u.total() > notified && NOTIFY.load(Ordering::Relaxed) {
                    notify(u);
                }
                notified = u.total();
            }
        }
        let (lock, cv) = &RECHECK;
        let hours = HOURS.load(Ordering::Relaxed) as u64;
        let mut now = lock.lock().unwrap();
        if !*now {
            now = cv.wait_timeout(now, Duration::from_secs(hours * 3600)).unwrap().0;
        }
        *now = false;
    }
}

/// Check again now (after an update run).
pub fn recheck() {
    let (lock, cv) = &RECHECK;
    *lock.lock().unwrap() = true;
    cv.notify_one();
}

pub fn snapshot() -> Option<Updates> {
    STATE.lock().unwrap().1
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

/// Upgrade in a terminal, then check again when it closes.
pub fn run_update(command: &str) {
    let cmd = if !command.trim().is_empty() {
        command.trim().to_string()
    } else if let Some(h) = aur_helper() {
        h.to_string()
    } else {
        "sudo pacman -Syu".to_string()
    };
    let script = format!("{cmd}; echo; read -r -p 'Done. Press Enter to close.' _");
    let Some(mut term) = prism_common::apps::terminal_command(&["sh".into(), "-c".into(), script]) else {
        eprintln!("[prism-ui] updates: no terminal found (set $TERMINAL)");
        return;
    };
    std::thread::spawn(move || {
        // app_command runs it in a systemd scope in the foreground: wait for the window to close.
        if let Ok(mut child) = term.spawn() {
            let _ = child.wait();
        }
        recheck();
    });
}

fn notify(u: Updates) {
    std::thread::spawn(move || {
        use std::collections::HashMap;
        use zbus::zvariant::Value;
        let Ok(conn) = zbus::blocking::Connection::session() else { return };
        const D: &str = "org.freedesktop.Notifications";
        let Ok(proxy) = zbus::blocking::Proxy::new(&conn, D, "/org/freedesktop/Notifications", D) else { return };
        let Ok(signals) = proxy.receive_all_signals() else { return };
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from("prism-updates"));
        let body = match (u.repo, u.aur) {
            (r, 0) => format!("{r} package{} can be upgraded.", if r == 1 { "" } else { "s" }),
            (0, a) => format!("{a} AUR package{} can be upgraded.", if a == 1 { "" } else { "s" }),
            (r, a) => format!("{r} from the repositories and {a} from the AUR can be upgraded."),
        };
        let actions = vec!["update", "Update"];
        let Ok(reply) = conn.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Updates", 0u32, "system-software-update", "Updates available", body.as_str(), actions, hints, -1i32)) else { return };
        let Ok(id) = reply.body().deserialize::<u32>() else { return };
        for m in signals {
            let member = m.header().member().map(|s| s.to_string()).unwrap_or_default();
            if member == "ActionInvoked" {
                if let Ok((nid, action)) = m.body().deserialize::<(u32, String)>() {
                    if nid == id {
                        if action == "update" || action == "default" {
                            run_update(&prism_common::Config::load().bar.updates_command);
                        }
                        return;
                    }
                }
            } else if member == "NotificationClosed" && m.body().deserialize::<(u32, u32)>().is_ok_and(|(nid, _)| nid == id) {
                return;
            }
        }
    });
}
