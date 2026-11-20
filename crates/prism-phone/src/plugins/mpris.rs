//! The PC's media players for the phone (desktop side "mpriscontrol"): the phone asks with
//! `kdeconnect.mpris.request` (player list, now playing, actions, seek, volume, album art) and
//! gets `kdeconnect.mpris` back, also whenever a player changes.
//!
//! D-Bus calls can block (a hung player), so they run on one worker thread, never under the
//! `Core` lock: handlers only queue requests to it.

use crate::core::{Core, Shared};
use crate::packet::Packet;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;
use zbus::blocking::{fdo::DBusProxy, Connection, MessageIterator, Proxy};
use zbus::message::Type as MsgType;
use zbus::zvariant::{OwnedValue, Value as ZValue};
use zbus::MatchRule;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
/// Album art larger than this is not sent.
const MAX_ART: u64 = 8 * 1024 * 1024;

pub enum Msg {
    /// A player appeared, went away or changed.
    Changed,
    /// A device connected: send it everything.
    Connected(String),
    /// A `kdeconnect.mpris.request` from a device.
    Request(String, Packet),
}

/// A packet from the phone about the PC's players.
pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    if core.cfg.phone_media_control {
        queue(core, Msg::Request(id.to_string(), p.clone()));
    }
}

pub fn on_connected(core: &mut Core, id: &str) {
    if core.cfg.phone_media_control && wants_media(core, id) {
        queue(core, Msg::Connected(id.to_string()));
    }
}

fn queue(core: &Core, msg: Msg) {
    if let Some(tx) = &core.plugins.mpris {
        let _ = tx.send(msg);
    }
}

/// The device shows the PC's players (the KDE Connect app's "Multimedia control").
fn wants_media(core: &Core, id: &str) -> bool {
    core.devices.get(id).is_some_and(|d| d.trusted.is_some() && d.link.is_some() && d.info.incoming.iter().any(|c| c == "kdeconnect.mpris"))
}

/// Start the worker; its queue goes into `PluginState::mpris`.
pub fn start(shared: Shared) {
    let (tx, rx) = channel();
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[prism-phoned] no session bus, media control is off: {e}");
            return;
        }
    };
    if let Err(e) = subscribe(&conn, &tx) {
        eprintln!("[prism-phoned] MPRIS signals: {e}");
    }
    shared.lock().unwrap().plugins.mpris = Some(tx);
    std::thread::spawn(move || Worker { shared, conn, players: Vec::new(), sent: HashMap::new(), sent_list: None }.run(rx));
}

/// Player changes (properties, seeks, players coming and going) wake the worker.
fn subscribe(conn: &Connection, tx: &Sender<Msg>) -> zbus::Result<()> {
    let rules = [
        MatchRule::builder().msg_type(MsgType::Signal).interface("org.freedesktop.DBus")?.member("NameOwnerChanged")?.arg0ns(ROOT_IFACE)?.build(),
        MatchRule::builder().msg_type(MsgType::Signal).interface("org.freedesktop.DBus.Properties")?.member("PropertiesChanged")?.path(MPRIS_PATH)?.build(),
        MatchRule::builder().msg_type(MsgType::Signal).interface(PLAYER_IFACE)?.member("Seeked")?.build(),
    ];
    for rule in rules {
        let iter = MessageIterator::for_match_rule(rule.to_owned(), conn, Some(64))?;
        let tx = tx.clone();
        std::thread::spawn(move || {
            for _ in iter.flatten() {
                if tx.send(Msg::Changed).is_err() {
                    break;
                }
            }
        });
    }
    Ok(())
}

/// One MPRIS player as the phone sees it.
struct Player {
    /// Unique display name (the player's Identity).
    name: String,
    bus: String,
    /// Everything but the position, which moves on its own.
    state: Map<String, Value>,
    position_ms: i64,
    track_id: Option<String>,
}

struct Worker {
    shared: Shared,
    conn: Connection,
    players: Vec<Player>,
    /// Last state sent per player name (without the position).
    sent: HashMap<String, Map<String, Value>>,
    sent_list: Option<Vec<String>>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Msg>) {
        while let Ok(first) = rx.recv() {
            // Signals come in bursts (a track change sets several properties): coalesce them.
            let mut changed = false;
            let mut batch = vec![first];
            if matches!(batch[0], Msg::Changed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            batch.extend(rx.try_iter());
            for msg in batch {
                match msg {
                    Msg::Changed => changed = true,
                    Msg::Connected(id) => {
                        self.refresh();
                        self.send_all_to(&id);
                    }
                    Msg::Request(id, p) => self.request(&id, &p),
                }
            }
            if changed {
                self.broadcast_changes();
            }
        }
    }

    /// Connected devices that show the PC's players.
    fn targets(&self) -> Vec<String> {
        let core = self.shared.lock().unwrap();
        if !core.cfg.phone_media_control {
            return Vec::new();
        }
        core.devices.keys().filter(|id| wants_media(&core, id)).cloned().collect()
    }

    fn send(&self, ids: &[String], p: Packet) {
        let core = self.shared.lock().unwrap();
        for id in ids {
            core.send(id, p.clone());
        }
    }

    fn refresh(&mut self) {
        self.players = list_players(&self.conn);
    }

    fn names(&self) -> Vec<String> {
        self.players.iter().map(|p| p.name.clone()).collect()
    }

    fn player_list_packet(&self) -> Packet {
        Packet::new("kdeconnect.mpris", json!({ "playerList": self.names(), "supportAlbumArtPayload": true }))
    }

    fn state_packet(p: &Player) -> Packet {
        let mut body = p.state.clone();
        body.insert("player".into(), json!(p.name));
        body.insert("pos".into(), json!(p.position_ms));
        Packet::new("kdeconnect.mpris", Value::Object(body))
    }

    fn send_all_to(&mut self, id: &str) {
        let ids = [id.to_string()];
        self.send(&ids, self.player_list_packet());
        for p in &self.players {
            self.send(&ids, Self::state_packet(p));
        }
    }

    /// Send what changed since last time to every device that shows the PC's players.
    fn broadcast_changes(&mut self) {
        let ids = self.targets();
        if ids.is_empty() {
            // Nobody to tell: a device that connects gets everything anyway.
            self.sent.clear();
            self.sent_list = None;
            return;
        }
        self.refresh();
        let names = self.names();
        if self.sent_list.as_ref() != Some(&names) {
            self.send(&ids, self.player_list_packet());
            self.sent_list = Some(names);
        }
        // Positions are not compared (they move); a seek or a status change sends them along.
        let mut sent = HashMap::new();
        for p in &self.players {
            // Seeked has no property change: resend players that are playing too, so the
            // phone's position (it extrapolates while playing) is corrected.
            let changed = self.sent.get(&p.name) != Some(&p.state) || p.state.get("isPlaying") == Some(&json!(true));
            if changed {
                self.send(&ids, Self::state_packet(p));
            }
            sent.insert(p.name.clone(), p.state.clone());
        }
        self.sent = sent;
    }

    fn request(&mut self, id: &str, p: &Packet) {
        let ids = [id.to_string()];
        if p.bool("requestPlayerList") {
            self.refresh();
            self.send(&ids, self.player_list_packet());
        }
        let Some(name) = p.str("player") else { return };
        let Some(player) = self.players.iter().find(|pl| pl.name == name) else {
            // Unknown (maybe from before a restart): tell the phone what there is.
            self.refresh();
            self.send(&ids, self.player_list_packet());
            return;
        };
        let bus = player.bus.clone();
        let track_id = player.track_id.clone();
        let art = player.state.get("albumArtUrl").and_then(Value::as_str).unwrap_or_default().to_string();
        let Some(proxy) = Proxy::new(&self.conn, bus.as_str(), MPRIS_PATH, PLAYER_IFACE).ok() else { return };

        if let Some(action) = p.str("action") {
            if matches!(action, "Play" | "Pause" | "PlayPause" | "Stop" | "Next" | "Previous") {
                if let Err(e) = proxy.call_method(action, &()) {
                    eprintln!("[prism-phoned] {name}: {action}: {e}");
                }
            }
        }
        if let Some(v) = p.int("setVolume") {
            let _ = proxy.set_property("Volume", (v.clamp(0, 100) as f64) / 100.0);
        }
        if let Some(us) = p.int("Seek") {
            let _ = proxy.call_method("Seek", &(us,));
        }
        if let (Some(ms), Some(track)) = (p.int("SetPosition"), track_id) {
            if let Ok(path) = zbus::zvariant::ObjectPath::try_from(track.as_str()) {
                let _ = proxy.call_method("SetPosition", &(path, ms.max(0) * 1000));
            }
        }
        if let Some(mode) = p.str("setLoopStatus") {
            if matches!(mode, "None" | "Track" | "Playlist") {
                let _ = proxy.set_property("LoopStatus", mode);
            }
        }
        if let Some(v) = p.body.get("setShuffle").and_then(Value::as_bool) {
            let _ = proxy.set_property("Shuffle", v);
        }
        if p.bool("requestNowPlaying") || p.bool("requestVolume") || p.body.get("SetPosition").is_some() || p.body.get("Seek").is_some() {
            self.refresh();
            if let Some(pl) = self.players.iter().find(|pl| pl.name == name) {
                self.send(&ids, Self::state_packet(pl));
            }
        }
        // Album art: only the file the player itself names right now, never any path asked for.
        if let Some(url) = p.str("albumArtUrl") {
            if !url.is_empty() && url == art {
                self.send_art(id, name, url);
            }
        }
    }

    fn send_art(&self, id: &str, name: &str, url: &str) {
        let Some(path) = url.strip_prefix("file://").map(percent_decode) else { return };
        let Ok(meta) = std::fs::metadata(&path) else { return };
        if !meta.is_file() || meta.len() > MAX_ART {
            return;
        }
        let file = path.clone();
        let port = crate::payload::serve(&self.shared, id, move |t| {
            let mut f = std::fs::File::open(&file)?;
            crate::payload::pump(t, &mut f, |_| true).map(|_| ())
        });
        match port {
            Ok(port) => {
                let mut p = Packet::new("kdeconnect.mpris", json!({ "player": name, "albumArtUrl": url, "transferringAlbumArt": true }));
                p.payload_size = Some(meta.len() as i64);
                p.payload_transfer_info = Some(json!({ "port": port }));
                self.send(&[id.to_string()], p);
            }
            Err(e) => eprintln!("[prism-phoned] album art: {e}"),
        }
    }
}

fn list_players(conn: &Connection) -> Vec<Player> {
    let Ok(dbus) = DBusProxy::new(conn) else { return Vec::new() };
    let mut buses: Vec<String> = dbus.list_names().map(|n| n.into_iter().map(|n| n.to_string()).filter(|n| n.starts_with(MPRIS_PREFIX)).collect()).unwrap_or_default();
    buses.sort();
    let mut players: Vec<Player> = Vec::new();
    for bus in buses {
        let Some(mut p) = read_player(conn, &bus) else { continue };
        // Two players with the same Identity (two browser windows): number them.
        let base = p.name.clone();
        let mut n = 2;
        while players.iter().any(|q| q.name == p.name) {
            p.name = format!("{base} [{n}]");
            n += 1;
        }
        players.push(p);
    }
    players
}

fn read_player(conn: &Connection, bus: &str) -> Option<Player> {
    let player = Proxy::new(conn, bus, MPRIS_PATH, PLAYER_IFACE).ok()?;
    let status: String = player.get_property("PlaybackStatus").ok()?;
    let root = Proxy::new(conn, bus, MPRIS_PATH, ROOT_IFACE).ok();
    let identity = root.and_then(|r| r.get_property::<String>("Identity").ok()).filter(|s| !s.trim().is_empty());
    let name = identity.unwrap_or_else(|| bus.trim_start_matches(MPRIS_PREFIX).split(".instance").next().unwrap_or(bus).to_string());
    let meta: HashMap<String, OwnedValue> = player.get_property("Metadata").unwrap_or_default();
    let flag = |k: &str| player.get_property::<bool>(k).unwrap_or(false);
    let (title, artist, album) = (meta_str(&meta, "xesam:title"), meta_str(&meta, "xesam:artist"), meta_str(&meta, "xesam:album"));
    let now_playing = if artist.is_empty() { title.clone() } else { format!("{artist} - {title}") };
    let mut state = Map::new();
    state.insert("title".into(), json!(title));
    state.insert("artist".into(), json!(artist));
    state.insert("album".into(), json!(album));
    state.insert("nowPlaying".into(), json!(now_playing));
    state.insert("albumArtUrl".into(), json!(meta_str(&meta, "mpris:artUrl")));
    state.insert("url".into(), json!(meta_str(&meta, "xesam:url")));
    state.insert("isPlaying".into(), json!(status == "Playing"));
    state.insert("length".into(), json!((meta_num(&meta, "mpris:length") / 1000.0) as i64));
    state.insert("canPlay".into(), json!(flag("CanPlay")));
    state.insert("canPause".into(), json!(flag("CanPause")));
    state.insert("canGoNext".into(), json!(flag("CanGoNext")));
    state.insert("canGoPrevious".into(), json!(flag("CanGoPrevious")));
    state.insert("canSeek".into(), json!(flag("CanSeek")));
    if let Ok(v) = player.get_property::<f64>("Volume") {
        state.insert("volume".into(), json!((v.clamp(0.0, 1.0) * 100.0).round() as i64));
    }
    if let Ok(v) = player.get_property::<String>("LoopStatus") {
        state.insert("loopStatus".into(), json!(v));
    }
    if let Ok(v) = player.get_property::<bool>("Shuffle") {
        state.insert("shuffle".into(), json!(v));
    }
    let position_ms = player.get_property::<i64>("Position").map(|us| us / 1000).unwrap_or(0);
    let track_id = Some(meta_str(&meta, "mpris:trackid")).filter(|s| s.starts_with('/'));
    Some(Player { name, bus: bus.to_string(), state, position_ms, track_id })
}

fn meta_str(meta: &HashMap<String, OwnedValue>, key: &str) -> String {
    match meta.get(key).map(|v| &**v) {
        Some(ZValue::Str(s)) => s.to_string(),
        Some(ZValue::ObjectPath(p)) => p.to_string(),
        Some(ZValue::Array(a)) => a.iter().filter_map(|v| if let ZValue::Str(s) = v { Some(s.to_string()) } else { None }).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

fn meta_num(meta: &HashMap<String, OwnedValue>, key: &str) -> f64 {
    match meta.get(key).map(|v| &**v) {
        Some(ZValue::I64(n)) => *n as f64,
        Some(ZValue::U64(n)) => *n as f64,
        Some(ZValue::I32(n)) => *n as f64,
        Some(ZValue::U32(n)) => *n as f64,
        Some(ZValue::F64(n)) => *n,
        _ => 0.0,
    }
}

/// `file://` URLs escape spaces and other bytes as %XX.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::percent_decode;

    #[test]
    fn decodes_file_urls() {
        assert_eq!(percent_decode("/tmp/My%20Art.png"), "/tmp/My Art.png");
        assert_eq!(percent_decode("/tmp/100%"), "/tmp/100%");
        assert_eq!(percent_decode("/tmp/a%2"), "/tmp/a%2");
    }
}
