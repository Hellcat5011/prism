use std::process::Command;

/// Maximum allowable audio volume percentage (safety ceiling)
pub const MAX_VOLUME: u32 = 100;

/// Parse wpctl get-volume output, returning (volume_pct, is_muted)
pub fn parse_wpctl_volume(raw: &str) -> (u32, bool) {
    let is_muted = raw.contains("[MUTED]");
    // Example: "Volume: 0.65" or "Volume: 0.65 [MUTED]"
    let vol_part = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.0);

    let pct = (vol_part * 100.0).round() as u32;
    (pct.min(MAX_VOLUME), is_muted)
}

/// Retrieve the current default sink volume (0-100) and mute status
pub fn get_volume() -> Result<(u32, bool), String> {
    let output = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .map_err(|e| format!("Failed to run wpctl: {}", e))?;

    if !output.status.success() {
        return Err("wpctl failed to get volume".to_string());
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_wpctl_volume(&text))
}

/// Set default sink volume to absolute percentage (0-100)
pub fn set_volume(pct: u32) -> Result<u32, String> {
    let clamped = pct.min(MAX_VOLUME);
    let val_str = format!("{:.2}", (clamped as f32) / 100.0);

    let status = Command::new("wpctl")
        .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &val_str])
        .status()
        .map_err(|e| format!("Failed to set volume: {}", e))?;

    if !status.success() {
        return Err("wpctl set-volume failed".to_string());
    }

    Ok(clamped)
}

/// Adjust volume by delta percentage (+5, -5), automatically unmuting on volume increase
pub fn change_volume(delta: i32) -> Result<u32, String> {
    let (current, is_muted) = get_volume().unwrap_or((50, false));
    let new_val = (current as i32 + delta).clamp(0, MAX_VOLUME as i32) as u32;

    set_volume(new_val)?;

    if delta > 0 && is_muted {
        let _ = Command::new("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "0"])
            .status();
    }

    Ok(new_val)
}

/// Toggle mute state on default sink, returning new mute status
pub fn toggle_mute() -> Result<bool, String> {
    let status = Command::new("wpctl")
        .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
        .status()
        .map_err(|e| format!("Failed to toggle mute: {}", e))?;

    if !status.success() {
        return Err("wpctl set-mute failed".to_string());
    }

    let (_, muted) = get_volume().unwrap_or((0, false));
    Ok(muted)
}

/// Get current panel brightness percentage (0-100)
pub fn get_brightness() -> Result<u32, String> {
    let output = Command::new("brightnessctl")
        .args(["-m"])
        .output()
        .map_err(|e| format!("Failed to run brightnessctl: {}", e))?;

    if !output.status.success() {
        return Err("brightnessctl failed".to_string());
    }

    // brightnessctl -m format: device,class,curr,curr_pct,max
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() >= 4 {
            let pct_str = parts[3].trim_end_matches('%');
            if let Ok(pct) = pct_str.parse::<u32>() {
                return Ok(pct.min(100));
            }
        }
    }

    Err("Could not parse brightness".to_string())
}

/// Set panel brightness to absolute percentage (1-100)
pub fn set_brightness(pct: u32) -> Result<u32, String> {
    let clamped = pct.clamp(1, 100);
    let arg = format!("{}%", clamped);

    let status = Command::new("brightnessctl")
        .args(["s", &arg, "-q"])
        .status()
        .map_err(|e| format!("Failed to set brightness: {}", e))?;

    if !status.success() {
        return Err("brightnessctl set failed".to_string());
    }

    Ok(clamped)
}

/// Adjust brightness by delta percentage (+5, -5)
pub fn change_brightness(delta: i32) -> Result<u32, String> {
    let current = get_brightness().unwrap_or(50);
    let new_val = (current as i32 + delta).clamp(1, 100) as u32;
    set_brightness(new_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wpctl_volume() {
        let (vol, muted) = parse_wpctl_volume("Volume: 0.65");
        assert_eq!(vol, 65);
        assert!(!muted);

        let (vol_muted, is_muted) = parse_wpctl_volume("Volume: 0.40 [MUTED]");
        assert_eq!(vol_muted, 40);
        assert!(is_muted);

        let (vol_cap, _) = parse_wpctl_volume("Volume: 1.50");
        assert_eq!(vol_cap, 100);
    }
}
