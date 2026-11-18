//! Receiving files over Bluetooth (replaces blueman's OBEX agent). While
//! `bluetooth_receive_files` is on, prismd registers an org.bluez.obex.Agent1 with obexd
//! (bluez-obex, session bus). Each incoming push asks through a notification (Accept /
//! Decline, declined after a minute); accepted files land in obexd's cache folder and are moved
//! to `bluetooth_download_dir` when the transfer completes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use prism_common::Config;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const OBEX: &str = "org.bluez.obex";
const AGENT_PATH: &str = "/org/prism/obex/agent";
const NOTIF: &str = "org.freedesktop.Notifications";
const NOTIF_PATH: &str = "/org/freedesktop/Notifications";
const ASK_SECS: u64 = 60;

type Props = HashMap<String, OwnedValue>;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.obex.Error")]
enum ObexError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

struct Agent;

#[zbus::interface(name = "org.bluez.obex.Agent1")]
impl Agent {
    fn release(&self) {}

    /// Runs on this connection's executor; blocking it only delays obexd, which waits anyway.
    fn authorize_push(&self, transfer: OwnedObjectPath) -> Result<String, ObexError> {
        authorize(transfer.as_str()).ok_or_else(|| ObexError::Rejected("Declined".into()))
    }

    fn cancel(&self) {}
}

static WANTED: AtomicBool = AtomicBool::new(false);
static DOWNLOAD_DIR: Mutex<String> = Mutex::new(String::new());
static SUPERVISOR: std::sync::Once = std::sync::Once::new();

pub fn apply(cfg: &Config) {
    WANTED.store(cfg.bluetooth_receive_files, Ordering::SeqCst);
    *DOWNLOAD_DIR.lock().unwrap() = cfg.bluetooth_download_dir.clone();
    if cfg.bluetooth_receive_files {
        SUPERVISOR.call_once(|| {
            std::thread::Builder::new().name("prism-obex".into()).spawn(supervise).ok();
        });
    }
}

fn owner(c: &Connection, name: &str) -> Option<String> {
    let d = zbus::blocking::fdo::DBusProxy::new(c).ok()?;
    d.get_name_owner(name.try_into().ok()?).ok().map(|o| o.to_string())
}

fn supervise() {
    let mut registered: Option<(Connection, Option<String>)> = None;
    let mut logged = false;
    loop {
        let wanted = WANTED.load(Ordering::SeqCst);
        match (&registered, wanted) {
            (None, true) => match register() {
                Ok(c) => {
                    eprintln!("[prismd/obex] receiving Bluetooth files");
                    let o = owner(&c, OBEX);
                    registered = Some((c, o));
                    logged = false;
                }
                Err(e) => {
                    if !logged {
                        eprintln!("[prismd/obex] cannot register the OBEX agent ({e}); retrying");
                        logged = true;
                    }
                }
            },
            (Some((c, _)), false) => {
                let _ = c.call_method(Some(OBEX), "/org/bluez/obex", Some("org.bluez.obex.AgentManager1"), "UnregisterAgent", &(zbus::zvariant::ObjectPath::try_from(AGENT_PATH).unwrap(),));
                registered = None;
                eprintln!("[prismd/obex] stopped receiving Bluetooth files");
            }
            (Some((c, o)), true) => {
                // obexd exited or restarted (its name changed owner): register again. obexd is
                // D-Bus activated, so registering starts it again.
                if owner(c, OBEX) != *o {
                    registered = None;
                }
            }
            (None, false) => {}
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn register() -> Result<Connection, String> {
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.serve_at(AGENT_PATH, Agent))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    let path = zbus::zvariant::ObjectPath::try_from(AGENT_PATH).unwrap();
    // Activates obexd when it is not running yet.
    conn.call_method(Some(OBEX), "/org/bluez/obex", Some("org.bluez.obex.AgentManager1"), "RegisterAgent", &(path,)).map_err(|e| e.to_string())?;
    Ok(conn)
}

fn get_all(conn: &Connection, dest: &str, path: &str, iface: &str) -> Props {
    conn.call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "GetAll", &(iface,))
        .ok()
        .and_then(|m| m.body().deserialize::<Props>().ok())
        .unwrap_or_default()
}

fn str_of(p: &Props, k: &str) -> String {
    match p.get(k).map(|v| &**v) {
        Some(Value::Str(s)) => s.to_string(),
        Some(Value::ObjectPath(o)) => o.to_string(),
        _ => String::new(),
    }
}

fn u64_of(p: &Props, k: &str) -> u64 {
    p.get(k).and_then(|v| u64::try_from(&**v).ok()).unwrap_or(0)
}

/// Bluetooth alias of the device with `address` (system bus), else the address.
fn device_name(address: &str) -> String {
    let found = Connection::system().ok().and_then(|sys| {
        let m = sys.call_method(Some("org.bluez"), "/", Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &()).ok()?;
        let objs: HashMap<OwnedObjectPath, HashMap<String, Props>> = m.body().deserialize().ok()?;
        objs.values().filter_map(|ifs| ifs.get("org.bluez.Device1")).find(|d| str_of(d, "Address").eq_ignore_ascii_case(address)).map(|d| str_of(d, "Alias"))
    });
    found.filter(|s| !s.is_empty()).unwrap_or_else(|| address.to_string())
}

fn human_size(n: u64) -> String {
    match n {
        0 => String::new(),
        n if n < 1024 => format!(" ({n} B)"),
        n if n < 1 << 20 => format!(" ({:.0} KB)", n as f64 / 1024.0),
        n if n < 1 << 30 => format!(" ({:.1} MB)", n as f64 / (1u64 << 20) as f64),
        n => format!(" ({:.2} GB)", n as f64 / (1u64 << 30) as f64),
    }
}

/// Post a notification and wait up to `timeout` for one of `actions`; None on timeout or
/// dismissal (the notification is closed on timeout).
pub(crate) fn ask(conn: &Connection, summary: &str, body: &str, icon: &str, actions: &[&str], timeout: Option<Duration>) -> Option<String> {
    let proxy = zbus::blocking::Proxy::new(conn, NOTIF, NOTIF_PATH, NOTIF).ok()?;
    let signals = proxy.receive_all_signals().ok()?;
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from("prism-bluetooth"));
    if timeout.is_some() {
        hints.insert("urgency", Value::U8(2));
    }
    let reply = conn.call_method(Some(NOTIF), NOTIF_PATH, Some(NOTIF), "Notify", &("Bluetooth", 0u32, icon, summary, body, actions.to_vec(), hints, -1i32)).ok()?;
    let id: u32 = reply.body().deserialize().ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for m in signals {
            let member = m.header().member().map(|s| s.to_string()).unwrap_or_default();
            if member == "ActionInvoked" {
                if let Ok((nid, key)) = m.body().deserialize::<(u32, String)>() {
                    if nid == id {
                        let _ = tx.send(Some(key));
                        return;
                    }
                }
            } else if member == "NotificationClosed" && m.body().deserialize::<(u32, u32)>().is_ok_and(|(n, _)| n == id) {
                let _ = tx.send(None);
                return;
            }
        }
    });
    let r = match timeout {
        Some(t) => rx.recv_timeout(t).ok().flatten(),
        None => rx.recv().ok().flatten(),
    };
    if r.is_none() {
        let _ = conn.call_method(Some(NOTIF), NOTIF_PATH, Some(NOTIF), "CloseNotification", &(id,));
    }
    r
}

/// A notification without actions, posted from its own thread.
pub(crate) fn ask_detached(summary: &str, body: &str, icon: &str) {
    let (s, b, i) = (summary.to_string(), body.to_string(), icon.to_string());
    std::thread::spawn(move || {
        if let Ok(c) = Connection::session() {
            ask(&c, &s, &b, &i, &[], None);
        }
    });
}

fn authorize(transfer: &str) -> Option<String> {
    let conn = Connection::session().ok()?;
    let t = get_all(&conn, OBEX, transfer, "org.bluez.obex.Transfer1");
    let raw = str_of(&t, "Name");
    // A bare file name: the sender does not choose where it goes.
    let name = Path::new(&raw).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty() && n != "." && n != "..").unwrap_or_else(|| "received-file".into());
    let session = str_of(&t, "Session");
    let from = device_name(&str_of(&get_all(&conn, OBEX, &session, "org.bluez.obex.Session1"), "Destination"));
    let body = format!("{from} wants to send {name}{}", human_size(u64_of(&t, "Size")));
    let answer = ask(&conn, "Incoming file", &body, "bluetooth", &["accept", "Accept", "decline", "Decline"], Some(Duration::from_secs(ASK_SECS)));
    if answer.as_deref() != Some("accept") {
        eprintln!("[prismd/obex] declined {name} from {from}");
        return None;
    }
    // Subscribe before obexd starts the transfer (a small file finishes at once).
    let proxy = zbus::blocking::Proxy::new(&conn, OBEX, transfer.to_string(), "org.freedesktop.DBus.Properties").ok()?;
    let signals = proxy.receive_signal("PropertiesChanged").ok()?;
    let (transfer, from2, name2) = (transfer.to_string(), from.clone(), name.clone());
    std::thread::spawn(move || follow(conn, signals, &transfer, &name2, &from2));
    Some(name)
}

/// Wait for the transfer to finish, then move the file to the download folder and announce it.
fn follow(conn: Connection, signals: zbus::blocking::proxy::SignalIterator<'static>, transfer: &str, name: &str, from: &str) {
    let mut file = String::new();
    let mut status = String::new();
    for m in signals {
        let Ok((_, changed, _)) = m.body().deserialize::<(String, Props, Vec<String>)>() else { continue };
        if file.is_empty() {
            file = str_of(&get_all(&conn, OBEX, transfer, "org.bluez.obex.Transfer1"), "Filename");
        }
        let s = str_of(&changed, "Status");
        if s == "complete" || s == "error" {
            status = s;
            break;
        }
    }
    if status != "complete" || file.is_empty() {
        ask_detached("Transfer failed", &format!("{name} from {from} was not received."), "dialog-error");
        return;
    }
    let dir = prism_common::config::expand_home(&DOWNLOAD_DIR.lock().unwrap());
    let _ = std::fs::create_dir_all(&dir);
    let dest = unique(&dir, name);
    let moved = std::fs::rename(&file, &dest).is_ok() || (std::fs::copy(&file, &dest).is_ok() && std::fs::remove_file(&file).is_ok());
    let dest = if moved { dest } else { PathBuf::from(&file) };
    let body = format!("{name} from {from}\n{}", dest.display());
    match ask(&conn, "File received", &body, "bluetooth", &["open", "Open", "folder", "Show in folder"], None).as_deref() {
        Some("open") | Some("default") => {
            let _ = prism_common::apps::app_command("xdg-open", &[dest.to_string_lossy().into_owned()]).spawn();
        }
        Some("folder") => {
            let _ = prism_common::apps::app_command("xdg-open", &[dir.to_string_lossy().into_owned()]).spawn();
        }
        _ => {}
    }
}

/// `dir/name`, or `dir/stem (n).ext` when taken.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

// ── Sending (OBEX Object Push client) ────────────────────────────────────

/// Replace-able notification: returns its id.
fn note(conn: &Connection, replaces: u32, summary: &str, body: &str, icon: &str) -> u32 {
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from("prism-bluetooth"));
    conn.call_method(Some(NOTIF), NOTIF_PATH, Some(NOTIF), "Notify", &("Bluetooth", replaces, icon, summary, body, Vec::<&str>::new(), hints, -1i32))
        .ok()
        .and_then(|m| m.body().deserialize::<u32>().ok())
        .unwrap_or(0)
}

/// Send `files` to the device with `address` (bluez-obex), one after another, with a
/// notification for progress and the result. Runs on its own thread.
pub fn send(address: String, files: Vec<PathBuf>) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::session() else { return };
        let name = device_name(&address);
        let n = files.len();
        let mut id = note(&conn, 0, &format!("Sending to {name}"), &format!("{n} file{}\u{2026}", if n == 1 { "" } else { "s" }), "bluetooth");
        let mut args: HashMap<&str, Value> = HashMap::new();
        args.insert("Target", Value::from("opp"));
        let session = conn
            .call_method(Some(OBEX), "/org/bluez/obex", Some("org.bluez.obex.Client1"), "CreateSession", &(address.as_str(), args))
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>());
        let session = match session {
            Ok(s) => s,
            Err(e) => {
                note(&conn, id, &format!("Could not send to {name}"), &format!("{e}"), "dialog-error");
                return;
            }
        };
        let mut sent = 0;
        let mut failed: Vec<String> = Vec::new();
        for (k, f) in files.iter().enumerate() {
            let fname = f.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            id = note(&conn, id, &format!("Sending to {name}"), &format!("{fname} ({} of {n})", k + 1), "bluetooth");
            match push(&conn, session.as_str(), f) {
                Ok(()) => sent += 1,
                Err(e) => {
                    eprintln!("[prismd/obex] send {fname} to {address}: {e}");
                    failed.push(fname);
                }
            }
        }
        let _ = conn.call_method(Some(OBEX), "/org/bluez/obex", Some("org.bluez.obex.Client1"), "RemoveSession", &(session,));
        if failed.is_empty() {
            note(&conn, id, &format!("Sent to {name}"), &format!("{sent} file{}", if sent == 1 { "" } else { "s" }), "bluetooth");
        } else {
            note(&conn, id, &format!("Sending to {name} failed"), &format!("Sent {sent}; not sent: {}", failed.join(", ")), "dialog-error");
        }
    });
}

/// Push one file and wait for its transfer to finish.
fn push(conn: &Connection, session: &str, file: &Path) -> Result<(), String> {
    use zbus::message::Type as MsgType;
    use zbus::MatchRule;
    // Subscribe before starting so a quick transfer is not missed.
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(OBEX)
        .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
        .and_then(|b| b.member("PropertiesChanged"))
        .and_then(|b| b.path_namespace(session.to_string()))
        .map(|b| b.build())
        .map_err(|e| e.to_string())?;
    let signals = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64)).map_err(|e| e.to_string())?;
    let path = file.to_str().ok_or("non-UTF-8 path")?;
    let reply = conn.call_method(Some(OBEX), session, Some("org.bluez.obex.ObjectPush1"), "SendFile", &(path,)).map_err(|e| e.to_string())?;
    let (transfer, _): (OwnedObjectPath, Props) = reply.body().deserialize().map_err(|e| e.to_string())?;
    for m in signals {
        let Ok(m) = m else { continue };
        if m.header().path().map(|p| p.as_str() != transfer.as_str()).unwrap_or(true) {
            continue;
        }
        let Ok((_, changed, _)) = m.body().deserialize::<(String, Props, Vec<String>)>() else { continue };
        match str_of(&changed, "Status").as_str() {
            "complete" => return Ok(()),
            "error" => return Err("transfer failed (refused or out of range)".into()),
            _ => {}
        }
    }
    Err("lost the connection to obexd".into())
}
