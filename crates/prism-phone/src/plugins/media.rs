//! The phone's media players (`kdeconnect.mpris`, desktop side "mprisremote"): what is playing,
//! with play/pause/next/previous and seek from the PC. Album art comes as a payload.

use crate::core::{Core, Shared};
use crate::identity::unix_ms;
use crate::packet::Packet;
use prism_common::phone::{phone_runtime_dir, MediaCommand, PhoneMedia};
use serde_json::{json, Value};

/// Ask for the player list (on connect and when the list changes).
pub fn on_connected(core: &mut Core, id: &str) {
    core.send(id, Packet::new("kdeconnect.mpris.request", json!({ "requestPlayerList": true })));
}

pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    let shared = core.shared();
    let Some(dev) = core.devices.get_mut(id) else { return };
    let media = dev.view.media.get_or_insert_with(PhoneMedia::default);
    let mut ask_now_playing = None;
    if let Some(list) = p.body.get("playerList").and_then(Value::as_array) {
        media.players = list.iter().filter_map(|v| v.as_str().map(String::from)).collect();
        if media.players.is_empty() {
            dev.view.media = None;
            core.publish();
            return;
        }
        if !media.players.contains(&media.player) {
            media.player = media.players[0].clone();
        }
        ask_now_playing = Some(media.player.clone());
    }
    if let Some(player) = p.str("player") {
        let player = player.to_string();
        if p.bool("transferringAlbumArt") {
            if let (Some(port), Some(shared)) = (p.payload_port(), shared.as_ref()) {
                fetch_art(shared, id, &player, p.str("albumArtUrl").unwrap_or_default(), port, p.payload_size.unwrap_or(-1));
            }
            return;
        }
        // Follow whichever player starts playing.
        if p.bool("isPlaying") && player != media.player {
            media.player = player.clone();
        }
        if player == media.player {
            let s = |k: &str| p.str(k).map(String::from);
            if let Some(v) = s("title") {
                media.title = v;
            }
            if let Some(v) = s("artist") {
                media.artist = v;
            }
            if let Some(v) = s("album") {
                media.album = v;
            }
            if p.body.get("isPlaying").is_some() {
                media.playing = p.bool("isPlaying");
            }
            if let Some(v) = p.int("pos") {
                media.position_ms = v;
                media.position_at_ms = unix_ms();
            }
            if let Some(v) = p.int("length") {
                media.length_ms = v;
            }
            let b = |k: &str, cur: bool| p.body.get(k).and_then(Value::as_bool).unwrap_or(cur);
            media.can_play = b("canPlay", media.can_play);
            media.can_pause = b("canPause", media.can_pause);
            media.can_next = b("canGoNext", media.can_next);
            media.can_previous = b("canGoPrevious", media.can_previous);
            media.can_seek = b("canSeek", media.can_seek);
            if let Some(url) = p.str("albumArtUrl") {
                let want = art_file(id, url);
                if url.is_empty() {
                    media.art_path = None;
                } else if std::path::Path::new(&want).exists() {
                    media.art_path = Some(want);
                } else if p.body.get("albumArtUrl").is_some() {
                    // Ask for the art as a payload.
                    let req = Packet::new("kdeconnect.mpris.request", json!({ "player": player, "albumArtUrl": url }));
                    core.send(id, req);
                }
            }
        }
    }
    if let Some(player) = ask_now_playing {
        core.send(id, Packet::new("kdeconnect.mpris.request", json!({ "player": player, "requestNowPlaying": true, "requestVolume": true })));
    }
    core.publish();
}

fn art_file(id: &str, url: &str) -> String {
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(format!("{id}{url}")).iter().take(8).map(|b| format!("{b:02x}")).collect();
    phone_runtime_dir().join(format!("art-{h}")).to_string_lossy().into_owned()
}

fn fetch_art(shared: &Shared, id: &str, player: &str, url: &str, port: u16, size: i64) {
    let (shared, id, player, url) = (shared.clone(), id.to_string(), player.to_string(), url.to_string());
    std::thread::spawn(move || {
        let path = art_file(&id, &url);
        let _ = std::fs::create_dir_all(phone_runtime_dir());
        let tmp = format!("{path}.part");
        let Ok(mut f) = std::fs::File::create(&tmp) else { return };
        if size > 8 * 1024 * 1024 {
            return;
        }
        match crate::payload::download(&shared, &id, port, size, &mut f, |_| true) {
            Ok(_) if std::fs::rename(&tmp, &path).is_ok() => {
                let mut core = shared.lock().unwrap();
                if let Some(m) = core.devices.get_mut(&id).and_then(|d| d.view.media.as_mut()).filter(|m| m.player == player) {
                    m.art_path = Some(path);
                }
                core.publish();
            }
            Ok(_) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                eprintln!("[prism-phoned] album art: {e}");
            }
        }
    });
}

pub fn command(core: &mut Core, id: &str, action: Option<MediaCommand>, seek: Option<i64>) -> Result<Option<String>, String> {
    let player = core.devices.get(id).and_then(|d| d.view.media.as_ref()).map(|m| m.player.clone()).ok_or("nothing is playing on the phone")?;
    let body = match (action, seek) {
        (Some(a), _) => {
            let name = match a {
                MediaCommand::PlayPause => "PlayPause",
                MediaCommand::Play => "Play",
                MediaCommand::Pause => "Pause",
                MediaCommand::Next => "Next",
                MediaCommand::Previous => "Previous",
            };
            json!({ "player": player, "action": name })
        }
        (None, Some(ms)) => {
            if let Some(m) = core.devices.get_mut(id).and_then(|d| d.view.media.as_mut()) {
                m.position_ms = ms;
                m.position_at_ms = unix_ms();
            }
            json!({ "player": player, "SetPosition": ms.max(0) })
        }
        _ => return Ok(None),
    };
    core.send(id, Packet::new("kdeconnect.mpris.request", body));
    core.publish();
    Ok(None)
}
