//! Calendar feed addresses in the user's keyring (the Secret Service: gnome-keyring, KeePassXC,
//! ...), so they are never in config.json, its snapshots or backups.
//!
//! Each feed is one item in the default collection with the attributes
//! `{"xdg:schema": "org.prism.CalendarFeed", "prism-calendar": <id>}`, its name as the label
//! (Prism's prefix stripped) and the address as the secret. Ids are creation times, so feeds
//! keep their order (and colours).
//!
//! Reading never asks for a password: prismd uses only unlocked items. Settings unlocks the
//! collection when it is locked ([`unlock`]); the keyring then asks through its own prompt
//! (Prism's `keyring_prompter.rs`).
//!
//! Secrets travel over the session bus with the "plain" algorithm; the bus is private to the
//! user's session.

use std::collections::HashMap;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";
const SCHEMA: &str = "org.prism.CalendarFeed";
const ATTR_ID: &str = "prism-calendar";
const LABEL_PREFIX: &str = "Prism calendar: ";

#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub id: String,
    pub name: String,
}

/// The secret part of a Secret Service `Secret` struct: (session, parameters, value, type).
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

struct Ring {
    conn: Connection,
    session: OwnedObjectPath,
}

fn err(e: impl std::fmt::Display) -> String {
    let s = e.to_string();
    if s.contains("ServiceUnknown") || s.contains("was not provided by any .service") {
        "no keyring is running (install gnome-keyring or another Secret Service)".into()
    } else {
        format!("keyring: {s}")
    }
}

impl Ring {
    fn open() -> Result<Ring, String> {
        let conn = Connection::session().map_err(err)?;
        let reply = conn.call_method(Some(DEST), SERVICE_PATH, Some(SERVICE), "OpenSession", &("plain", Value::from(""))).map_err(err)?;
        let (_, session): (OwnedValue, OwnedObjectPath) = reply.body().deserialize().map_err(err)?;
        Ok(Ring { conn, session })
    }

    fn proxy(&self, path: &str, iface: &'static str) -> Result<Proxy<'_>, String> {
        Proxy::new(&self.conn, DEST, path.to_string(), iface).map_err(err)
    }

    fn default_collection(&self) -> Result<OwnedObjectPath, String> {
        let reply = self.conn.call_method(Some(DEST), SERVICE_PATH, Some(SERVICE), "ReadAlias", &("default",)).map_err(err)?;
        let path: OwnedObjectPath = reply.body().deserialize().map_err(err)?;
        if path.as_str() == "/" {
            return Err("the keyring has no default collection (open your keyring app once to create one)".into());
        }
        Ok(path)
    }

    /// (unlocked, locked) items of Prism's calendar feeds.
    fn items(&self) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), String> {
        let attrs: HashMap<&str, &str> = HashMap::from([("xdg:schema", SCHEMA)]);
        let reply = self.conn.call_method(Some(DEST), SERVICE_PATH, Some(SERVICE), "SearchItems", &(attrs,)).map_err(err)?;
        reply.body().deserialize().map_err(err)
    }

    fn feed(&self, item: &OwnedObjectPath) -> Result<Feed, String> {
        let p = self.proxy(item.as_str(), ITEM)?;
        let attrs: HashMap<String, String> = p.get_property("Attributes").map_err(err)?;
        let label: String = p.get_property("Label").map_err(err)?;
        Ok(Feed { id: attrs.get(ATTR_ID).cloned().unwrap_or_default(), name: label.strip_prefix(LABEL_PREFIX).unwrap_or(&label).to_string() })
    }

    fn secret(&self, item: &OwnedObjectPath) -> Result<String, String> {
        let reply = self.conn.call_method(Some(DEST), item.as_str(), Some(ITEM), "GetSecret", &(&self.session,)).map_err(err)?;
        let (_, _, value, _): Secret = reply.body().deserialize().map_err(err)?;
        String::from_utf8(value).map_err(|_| "keyring: a calendar address is not text".to_string())
    }

    /// Run a Secret Service prompt (the keyring asks the user); true when it completed.
    fn prompt(&self, prompt: &OwnedObjectPath) -> Result<bool, String> {
        if prompt.as_str() == "/" {
            return Ok(true);
        }
        let p = self.proxy(prompt.as_str(), PROMPT)?;
        let mut done = p.receive_signal("Completed").map_err(err)?;
        p.call_method("Prompt", &("",)).map_err(err)?;
        // The user may take a while to type; give up after five minutes.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(m) = done.next() {
                let _ = tx.send(m.body().deserialize::<(bool, OwnedValue)>().map(|(dismissed, _)| !dismissed).unwrap_or(false));
            }
        });
        Ok(rx.recv_timeout(Duration::from_secs(300)).unwrap_or(false))
    }

    fn sorted_feeds(&self, items: &[OwnedObjectPath]) -> Vec<(Feed, OwnedObjectPath)> {
        let mut v: Vec<(Feed, OwnedObjectPath)> = items.iter().filter_map(|i| self.feed(i).ok().map(|f| (f, i.clone()))).collect();
        v.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        v
    }
}

/// Whether the default collection is locked (reading it would need the keyring password).
pub fn locked() -> Result<bool, String> {
    let ring = Ring::open()?;
    let c = ring.default_collection()?;
    let locked = ring.proxy(c.as_str(), COLLECTION)?.get_property::<bool>("Locked").map_err(err);
    locked
}

/// Unlock the default collection: the keyring asks for its password. True when unlocked.
pub fn unlock() -> Result<bool, String> {
    let ring = Ring::open()?;
    let c = ring.default_collection()?;
    let objects = vec![ObjectPath::try_from(c.as_str()).map_err(err)?];
    let reply = ring.conn.call_method(Some(DEST), SERVICE_PATH, Some(SERVICE), "Unlock", &(objects,)).map_err(err)?;
    let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = reply.body().deserialize().map_err(err)?;
    if !unlocked.is_empty() {
        return Ok(true);
    }
    ring.prompt(&prompt)
}

/// Unlock the default collection with a password the user just typed (the lock screen's,
/// when `keyring_unlock_with_lock` is on), without a prompt. gnome-keyring's
/// `UnlockWithMasterPassword` (what Seahorse uses); other keyrings do not have it. True when
/// the collection is unlocked afterwards (false: a different password, or not gnome-keyring).
pub fn unlock_with_password(password: &str) -> Result<bool, String> {
    let ring = Ring::open()?;
    let c = ring.default_collection()?;
    if !ring.proxy(c.as_str(), COLLECTION)?.get_property::<bool>("Locked").map_err(err)? {
        return Ok(true);
    }
    let collection = ObjectPath::try_from(c.as_str()).map_err(err)?;
    let session = ObjectPath::try_from(ring.session.as_str()).map_err(err)?;
    let mut bytes = password.as_bytes().to_vec();
    let secret = (session, Vec::<u8>::new(), bytes.clone(), "text/plain");
    let r = ring.conn.call_method(Some(DEST), SERVICE_PATH, Some("org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface"), "UnlockWithMasterPassword", &(collection, secret));
    bytes.iter_mut().for_each(|b| *b = 0);
    if let Err(e) = r {
        // A wrong password is an error too; say nothing about the password itself.
        let s = e.to_string();
        return Err(if s.contains("UnknownMethod") || s.contains("UnknownInterface") { "the keyring is not gnome-keyring".into() } else { "the keyring did not accept that password".into() });
    }
    let locked = ring.proxy(c.as_str(), COLLECTION)?.get_property::<bool>("Locked").map_err(err)?;
    Ok(!locked)
}

// ── Unlocking at login (greetd + pam_gnome_keyring) ─────────────────────────────────────────

/// greetd's PAM service. pam_gnome_keyring there unlocks the keyring named "login" with the
/// password typed into the greeter.
pub const GREETD_PAM: &str = "/etc/pam.d/greetd";
/// Marks each line Prism added (so turning it off removes only those).
const PAM_MARK: &str = "# prism: keyring";

/// Whether greetd's PAM service loads pam_gnome_keyring.
pub fn greeter_unlocks() -> bool {
    std::fs::read_to_string(GREETD_PAM).is_ok_and(|t| t.lines().any(|l| !l.trim_start().starts_with('#') && l.contains("pam_gnome_keyring.so")))
}

/// Add or remove pam_gnome_keyring in greetd's PAM service, as root through pkexec (polkit asks
/// for the password). Only system programs run as root: tee appends, sed removes Prism's lines.
pub fn set_greeter_unlocks(on: bool) -> Result<(), String> {
    use std::io::Write;
    if !std::path::Path::new(GREETD_PAM).exists() {
        return Err(format!("{GREETD_PAM} is missing: is greetd installed?"));
    }
    if on == greeter_unlocks() {
        return Ok(());
    }
    let status = if on {
        // Order inside the file does not matter across types: appended auth and session lines
        // run after the existing ones of their type.
        let lines = format!("{PAM_MARK}\nauth       optional     pam_gnome_keyring.so\n{PAM_MARK}\nsession    optional     pam_gnome_keyring.so auto_start\n");
        let mut child = std::process::Command::new("pkexec")
            .args(["/usr/bin/tee", "-a", GREETD_PAM])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("pkexec: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(lines.as_bytes()).map_err(|e| e.to_string())?;
        }
        child.wait().map_err(|e| e.to_string())?
    } else {
        std::process::Command::new("pkexec").args(["/usr/bin/sed", "-i", "-e", &format!("/^{PAM_MARK}$/,+1d"), GREETD_PAM]).status().map_err(|e| format!("pkexec: {e}"))?
    };
    match status.code() {
        Some(0) if greeter_unlocks() == on => Ok(()),
        Some(0) => Err(if on { "the change did not take".into() } else { format!("{GREETD_PAM} loads pam_gnome_keyring in lines Prism did not add; remove them there") }),
        Some(126) => Err("cancelled".into()),
        _ => Err("could not change the login setup (no permission?)".into()),
    }
}

fn keyrings_dir() -> std::path::PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    data.join("keyrings")
}

/// File name (without .keyring) of gnome-keyring's default keyring, if it has one.
pub fn default_keyring_file() -> Option<String> {
    let name = std::fs::read_to_string(keyrings_dir().join("default")).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn keyring_daemons() -> bool {
    std::process::Command::new("pgrep").args(["-u", &unsafe { libc::getuid() }.to_string(), "-x", "gnome-keyring-d"]).stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Make the default keyring gnome-keyring's "login" keyring (the one pam_gnome_keyring unlocks):
/// back up the keyrings folder, stop the daemon, rename the file, point `default` at it and
/// start the daemon again (the keyring is then locked until unlocked). Returns the backup
/// folder, or None when it already was "login".
pub fn make_login_default() -> Result<Option<std::path::PathBuf>, String> {
    let dir = keyrings_dir();
    let Some(name) = default_keyring_file() else { return Err("gnome-keyring has no default keyring here".into()) };
    if name == "login" {
        return Ok(None);
    }
    let from = dir.join(format!("{name}.keyring"));
    let to = dir.join("login.keyring");
    if !from.is_file() {
        return Err(format!("{} is missing", from.display()));
    }
    if to.exists() {
        return Err("a \"login\" keyring already exists next to your default one; make it the default in Seahorse instead".into());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let backup = dir.with_file_name(format!("keyrings.prism-backup-{stamp}"));
    let cp = std::process::Command::new("cp").arg("-a").arg(&dir).arg(&backup).status().map_err(|e| format!("cp: {e}"))?;
    if !cp.success() {
        return Err("could not back up the keyrings".into());
    }
    let systemctl = |args: &[&str]| {
        let _ = std::process::Command::new("systemctl").arg("--user").args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    };
    systemctl(&["stop", "gnome-keyring-daemon.socket", "gnome-keyring-daemon.service"]);
    // Daemons started outside systemd (XDG autostart).
    let _ = std::process::Command::new("pkill").args(["-u", &unsafe { libc::getuid() }.to_string(), "-x", "gnome-keyring-d"]).status();
    let t = std::time::Instant::now();
    while keyring_daemons() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let result = (|| {
        if keyring_daemons() {
            return Err("the keyring daemon did not stop".to_string());
        }
        std::fs::rename(&from, &to).map_err(|e| format!("renaming the keyring: {e}"))?;
        crate::clipboard::write_private(&dir.join("default"), b"login").map_err(|e| format!("setting the default keyring: {e}"))
    })();
    systemctl(&["start", "gnome-keyring-daemon.socket", "gnome-keyring-daemon.service"]);
    result.map(|_| Some(backup))
}

/// How many feeds there are (readable while locked: only the names and ids are public).
pub fn count() -> Result<usize, String> {
    let ring = Ring::open()?;
    let (a, b) = ring.items()?;
    Ok(a.len() + b.len())
}

/// The feeds' ids and names (no addresses), in order.
pub fn feeds() -> Result<Vec<Feed>, String> {
    let ring = Ring::open()?;
    let (a, b) = ring.items()?;
    let all: Vec<OwnedObjectPath> = a.into_iter().chain(b).collect();
    Ok(ring.sorted_feeds(&all).into_iter().map(|(f, _)| f).collect())
}

/// The feeds with their addresses, in order: only those the keyring gives without asking
/// (None when any feed is locked, so callers keep what they had).
pub fn addresses() -> Result<Option<Vec<(Feed, String)>>, String> {
    let ring = Ring::open()?;
    let (unlocked, locked) = ring.items()?;
    if !locked.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for (f, item) in ring.sorted_feeds(&unlocked) {
        out.push((f, ring.secret(&item)?));
    }
    Ok(Some(out))
}

/// Store a feed in the default collection.
pub fn add(name: &str, address: &str) -> Result<Feed, String> {
    let ring = Ring::open()?;
    let c = ring.default_collection()?;
    let id = format!("{:020}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let name = name.trim();
    let attrs: HashMap<&str, &str> = HashMap::from([("xdg:schema", SCHEMA), (ATTR_ID, id.as_str())]);
    let label = format!("{LABEL_PREFIX}{name}");
    let props: HashMap<&str, Value> = HashMap::from([("org.freedesktop.Secret.Item.Label", Value::from(label.as_str())), ("org.freedesktop.Secret.Item.Attributes", Value::from(attrs))]);
    let session = ObjectPath::try_from(ring.session.as_str()).map_err(err)?;
    let secret = (session, Vec::<u8>::new(), address.trim().as_bytes().to_vec(), "text/plain");
    let reply = ring.conn.call_method(Some(DEST), c.as_str(), Some(COLLECTION), "CreateItem", &(props, secret, false)).map_err(err)?;
    let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = reply.body().deserialize().map_err(err)?;
    if item.as_str() == "/" && !ring.prompt(&prompt)? {
        return Err("the keyring did not save it".into());
    }
    Ok(Feed { id, name: name.to_string() })
}

/// Delete a feed.
pub fn remove(id: &str) -> Result<(), String> {
    let ring = Ring::open()?;
    let (a, b) = ring.items()?;
    for item in a.iter().chain(&b) {
        if ring.feed(item).is_ok_and(|f| f.id == id) {
            let reply = ring.conn.call_method(Some(DEST), item.as_str(), Some(ITEM), "Delete", &()).map_err(err)?;
            let prompt: OwnedObjectPath = reply.body().deserialize().map_err(err)?;
            ring.prompt(&prompt)?;
            return Ok(());
        }
    }
    Err("that calendar is not in the keyring".into())
}

/// A name for a feed from its address: "Google Calendar", or the host.
pub fn suggested_name(address: &str) -> String {
    let host = address.split("://").nth(1).unwrap_or(address).split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    match host.as_str() {
        h if h.ends_with("calendar.google.com") => "Google Calendar".into(),
        h if h.ends_with("icloud.com") => "iCloud Calendar".into(),
        h if h.contains("outlook") || h.contains("office365") || h.contains("live.com") => "Outlook Calendar".into(),
        "" => "Calendar".into(),
        h => h.to_string(),
    }
}

/// An address Prism can fetch: http(s) (webcal:// becomes https://) or a local .ics file.
pub fn normalize_address(input: &str) -> Option<String> {
    let a = input.trim();
    if a.is_empty() || a.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = a.to_lowercase();
    if let Some(rest) = lower.strip_prefix("webcal://") {
        return Some(format!("https://{}", &a[a.len() - rest.len()..]));
    }
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(a.to_string());
    }
    let path = if let Some(rest) = a.strip_prefix("~/") { format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()) } else { a.to_string() };
    std::path::Path::new(&path).is_file().then_some(a.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_addresses() {
        assert_eq!(suggested_name("https://calendar.google.com/calendar/ical/x/private-y/basic.ics"), "Google Calendar");
        assert_eq!(suggested_name("https://p01-caldav.icloud.com/x"), "iCloud Calendar");
        assert_eq!(suggested_name("https://cloud.example.org/remote.php/dav/x.ics"), "cloud.example.org");
        assert_eq!(normalize_address(" webcal://Example.com/a.ics "), Some("https://Example.com/a.ics".into()));
        assert_eq!(normalize_address("https://example.com/a b"), None);
        assert_eq!(normalize_address("not a url"), None);
        assert_eq!(normalize_address("/nonexistent/file.ics"), None);
    }

    /// Against the real keyring: add, list, read back, remove. `cargo test -p prism-common
    /// keyring_round_trip -- --ignored` (needs an unlocked keyring).
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let f = add("Prism test calendar", "https://example.invalid/prism-test.ics").unwrap();
        assert!(feeds().unwrap().iter().any(|x| x.id == f.id && x.name == "Prism test calendar"));
        let all = addresses().unwrap().expect("unlocked");
        assert!(all.iter().any(|(x, a)| x.id == f.id && a == "https://example.invalid/prism-test.ics"));
        remove(&f.id).unwrap();
        assert!(!feeds().unwrap().iter().any(|x| x.id == f.id));
    }
}
