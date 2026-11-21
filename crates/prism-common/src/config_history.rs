//! Settings history and backups.
//!
//! * Snapshots: Settings copies config.json to `~/.local/state/prism/config-history/<unix>.json`
//!   before the first change of each session (only if it differs from the newest snapshot);
//!   the newest `KEEP` are kept, and Settings can go back to any of them.
//! * Backups: the whole `~/.config/prism` folder (config, dock pins, notes, palette) as one
//!   .tar.gz, and back. An import is snapshotted first, so it can be undone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Config;

/// Snapshots kept.
const KEEP: usize = 20;

fn history_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"));
    base.join("prism/config-history")
}

/// Snapshots, newest first: (unix seconds, file).
pub fn list() -> Vec<(u64, PathBuf)> {
    let Ok(dir) = fs::read_dir(history_dir()) else { return Vec::new() };
    let mut out: Vec<(u64, PathBuf)> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let ts = name.strip_suffix(".json")?.parse::<u64>().ok()?;
            Some((ts, e.path()))
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.0));
    out
}

/// Copy the config on disk into the history (unless the newest snapshot is the same).
pub fn snapshot() -> Result<(), String> {
    let current = fs::read(Config::config_path()).map_err(|e| e.to_string())?;
    let all = list();
    if all.first().and_then(|(_, p)| fs::read(p).ok()).is_some_and(|newest| newest == current) {
        return Ok(());
    }
    let dir = history_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Two snapshots within a second: keep both.
    while dir.join(format!("{ts}.json")).exists() {
        ts += 1;
    }
    fs::write(dir.join(format!("{ts}.json")), current).map_err(|e| e.to_string())?;
    for (_, old) in list().into_iter().skip(KEEP) {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

/// The config saved in a snapshot.
pub fn load(path: &Path) -> Option<Config> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn config_dir() -> PathBuf {
    Config::config_path().parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Write `~/.config/prism` as a .tar.gz to `dest`.
pub fn export(dest: &Path) -> Result<(), String> {
    let dir = config_dir();
    let parent = dir.parent().ok_or("no config folder")?;
    let name = dir.file_name().ok_or("no config folder")?;
    let out = Command::new("tar")
        .arg("-czf")
        .arg(dest)
        .arg("-C")
        .arg(parent)
        .arg("--exclude=*.tmp")
        .arg(name)
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("Export failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Whether every entry of a backup's listing stays inside the `prism/` folder.
fn safe_listing(listing: &str, folder: &str) -> bool {
    let mut any = false;
    for line in listing.lines().map(str::trim).filter(|l| !l.is_empty()) {
        any = true;
        let p = line.strip_prefix("./").unwrap_or(line);
        let inside = p == folder || p == format!("{folder}/") || p.starts_with(&format!("{folder}/"));
        if !inside || p.starts_with('/') || p.split('/').any(|c| c == "..") {
            return false;
        }
    }
    any
}

/// Restore a backup made by `export`: snapshots the current config first, then unpacks the
/// archive over `~/.config/prism` (files the backup lacks are kept).
pub fn import(src: &Path) -> Result<(), String> {
    let dir = config_dir();
    let parent = dir.parent().ok_or("no config folder")?;
    let folder = dir.file_name().ok_or("no config folder")?.to_string_lossy().to_string();
    let listing = Command::new("tar").arg("-tzf").arg(src).output().map_err(|e| format!("tar: {e}"))?;
    if !listing.status.success() {
        return Err("Not a Prism backup (cannot read the archive)".into());
    }
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    if !safe_listing(&listing, &folder) || !listing.lines().any(|l| l.trim().ends_with("/config.json")) {
        return Err("Not a Prism backup".into());
    }
    // Only plain files and folders: a link could point outside the folder.
    let verbose = Command::new("tar").arg("-tvzf").arg(src).output().map_err(|e| format!("tar: {e}"))?;
    if String::from_utf8_lossy(&verbose.stdout).lines().any(|l| !(l.starts_with('-') || l.starts_with('d'))) {
        return Err("Not a Prism backup (it contains links)".into());
    }
    let _ = snapshot();
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(src)
        .arg("-C")
        .arg(parent)
        .arg("--no-same-owner")
        .arg("--no-same-permissions")
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("Import failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_listing_must_stay_in_the_folder() {
        assert!(safe_listing("prism/\nprism/config.json\nprism/dock.json\n", "prism"));
        assert!(!safe_listing("prism/config.json\nprism/../.bashrc\n", "prism"));
        assert!(!safe_listing("prism/config.json\n.bashrc\n", "prism"));
        assert!(!safe_listing("/etc/passwd\n", "prism"));
        assert!(!safe_listing("", "prism"));
    }
}
