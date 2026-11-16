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
            .map(|mut entries| entries.any(|e| e.map(|entry| entry.path().extension().map_or(false, |ext| ext == "png")).unwrap_or(false)))
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
                    if p.extension().map_or(false, |ext| ext == "png") {
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

/// Generate depth mask for given wallpaper
pub fn generate_depth(wallpaper: &Path) -> Result<String, String> {
    if !wallpaper.exists() {
        return Err(format!("Wallpaper file does not exist: {:?}", wallpaper));
    }

    let script = find_script("scripts/wallpaper-depth.py")
        .ok_or_else(|| "Could not find scripts/wallpaper-depth.py".to_string())?;

    let output = Command::new("python3")
        .args([script.to_str().unwrap(), "--generate", wallpaper.to_str().unwrap()])
        .output()
        .map_err(|e| format!("Failed to spawn wallpaper-depth.py: {}", e))?;

    if output.status.success() {
        Ok("Depth mask generation completed successfully".to_string())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(format!("Depth mask generation failed: {}", err))
    }
}

/// Clear cached wallpaper depth masks
pub fn clear_depth() -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let cache_home = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{}/.cache", home));
    let cache_dir = PathBuf::from(cache_home).join("Prism/wallpaper-depth");

    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir).map_err(|e| format!("Failed to clear depth cache: {}", e))?;
    }

    Ok(())
}

fn find_script(rel_path: &str) -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(rel_path),
        PathBuf::from("..").join(rel_path),
        PathBuf::from("../..").join(rel_path),
    ];
    for p in &candidates {
        if p.exists() {
            return Some(p.clone());
        }
    }
    None
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
