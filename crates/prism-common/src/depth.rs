use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DepthStatus {
    pub has_model: bool,
    pub has_mask: bool,
    pub mask_path: Option<PathBuf>,
}

/// Retrieve the current status of wallpaper depth estimation
pub fn get_depth_status() -> DepthStatus {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home));
    let cache_home = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{}/.cache", home));

    let model_path = PathBuf::from(data_home).join("prism/wallpaper-depth/models/depth-anything-v2-small.onnx");
    let has_model = model_path.exists();

    let cache_dir = PathBuf::from(cache_home).join("Prism/wallpaper-depth");
    let has_mask = if cache_dir.exists() {
        fs::read_dir(&cache_dir)
            .map(|mut entries| entries.any(|e| e.map(|entry| entry.path().extension().is_some_and(|ext| ext == "png")).unwrap_or(false)))
            .unwrap_or(false)
    } else {
        false
    };

    let mask_path = if has_mask {
        fs::read_dir(&cache_dir)
            .ok()
            .and_then(|mut entries| {
                entries.find_map(|e| {
                    let p = e.ok()?.path();
                    if p.extension().is_some_and(|ext| ext == "png") {
                        Some(p)
                    } else {
                        None
                    }
                })
            })
    } else {
        None
    };

    DepthStatus {
        has_model,
        has_mask,
        mask_path,
    }
}

/// Depth settings that decide which mask belongs to a wallpaper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthParams {
    pub threshold: u32,
    pub feather: u32,
    /// Script device: "cpu", "gpu" or "auto".
    pub device: String,
}

impl DepthParams {
    pub fn from_config(cfg: &crate::Config) -> DepthParams {
        let device = match cfg.depth_device.as_str() {
            "cuda" | "gpu" => "gpu",
            "auto" => "auto",
            _ => "cpu",
        };
        DepthParams { threshold: cfg.depth_threshold.min(100), feather: cfg.depth_feather.min(50), device: device.into() }
    }

    fn args(&self) -> Vec<String> {
        vec!["--threshold".into(), self.threshold.to_string(), "--feather".into(), self.feather.to_string(), "--device".into(), self.device.clone()]
    }
}

/// `wallpaper-depth.py` from the installed scripts or the repository.
pub fn script() -> Option<PathBuf> {
    crate::keybinds::find_script("scripts/wallpaper-depth.py")
}

/// Generate (or fetch from cache) the foreground mask for `wallpaper`. Slow on a cache miss:
/// call it off the main thread. Returns the mask PNG (white, alpha = foreground).
pub fn generate_mask(wallpaper: &Path, params: &DepthParams) -> Result<PathBuf, String> {
    if !wallpaper.is_file() {
        return Err(format!("wallpaper not found: {}", wallpaper.display()));
    }
    let script = script().ok_or("wallpaper-depth.py not found")?;
    let output = Command::new("python3")
        .arg(&script)
        .arg("generate")
        .arg(wallpaper)
        .args(params.args())
        .output()
        .map_err(|e| format!("running wallpaper-depth.py: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut error = None;
    for line in stdout.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(mask) = v.get("mask").and_then(|m| m.as_str()) {
            if v.get("status").and_then(|s| s.as_str()) == Some("ready") && Path::new(mask).is_file() {
                return Ok(PathBuf::from(mask));
            }
        }
        if error.is_none() {
            error = v.get("error").or_else(|| v.get("message")).and_then(|m| m.as_str()).map(String::from);
        }
    }
    Err(error.unwrap_or_else(|| {
        let err = String::from_utf8_lossy(&output.stderr);
        format!("depth generation failed: {}", err.lines().last().unwrap_or("no output"))
    }))
}

/// Pre-generate masks for every wallpaper in `dir` (long-running child process).
pub fn pregenerate_command(dir: &Path, params: &DepthParams) -> Option<Command> {
    let script = script()?;
    let mut c = Command::new("nice");
    c.args(["-n", "19", "python3"]).arg(script).arg("pregenerate").arg(dir).args(params.args());
    Some(c)
}

/// File naming the mask the desktop widgets should use (empty: none).
pub fn mask_pointer_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".cache/prism/depth-mask.txt")
}

/// Publish the active mask (atomic rename so inotify readers see a whole file).
pub fn publish_mask(mask: Option<&Path>) {
    let p = mask_pointer_path();
    if current_mask().as_deref() == mask && p.exists() {
        return;
    }
    let _ = fs::create_dir_all(p.parent().unwrap());
    let tmp = p.with_extension("tmp");
    let text = mask.map(|m| m.display().to_string()).unwrap_or_default();
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, &p);
    }
}

/// The published mask, if it still exists.
pub fn current_mask() -> Option<PathBuf> {
    let text = fs::read_to_string(mask_pointer_path()).ok()?;
    let p = PathBuf::from(text.trim());
    (!text.trim().is_empty() && p.is_file()).then_some(p)
}

/// Clear cached wallpaper depth masks
pub fn clear_depth() -> Result<(), String> {
    publish_mask(None);
    let home = std::env::var("HOME").unwrap_or_default();
    let cache_home = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{}/.cache", home));
    let cache_dir = PathBuf::from(cache_home).join("Prism/wallpaper-depth");

    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir).map_err(|e| format!("Failed to clear depth cache: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_depth_status_query() {
        let status = get_depth_status();
        // Simply checks that the check runs and returns a boolean state without panicking
        let _ = status.has_model;
    }
}
