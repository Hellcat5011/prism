use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeybindEntry {
    #[serde(default)]
    pub mods: String,
    pub key: String,
    pub action: String,
    #[serde(alias = "section", alias = "category")]
    pub category: String,
}

impl KeybindEntry {
    pub fn formatted_combo(&self) -> String {
        if self.mods.is_empty() {
            self.key.clone()
        } else {
            format!("{}+{}", self.mods, self.key)
        }
    }
}

/// Load keybindings for the active Wayland compositor
pub fn load_keybinds() -> Vec<KeybindEntry> {
    let script_path = if env::var("NIRI_SOCKET").is_ok() {
        find_script("scripts/parse-niri-binds.py")
    } else if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        find_script("scripts/parse-hyprland-binds.py")
    } else {
        // MangoWM or default
        find_script("scripts/parse-mango-binds.py")
    };

    if let Some(script) = script_path {
        if let Ok(output) = Command::new("python3").arg(&script).output() {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Ok(entries) = serde_json::from_str::<Vec<KeybindEntry>>(&text) {
                    if !entries.is_empty() {
                        return entries;
                    }
                } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(arr) = val.as_array() {
                        let parsed: Vec<KeybindEntry> = arr.iter().filter_map(|item| {
                            let key = item.get("key")?.as_str()?.to_string();
                            let action = item.get("action")?.as_str()?.to_string();
                            let category = item.get("section")
                                .or_else(|| item.get("category"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("General")
                                .to_string();
                            let mods = item.get("mods")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            Some(KeybindEntry { mods, key, action, category })
                        }).collect();
                        if !parsed.is_empty() {
                            return parsed;
                        }
                    }
                }
            }
        }
    }

    fallback_keybinds()
}

fn find_script(rel_path: &str) -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(rel_path),
        PathBuf::from("..").join(rel_path),
        PathBuf::from("../..").join(rel_path),
        PathBuf::from("/mnt/hdd/config/quickshell/prism").join(rel_path),
    ];

    if let Ok(home) = env::var("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/share/prism").join(rel_path));
        candidates.push(PathBuf::from(&home).join(".config/quickshell/prism").join(rel_path));
    }

    if let Ok(exe) = env::current_exe() {
        if let Some(p) = exe.parent() {
            candidates.push(p.join("../share/prism").join(rel_path));
            candidates.push(p.join(rel_path));
        }
    }

    for p in &candidates {
        if p.exists() {
            return Some(p.clone());
        }
    }
    None
}

fn fallback_keybinds() -> Vec<KeybindEntry> {
    vec![
        KeybindEntry {
            mods: "Super".to_string(),
            key: "Space".to_string(),
            action: "Open App Launcher".to_string(),
            category: "Launcher".to_string(),
        },
        KeybindEntry {
            mods: "Super".to_string(),
            key: "L".to_string(),
            action: "Lock Session".to_string(),
            category: "Session".to_string(),
        },
        KeybindEntry {
            mods: "Super".to_string(),
            key: "V".to_string(),
            action: "Clipboard History".to_string(),
            category: "Tools".to_string(),
        },
        KeybindEntry {
            mods: "Super".to_string(),
            key: "Period".to_string(),
            action: "Emoji Picker".to_string(),
            category: "Tools".to_string(),
        },
        KeybindEntry {
            mods: "Super".to_string(),
            key: "P".to_string(),
            action: "Color Picker".to_string(),
            category: "Tools".to_string(),
        },
        KeybindEntry {
            mods: "Super+Shift".to_string(),
            key: "E".to_string(),
            action: "Power Menu".to_string(),
            category: "Session".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_keybinds() {
        let binds = fallback_keybinds();
        assert!(!binds.is_empty());
        assert_eq!(binds[0].category, "Launcher");
    }

    #[test]
    fn test_load_keybinds_live() {
        let binds = load_keybinds();
        assert!(binds.len() > 10, "Expected >10 keybinds, got {}", binds.len());
    }
}
