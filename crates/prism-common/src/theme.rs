use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Theme {
    #[serde(default = "default_background")]
    pub background: String,

    #[serde(default = "default_on_background")]
    pub on_background: String,

    #[serde(default = "default_surface")]
    pub surface: String,

    #[serde(default = "default_surface_variant")]
    pub surface_variant: String,

    #[serde(default = "default_on_surface")]
    pub on_surface: String,

    #[serde(default = "default_on_surface_variant")]
    pub on_surface_variant: String,

    #[serde(default = "default_primary")]
    pub primary: String,

    #[serde(default = "default_on_primary")]
    pub on_primary: String,

    #[serde(default = "default_primary_container")]
    pub primary_container: String,

    #[serde(default = "default_secondary")]
    pub secondary: String,

    #[serde(default = "default_secondary_container")]
    pub secondary_container: String,

    #[serde(default = "default_outline")]
    pub outline: String,

    #[serde(default = "default_outline_variant")]
    pub outline_variant: String,

    #[serde(default = "default_error")]
    pub error: String,

    #[serde(default = "default_shadow")]
    pub shadow: String,

    #[serde(default = "default_inverse_primary")]
    pub inverse_primary: String,

    #[serde(default = "default_on_primary_container")]
    pub on_primary_container: String,

    #[serde(default = "default_inverse_surface")]
    pub inverse_surface: String,

    #[serde(default = "default_inverse_on_surface")]
    pub inverse_on_surface: String,
}

fn default_background() -> String { "#0f1417".to_string() }
fn default_on_background() -> String { "#dfe3e7".to_string() }
fn default_surface() -> String { "#0f1417".to_string() }
fn default_surface_variant() -> String { "#40484c".to_string() }
fn default_on_surface() -> String { "#dfe3e7".to_string() }
fn default_on_surface_variant() -> String { "#c0c8cd".to_string() }
fn default_primary() -> String { "#8bd0f0".to_string() }
fn default_on_primary() -> String { "#003546".to_string() }
fn default_primary_container() -> String { "#004d64".to_string() }
fn default_secondary() -> String { "#b4cad6".to_string() }
fn default_secondary_container() -> String { "#354a54".to_string() }
fn default_outline() -> String { "#8a9297".to_string() }
fn default_outline_variant() -> String { "#40484c".to_string() }
fn default_error() -> String { "#ffb4ab".to_string() }
fn default_shadow() -> String { "#000000".to_string() }
fn default_inverse_primary() -> String { "#136682".to_string() }
fn default_on_primary_container() -> String { "#bee9ff".to_string() }
fn default_inverse_surface() -> String { "#dfe3e7".to_string() }
fn default_inverse_on_surface() -> String { "#2c3134".to_string() }

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: default_background(),
            on_background: default_on_background(),
            surface: default_surface(),
            surface_variant: default_surface_variant(),
            on_surface: default_on_surface(),
            on_surface_variant: default_on_surface_variant(),
            primary: default_primary(),
            on_primary: default_on_primary(),
            primary_container: default_primary_container(),
            secondary: default_secondary(),
            secondary_container: default_secondary_container(),
            outline: default_outline(),
            outline_variant: default_outline_variant(),
            error: default_error(),
            shadow: default_shadow(),
            inverse_primary: default_inverse_primary(),
            on_primary_container: default_on_primary_container(),
            inverse_surface: default_inverse_surface(),
            inverse_on_surface: default_inverse_on_surface(),
        }
    }
}

impl Theme {
    pub fn colors_path() -> PathBuf {
        if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
            PathBuf::from(config_home).join("prism").join("colors.json")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".config").join("prism").join("colors.json")
        } else {
            PathBuf::from("colors.json")
        }
    }

    /// Where matugen's palette actually lives. `colors_path()` is preferred; the template in the
    /// user's matugen config still writes the QML-era location, so fall back to it (before this
    /// the Rust UI silently used the built-in defaults and never followed the wallpaper).
    pub fn source_path() -> PathBuf {
        let primary = Self::colors_path();
        if primary.exists() {
            return primary;
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let legacy = PathBuf::from(home).join(".config/quickshell/prism/data/colors.json");
        if legacy.exists() {
            legacy
        } else {
            primary
        }
    }

    pub fn load() -> Self {
        let path = Self::source_path();
        if path.exists() {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(theme) = serde_json::from_str(&data) {
                    return theme;
                }
            }
        }
        Self::default()
    }

    pub fn load_from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let data = fs::read_to_string(path)?;
        let theme = serde_json::from_str(&data)?;
        Ok(theme)
    }
}
