use serde::{Deserialize, Serialize};
use std::process::Command;

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

/// Query current media track from playerctl
pub fn query_current_media() -> Option<MediaTrack> {
    let output = Command::new("playerctl")
        .args([
            "metadata",
            "--format",
            "{{playerName}}|{{status}}|{{title}}|{{artist}}|{{album}}|{{mpris:artUrl}}|{{position}}|{{mpris:length}}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    MediaTrack::parse_formatted(&text)
}

/// Send play-pause toggle command
pub fn play_pause() -> Result<(), String> {
    Command::new("playerctl")
        .arg("play-pause")
        .output()
        .map_err(|e| format!("Failed to execute playerctl: {}", e))
        .map(|_| ())
}

/// Send next track command
pub fn next() -> Result<(), String> {
    Command::new("playerctl")
        .arg("next")
        .output()
        .map_err(|e| format!("Failed to execute playerctl: {}", e))
        .map(|_| ())
}

/// Send previous track command
pub fn previous() -> Result<(), String> {
    Command::new("playerctl")
        .arg("previous")
        .output()
        .map_err(|e| format!("Failed to execute playerctl: {}", e))
        .map(|_| ())
}

/// Send stop command
pub fn stop() -> Result<(), String> {
    Command::new("playerctl")
        .arg("stop")
        .output()
        .map_err(|e| format!("Failed to execute playerctl: {}", e))
        .map(|_| ())
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
