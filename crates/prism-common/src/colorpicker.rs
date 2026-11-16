use serde::{Deserialize, Serialize};
use std::process::Command;

/// Represents a sampled screen color in multiple color spaces
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorSample {
    pub hex: String,
    pub rgb: (u8, u8, u8),
    pub hsl: (u16, u8, u8), // Hue 0-360, Saturation 0-100, Lightness 0-100
}

impl ColorSample {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let (r, g, b) = hex_to_rgb(hex)?;
        let hsl = rgb_to_hsl(r, g, b);
        let normalized_hex = format!("#{:02x}{:02x}{:02x}", r, g, b);
        Some(Self {
            hex: normalized_hex,
            rgb: (r, g, b),
            hsl,
        })
    }
}

/// Convert a hex string (#RRGGBB or RRGGBB) to (r, g, b)
pub fn hex_to_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
    let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
    let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Convert (r, g, b) to hex string (#rrggbb)
pub fn rgb_to_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

/// Convert (r, g, b) to HSL: (H: 0-360, S: 0-100%, L: 0-100%)
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (u16, u8, u8) {
    let r_f = r as f32 / 255.0;
    let g_f = g as f32 / 255.0;
    let b_f = b as f32 / 255.0;

    let max = r_f.max(g_f).max(b_f);
    let min = r_f.min(g_f).min(b_f);
    let delta = max - min;

    let l = (max + min) / 2.0;

    let (h, s) = if delta == 0.0 {
        (0.0, 0.0)
    } else {
        let s = if l < 0.5 {
            delta / (max + min)
        } else {
            delta / (2.0 - max - min)
        };

        let mut h = if max == r_f {
            (g_f - b_f) / delta + (if g_f < b_f { 6.0 } else { 0.0 })
        } else if max == g_f {
            (b_f - r_f) / delta + 2.0
        } else {
            (r_f - g_f) / delta + 4.0
        };

        h *= 60.0;
        (h, s)
    };

    (
        h.round() as u16,
        (s * 100.0).round() as u8,
        (l * 100.0).round() as u8,
    )
}

/// Sample a screen pixel via hyprpicker or wayland screen sample tool
pub fn pick_color() -> Result<ColorSample, String> {
    // Try hyprpicker first
    if let Ok(output) = Command::new("hyprpicker").args(["-a", "-f", "hex"]).output() {
        if output.status.success() {
            let hex_raw = String::from_utf8_lossy(&output.stdout);
            if let Some(sample) = ColorSample::from_hex(&hex_raw) {
                return Ok(sample);
            }
        }
    }

    // Try grim + convert single pixel as fallback
    let tmp_path = "/tmp/prism_pixel.png";
    if let Ok(status) = Command::new("grim").args(["-g", "0,0 1x1", tmp_path]).status() {
        if status.success() {
            // Read pixel from imagemagick if available
            if let Ok(output) = Command::new("magick").args([tmp_path, "-format", "%[hex:p{0,0}]", "info:"]).output() {
                if output.status.success() {
                    let hex_raw = String::from_utf8_lossy(&output.stdout);
                    if let Some(sample) = ColorSample::from_hex(&hex_raw) {
                        return Ok(sample);
                    }
                }
            }
        }
    }

    Err("No color picker utility (hyprpicker / grim) available or capture cancelled".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_conversions() {
        let (r, g, b) = hex_to_rgb("#8bd0f0").unwrap();
        assert_eq!((r, g, b), (139, 208, 240));
        assert_eq!(rgb_to_hex(r, g, b), "#8bd0f0");

        assert_eq!(hex_to_rgb("invalid"), None);
    }

    #[test]
    fn test_rgb_to_hsl() {
        // Red
        let (h, s, l) = rgb_to_hsl(255, 0, 0);
        assert_eq!(h, 0);
        assert_eq!(s, 100);
        assert_eq!(l, 50);

        // White
        let (_, s_w, l_w) = rgb_to_hsl(255, 255, 255);
        assert_eq!(s_w, 0);
        assert_eq!(l_w, 100);

        // Black
        let (_, s_b, l_b) = rgb_to_hsl(0, 0, 0);
        assert_eq!(s_b, 0);
        assert_eq!(l_b, 0);
    }

    #[test]
    fn test_color_sample_creation() {
        let sample = ColorSample::from_hex("#ff8800").unwrap();
        assert_eq!(sample.hex, "#ff8800");
        assert_eq!(sample.rgb, (255, 136, 0));
        assert_eq!(sample.hsl.0, 32); // orange hue
    }
}
