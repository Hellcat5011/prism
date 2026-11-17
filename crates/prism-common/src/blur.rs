//! Compositor blur strength (QML Config.applyNiriBlur): level 1..=5 picks a passes/offset
//! preset. Niri reads it from `~/.config/niri/user/shell-blur.kdl` (include that file from the
//! niri config); Hyprland gets it through `hyprctl keyword`. Mango has no runtime blur knob, so
//! nothing happens there.

use std::process::{Command, Stdio};

/// (passes, offset) per level, as in QML `blurPresets`.
const PRESETS: [(u32, f64); 5] = [(2, 2.0), (3, 3.0), (3, 4.5), (4, 5.0), (4, 7.0)];

pub const DEFAULT_LEVEL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurCompositor {
    Niri,
    Hyprland,
}

/// The running compositor, if its blur can be tuned.
pub fn compositor() -> Option<BlurCompositor> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    if std::env::var_os("NIRI_SOCKET").is_some() || desktop.split(':').any(|d| d == "niri") {
        Some(BlurCompositor::Niri)
    } else if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() || desktop.split(':').any(|d| d == "hyprland") {
        Some(BlurCompositor::Hyprland)
    } else {
        None
    }
}

/// The level stored in config (`blur_strength`), clamped to 1..=5. Values from before the levels
/// existed (pixel radii above 5) fall back to the default level.
pub fn level(blur_strength: f32) -> u8 {
    if (1.0..=5.0).contains(&blur_strength) {
        blur_strength.round() as u8
    } else {
        DEFAULT_LEVEL
    }
}

fn niri_kdl(level: u8) -> String {
    let (passes, offset) = PRESETS[(level.clamp(1, 5) - 1) as usize];
    format!("blur {{\n    passes {passes}\n    offset {offset:.1}\n}}\n")
}

/// Apply `level` to the running compositor. Returns an error message when writing failed.
pub fn apply(level: u8) -> Result<(), String> {
    let (passes, offset) = PRESETS[(level.clamp(1, 5) - 1) as usize];
    match compositor() {
        Some(BlurCompositor::Niri) => {
            let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
            let dir = std::path::Path::new(&home).join(".config/niri/user");
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = dir.join("shell-blur.kdl");
            std::fs::write(&path, niri_kdl(level)).map_err(|e| format!("{}: {e}", path.display()))
        }
        Some(BlurCompositor::Hyprland) => {
            let batch = format!("keyword decoration:blur:passes {passes} ; keyword decoration:blur:size {}", offset.round() as u32);
            Command::new("hyprctl")
                .args(["--batch", &batch])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("hyprctl: {e}"))
                .map(|_| ())
        }
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_presets() {
        assert_eq!(level(3.0), 3);
        assert_eq!(level(1.4), 1);
        assert_eq!(level(15.67), DEFAULT_LEVEL);
        assert_eq!(level(0.0), DEFAULT_LEVEL);
        assert_eq!(niri_kdl(5), "blur {\n    passes 4\n    offset 7.0\n}\n");
    }
}
