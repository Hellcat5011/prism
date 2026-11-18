use serde::{Deserialize, Serialize};

/// Represents active media player track metadata
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MediaTrack {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub status: String,
    pub position_secs: f64,
    pub length_secs: f64,
}

impl MediaTrack {
    /// Parse metadata from a delimiter-separated format string:
    /// `{{playerName}}|{{status}}|{{title}}|{{artist}}|{{album}}|{{mpris:artUrl}}|{{position}}|{{mpris:length}}`
    pub fn parse_formatted(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let parts: Vec<&str> = trimmed.split('|').collect();
        if parts.len() < 5 {
            return None;
        }

        let player = parts.first().unwrap_or(&"").trim().to_string();
        let status = parts.get(1).unwrap_or(&"Stopped").trim().to_string();
        let title = parts.get(2).unwrap_or(&"").trim().to_string();
        let artist = parts.get(3).unwrap_or(&"").trim().to_string();
        let album = parts.get(4).unwrap_or(&"").trim().to_string();
        let art_url = parts.get(5).and_then(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        });

        let position_secs = parts
            .get(6)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1_000_000.0; // playerctl position can be microseconds or raw seconds; handle if microseconds

        let length_secs = parts
            .get(7)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1_000_000.0;

        Some(MediaTrack {
            player,
            title,
            artist,
            album,
            art_url,
            status,
            position_secs,
            length_secs,
        })
    }
}

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

fn session() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::Connection::session().map_err(|e| format!("D-Bus session: {e}"))
}

fn player_proxy<'a>(conn: &zbus::blocking::Connection, bus: &'a str) -> Option<zbus::blocking::Proxy<'a>> {
    zbus::blocking::Proxy::new(conn, bus.to_string(), MPRIS_PATH, PLAYER_IFACE).ok()
}

/// The player playerctl would pick: the first playing one, else the first listed.
fn active_player(conn: &zbus::blocking::Connection) -> Option<String> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let mut names: Vec<String> = dbus.list_names().ok()?.into_iter().map(|n| n.to_string()).filter(|n| n.starts_with(MPRIS_PREFIX)).collect();
    names.sort();
    names
        .iter()
        .find(|n| player_proxy(conn, n).and_then(|p| p.get_property::<String>("PlaybackStatus").ok()).as_deref() == Some("Playing"))
        .or(names.first())
        .cloned()
}

fn meta_str(meta: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> String {
    use zbus::zvariant::Value;
    match meta.get(key).map(|v| &**v) {
        Some(Value::Str(s)) => s.to_string(),
        Some(Value::ObjectPath(p)) => p.to_string(),
        Some(Value::Array(a)) => a.iter().filter_map(|v| if let Value::Str(s) = v { Some(s.to_string()) } else { None }).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

fn meta_num(meta: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>, key: &str) -> f64 {
    use zbus::zvariant::Value;
    match meta.get(key).map(|v| &**v) {
        Some(Value::I64(n)) => *n as f64,
        Some(Value::U64(n)) => *n as f64,
        Some(Value::I32(n)) => *n as f64,
        Some(Value::U32(n)) => *n as f64,
        Some(Value::F64(n)) => *n,
        _ => 0.0,
    }
}

/// Query the current media track over MPRIS.
pub fn query_current_media() -> Option<MediaTrack> {
    let conn = session().ok()?;
    let bus = active_player(&conn)?;
    let p = player_proxy(&conn, &bus)?;
    let meta: std::collections::HashMap<String, zbus::zvariant::OwnedValue> = p.get_property("Metadata").ok()?;
    let status = p.get_property::<String>("PlaybackStatus").unwrap_or_else(|_| "Stopped".into());
    let position = p.get_property::<i64>("Position").unwrap_or(0) as f64;
    let art = meta_str(&meta, "mpris:artUrl");
    // playerName as playerctl prints it: the bus name without the instance suffix.
    let player = bus.trim_start_matches(MPRIS_PREFIX).split(".instance").next().unwrap_or("").to_string();
    Some(MediaTrack {
        player,
        title: meta_str(&meta, "xesam:title"),
        artist: meta_str(&meta, "xesam:artist"),
        album: meta_str(&meta, "xesam:album"),
        art_url: (!art.is_empty()).then_some(art),
        status,
        position_secs: position / 1_000_000.0,
        length_secs: meta_num(&meta, "mpris:length") / 1_000_000.0,
    })
}

fn call(method: &str) -> Result<(), String> {
    let conn = session()?;
    let bus = active_player(&conn).ok_or("No players found")?;
    let p = player_proxy(&conn, &bus).ok_or("No players found")?;
    p.call_method(method, &()).map(|_| ()).map_err(|e| format!("{method}: {e}"))
}

/// Send play-pause toggle command
pub fn play_pause() -> Result<(), String> {
    call("PlayPause")
}

/// Send next track command
pub fn next() -> Result<(), String> {
    call("Next")
}

/// Send previous track command
pub fn previous() -> Result<(), String> {
    call("Previous")
}

/// Send stop command
pub fn stop() -> Result<(), String> {
    call("Stop")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_formatted_track() {
        let raw = "spotify|Playing|Starboy|The Weeknd|Starboy|file:///tmp/art.png|30000000|230000000";
        let track = MediaTrack::parse_formatted(raw).expect("Failed to parse track");
        assert_eq!(track.player, "spotify");
        assert_eq!(track.status, "Playing");
        assert_eq!(track.title, "Starboy");
        assert_eq!(track.artist, "The Weeknd");
        assert_eq!(track.album, "Starboy");
        assert_eq!(track.art_url, Some("file:///tmp/art.png".to_string()));
        assert!((track.position_secs - 30.0).abs() < 0.01);
        assert!((track.length_secs - 230.0).abs() < 0.01);
    }

    #[test]
    fn test_parse_empty_string() {
        assert!(MediaTrack::parse_formatted("").is_none());
        assert!(MediaTrack::parse_formatted("   ").is_none());
    }
}
