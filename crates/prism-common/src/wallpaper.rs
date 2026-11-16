use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Retrieves the current wallpaper path if saved
pub fn get_current_wallpaper() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from)?;
    
    // Check ~/.cache/prism/current-wallpaper.txt
    let cache_file = home.join(".cache/prism/current-wallpaper.txt");
    if let Ok(content) = fs::read_to_string(&cache_file) {
        let p = PathBuf::from(content.trim());
        if p.exists() {
            return Some(p);
        }
    }

    // Check data/current-wallpaper.txt in workspace
    let data_file = PathBuf::from("data/current-wallpaper.txt");
    if let Ok(content) = fs::read_to_string(&data_file) {
        let p = PathBuf::from(content.trim());
        if p.exists() {
            return Some(p);
        }
    }

    // Check ~/.wa.jpg
    let wa = home.join(".wa.jpg");
    if wa.exists() {
        return Some(wa);
    }

    None
}

/// Sets the desktop wallpaper and triggers dynamic theming via matugen
pub fn set_wallpaper(
    image_path: &Path,
    daemon: Option<&str>,
    scheme: Option<&str>,
    mode: Option<&str>,
) -> Result<(), String> {
    if !image_path.exists() {
        return Err(format!("Image file does not exist: {:?}", image_path));
    }

    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME environment variable not set".to_string())?;

    let wa_jpg = home.join(".wa.jpg");
    let wa_thumb = home.join(".wa-thumb.jpg");

    // Copy or resize using magick
    let has_magick = Command::new("magick")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_magick {
        let _ = Command::new("magick")
            .args([
                image_path.to_str().unwrap_or(""),
                "-resize",
                "1920x>",
                "-quality",
                "95",
                wa_jpg.to_str().unwrap_or(""),
            ])
            .output();

        let _ = Command::new("magick")
            .args([
                image_path.to_str().unwrap_or(""),
                "-resize",
                "960x>",
                "-quality",
                "90",
                wa_thumb.to_str().unwrap_or(""),
            ])
            .output();
    } else {
        fs::copy(image_path, &wa_jpg)
            .map_err(|e| format!("Failed to copy to ~/.wa.jpg: {}", e))?;
        fs::copy(image_path, &wa_thumb)
            .map_err(|e| format!("Failed to copy to ~/.wa-thumb.jpg: {}", e))?;
    }

    // Save to cache file
    let prism_cache_dir = home.join(".cache/prism");
    let _ = fs::create_dir_all(&prism_cache_dir);
    let _ = fs::write(
        prism_cache_dir.join("current-wallpaper.txt"),
        image_path.to_string_lossy().as_bytes(),
    );

    // Invoke wallpaper daemon
    let daemon_name = daemon.unwrap_or("awww");
    if daemon_name == "awww" {
        let _ = Command::new("awww")
            .args([
                "img",
                image_path.to_str().unwrap_or(""),
                "--transition-type",
                "random",
                "--transition-duration",
                "1",
                "--transition-fps",
                "60",
            ])
            .output();
    } else if daemon_name == "swww" {
        let _ = Command::new("swww")
            .args([
                "img",
                image_path.to_str().unwrap_or(""),
                "--transition-type",
                "random",
                "--transition-duration",
                "1",
                "--transition-fps",
                "60",
            ])
            .output();
    }

    // Invoke matugen if installed
    let scheme_str = scheme.unwrap_or("scheme-smart");
    let mode_str = mode.unwrap_or("smart");

    let has_matugen = Command::new("matugen")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_matugen {
        let _ = Command::new("matugen")
            .args([
                "image",
                image_path.to_str().unwrap_or(""),
                "-m",
                mode_str,
                "-t",
                scheme_str,
                "--source-color-index",
                "0",
            ])
            .output();
    }

    // Best-effort greeter wallpaper synchronization
    sync_greeter_wallpaper(&wa_jpg);

    Ok(())
}

/// Copies the given wallpaper file to greetd state directories if accessible
pub fn sync_greeter_wallpaper(source: &Path) {
    let state_dirs = [
        "/var/lib/greetd/prism",
        "/var/lib/greetd/quickshell-greeter",
    ];
    for dir_str in &state_dirs {
        let dir = Path::new(dir_str);
        if dir.is_dir() {
            let target = dir.join("wallpaper.jpg");
            let _ = fs::copy(source, &target);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o644));
            }
        }
    }
}

/// Scans the given directory for image wallpaper files (.jpg, .jpeg, .png, .webp)
pub fn scan_wallpapers(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let ext_low = ext.to_lowercase();
                    if matches!(ext_low.as_str(), "jpg" | "jpeg" | "png" | "webp") {
                        results.push(path);
                    }
                }
            }
        }
    }
    results.sort();
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_wallpaper_fallback() {
        // Function handles missing files gracefully
        let _ = get_current_wallpaper();
    }
}
