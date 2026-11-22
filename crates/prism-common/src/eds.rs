//! evolution-data-server (EDS) calendars over D-Bus: the calendars of the user's online
//! accounts (GNOME Online Accounts: Google, Microsoft, CalDAV servers such as iCloud) and EDS's
//! local "Personal" calendar. EDS keeps them in sync with their servers; Prism reads the events
//! of a time range and creates, changes and removes events as iCalendar (VEVENT) text.
//!
//! EDS closes a calendar when the client that opened it leaves the bus, so a `Client` keeps its
//! connection and the calendars it opened for as long as it lives.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

const SOURCES: &str = "org.gnome.evolution.dataserver.Sources5";
const SOURCES_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
const SOURCE_IFACE: &str = "org.gnome.evolution.dataserver.Source";
const FACTORY: &str = "org.gnome.evolution.dataserver.Calendar8";
const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";
const FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.CalendarFactory";
const CAL_IFACE: &str = "org.gnome.evolution.dataserver.Calendar";

/// The UID of EDS's local calendar ("Personal"), always there.
pub const LOCAL: &str = "system-calendar";

/// One calendar EDS knows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calendar {
    /// The EDS source UID.
    pub uid: String,
    pub name: String,
    /// The account it belongs to (its collection's name, usually the address), empty for local
    /// calendars.
    pub account: String,
    /// "#rrggbb", or empty.
    pub color: String,
}

/// An EDS source's key file: section -> key -> value (translated keys left out).
fn key_file(data: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in data.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if !k.contains('[') {
                out.entry(section.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    out
}

/// The calendars EDS has that hold events, enabled and of enabled accounts: online accounts'
/// calendars first (by account, then name), the local ones last.
pub fn calendars(conn: &Connection) -> Result<Vec<Calendar>, String> {
    let om = zbus::blocking::fdo::ObjectManagerProxy::builder(conn)
        .destination(SOURCES)
        .and_then(|b| b.path(SOURCES_PATH))
        .and_then(|b| b.build())
        .map_err(|e| format!("evolution-data-server: {e}"))?;
    let objects = om.get_managed_objects().map_err(|e| format!("evolution-data-server: {e}"))?;
    let mut sources: HashMap<String, HashMap<String, HashMap<String, String>>> = HashMap::new();
    for ifaces in objects.values() {
        let Some(props) = ifaces.iter().find(|(name, _)| name.as_str() == SOURCE_IFACE).map(|(_, p)| p) else { continue };
        let text = |k: &str| props.get(k).and_then(|v| String::try_from(v.try_clone().ok()?).ok()).unwrap_or_default();
        let uid = text("UID");
        if !uid.is_empty() {
            sources.insert(uid, key_file(&text("Data")));
        }
    }
    let get = |s: &HashMap<String, HashMap<String, String>>, sec: &str, key: &str| s.get(sec).and_then(|m| m.get(key)).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for (uid, s) in &sources {
        let Some(cal) = s.get("Calendar") else { continue };
        let backend = cal.get("BackendName").map(String::as_str).unwrap_or("");
        // Stubs are templates; weather and contacts' birthdays are not events of the user's.
        if uid.ends_with("-stub") || matches!(backend, "weather" | "contacts") || get(s, "Data Source", "Enabled") == "false" {
            continue;
        }
        let parent = get(s, "Data Source", "Parent");
        let account = match sources.get(&parent) {
            Some(p) => {
                if get(p, "Data Source", "Enabled") == "false" || get(p, "Collection", "CalendarEnabled") == "false" {
                    continue;
                }
                if p.contains_key("Collection") { get(p, "Data Source", "DisplayName") } else { String::new() }
            }
            None => String::new(),
        };
        out.push(Calendar { uid: uid.clone(), name: get(s, "Data Source", "DisplayName"), account, color: cal.get("Color").cloned().unwrap_or_default() });
    }
    out.sort_by(|a, b| (a.account.is_empty(), &a.account, &a.name).cmp(&(b.account.is_empty(), &b.account, &b.name)));
    Ok(out)
}

/// The query for events that overlap [from, to) (Unix seconds).
fn range_query(from: i64, to: i64) -> String {
    let t = |x: i64| crate::calendar::ical_stamp(x, false);
    format!("(occur-in-time-range? (make-time \"{}\") (make-time \"{}\"))", t(from), t(to))
}

/// A connection to EDS and the calendars opened on it.
pub struct Client {
    conn: Connection,
    /// Source UID -> (bus name, object path) of its opened calendar.
    open: HashMap<String, (String, OwnedObjectPath)>,
}

impl Client {
    pub fn new() -> Result<Client, String> {
        let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;
        Ok(Client { conn, open: HashMap::new() })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn calendars(&self) -> Result<Vec<Calendar>, String> {
        calendars(&self.conn)
    }

    fn proxy(&mut self, uid: &str) -> Result<Proxy<'static>, String> {
        if !self.open.contains_key(uid) {
            let factory = Proxy::new(&self.conn, FACTORY, FACTORY_PATH, FACTORY_IFACE).map_err(|e| e.to_string())?;
            let (path, bus): (String, String) = factory.call("OpenCalendar", &(uid,)).map_err(|e| format!("opening the calendar: {e}"))?;
            let path = OwnedObjectPath::try_from(path).map_err(|e| e.to_string())?;
            let p = Proxy::new_owned(self.conn.clone(), bus.clone(), path.clone(), CAL_IFACE).map_err(|e| e.to_string())?;
            let _: Vec<String> = p.call("Open", &()).map_err(|e| format!("opening the calendar: {e}"))?;
            self.open.insert(uid.to_string(), (bus, path));
        }
        let (bus, path) = &self.open[uid];
        Proxy::new_owned(self.conn.clone(), bus.clone(), path.clone(), CAL_IFACE).map_err(|e| e.to_string())
    }

    /// Runs `f` on the calendar `uid`, opening it again once if EDS closed it meanwhile.
    fn with<R>(&mut self, uid: &str, f: impl Fn(&Proxy) -> zbus::Result<R>) -> Result<R, String> {
        let p = self.proxy(uid)?;
        match f(&p) {
            Ok(r) => Ok(r),
            Err(_) => {
                self.open.remove(uid);
                let p = self.proxy(uid)?;
                f(&p).map_err(|e| message(&e))
            }
        }
    }

    /// The VEVENTs of calendar `uid` that overlap [from, to): repeating events once, with
    /// their rule (and their changed occurrences as their own VEVENTs).
    pub fn events(&mut self, uid: &str, from: i64, to: i64) -> Result<Vec<String>, String> {
        let q = range_query(from, to);
        self.with(uid, |p| p.call("GetObjectList", &(q.as_str(),)))
    }

    /// Whether events can be added to calendar `uid`.
    pub fn writable(&mut self, uid: &str) -> Result<bool, String> {
        self.with(uid, |p| p.get_property::<bool>("Writable"))
    }

    /// The event `event_uid` (the whole repeating event when it repeats).
    pub fn get(&mut self, uid: &str, event_uid: &str) -> Result<String, String> {
        self.with(uid, |p| p.call("GetObject", &(event_uid, "")))
    }

    /// Adds `vevent`; returns its UID.
    pub fn create(&mut self, uid: &str, vevent: &str) -> Result<String, String> {
        let uids: Vec<String> = self.with(uid, |p| p.call("CreateObjects", &(vec![vevent], 0u32)))?;
        Ok(uids.into_iter().next().unwrap_or_default())
    }

    /// Replaces the event with `vevent`'s UID (all occurrences when it repeats).
    pub fn modify(&mut self, uid: &str, vevent: &str) -> Result<(), String> {
        self.with(uid, |p| p.call::<_, _, ()>("ModifyObjects", &(vec![vevent], "all", 0u32)))
    }

    /// Removes the event `event_uid`: only the occurrence `rid` when given, else all of it.
    pub fn remove(&mut self, uid: &str, event_uid: &str, rid: &str) -> Result<(), String> {
        let mode = if rid.is_empty() { "all" } else { "this" };
        self.with(uid, |p| p.call::<_, _, ()>("RemoveObjects", &(vec![(event_uid, rid)], mode, 0u32)))
    }

    /// Asks EDS to sync calendar `uid` with its server now (local calendars have nothing to do).
    pub fn refresh(&mut self, uid: &str) {
        let _ = self.with(uid, |p| p.call::<_, _, ()>("Refresh", &()));
    }
}

/// An EDS error without the D-Bus error name in front.
fn message(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(m), _) => m.clone(),
        e => e.to_string(),
    }
}

/// Whether evolution-data-server is installed (its calendar service can be started).
pub fn available() -> bool {
    ["/usr/share/dbus-1/services/org.gnome.evolution.dataserver.Calendar8.service", "/usr/share/dbus-1/services/org.gnome.evolution.dataserver.Sources5.service"].iter().all(|p| std::path::Path::new(p).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_source_key_files() {
        let data = "\n[Data Source]\nDisplayName=Work\nDisplayName[de]=Arbeit\nEnabled=true\nParent=abc\n\n[Calendar]\nBackendName=caldav\nColor=#3465a4\n";
        let k = key_file(data);
        assert_eq!(k["Data Source"]["DisplayName"], "Work");
        assert_eq!(k["Calendar"]["Color"], "#3465a4");
        assert!(!k["Data Source"].contains_key("DisplayName[de]"));
    }

    /// Against the running evolution-data-server, on its local calendar (removes what it adds):
    /// `cargo test --release -p prism-common eds_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn eds_live() {
        use crate::calendar::{self, Draft};
        let mut c = Client::new().unwrap();
        let cals = c.calendars().unwrap();
        println!("calendars: {:?}", cals.iter().map(|c| (&c.uid, &c.name, &c.account)).collect::<Vec<_>>());
        assert!(cals.iter().any(|c| c.uid == LOCAL));
        assert!(c.writable(LOCAL).unwrap());
        let day = 86400;
        let t0 = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 / day + 30) * day + 9 * 3600;
        let uid = calendar::new_uid();
        // A daily event, five times.
        let d = Draft { summary: "Prism live test".into(), location: "Here".into(), all_day: false, start: t0, end: t0 + 3600 };
        let mut ics = calendar::new_vevent(&uid, &d, t0);
        ics = ics.replace("END:VEVENT", "RRULE:FREQ=DAILY;COUNT=5\r\nEND:VEVENT");
        assert_eq!(c.create(LOCAL, &ics).unwrap(), uid);
        let read = |c: &mut Client| {
            let list = c.events(LOCAL, t0 - day, t0 + 10 * day).unwrap();
            calendar::occurrences(&list.concat(), 0, t0 - day, t0 + 10 * day).into_iter().filter(|o| o.uid == uid || o.summary.starts_with("Prism live")).collect::<Vec<_>>()
        };
        let occ = read(&mut c);
        assert_eq!(occ.len(), 5, "{occ:?}");
        // All of them: the third moved 30 minutes later, retitled.
        let third = occ[2].clone();
        let changed = Draft { summary: "Prism live test, moved".into(), start: third.start + 1800, end: third.end + 1800, ..d.clone() };
        let text = calendar::edit_vevent(&c.get(LOCAL, &uid).unwrap(), &changed, third.start, t0).unwrap();
        c.modify(LOCAL, &text).unwrap();
        let occ = read(&mut c);
        assert_eq!(occ.len(), 5);
        assert!(occ.iter().all(|o| o.summary == "Prism live test, moved" && (o.start - t0) % day == 1800), "{occ:?}");
        // Only the second: removed from the series, added alone.
        let second = occ[1].clone();
        c.remove(LOCAL, &uid, &second.rid).unwrap();
        let alone = calendar::new_uid();
        c.create(LOCAL, &calendar::new_vevent(&alone, &Draft { summary: "Prism live single".into(), ..changed.clone() }, t0)).unwrap();
        let occ = read(&mut c);
        assert_eq!(occ.iter().filter(|o| o.uid == uid).count(), 4, "{occ:?}");
        assert!(occ.iter().any(|o| o.uid == alone));
        // Delete everything.
        c.remove(LOCAL, &uid, "").unwrap();
        c.remove(LOCAL, &alone, "").unwrap();
        assert!(read(&mut c).is_empty());
    }

    #[test]
    fn range_query_is_utc() {
        assert_eq!(range_query(0, 86400), "(occur-in-time-range? (make-time \"19700101T000000Z\") (make-time \"19700102T000000Z\"))");
    }
}
