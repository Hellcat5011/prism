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
/// How screen brightness is controlled on this machine (detected once).
#[derive(Clone, Debug, PartialEq)]
enum Backlight {
    /// Laptop panel via brightnessctl (backlight class).
    Brightnessctl,
    /// External monitor over DDC/CI: ddcutil on this I2C bus number.
    Ddc(String),
    None,
}

fn backlight() -> &'static Backlight {
    static B: std::sync::OnceLock<Backlight> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        let panel = Command::new("brightnessctl")
            .args(["-m", "-c", "backlight", "info"])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false);
        if panel {
            return Backlight::Brightnessctl;
        }
        Command::new("ddcutil")
            .args(["detect", "--brief"])
            .output()
            .ok()
            .and_then(|o| parse_ddc_bus(&String::from_utf8_lossy(&o.stdout)))
            .map(Backlight::Ddc)
            .unwrap_or(Backlight::None)
    })
}

/// First "I2C bus: /dev/i2c-N" in `ddcutil detect --brief` output.
fn parse_ddc_bus(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("I2C bus:"))
        .filter_map(|v| v.trim().strip_prefix("/dev/i2c-"))
        .map(|n| n.trim().to_string())
        .find(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// `ddcutil getvcp 10 --brief` output "VCP 10 C <cur> <max>" to a percentage.
fn parse_ddc_brightness(text: &str) -> Option<u32> {
    let f: Vec<&str> = text.split_whitespace().collect();
    let i = f.iter().position(|t| *t == "C")?;
    let cur: u32 = f.get(i + 1)?.parse().ok()?;
    let max: u32 = f.get(i + 2)?.parse().ok()?;
    (max > 0).then(|| (cur * 100 + max / 2) / max)
}

fn brightness_cache_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/prism/brightness")
}

/// Last brightness Prism set, without touching the hardware. Reading over DDC/CI goes through
/// the GPU's display link and stalls the compositor for a few hundred ms, so UIs use this.
pub fn cached_brightness() -> Option<u32> {
    std::fs::read_to_string(brightness_cache_path()).ok()?.trim().parse().ok()
}

fn store_brightness(pct: u32) {
    let path = brightness_cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, pct.to_string());
}

/// Current screen brightness in percent.
pub fn get_brightness() -> Result<u32, String> {
    match backlight() {
        Backlight::Brightnessctl => {
            let output = Command::new("brightnessctl")
                .args(["-m", "-c", "backlight", "info"])
                .output()
                .map_err(|e| format!("Failed to run brightnessctl: {}", e))?;
            // brightnessctl -m format: device,class,curr,curr_pct,max
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines()
                .filter_map(|l| l.split(',').nth(3))
                .filter_map(|p| p.trim_end_matches('%').parse::<u32>().ok())
                .map(|p| p.min(100))
                .next()
                .ok_or_else(|| "Could not parse brightness".to_string())
        }
        Backlight::Ddc(bus) => {
            let output = Command::new("ddcutil")
                .args(["-b", bus, "getvcp", "10", "--brief"])
                .output()
                .map_err(|e| format!("Failed to run ddcutil: {}", e))?;
            parse_ddc_brightness(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| "Could not parse brightness".to_string())
        }
        Backlight::None => Err("no brightness control found".to_string()),
    }
}

/// Set screen brightness to an absolute percentage (1-100).
pub fn set_brightness(pct: u32) -> Result<u32, String> {
    let clamped = pct.clamp(1, 100);
    let status = match backlight() {
        Backlight::Brightnessctl => Command::new("brightnessctl")
            .args(["-c", "backlight", "s", &format!("{}%", clamped), "-q"])
            .status(),
        Backlight::Ddc(bus) => Command::new("ddcutil")
            .args(["-b", bus, "--noverify", "setvcp", "10", &clamped.to_string()])
            .status(),
        Backlight::None => return Err("no brightness control found".to_string()),
    }
    .map_err(|e| format!("Failed to set brightness: {}", e))?;
    if !status.success() {
        return Err("setting brightness failed".to_string());
    }
    store_brightness(clamped);
    Ok(clamped)
}

/// Adjust brightness by delta percentage (+5, -5)
pub fn change_brightness(delta: i32) -> Result<u32, String> {
    let current = match backlight() {
        Backlight::Ddc(_) => cached_brightness().or_else(|| get_brightness().ok()).unwrap_or(50),
        _ => get_brightness().unwrap_or(50),
    };
    let new_val = (current as i32 + delta).clamp(1, 100) as u32;
    set_brightness(new_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddcutil_output() {
        let detect = "Display 1\n   I2C bus:          /dev/i2c-7\n   DRM connector:    card1-DP-2\n";
        assert_eq!(parse_ddc_bus(detect), Some("7".to_string()));
        assert_eq!(parse_ddc_bus("No displays found"), None);
        assert_eq!(parse_ddc_brightness("VCP 10 C 50 100"), Some(50));
        assert_eq!(parse_ddc_brightness("VCP 10 C 30 60"), Some(50));
        assert_eq!(parse_ddc_brightness("garbage"), None);
    }

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

/// Battery attributes that stop charging early (laptops whose firmware supports it).
pub fn charge_limit_files() -> Vec<std::path::PathBuf> {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return Vec::new() };
    let mut out: Vec<std::path::PathBuf> = dir
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("BAT"))
        .map(|e| e.path().join("charge_control_end_threshold"))
        .filter(|p| p.exists())
        .collect();
    out.sort();
    out
}

/// Stop charging at `limit` % (0 or 100: charge fully). Writes the attribute directly (the
/// installer's udev rule makes it writable for the wheel group); with `ask`, falls back to
/// `pkexec tee` (a password prompt) when that is not allowed.
pub fn set_charge_limit(limit: u8, ask: bool) -> Result<(), String> {
    let value = if limit == 0 { 100 } else { limit.clamp(20, 100) };
    let files = charge_limit_files();
    if files.is_empty() {
        return Err("no battery with a charge limit".into());
    }
    for f in files {
        if std::fs::read_to_string(&f).is_ok_and(|v| v.trim() == value.to_string()) {
            continue;
        }
        match std::fs::write(&f, format!("{value}\n")) {
            Ok(()) => {}
            Err(e) if ask && e.kind() == std::io::ErrorKind::PermissionDenied => {
                use std::io::Write;
                let mut child = Command::new("pkexec")
                    .arg("tee")
                    .arg(&f)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| format!("pkexec: {e}"))?;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = writeln!(stdin, "{value}");
                }
                if !child.wait().map_err(|e| e.to_string())?.success() {
                    return Err(format!("could not write {}", f.display()));
                }
            }
            Err(e) => return Err(format!("{}: {e}", f.display())),
        }
    }
    Ok(())
}
