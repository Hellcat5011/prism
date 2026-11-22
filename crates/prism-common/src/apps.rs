use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// A command for launching a user application in its own systemd scope (app.slice), so it is
/// not part of Prism's service: restarting or stopping prismd must never kill the user's apps.
/// Falls back to running the program directly without systemd.
pub fn app_command(program: &str, args: &[String]) -> Command {
    if Path::new("/usr/bin/systemd-run").exists() || Path::new("/bin/systemd-run").exists() {
        let mut cmd = Command::new("systemd-run");
        let name: String = Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("app")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros()).unwrap_or(0);
        cmd.args(["--user", "--scope", "--quiet", "--collect", "--slice=app.slice"]);
        cmd.arg(format!("--unit=app-prism-{name}-{stamp}"));
        cmd.arg("--").arg(program).args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}

/// The user's terminal: `$TERMINAL`, else the first installed of the common ones.
pub fn terminal() -> Option<String> {
    let found = |t: &str| t.contains('/') && Path::new(t).exists() || std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(t).is_file()));
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.is_empty() && found(&t) {
            return Some(t);
        }
    }
    ["xdg-terminal-exec", "ghostty", "kitty", "foot", "alacritty", "wezterm", "konsole", "gnome-terminal", "xfce4-terminal", "xterm"].into_iter().find(|t| found(t)).map(String::from)
}

/// A terminal running `args` (in an app scope, like `app_command`).
pub fn terminal_command(args: &[String]) -> Option<Command> {
    let term = terminal()?;
    let name = Path::new(&term).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let mut full: Vec<String> = match name.as_str() {
        // These take the command directly (or after --).
        "xdg-terminal-exec" | "kitty" => Vec::new(),
        "gnome-terminal" => vec!["--".into()],
        "wezterm" => vec!["start".into(), "--".into()],
        _ => vec!["-e".into()],
    };
    full.extend(args.iter().cloned());
    Some(app_command(&term, &full))
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub exec: String,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub terminal: bool,
    pub no_display: bool,
    /// StartupWMClass: the app id its windows report, when that differs from the file name.
    #[serde(default)]
    pub wm_class: Option<String>,
    /// The file's `[Desktop Action ...]` entries (New Window, New Private Window, ...), in the
    /// order its `Actions=` key lists them.
    #[serde(default)]
    pub actions: Vec<DesktopAction>,
}

/// An action section as read: id, Name, Exec, Icon.
type RawAction = (String, Option<String>, Option<String>, Option<String>);

/// One `[Desktop Action <id>]` of a desktop file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DesktopAction {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
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
        let mut keywords: Vec<String> = Vec::new();
        let mut terminal = false;
        let mut wm_class: Option<String> = None;
        let mut no_display = false;
        let mut is_application = false;
        let mut action_ids: Vec<String> = Vec::new();
        // Sections read after [Desktop Entry]: (id, name, exec, icon) of each action.
        let mut actions: Vec<RawAction> = Vec::new();
        let mut in_action = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                in_action = false;
                if trimmed == "[Desktop Entry]" {
                    in_desktop_entry = true;
                } else if let Some(id) = trimmed.strip_prefix("[Desktop Action ").and_then(|t| t.strip_suffix(']')) {
                    in_desktop_entry = false;
                    in_action = true;
                    actions.push((id.trim().to_string(), None, None, None));
                } else {
                    in_desktop_entry = false;
                }
                continue;
            }

            if in_action {
                if let (Some((key, val)), Some(a)) = (trimmed.split_once('='), actions.last_mut()) {
                    match key.trim() {
                        "Name" if a.1.is_none() => a.1 = Some(val.trim().to_string()),
                        "Exec" => a.2 = Some(val.trim().to_string()),
                        "Icon" => a.3 = Some(val.trim().to_string()),
                        _ => {}
                    }
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
                    "StartupWMClass" => wm_class = Some(val.to_string()),
                    "NoDisplay" => no_display = val.eq_ignore_ascii_case("true"),
                    "Type" => is_application = val.eq_ignore_ascii_case("Application"),
                    "Actions" => {
                        action_ids = val.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                    }
                    "Keywords" => {
                        keywords = val.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                    }
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
            keywords,
            terminal,
            wm_class,
            no_display,
            actions: action_ids
                .iter()
                .filter_map(|id| {
                    let (_, name, exec, icon) = actions.iter().find(|a| &a.0 == id)?;
                    Some(DesktopAction { id: id.clone(), name: name.clone()?, exec: exec.clone()?, icon: icon.clone() })
                })
                .collect(),
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
            let mut cmd = app_command("/usr/bin/gtk-launch", std::slice::from_ref(&self.id));
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
            let mut term_args = vec!["-e".to_string()];
            term_args.extend(args.iter().cloned());
            let mut cmd = app_command(&term, &term_args);
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
            let mut cmd = app_command(&args[0], &args[1..]);
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

    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// QML Fuzzy.scoreLegacy: subsequence match with bonuses for contiguous runs, word starts and a
/// prefix match; longer texts score slightly lower. None when `query` is not a subsequence.
pub fn fuzzy_score(query: &str, text: &str) -> Option<f64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(0.0);
    }
    let (mut qi, mut score, mut last) = (0usize, 0.0f64, -2isize);
    for (ti, &c) in t.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if c == q[qi] {
            score += if last == ti as isize - 1 { 5.0 } else { 1.0 };
            if ti == 0 || t[ti - 1] == ' ' || t[ti - 1] == '-' {
                score += 10.0;
            }
            last = ti as isize;
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    score -= t.len() as f64 * 0.1;
    if t.starts_with(&q[..]) {
        score += 20.0;
    }
    Some(score)
}

/// Applications matching `query`, best first (QML SearchProviderApps: name, generic name x0.8,
/// keywords x0.5). An empty query returns every app in name order.
pub fn search_apps(apps: &[DesktopApp], query: &str) -> Vec<DesktopApp> {
    let q = query.trim();
    if q.is_empty() {
        return apps.to_vec();
    }
    let mut scored: Vec<(f64, &DesktopApp)> = apps
        .iter()
        .filter_map(|app| {
            let mut best = fuzzy_score(q, &app.name);
            let mut consider = |s: Option<f64>, w: f64| {
                if let Some(s) = s {
                    let s = s * w;
                    if best.map(|b| s > b).unwrap_or(true) {
                        best = Some(s);
                    }
                }
            };
            if let Some(g) = &app.generic_name {
                consider(fuzzy_score(q, g), 0.8);
            }
            for k in &app.keywords {
                consider(fuzzy_score(q, k), 0.5);
            }
            best.map(|s| (s, app))
        })
        .collect();
    // Stable sort keeps name order for equal scores.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
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

    #[test]
    fn desktop_actions() {
        let sample = "[Desktop Entry]\nName=Firefox\nExec=firefox %u\nType=Application\nActions=new-window;new-private-window;missing;\n\n[Desktop Action new-window]\nName=New Window\nExec=firefox --new-window %u\n\n[Desktop Action new-private-window]\nName=New Private Window\nName[de]=Neues privates Fenster\nExec=firefox --private-window %u\nIcon=firefox-private\n\n[Desktop Action unlisted]\nName=Unlisted\nExec=firefox --x\n";
        let app = DesktopApp::parse_str(sample, Path::new("firefox.desktop")).unwrap();
        assert_eq!(app.exec, "firefox %u");
        let names: Vec<_> = app.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["New Window", "New Private Window"]);
        assert_eq!(app.actions[1].exec, "firefox --private-window %u");
        assert_eq!(app.actions[1].icon.as_deref(), Some("firefox-private"));
    }
}
