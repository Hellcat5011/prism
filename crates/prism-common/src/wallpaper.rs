use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::filter::Filter;

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

fn filtered_dir(home: &Path) -> PathBuf {
    home.join(".cache/prism/filtered")
}

fn filtered_pointer(home: &Path) -> PathBuf {
    home.join(".cache/prism/current-wallpaper-filtered.txt")
}

fn active_filter_file(home: &Path) -> PathBuf {
    home.join(".cache/prism/current-wallpaper-filter.txt")
}

/// The filter the current wallpaper was applied with.
pub fn active_filter() -> Filter {
    std::env::var("HOME")
        .ok()
        .and_then(|h| fs::read_to_string(active_filter_file(Path::new(&h))).ok())
        .map(|id| Filter::from_id(id.trim()))
        .unwrap_or_default()
}


/// The picture to put on screen: the filtered copy of the current wallpaper when a filter is on.
/// `get_current_wallpaper` stays the original (depth masks are generated from it).
pub fn displayed_wallpaper() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from)?;
    if let Ok(content) = fs::read_to_string(filtered_pointer(&home)) {
        let p = PathBuf::from(content.trim());
        if p.exists() {
            return Some(p);
        }
    }
    get_current_wallpaper()
}

/// The file to display for `image_path` under `filter`: the image itself for `Filter::Normal`,
/// else a full-size filtered copy in the cache (rendered once).
pub fn render_filtered(image_path: &Path, filter: Filter) -> Result<PathBuf, String> {
    if filter == Filter::Normal {
        return Ok(image_path.to_path_buf());
    }
    use std::hash::{Hash, Hasher};
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME environment variable not set".to_string())?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    image_path.hash(&mut hash);
    fs::metadata(image_path).and_then(|m| m.modified()).ok().hash(&mut hash);
    filter.hash(&mut hash);
    let dir = filtered_dir(&home);
    let out = dir.join(format!("{:016x}.jpg", hash.finish()));
    if !out.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        crate::pixbuf::save_jpeg_filtered(image_path, &out, i32::MAX, 96, filter)?;
    }
    Ok(out)
}

/// Sets the desktop wallpaper and triggers dynamic theming via matugen
pub fn set_wallpaper(
    image_path: &Path,
    daemon: Option<&str>,
    scheme: Option<&str>,
    mode: Option<&str>,
) -> Result<(), String> {
    set_wallpaper_filtered(image_path, Filter::Normal, daemon, scheme, mode)
}

/// `set_wallpaper` with a colour filter, also applied to the lock screen and greeter copies.
pub fn set_wallpaper_filtered(
    image_path: &Path,
    filter: Filter,
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

    let shown = render_filtered(image_path, filter)?;

    // Downscaled copies for the lock screen, settings previews and matugen.
    if crate::pixbuf::save_jpeg_filtered(image_path, &wa_jpg, 1920, 95, filter).is_err() {
        fs::copy(image_path, &wa_jpg).map_err(|e| format!("Failed to copy to ~/.wa.jpg: {}", e))?;
    }
    if crate::pixbuf::save_jpeg_filtered(image_path, &wa_thumb, 960, 90, filter).is_err() {
        fs::copy(image_path, &wa_thumb).map_err(|e| format!("Failed to copy to ~/.wa-thumb.jpg: {}", e))?;
    }

    // Save to cache file
    let prism_cache_dir = home.join(".cache/prism");
    let _ = fs::create_dir_all(&prism_cache_dir);
    let _ = fs::write(
        prism_cache_dir.join("current-wallpaper.txt"),
        image_path.to_string_lossy().as_bytes(),
    );
    if shown == image_path {
        let _ = fs::remove_file(filtered_pointer(&home));
    } else {
        let _ = fs::write(filtered_pointer(&home), shown.to_string_lossy().as_bytes());
    }
    if filter == Filter::Normal {
        let _ = fs::remove_file(active_filter_file(&home));
    } else {
        let _ = fs::write(active_filter_file(&home), filter.id());
    }
    // Only the copy on screen is kept.
    if let Ok(dir) = fs::read_dir(filtered_dir(&home)) {
        for f in dir.flatten().filter(|f| f.path() != shown) {
            let _ = fs::remove_file(f.path());
        }
    }

    // External wallpaper daemons. The built-in one ("prism") is told by the caller.
    let daemon_name = daemon.unwrap_or("prism");
    if daemon_name == "awww" {
        let _ = Command::new("awww")
            .args([
                "img",
                shown.to_str().unwrap_or(""),
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
                shown.to_str().unwrap_or(""),
                "--transition-type",
                "random",
                "--transition-duration",
                "1",
                "--transition-fps",
                "60",
            ])
            .output();
    }

    let theme_source = if filter.is_monochrome() { image_path } else { shown.as_path() };
    run_matugen(theme_source, scheme, mode);

    // Best-effort greeter wallpaper synchronization
    sync_greeter_wallpaper(&wa_jpg);

    Ok(())
}

/// Where prismd notes whether the "auto" colour mode is light or dark now.
fn auto_mode_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".cache/prism/auto-theme")
}

/// The matugen mode for a configured one: "auto" (light by day, dark by night on the Night
/// Light schedule; prismd keeps the state) resolves to "light" or "dark".
pub fn matugen_mode(mode: &str) -> String {
    if mode != "auto" {
        return mode.to_string();
    }
    match fs::read_to_string(auto_mode_path()).map(|s| s.trim().to_string()) {
        Ok(m) if m == "light" => m,
        _ => "dark".to_string(),
    }
}

/// prismd: the auto colour mode is now `mode` ("light" or "dark"). Returns true if it changed.
pub fn set_auto_mode(mode: &str) -> bool {
    if fs::read_to_string(auto_mode_path()).is_ok_and(|s| s.trim() == mode) {
        return false;
    }
    let p = auto_mode_path();
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    fs::write(&p, mode).is_ok()
}

/// Re-run matugen on the current wallpaper (~/.wa.jpg) with this scheme and mode.
pub fn regenerate_palette(scheme: &str, mode: &str) -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let out = Command::new("matugen")
        .args(["image", &format!("{home}/.wa.jpg"), "-m", &matugen_mode(mode), "-t", scheme, "--source-color-index", "0"])
        .output()
        .map_err(|e| format!("matugen not available: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("matugen failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")))
    }
}

/// Derive the colour palette from `image`, if matugen is installed.
fn run_matugen(image: &Path, scheme: Option<&str>, mode: Option<&str>) {
    let has_matugen = Command::new("matugen").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if has_matugen {
        let _ = Command::new("matugen")
            .args([
                "image",
                image.to_str().unwrap_or(""),
                "-m",
                &matugen_mode(mode.unwrap_or("smart")),
                "-t",
                scheme.unwrap_or("scheme-smart"),
                "--source-color-index",
                "0",
            ])
            .output();
    }
}

/// Name of the greeter's copy of the video wallpaper in its state directory.
pub const GREETER_VIDEO: &str = "wallpaper-video";

/// Make `video` the wallpaper: a picture from it becomes the still the lock screen, previews and
/// palette work from (no filter applies), and the greeter gets its own copy of the video.
pub fn set_video_wallpaper(video: &Path, scheme: Option<&str>, mode: Option<&str>) -> Result<(), String> {
    let home = std::env::var("HOME").map(PathBuf::from).map_err(|_| "HOME environment variable not set".to_string())?;
    let dir = home.join(".cache/prism");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let frame = dir.join("video-frame.ppm");
    crate::video::extract_still(video, &frame)?;
    let (wa_jpg, wa_thumb) = (home.join(".wa.jpg"), home.join(".wa-thumb.jpg"));
    let saved = crate::pixbuf::save_jpeg_filtered(&frame, &wa_jpg, 1920, 95, Filter::Normal)
        .and_then(|_| crate::pixbuf::save_jpeg_filtered(&frame, &wa_thumb, 960, 90, Filter::Normal));
    let _ = fs::remove_file(&frame);
    saved?;
    run_matugen(&wa_jpg, scheme, mode);
    sync_greeter_wallpaper(&wa_jpg);
    sync_greeter_video(Some(video));
    Ok(())
}

/// A picture was chosen while a video plays: go back to the daemon used before the video.
pub fn leave_video_wallpaper() -> Result<(), String> {
    let mut cfg = crate::Config::load();
    if cfg.wallpaper_daemon != "prism-video" {
        return Ok(());
    }
    cfg.wallpaper_daemon = cfg.still_daemon().to_string();
    cfg.save().map_err(|e| e.to_string())?;
    sync_greeter_video(None);
    Ok(())
}

/// Back from a video to the still wallpaper it replaced.
pub fn restore_still_wallpaper(daemon: &str, scheme: Option<&str>, mode: Option<&str>) -> Result<(), String> {
    sync_greeter_video(None);
    match displayed_wallpaper() {
        Some(path) => set_wallpaper(&path, Some(daemon), scheme, mode),
        None => Ok(()),
    }
}

/// Give the greeter (another user) its own copy of the video, or take it away.
pub fn sync_greeter_video(video: Option<&Path>) {
    for dir in ["/var/lib/greetd/prism", "/var/lib/greetd/quickshell-greeter"].iter().map(Path::new).filter(|d| d.is_dir()) {
        let target = dir.join(GREETER_VIDEO);
        let Some(video) = video else {
            let _ = fs::remove_file(&target);
            continue;
        };
        // Already copied: the same size, and not older than the original.
        let current = fs::metadata(video).ok().zip(fs::metadata(&target).ok()).is_some_and(|(from, to)| {
            from.len() == to.len() && from.modified().ok() <= to.modified().ok()
        });
        if current {
            continue;
        }
        let tmp = dir.join(format!("{GREETER_VIDEO}.{}.tmp", std::process::id()));
        if fs::copy(video, &tmp).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
            if fs::rename(&tmp, &target).is_err() {
                let _ = fs::remove_file(&tmp);
            }
        }
    }
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
