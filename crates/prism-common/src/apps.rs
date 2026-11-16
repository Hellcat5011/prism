use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub exec: String,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    pub terminal: bool,
    pub no_display: bool,
}

impl DesktopApp {
    pub fn parse_file(path: &Path) -> Option<Self> {
        let content = fs::read_to_string(path).ok()?;
        Self::parse_str(&content, path)
    }

    pub fn parse_str(content: &str, path: &Path) -> Option<Self> {
        let file_stem = path.file_stem()?.to_str()?.to_string();

        let mut in_desktop_entry = false;
        let mut name: Option<String> = None;
        let mut generic_name: Option<String> = None;
        let mut comment: Option<String> = None;
        let mut exec: Option<String> = None;
        let mut icon: Option<String> = None;
        let mut categories: Vec<String> = Vec::new();
        let mut terminal = false;
        let mut no_display = false;
        let mut is_application = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                if trimmed == "[Desktop Entry]" {
                    in_desktop_entry = true;
                } else if in_desktop_entry {
                    // Left [Desktop Entry] section
                    break;
                }
                continue;
            }

            if !in_desktop_entry {
                continue;
            }

            if let Some((key, val)) = trimmed.split_once('=') {
                let key = key.trim();
                let val = val.trim();

                match key {
                    "Name" if name.is_none() => name = Some(val.to_string()),
                    "GenericName" if generic_name.is_none() => generic_name = Some(val.to_string()),
                    "Comment" if comment.is_none() => comment = Some(val.to_string()),
                    "Exec" => exec = Some(val.to_string()),
                    "Icon" => icon = Some(val.to_string()),
                    "Terminal" => terminal = val.eq_ignore_ascii_case("true"),
                    "NoDisplay" => no_display = val.eq_ignore_ascii_case("true"),
                    "Type" => is_application = val.eq_ignore_ascii_case("Application"),
                    "Categories" => {
                        categories = val
                            .split(';')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                    _ => {}
                }
            }
        }

        if !is_application || no_display || name.is_none() || exec.is_none() {
            return None;
        }

        Some(Self {
            id: file_stem,
            name: name.unwrap(),
            generic_name,
            comment,
            exec: exec.unwrap(),
            icon,
            categories,
            terminal,
            no_display,
        })
    }

    /// Cleans the Exec string by stripping Freedesktop field codes (%f, %u, etc.)
    pub fn clean_exec_args(&self) -> Vec<String> {
        let parts: Vec<&str> = self.exec.split_whitespace().collect();
        let mut args = Vec::new();

        for part in parts {
            if part.starts_with('%') {
                continue; // Skip field codes like %u, %F, %k
            }
            // Strip enclosing quotes if present
            let clean = part.trim_matches('"').trim_matches('\'');
            if !clean.is_empty() {
                args.push(clean.to_string());
            }
        }
        args
    }

    /// Spawns the application process
    pub fn launch(&self) -> std::io::Result<Child> {
        // Try gtk-launch first for desktop applications
        if !self.terminal && Path::new("/usr/bin/gtk-launch").exists() && !self.id.is_empty() {
            let mut cmd = Command::new("/usr/bin/gtk-launch");
            cmd.arg(&self.id);
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    cmd.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
            }
            if let Ok(child) = cmd.spawn() {
                return Ok(child);
            }
        }

        let args = self.clean_exec_args();
        if args.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Empty Exec command",
            ));
        }

        if self.terminal {
            // Try common terminals
            let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".to_string());
            let mut cmd = Command::new(&term);
            cmd.arg("-e").args(&args);
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    cmd.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
            }
            cmd.spawn()
        } else {
            let mut cmd = Command::new(&args[0]);
            if args.len() > 1 {
                cmd.args(&args[1..]);
            }
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    cmd.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
            }
            cmd.spawn()
        }
    }

    /// Computes match score against query (higher is better, 0 = no match)
    pub fn match_score(&self, query: &str) -> i32 {
        let q = query.to_lowercase();
        let name_lower = self.name.to_lowercase();

        if name_lower == q {
            return 1000;
        }
        if name_lower.starts_with(&q) {
            return 500 + (100 - name_lower.len() as i32);
        }
        if name_lower.contains(&q) {
            return 250;
        }

        if let Some(ref gen) = self.generic_name {
            let gen_lower = gen.to_lowercase();
            if gen_lower.starts_with(&q) {
                return 150;
            }
            if gen_lower.contains(&q) {
                return 100;
            }
        }

        if let Some(ref comment) = self.comment {
            if comment.to_lowercase().contains(&q) {
                return 50;
            }
        }

        if self.id.to_lowercase().contains(&q) {
            return 40;
        }

        0
    }
}

/// Discovers standard application search paths
pub fn application_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".local/share/applications"));
    }

    if let Ok(data_dirs) = std::env::var("XDG_DATA_DIRS") {
        for dir in data_dirs.split(':') {
            paths.push(PathBuf::from(dir).join("applications"));
        }
    } else {
        paths.push(PathBuf::from("/usr/local/share/applications"));
        paths.push(PathBuf::from("/usr/share/applications"));
    }

    paths
}

/// Scans search paths and loads all valid DesktopApp entries
pub fn load_all_desktop_apps() -> Vec<DesktopApp> {
    let mut apps = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for dir in application_search_paths() {
        if !dir.exists() || !dir.is_dir() {
            continue;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("desktop") {
                    if let Some(app) = DesktopApp::parse_file(&path) {
                        if !seen_ids.contains(&app.id) {
                            seen_ids.insert(app.id.clone());
                            apps.push(app);
                        }
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// Searches applications matching query, sorted by relevance score
pub fn search_apps(apps: &[DesktopApp], query: &str) -> Vec<DesktopApp> {
    let q = query.trim();
    if q.is_empty() {
        return apps.to_vec();
    }

    let mut scored: Vec<(i32, &DesktopApp)> = apps
        .iter()
        .filter_map(|app| {
            let score = app.match_score(q);
            if score > 0 {
                Some((score, app))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, app)| app.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_desktop_entry() {
        let sample = r#"
[Desktop Entry]
Name=Firefox Web Browser
GenericName=Web Browser
Comment=Browse the Web
Exec=firefox -P default %u
Icon=firefox
Terminal=false
Type=Application
Categories=Network;WebBrowser;
NoDisplay=false
"#;
        let app = DesktopApp::parse_str(sample, Path::new("firefox.desktop")).unwrap();
        assert_eq!(app.id, "firefox");
        assert_eq!(app.name, "Firefox Web Browser");
        assert_eq!(app.generic_name.as_deref(), Some("Web Browser"));
        assert_eq!(app.clean_exec_args(), vec!["firefox", "-P", "default"]);
    }

    #[test]
    fn test_search_scoring() {
        let sample1 = r#"
[Desktop Entry]
Name=Alacritty
Exec=alacritty
Type=Application
"#;
        let sample2 = r#"
[Desktop Entry]
Name=Foot Terminal
GenericName=Terminal Emulator
Exec=foot
Type=Application
"#;
        let app1 = DesktopApp::parse_str(sample1, Path::new("alacritty.desktop")).unwrap();
        let app2 = DesktopApp::parse_str(sample2, Path::new("foot.desktop")).unwrap();

        let apps = vec![app1, app2];
        let results = search_apps(&apps, "terminal");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Foot Terminal");

        let exact = search_apps(&apps, "alacritty");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].name, "Alacritty");
    }
}
