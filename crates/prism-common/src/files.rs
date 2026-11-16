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
