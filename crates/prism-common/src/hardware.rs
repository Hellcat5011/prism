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
        Backlight::Ddc(bus) => {
            // Straight over I2C (~20 ms), else ddcutil (~0.4 s and busy on the GPU's bus).
            if let Some(max) = fade::ddc_max(bus) {
                if fade::ddc_write(bus, (clamped * max + 50) / 100).is_ok() {
                    store_brightness(clamped);
                    return Ok(clamped);
                }
            }
            Command::new("ddcutil").args(["-b", bus, "--noverify", "setvcp", "10", &clamped.to_string()]).status()
        }
        Backlight::None => return Err("no brightness control found".to_string()),
    }
    .map_err(|e| format!("Failed to set brightness: {}", e))?;
    if !status.success() {
        return Err("setting brightness failed".to_string());
    }
    store_brightness(clamped);
    Ok(clamped)
}

/// Raw brightness control for fades (the idle dim): finer steps than percent and, for DDC/CI,
/// writes straight to the I2C bus instead of a ddcutil process per step (~0.4 s each).
pub mod fade {
    use super::{backlight, cached_brightness, Backlight, Command};
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};

    /// (current, maximum) in the device's own units. DDC: current from Prism's last set value
    /// (reading over DDC stalls the compositor), maximum read once.
    pub fn read() -> Option<(u32, u32)> {
        match backlight() {
            Backlight::Brightnessctl => {
                let out = Command::new("brightnessctl").args(["-m", "-c", "backlight", "info"]).output().ok()?;
                // device,class,curr,curr_pct,max
                let text = String::from_utf8_lossy(&out.stdout);
                let f: Vec<&str> = text.lines().next()?.split(',').collect();
                Some((f.get(2)?.parse().ok()?, f.get(4)?.trim().parse().ok()?))
            }
            Backlight::Ddc(bus) => {
                let max = ddc_max(bus)?;
                let pct = cached_brightness().or_else(|| super::get_brightness().ok())?;
                Some(((pct * max + 50) / 100, max))
            }
            Backlight::None => None,
        }
    }

    /// Set the raw value. Blocks for the write (DDC: also until 50 ms after the previous one).
    pub fn set(value: u32) -> Result<(), String> {
        match backlight() {
            Backlight::Brightnessctl => {
                let ok = Command::new("brightnessctl").args(["-c", "backlight", "s", &value.to_string(), "-q"]).status().map_err(|e| e.to_string())?.success();
                if ok { Ok(()) } else { Err("brightnessctl failed".into()) }
            }
            Backlight::Ddc(bus) => {
                if ddc_write(bus, value).is_ok() {
                    return Ok(());
                }
                let ok = Command::new("ddcutil").args(["-b", bus, "--noverify", "setvcp", "10", &value.to_string()]).status().map_err(|e| e.to_string())?.success();
                if ok { Ok(()) } else { Err("ddcutil failed".into()) }
            }
            Backlight::None => Err("no brightness control found".into()),
        }
    }

    /// Whether `set` is quick enough for many small steps.
    pub fn fast() -> bool {
        match backlight() {
            Backlight::Brightnessctl => true,
            Backlight::Ddc(bus) => DEV.lock().is_ok_and(|d| d.is_some()) || open(bus).is_ok(),
            Backlight::None => false,
        }
    }

    /// The monitor's brightness maximum: read once with ddcutil, then kept in
    /// `~/.cache/prism/ddc-max` (`<bus> <max>`) so other processes need no DDC read.
    pub(super) fn ddc_max(bus: &str) -> Option<u32> {
        static MAX: OnceLock<Option<u32>> = OnceLock::new();
        *MAX.get_or_init(|| {
            let cache = super::brightness_cache_path().with_file_name("ddc-max");
            if let Some(m) = std::fs::read_to_string(&cache).ok().and_then(|t| {
                let (b, m) = t.trim().split_once(' ')?;
                (b == bus).then(|| m.parse::<u32>().ok()).flatten()
            }) {
                return Some(m);
            }
            let out = Command::new("ddcutil").args(["-b", bus, "getvcp", "10", "--brief"]).output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            let f: Vec<&str> = text.split_whitespace().collect();
            let i = f.iter().position(|t| *t == "C")?;
            let max = f.get(i + 2)?.parse().ok().filter(|m| *m > 0)?;
            let _ = std::fs::write(&cache, format!("{bus} {max}"));
            Some(max)
        })
    }

    /// The open bus, and when it was last written (DDC/CI wants 50 ms between commands).
    static DEV: Mutex<Option<std::fs::File>> = Mutex::new(None);
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    fn open(bus: &str) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        const I2C_SLAVE: libc::c_ulong = 0x0703;
        let f = std::fs::OpenOptions::new().read(true).write(true).open(format!("/dev/i2c-{bus}")).map_err(|e| e.to_string())?;
        // SAFETY: plain ioctl on an fd we own; the address is DDC/CI's 0x37.
        if unsafe { libc::ioctl(f.as_raw_fd(), I2C_SLAVE as _, 0x37 as libc::c_ulong) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        *DEV.lock().map_err(|_| "poisoned")? = Some(f);
        Ok(())
    }

    /// DDC/CI "Set VCP Feature" 0x10 (brightness), at most one per 50 ms.
    pub(super) fn ddc_write(bus: &str, value: u32) -> Result<(), String> {
        if DEV.lock().map_err(|_| "poisoned")?.is_none() {
            open(bus)?;
        }
        let packet = set_vcp_packet(0x10, value.min(0xffff) as u16);
        let mut last = LAST.lock().map_err(|_| "poisoned")?;
        if let Some(t) = *last {
            let rest = std::time::Duration::from_millis(50).saturating_sub(t.elapsed());
            if !rest.is_zero() {
                std::thread::sleep(rest);
            }
        }
        let mut dev = DEV.lock().map_err(|_| "poisoned")?;
        let f = dev.as_mut().ok_or("closed")?;
        let r = f.write_all(&packet).map_err(|e| {
            *dev = None;
            e.to_string()
        });
        *last = Some(std::time::Instant::now());
        r
    }

    /// Source 0x51, length 0x84 (4 bytes follow), opcode 0x03, VCP code, value, checksum: XOR
    /// of the destination address byte 0x6e and every byte before it.
    pub(crate) fn set_vcp_packet(code: u8, value: u16) -> [u8; 7] {
        let mut p = [0x51, 0x84, 0x03, code, (value >> 8) as u8, value as u8, 0];
        p[6] = p[..6].iter().fold(0x6e, |a, b| a ^ b);
        p
    }
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
    fn ddc_set_vcp_packet() {
        // Matches what ddcutil sends for `setvcp 10 50`.
        assert_eq!(fade::set_vcp_packet(0x10, 50), [0x51, 0x84, 0x03, 0x10, 0x00, 0x32, 0x51 ^ 0x84 ^ 0x03 ^ 0x10 ^ 0x32 ^ 0x6e]);
    }

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

#[cfg(test)]
mod fade_live {
    /// `cargo test --release -p prism-common fade_live -- --ignored --nocapture` (changes brightness).
    #[test]
    #[ignore]
    fn fade_live() {
        let (cur, max) = super::fade::read().expect("no brightness control");
        println!("cur {cur} max {max} fast {}", super::fade::fast());
        let t = std::time::Instant::now();
        let mut n = 0;
        for v in (cur / 2..=cur).rev().chain(cur / 2..=cur) {
            super::fade::set(v).unwrap();
            n += 1;
        }
        println!("{n} writes in {:?} ({:?} each)", t.elapsed(), t.elapsed() / n);
        let pct = super::cached_brightness().unwrap_or(100);
        let t = std::time::Instant::now();
        for v in (pct.saturating_sub(20).max(1)..=pct).rev().chain(pct.saturating_sub(20).max(1)..=pct) {
            super::set_brightness(v).unwrap();
        }
        println!("set_brightness: 42 steps in {:?}", t.elapsed());
    }
}
