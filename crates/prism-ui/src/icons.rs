// ─────────────────────────────────────────────────────────────────────────
// icons.rs — freedesktop icon theme lookup that follows gsettings
//
// The icon theme is org.gnome.desktop.interface icon-theme (what GTK apps and
// the QML shell used). Lookup follows the Icon Theme Specification: the theme,
// its `Inherits=` chain, then hicolor, then /usr/share/pixmaps; within a theme
// the directory whose size best matches the request wins (index.theme
// Size/MinSize/MaxSize/Threshold/Type, scalable allowed).
//
// The index is built once per theme on a background thread and rebuilt when
// `gsettings monitor` reports a change; `generation()` lets callers drop
// cached lookups and redraw.
// ─────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug)]
struct DirInfo {
    size: u32,
    min: u32,
    max: u32,
    threshold: u32,
    kind: DirKind,
    scale: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DirKind {
    Fixed,
    Scalable,
    Threshold,
}

impl DirInfo {
    fn matches(&self, size: u32) -> bool {
        match self.kind {
            DirKind::Fixed => self.size == size,
            DirKind::Scalable => self.min <= size && size <= self.max,
            DirKind::Threshold => self.size.saturating_sub(self.threshold) <= size && size <= self.size + self.threshold,
        }
    }

    fn distance(&self, size: u32) -> u32 {
        match self.kind {
            DirKind::Fixed => self.size.abs_diff(size),
            DirKind::Scalable => {
                if size < self.min {
                    self.min - size
                } else { size.saturating_sub(self.max) }
            }
            DirKind::Threshold => {
                if size + self.threshold < self.size {
                    self.size - self.threshold - size
                } else if size > self.size + self.threshold {
                    size - self.size - self.threshold
                } else {
                    0
                }
            }
        }
    }
}

/// One theme: icon name -> candidate files with their directory info.
struct Theme {
    icons: HashMap<String, Vec<(usize, PathBuf)>>,
    dirs: Vec<DirInfo>,
}

struct Index {
    /// Themes in lookup order (user theme, inherited themes..., hicolor).
    themes: Vec<Theme>,
    pixmaps: HashMap<String, PathBuf>,
}

static INDEX: OnceLock<Mutex<Option<Arc<Index>>>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(1);

fn slot() -> &'static Mutex<Option<Arc<Index>>> {
    INDEX.get_or_init(|| Mutex::new(None))
}

/// Bumped whenever the icon theme changes; cached icon paths/images should be dropped.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// Build the index in the background now and keep it in sync with gsettings. `on_change`
/// runs (on the watcher thread) after a theme change has been indexed.
pub fn start(on_change: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("prism-icons".into())
        .spawn(move || {
            rebuild();
            watch(on_change);
        })
        .ok();
}

/// Look up `name` at roughly `size` px. Absolute paths are returned as-is.
pub fn lookup(name: &str, size: u32) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') {
        return Path::new(name).exists().then(|| PathBuf::from(name));
    }
    let name = name.strip_suffix(".png").or_else(|| name.strip_suffix(".svg")).unwrap_or(name);
    let index = current();
    for theme in &index.themes {
        if let Some(found) = best_in_theme(theme, name, size) {
            return Some(found);
        }
    }
    index.pixmaps.get(name).cloned()
}

fn current() -> Arc<Index> {
    if let Some(idx) = slot().lock().ok().and_then(|g| g.clone()) {
        return idx;
    }
    rebuild();
    slot().lock().ok().and_then(|g| g.clone()).expect("icon index")
}

fn best_in_theme(theme: &Theme, name: &str, size: u32) -> Option<PathBuf> {
    let candidates = theme.icons.get(name)?;
    // Exact size match first (prefer scale 1), then the closest directory.
    if let Some((_, p)) = candidates.iter().find(|(d, _)| theme.dirs[*d].scale == 1 && theme.dirs[*d].matches(size)) {
        return Some(p.clone());
    }
    candidates
        .iter()
        .min_by_key(|(d, _)| {
            let di = &theme.dirs[*d];
            // Prefer larger icons over smaller ones at equal distance (they downscale cleanly).
            (di.distance(size), if di.size >= size { 0 } else { 1 })
        })
        .map(|(_, p)| p.clone())
}

pub fn current_theme_name() -> String {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .or_else(gtk_ini_theme)
        .unwrap_or_else(|| "hicolor".to_string())
}

fn gtk_ini_theme() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    for f in [format!("{}/.config/gtk-4.0/settings.ini", home), format!("{}/.config/gtk-3.0/settings.ini", home)] {
        if let Ok(s) = std::fs::read_to_string(&f) {
            for line in s.lines() {
                if let Some(v) = line.trim().strip_prefix("gtk-icon-theme-name") {
                    let v = v.trim_start_matches([' ', '=']).trim().trim_matches('"');
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

fn base_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{}/.local/share", home));
    let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    let mut v = vec![PathBuf::from(&data_home).join("icons"), PathBuf::from(&home).join(".icons")];
    for d in data_dirs.split(':').filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(d).join("icons"));
    }
    v
}

/// Index the icon files again (apps were installed or removed).
pub fn refresh() {
    rebuild();
}

fn rebuild() {
    let start = current_theme_name();
    let bases = base_dirs();
    // Resolve the inheritance chain breadth-first, hicolor last.
    let mut order: Vec<String> = Vec::new();
    let mut queue = vec![start];
    while let Some(t) = queue.first().cloned() {
        queue.remove(0);
        if order.contains(&t) || t == "hicolor" {
            continue;
        }
        let inherits = bases
            .iter()
            .map(|b| b.join(&t).join("index.theme"))
            .find(|p| p.exists())
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("Inherits=").map(|v| v.to_string())))
            .unwrap_or_default();
        order.push(t);
        for i in inherits.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            queue.push(i.to_string());
        }
    }
    order.push("hicolor".to_string());

    let themes = order.iter().map(|t| index_theme(t, &bases)).collect();
    let mut pixmaps = HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/usr/share/pixmaps") {
        for e in rd.flatten() {
            let p = e.path();
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                pixmaps.entry(stem.to_string()).or_insert(p.clone());
            }
        }
    }
    if let Ok(mut g) = slot().lock() {
        *g = Some(Arc::new(Index { themes, pixmaps }));
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn index_theme(name: &str, bases: &[PathBuf]) -> Theme {
    let mut dirs = Vec::new();
    let mut icons: HashMap<String, Vec<(usize, PathBuf)>> = HashMap::new();
    // A theme has one index.theme, but its icons can be in every base directory: apps installed
    // for one user put theirs in ~/.local/share/icons/hicolor, which has no index of its own.
    let Some(text) = bases.iter().find_map(|b| std::fs::read_to_string(b.join(name).join("index.theme")).ok()) else {
        return Theme { icons, dirs };
    };
    for (subdir, info) in parse_index(&text) {
        let di = dirs.len();
        dirs.push(info);
        for base in bases {
            let Ok(rd) = std::fs::read_dir(base.join(name).join(&subdir)) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                if !matches!(ext, "png" | "svg" | "xpm") {
                    continue;
                }
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    icons.entry(stem.to_string()).or_default().push((di, p.clone()));
                }
            }
        }
    }
    Theme { icons, dirs }
}

/// Directories listed in index.theme with their size information.
fn parse_index(text: &str) -> Vec<(String, DirInfo)> {
    let mut listed: Vec<String> = Vec::new();
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current = line[1..line.len() - 1].to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            if current == "Icon Theme" && (k == "Directories" || k == "ScaledDirectories") {
                listed.extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
            }
            sections.entry(current.clone()).or_default().insert(k, v);
        }
    }
    listed
        .into_iter()
        .filter_map(|d| {
            let sec = sections.get(&d)?;
            let num = |k: &str| sec.get(k).and_then(|v| v.parse::<u32>().ok());
            let size = num("Size")?;
            let kind = match sec.get("Type").map(|s| s.as_str()) {
                Some("Fixed") => DirKind::Fixed,
                Some("Scalable") => DirKind::Scalable,
                _ => DirKind::Threshold,
            };
            Some((
                d,
                DirInfo {
                    size,
                    min: num("MinSize").unwrap_or(size),
                    max: num("MaxSize").unwrap_or(size),
                    threshold: num("Threshold").unwrap_or(2),
                    kind,
                    scale: num("Scale").unwrap_or(1),
                },
            ))
        })
        .collect()
}

/// Follow gsettings; rebuild and notify on change. Restarts the monitor if it dies.
fn watch(on_change: impl Fn()) {
    use std::io::{BufRead, BufReader};
    loop {
        let child = std::process::Command::new("gsettings")
            .args(["monitor", "org.gnome.desktop.interface", "icon-theme"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return };
        if let Some(out) = child.stdout.take() {
            for _line in BufReader::new(out).lines().map_while(Result::ok) {
                rebuild();
                on_change();
            }
        }
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_found_in_a_base_directory_without_an_index() {
        let root = std::env::temp_dir().join(format!("prism-icons-test-{}", std::process::id()));
        let (system, user) = (root.join("system"), root.join("user"));
        std::fs::create_dir_all(system.join("hicolor/48x48/apps")).unwrap();
        std::fs::create_dir_all(user.join("hicolor/48x48/apps")).unwrap();
        std::fs::write(system.join("hicolor/index.theme"), "[Icon Theme]\nDirectories=48x48/apps\n[48x48/apps]\nSize=48\nType=Fixed\n").unwrap();
        std::fs::write(user.join("hicolor/48x48/apps/local-app.png"), b"").unwrap();
        let theme = index_theme("hicolor", &[user, system]);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(theme.dirs.len(), 1);
        assert!(theme.icons.contains_key("local-app"));
    }

    #[test]
    fn parses_index_dirs() {
        let text = "[Icon Theme]\nDirectories=48x48/apps,scalable/apps\n[48x48/apps]\nSize=48\nType=Fixed\n[scalable/apps]\nSize=64\nMinSize=8\nMaxSize=512\nType=Scalable\n";
        let dirs = parse_index(text);
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].1.matches(48));
        assert!(dirs[1].1.matches(300));
        assert_eq!(dirs[0].1.distance(32), 16);
    }
}
