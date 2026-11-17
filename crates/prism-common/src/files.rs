use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileResult {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub score: i32,
}

impl FileResult {
    pub fn compute_score(path_str: &str, query: &str, is_dir: bool) -> i32 {
        let path = Path::new(path_str);
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path_str)
            .to_lowercase();
        let q = query.to_lowercase();

        let mut score = 0;

        if name == q {
            score += 1000;
        } else if name.starts_with(&q) {
            score += 500 + (50 - name.len().min(50) as i32);
        } else if name.contains(&q) {
            score += 250;
        } else if path_str.to_lowercase().contains(&q) {
            score += 100;
        } else {
            return 0;
        }

        // Prefer files within $HOME
        if let Ok(home) = std::env::var("HOME") {
            if path_str.starts_with(&home) {
                score += 150;
            }
        }

        // Prefer folders slightly for navigation
        if is_dir {
            score += 20;
        }

        // Shorter paths are generally cleaner results
        score += 100 - (path_str.len() / 2).min(100) as i32;

        score
    }
}

/// plocate candidates per query and how many of them get classified (QML SearchProviderFiles).
const PLOCATE_LIMIT: &str = "2000";
const STAT_BATCH: usize = 300;
const HOME_DIRS: [&str; 8] = ["Desktop", "Documents", "Downloads", "Music", "Pictures", "Videos", "Templates", "Public"];

fn has_glob(q: &str) -> bool {
    q.chars().any(|c| matches!(c, '*' | '?' | '[' | ']' | '\\'))
}

/// Rank candidate paths for `q` like QML: exact basename, then basename prefix, then inside
/// $HOME, then shorter, then alphabetical. Hidden paths are dropped unless `include_hidden`.
pub fn rank_paths(mut paths: Vec<String>, q: &str, home: &str, include_hidden: bool) -> Vec<String> {
    let ql = q.to_lowercase();
    let glob = has_glob(q);
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
    paths.retain(|p| (include_hidden || !p.contains("/.")) && (!glob || base(p).contains(&ql)));
    paths.sort_by(|a, b| {
        let (ba, bb) = (base(a), base(b));
        let key = |p: &str, bn: &str| {
            (bn != ql, !bn.starts_with(&ql), !(!home.is_empty() && p.starts_with(home)), p.len())
        };
        key(a, &ba).cmp(&key(b, &bb)).then_with(|| a.cmp(b))
    });
    paths.dedup();
    paths
}

/// Folders and files whose name contains `query` (plocate basename search), best first.
/// Needs at least 2 characters.
pub fn search_paths(query: &str, include_hidden: bool) -> (Vec<FileResult>, Vec<FileResult>) {
    let q = query.trim();
    if q.chars().count() < 2 {
        return (Vec::new(), Vec::new());
    }
    let arg = if has_glob(q) {
        let mut e = String::from("*");
        for c in q.chars() {
            if matches!(c, '*' | '?' | '[' | ']' | '\\') {
                e.push('\\');
            }
            e.push(c);
        }
        e.push('*');
        e
    } else {
        q.to_string()
    };
    let mut paths: Vec<String> = Command::new("plocate")
        .args(["-i", "-l", PLOCATE_LIMIT, "-b", "--null", &arg])
        .output()
        .ok()
        .map(|o| o.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()).filter_map(|s| std::str::from_utf8(s).ok().map(String::from)).collect())
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let ql = q.to_lowercase();
    let mut dirs: Vec<&str> = HOME_DIRS.to_vec();
    if include_hidden {
        dirs.push(".config");
    }
    for d in dirs {
        if d.to_lowercase().contains(&ql) {
            let full = format!("{home}/{d}");
            if !paths.contains(&full) {
                paths.push(full);
            }
        }
    }
    let ranked = rank_paths(paths, q, &home, include_hidden);
    let (mut folders, mut files) = (Vec::new(), Vec::new());
    for p in ranked.into_iter().take(STAT_BATCH) {
        let Ok(meta) = fs::metadata(&p) else { continue };
        let name = p.rsplit('/').next().unwrap_or(&p).to_string();
        let r = FileResult { name, is_dir: meta.is_dir(), score: 0, path: p };
        if r.is_dir {
            folders.push(r);
        } else {
            files.push(r);
        }
    }
    (folders, files)
}

/// Themed icon name for a file (QML SearchProviderFiles._iconForPath).
pub fn icon_for_path(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_lowercase(),
        _ => return "text-x-generic",
    };
    let is = |list: &[&str]| list.contains(&ext.as_str());
    if is(&["png", "jpg", "jpeg", "svg", "webp", "gif", "bmp", "ico", "tiff", "avif"]) {
        "image-x-generic"
    } else if is(&["mp3", "flac", "wav", "ogg", "m4a", "opus", "aac", "wma"]) {
        "audio-x-generic"
    } else if is(&["mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v"]) {
        "video-x-generic"
    } else if ext == "pdf" {
        "application-pdf"
    } else if is(&["doc", "docx", "odt", "rtf"]) {
        "x-office-document"
    } else if is(&["xls", "xlsx", "ods", "csv"]) {
        "x-office-spreadsheet"
    } else if is(&["ppt", "pptx", "odp"]) {
        "x-office-presentation"
    } else if is(&["zip", "tar", "gz", "bz2", "xz", "7z", "rar", "zst", "tgz"]) {
        "package-x-generic"
    } else if is(&["bin", "so", "run", "desktop", "exe"]) {
        "application-x-executable"
    } else {
        "text-x-generic"
    }
}

/// Folders then files matching `query`, at most `limit` (used by `prism-ctl search`).
pub fn search_files(query: &str, limit: usize) -> Vec<FileResult> {
    let (mut folders, files) = search_paths(query, false);
    folders.extend(files);
    folders.truncate(limit);
    folders
}

// ── plocate index (QML IndexService) ──────────────────────────────────────

/// The database the file search reads.
pub const PLOCATE_DB: &str = "/var/lib/plocate/plocate.db";

/// When the plocate database was last written, as local "DD/MM/YYYY HH:MM".
pub fn last_indexed() -> Option<String> {
    let mtime = std::fs::metadata(PLOCATE_DB).ok()?.modified().ok()?;
    let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    Some(format!("{:02}/{:02}/{} {:02}:{:02}", tm.tm_mday, tm.tm_mon + 1, tm.tm_year + 1900, tm.tm_hour, tm.tm_min))
}

/// Rebuild the plocate index through polkit (`pkexec updatedb --prune-bind-mounts no`, as QML).
/// Blocks for the whole run; Err carries the message to show. `on_dialog_closed` runs once the
/// polkit dialog is gone: when pkexec has authenticated (it execs updatedb in the same process)
/// or exited (cancelled or failed), so callers can bring their UI back during the long rebuild.
pub fn rebuild_index(on_dialog_closed: impl FnOnce()) -> Result<(), String> {
    let mut child = Command::new("pkexec")
        .args(["updatedb", "--prune-bind-mounts", "no"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("pkexec unavailable: {e}"))?;
    let comm = format!("/proc/{}/comm", child.id());
    let mut on_closed = Some(on_dialog_closed);
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if on_closed.is_some() && std::fs::read_to_string(&comm).is_ok_and(|c| c.trim() == "updatedb") {
            (on_closed.take().unwrap())();
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    };
    if let Some(f) = on_closed.take() {
        f();
    }
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }
    if status.success() {
        return Ok(());
    }
    let err = stderr.to_lowercase();
    let code = status.code().unwrap_or(-1);
    if code == 126 || err.contains("dismissed") || err.contains("cancel") {
        Err("Cancelled".into())
    } else if code == 127 && (err.contains("agent") || err.contains("authentication")) {
        Err("No polkit authentication agent running".into())
    } else {
        let last = stderr.lines().last().unwrap_or("").trim().to_string();
        Err(if last.is_empty() { "Failed".into() } else { format!("Failed: {last}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_follows_qml() {
        let paths = vec![
            "/usr/share/doc/notes".to_string(),
            "/home/u/old-notes.txt".to_string(),
            "/home/u/notes.md".to_string(),
            "/home/u/notes".to_string(),
            "/home/u/.cache/notes".to_string(),
        ];
        let r = rank_paths(paths, "notes", "/home/u", false);
        assert_eq!(r, ["/home/u/notes", "/usr/share/doc/notes", "/home/u/notes.md", "/home/u/old-notes.txt"]);
        assert_eq!(icon_for_path("/a/b.PDF"), "application-pdf");
        assert_eq!(icon_for_path("/a/.bashrc"), "text-x-generic");
    }

    #[test]
    fn test_compute_score() {
        let exact_score = FileResult::compute_score("/home/user/notes.txt", "notes.txt", false);
        let prefix_score = FileResult::compute_score("/home/user/notes-2026.txt", "notes", false);
        let substring_score = FileResult::compute_score("/home/user/my-notes-old.txt", "notes", false);

        assert!(exact_score > prefix_score);
        assert!(prefix_score > substring_score);
    }
}
