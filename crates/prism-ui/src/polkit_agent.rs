//! Polkit authentication agent (replaces polkit-gnome). While `polkit_agent_enabled` is on,
//! prism-ui registers org.freedesktop.PolicyKit1.AuthenticationAgent for the user's graphical
//! session and asks for passwords with the prompt card (prompt.rs).
//!
//! The password is checked by polkit's helper: through `/run/polkit/agent-helper.socket`
//! (polkit 127+, socket-activated; user name and cookie are written first) or the setuid
//! `polkit-agent-helper-1 <user>` (cookie on stdin). The helper speaks a line protocol
//! (`PAM_PROMPT_ECHO_OFF ...`, `SUCCESS`, `FAILURE`) and reports the result to polkitd itself.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

use crate::prompt::{self, Field, Request, Verdict};

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AGENT_PATH: &str = "/org/prism/PolicyKit1/AuthenticationAgent";
const SOCKET: &str = "/run/polkit/agent-helper.socket";
const HELPER: &str = "/usr/lib/polkit-1/polkit-agent-helper-1";

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
}

/// Cookies of authentications cancelled by polkitd.
static CANCELLED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

struct Agent;

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), PolkitError> {
        // The dialog blocks; run it off the D-Bus executor so CancelAuthentication still arrives.
        blocking::unblock(move || authenticate(&action_id, &message, &cookie, &identities)).await
    }

    fn cancel_authentication(&self, cookie: String) {
        CANCELLED.lock().unwrap().get_or_insert_with(HashSet::new).insert(cookie);
    }
}

fn cancelled(cookie: &str) -> bool {
    CANCELLED.lock().unwrap().as_ref().is_some_and(|s| s.contains(cookie))
}

fn user_name(uid: u32) -> Option<String> {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned())
    }
}

/// The identity to authenticate as: the current user when allowed, else the first user listed.
fn pick_user(identities: &[(String, HashMap<String, OwnedValue>)]) -> Option<String> {
    let me = unsafe { libc::getuid() };
    let uids: Vec<u32> = identities
        .iter()
        .filter(|(kind, _)| kind == "unix-user")
        .filter_map(|(_, d)| d.get("uid").and_then(|v| u32::try_from(&**v).ok()))
        .collect();
    let uid = if uids.contains(&me) { me } else { *uids.first()? };
    user_name(uid)
}

fn authenticate(action_id: &str, message: &str, cookie: &str, identities: &[(String, HashMap<String, OwnedValue>)]) -> Result<(), PolkitError> {
    let result = run(action_id, message, cookie, identities);
    if let Some(s) = CANCELLED.lock().unwrap().as_mut() {
        s.remove(cookie);
    }
    result
}

fn run(action_id: &str, message: &str, cookie: &str, identities: &[(String, HashMap<String, OwnedValue>)]) -> Result<(), PolkitError> {
    let user = pick_user(identities).ok_or_else(|| PolkitError::Failed("no user identity to authenticate as".into()))?;
    let session = prompt::ask(Request {
        title: "Authentication Required".into(),
        message: message.to_string(),
        detail: action_id.to_string(),
        icon: "lock",
        fields: vec![Field { label: format!("Password for {user}"), secret: true, value: String::new() }],
        ok_label: "Authenticate".into(),
        choices: Vec::new(),
    })
    .ok_or_else(|| PolkitError::Failed("prompt unavailable".into()))?;
    loop {
        // Poll so a cancel from polkitd closes the card.
        let answer = loop {
            match session.answers.recv_timeout(Duration::from_millis(250)) {
                Ok(a) => break a,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancelled(cookie) => {
                    session.verdict(Verdict::Cancel);
                    return Err(PolkitError::Cancelled("cancelled by polkit".into()));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break None,
            }
        };
        let Some(password) = answer.and_then(|v| v.into_iter().next()) else {
            return Err(PolkitError::Cancelled("dismissed".into()));
        };
        match check(&user, cookie, &password) {
            Ok(true) => {
                session.verdict(Verdict::Done);
                return Ok(());
            }
            Ok(false) => session.verdict(Verdict::Retry("Wrong password. Try again.".into())),
            Err(e) => {
                eprintln!("[prism-ui/polkit] helper: {e}");
                session.verdict(Verdict::Retry(format!("Authentication failed: {e}")));
            }
        }
        if cancelled(cookie) {
            session.verdict(Verdict::Cancel);
            return Err(PolkitError::Cancelled("cancelled by polkit".into()));
        }
    }
}

/// One PAM conversation through polkit's helper. Ok(true) on SUCCESS.
fn check(user: &str, cookie: &str, password: &str) -> Result<bool, String> {
    let (mut w, r): (Box<dyn Write>, Box<dyn std::io::Read>) = if std::path::Path::new(SOCKET).exists() {
        let s = std::os::unix::net::UnixStream::connect(SOCKET).map_err(|e| format!("{SOCKET}: {e}"))?;
        let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
        let mut w = s.try_clone().map_err(|e| e.to_string())?;
        write!(w, "{user}\n{cookie}\n").map_err(|e| e.to_string())?;
        (Box::new(w), Box::new(s))
    } else {
        let mut child = std::process::Command::new(HELPER)
            .arg(user)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("{HELPER}: {e}"))?;
        let mut w = child.stdin.take().unwrap();
        writeln!(w, "{cookie}").map_err(|e| e.to_string())?;
        let r = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        (Box::new(w), Box::new(r))
    };
    let mut sent = false;
    for line in BufReader::new(r).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.starts_with("PAM_PROMPT_ECHO_OFF") || line.starts_with("PAM_PROMPT_ECHO_ON") {
            // One password per attempt; a second prompt (e.g. a new password) is answered empty.
            let reply = if sent { "" } else { password };
            sent = true;
            writeln!(w, "{reply}").map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
        } else if line == "SUCCESS" {
            return Ok(true);
        } else if line == "FAILURE" {
            return Ok(false);
        } else if let Some(m) = line.strip_prefix("PAM_ERROR_MSG ") {
            eprintln!("[prism-ui/polkit] {m}");
        }
    }
    Err("the helper closed the connection".into())
}

// ── Registration ─────────────────────────────────────────────────────────

/// The user's graphical session: XDG_SESSION_ID, else logind's User.Display (prism-ui runs as
/// a user service, outside any session).
fn session_id() -> Option<String> {
    if let Ok(id) = std::env::var("XDG_SESSION_ID") {
        return Some(id);
    }
    let sys = Connection::system().ok()?;
    let uid = unsafe { libc::getuid() };
    let reply = sys.call_method(Some("org.freedesktop.login1"), "/org/freedesktop/login1", Some("org.freedesktop.login1.Manager"), "GetUser", &(uid,)).ok()?;
    let user: zbus::zvariant::OwnedObjectPath = reply.body().deserialize().ok()?;
    let p = zbus::blocking::Proxy::new(&sys, "org.freedesktop.login1", user, "org.freedesktop.login1.User").ok()?;
    let (id, _): (String, zbus::zvariant::OwnedObjectPath) = p.get_property("Display").ok()?;
    (!id.is_empty()).then_some(id)
}

fn subject(id: &str) -> (&'static str, HashMap<&'static str, Value<'_>>) {
    let mut d = HashMap::new();
    d.insert("session-id", Value::from(id));
    ("unix-session", d)
}

/// Unique bus name of polkitd right now (changes when it restarts; it then forgets its agents).
fn authority_owner(conn: &Connection) -> Option<String> {
    let reply = conn.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "GetNameOwner", &(AUTHORITY,)).ok()?;
    reply.body().deserialize::<String>().ok()
}

fn register() -> Result<(Connection, String), String> {
    let id = session_id().ok_or("no graphical login session found")?;
    let conn = zbus::blocking::connection::Builder::system()
        .and_then(|b| b.serve_at(AGENT_PATH, Agent))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    let locale = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into());
    conn.call_method(Some(AUTHORITY), AUTHORITY_PATH, Some("org.freedesktop.PolicyKit1.Authority"), "RegisterAuthenticationAgent", &(subject(&id), locale.as_str(), AGENT_PATH))
        .map_err(|e| e.to_string())?;
    Ok((conn, id))
}

static WANTED: AtomicBool = AtomicBool::new(false);
static SUPERVISOR: std::sync::Once = std::sync::Once::new();

pub fn apply(cfg: &prism_common::Config) {
    WANTED.store(cfg.polkit_agent_enabled, Ordering::SeqCst);
    if cfg.polkit_agent_enabled {
        SUPERVISOR.call_once(|| {
            std::thread::Builder::new().name("prism-polkit".into()).spawn(supervise).ok();
        });
    }
}

fn supervise() {
    let mut reg: Option<(Connection, String)> = None;
    let mut owner: Option<String> = None;
    let mut logged = false;
    loop {
        let wanted = WANTED.load(Ordering::SeqCst);
        match (&reg, wanted) {
            (None, true) => match register() {
                Ok(r) => {
                    eprintln!("[prism-ui/polkit] authentication agent registered for session {}", r.1);
                    owner = authority_owner(&r.0);
                    reg = Some(r);
                    logged = false;
                }
                Err(e) => {
                    if !logged {
                        eprintln!("[prism-ui/polkit] cannot register ({e}); retrying");
                        logged = true;
                    }
                }
            },
            (Some((c, id)), false) => {
                let _ = c.call_method(Some(AUTHORITY), AUTHORITY_PATH, Some("org.freedesktop.PolicyKit1.Authority"), "UnregisterAuthenticationAgent", &(subject(id), AGENT_PATH));
                reg = None;
                eprintln!("[prism-ui/polkit] authentication agent unregistered");
            }
            (Some((c, _)), true) if authority_owner(c) != owner => {
                eprintln!("[prism-ui/polkit] polkitd restarted; registering again");
                reg = None;
                continue;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
