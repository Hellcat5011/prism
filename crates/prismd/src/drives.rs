//! Removable drives (replaces udiskie): while `drives_enabled` is on, prismd follows UDisks2 on
//! the system bus. A filesystem that appears on a removable drive (USB stick, SD card, phone in
//! mass-storage mode) is mounted when `drives_automount` is on, and announced with a
//! notification: Open shows it in the file manager, Eject unmounts every filesystem of the
//! drive and powers it off so it can be pulled out.
//!
//! Only drives that appear while Prism runs are announced (not the ones present at login), and
//! nothing UDisks marks as system or ignored. Encrypted volumes are left to the file manager.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::MatchRule;

use prism_common::Config;

const UDISKS: &str = prism_common::drives::UDISKS;
const FS: &str = "org.freedesktop.UDisks2.Filesystem";

static WANTED: AtomicBool = AtomicBool::new(false);
static AUTOMOUNT: AtomicBool = AtomicBool::new(true);
static SUPERVISOR: std::sync::Once = std::sync::Once::new();
/// Block devices announced (so a re-added interface is not announced twice).
static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn apply(cfg: &Config) {
    WANTED.store(cfg.drives_enabled, Ordering::SeqCst);
    AUTOMOUNT.store(cfg.drives_automount, Ordering::SeqCst);
    if cfg.drives_enabled {
        SUPERVISOR.call_once(|| {
            std::thread::Builder::new().name("prism-drives".into()).spawn(supervise).ok();
        });
    }
}

fn supervise() {
    let mut logged = false;
    loop {
        if let Err(e) = watch() {
            if !logged {
                eprintln!("[prismd/drives] {e}; retrying");
                logged = true;
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// Follow UDisks2 until the bus connection fails.
fn watch() -> Result<(), String> {
    let e = |e: zbus::Error| e.to_string();
    let conn = Connection::system().map_err(e)?;
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .sender(UDISKS)
        .map_err(e)?
        .interface("org.freedesktop.DBus.ObjectManager")
        .map_err(e)?
        .member("InterfacesAdded")
        .map_err(e)?
        .build();
    let signals = MessageIterator::for_match_rule(rule, &conn, Some(64)).map_err(e)?;
    eprintln!("[prismd/drives] watching for removable drives");
    for msg in signals {
        let msg = msg.map_err(e)?;
        if !WANTED.load(Ordering::SeqCst) {
            continue;
        }
        let Ok((path, ifaces)) = msg.body().deserialize::<(OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>)>() else { continue };
        if !ifaces.contains_key(FS) {
            continue;
        }
        let path = path.to_string();
        if !SEEN.lock().unwrap().get_or_insert_with(HashSet::new).insert(path.clone()) {
            continue;
        }
        let conn = conn.clone();
        std::thread::spawn(move || {
            // UDisks fills in the block's properties right after; give it a moment.
            std::thread::sleep(Duration::from_millis(300));
            if let Err(err) = announce(&conn, &path) {
                eprintln!("[prismd/drives] {path}: {err}");
            }
            SEEN.lock().unwrap().get_or_insert_with(HashSet::new).remove(&path);
        });
    }
    Err("UDisks2 signals ended".into())
}

fn announce(conn: &Connection, path: &str) -> Result<(), String> {
    use prism_common::drives;
    let Some(v) = drives::find(conn, path) else { return Ok(()) };
    let title = v.title();
    let mounted = if AUTOMOUNT.load(Ordering::SeqCst) {
        match drives::mount(conn, &v) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("[prismd/drives] mount {}: {e}", v.label);
                None
            }
        }
    } else {
        v.mount.clone()
    };
    let session = Connection::session().map_err(|e| e.to_string())?;
    let (body, actions): (String, &[&str]) = match &mounted {
        Some(p) => (format!("Mounted at {p}"), &["open", "Open", "eject", "Eject"]),
        None => ("Connected".into(), &["mount", "Open", "eject", "Eject"]),
    };
    let answer = crate::obex::ask_as(&session, "Drives", &format!("{title} connected"), &body, "drive-removable-media", actions, None);
    match answer.as_deref() {
        Some("open") | Some("default") | Some("mount") => {
            let p = match mounted {
                Some(p) => p,
                None => drives::mount(conn, &v)?,
            };
            let _ = prism_common::apps::app_command("xdg-open", &[p]).spawn();
        }
        Some("eject") => match drives::eject(conn, &v.drive) {
            Ok(()) => notify_plain(&session, &format!("{} can be removed", v.label), "It was unmounted and switched off.", "media-eject"),
            Err(e) => notify_plain(&session, &format!("{} is in use", v.label), &e, "dialog-warning"),
        },
        _ => {}
    }
    Ok(())
}

fn notify_plain(session: &Connection, summary: &str, body: &str, icon: &str) {
    let (s, b, i, c) = (summary.to_string(), body.to_string(), icon.to_string(), session.clone());
    std::thread::spawn(move || {
        crate::obex::ask_as(&c, "Drives", &s, &b, &i, &[], None);
    });
}
