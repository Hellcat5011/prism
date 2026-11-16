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

/// Executes `plocate` to search for files and folders matching the given query pattern.
pub fn search_files(query: &str, limit: usize) -> Vec<FileResult> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }

    let pattern = format!("*{}*", q);

    let output = match Command::new("plocate")
        .arg("-i")
        .arg("-l")
        .arg("300")
        .arg("--null")
        .arg(&pattern)
        .output()
    {
        Ok(out) => out,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let mut results: Vec<FileResult> = output
        .stdout
        .split(|&b| b == 0)
        .filter_map(|slice| {
            if slice.is_empty() {
                return None;
            }
            let path_str = std::str::from_utf8(slice).ok()?;
            let path = Path::new(path_str);

            let is_dir = fs::symlink_metadata(path)
                .map(|m| m.is_dir())
                .unwrap_or(false);

            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path_str)
                .to_string();

            let score = FileResult::compute_score(path_str, q, is_dir);
            if score > 0 {
                Some(FileResult {
                    path: path_str.to_string(),
                    name,
                    is_dir,
                    score,
                })
            } else {
                None
            }
        })
        .collect();

    results.sort_by(|a, b| b.score.cmp(&a.score));
    results.truncate(limit);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_score() {
        let exact_score = FileResult::compute_score("/home/user/notes.txt", "notes.txt", false);
        let prefix_score = FileResult::compute_score("/home/user/notes-2026.txt", "notes", false);
        let substring_score = FileResult::compute_score("/home/user/my-notes-old.txt", "notes", false);

        assert!(exact_score > prefix_score);
        assert!(prefix_score > substring_score);
    }
}
