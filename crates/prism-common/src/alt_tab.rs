//! Does the compositor already use Alt+Tab? Before Prism's window switcher takes the key, the
//! compositor's own config (MangoWM, Hyprland or Niri, following its `source` / `include` lines)
//! is searched for Alt+Tab bindings, and the user is told which lines to change and what to put
//! there instead.

use std::path::{Path, PathBuf};

use crate::config::expand_home;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Compositor {
    Mango,
    Hyprland,
    Niri,
}

impl Compositor {
    pub fn name(self) -> &'static str {
        match self {
            Compositor::Mango => "MangoWM",
            Compositor::Hyprland => "Hyprland",
            Compositor::Niri => "Niri",
        }
    }

    /// The running compositor, from the session environment.
    pub fn current() -> Option<Compositor> {
        let desk = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
        if desk.contains("mango") {
            Some(Compositor::Mango)
        } else if desk.contains("hyprland") || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
            Some(Compositor::Hyprland)
        } else if desk.contains("niri") || std::env::var_os("NIRI_SOCKET").is_some() {
            Some(Compositor::Niri)
        } else {
            None
        }
    }

    fn main_config(self) -> PathBuf {
        match self {
            Compositor::Mango => expand_home("~/.config/mango/config.conf"),
            Compositor::Hyprland => expand_home("~/.config/hypr/hyprland.conf"),
            Compositor::Niri => expand_home("~/.config/niri/config.kdl"),
        }
    }

    /// The lines that make Alt+Tab (and Alt+Shift+Tab) open Prism's switcher.
    pub fn prism_binds(self) -> &'static str {
        match self {
            Compositor::Mango => "bind=ALT,Tab,spawn,prism-ctl switcher next\nbind=ALT+SHIFT,Tab,spawn,prism-ctl switcher prev",
            Compositor::Hyprland => "bind = ALT, Tab, exec, prism-ctl switcher next\nbind = ALT SHIFT, Tab, exec, prism-ctl switcher prev",
            Compositor::Niri => "Alt+Tab { spawn \"prism-ctl\" \"switcher\" \"next\"; }\nAlt+Shift+Tab { spawn \"prism-ctl\" \"switcher\" \"prev\"; }",
        }
    }
}

/// An Alt+Tab binding found in the compositor's config.
#[derive(Clone, Debug, PartialEq)]
pub struct Bind {
    pub file: PathBuf,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

/// The config files: the main one and everything it sources, in order, each once.
fn files(c: Compositor) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut todo = vec![c.main_config()];
    while let Some(f) = todo.pop() {
        if out.contains(&f) || out.len() > 64 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
        for line in text.lines() {
            if let Some(p) = include_of(c, line) {
                let p = if p.starts_with('~') { expand_home(&p) } else { dir.join(p.trim_start_matches("./")) };
                todo.push(p);
            }
        }
        out.push(f);
    }
    out
}

/// The file a `source` / `include` line pulls in.
fn include_of(c: Compositor, line: &str) -> Option<String> {
    let l = line.trim();
    match c {
        Compositor::Mango | Compositor::Hyprland => {
            let (k, v) = l.split_once('=')?;
            matches!(k.trim(), "source" | "source_optional").then(|| v.trim().trim_matches('"').to_string())
        }
        Compositor::Niri => l.strip_prefix("include").map(|v| v.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty()),
    }
}

/// Whether a config line binds Alt+Tab or Alt+Shift+Tab (to something other than Prism).
pub fn binds_alt_tab(c: Compositor, line: &str) -> bool {
    let l = line.trim();
    if l.starts_with('#') || l.starts_with("//") || l.contains("prism-ctl") {
        return false;
    }
    let lower = l.to_lowercase();
    match c {
        Compositor::Mango | Compositor::Hyprland => {
            let Some((k, v)) = lower.split_once('=') else { return false };
            if !k.trim().starts_with("bind") {
                return false;
            }
            let mut parts = v.split(',').map(str::trim);
            let (mods, key) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let mods: Vec<&str> = mods.split(['+', ' ', '_']).filter(|m| !m.is_empty()).collect();
            key == "tab" && mods.contains(&"alt") && mods.iter().all(|m| matches!(*m, "alt" | "shift"))
        }
        Compositor::Niri => {
            let head = lower.split('{').next().unwrap_or("").trim();
            head == "alt+tab" || head == "alt+shift+tab" || head == "shift+alt+tab"
        }
    }
}

/// The compositor and its Alt+Tab bindings that would clash with Prism's switcher.
pub fn conflicts() -> Option<(Compositor, Vec<Bind>)> {
    let c = Compositor::current()?;
    let mut found = Vec::new();
    for f in files(c) {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        for (i, line) in text.lines().enumerate() {
            if binds_alt_tab(c, line) {
                found.push(Bind { file: f.clone(), line: i + 1, text: line.trim().to_string() });
            }
        }
    }
    Some((c, found))
}

/// Whether the compositor already sends Alt+Tab to Prism.
pub fn prism_bound() -> bool {
    let Some(c) = Compositor::current() else { return false };
    files(c).iter().filter_map(|f| std::fs::read_to_string(f).ok()).any(|t| t.lines().any(|l| l.contains("prism-ctl switcher") && !l.trim_start().starts_with('#')))
}

/// What to tell the user after turning the switcher on (None: nothing to do).
pub fn advice() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let short = |p: &Path| {
        let s = p.display().to_string();
        if !home.is_empty() && s.starts_with(&home) { format!("~{}", &s[home.len()..]) } else { s }
    };
    let Some((c, binds)) = conflicts() else {
        return Some("Bind Alt+Tab to \u{201c}prism-ctl switcher next\u{201d} (and Alt+Shift+Tab to \u{201c}prism-ctl switcher prev\u{201d}) in your compositor.".into());
    };
    let add = c.prism_binds().replace('\n', "   ");
    if binds.is_empty() {
        if prism_bound() {
            return None;
        }
        return Some(format!("{} does not bind Alt+Tab yet. Add to {}:   {add}", c.name(), short(&c.main_config())));
    }
    let lines: Vec<String> = binds.iter().map(|b| format!("{}:{}  {}", short(&b.file), b.line, b.text)).collect();
    let (comment, reload) = match c {
        Compositor::Mango => ("#", "then reload (Super+Ctrl+R or \u{201c}mmsg dispatch reload_config\u{201d})"),
        Compositor::Hyprland => ("#", "Hyprland reloads by itself"),
        Compositor::Niri => ("//", "Niri reloads by itself"),
    };
    Some(format!(
        "{} already uses Alt+Tab: {}. Comment it out (put {comment} in front), add   {add}   {reload}.",
        c.name(),
        lines.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_alt_tab() {
        assert!(binds_alt_tab(Compositor::Mango, "bind=ALT,Tab,focusstack,next"));
        assert!(binds_alt_tab(Compositor::Mango, "bind=ALT+SHIFT,Tab,focusstack,prev"));
        assert!(!binds_alt_tab(Compositor::Mango, "bind=SUPER,Tab,togglejump,"));
        assert!(!binds_alt_tab(Compositor::Mango, "#bind=ALT,Tab,focusstack,next"));
        assert!(!binds_alt_tab(Compositor::Mango, "bind=ALT,Tab,spawn,prism-ctl switcher next"));
        assert!(binds_alt_tab(Compositor::Hyprland, "bind = ALT, Tab, cyclenext"));
        assert!(binds_alt_tab(Compositor::Hyprland, "bind = ALT SHIFT, Tab, cyclenext, prev"));
        assert!(binds_alt_tab(Compositor::Niri, "    Alt+Tab { focus-window-previous; }"));
        assert!(!binds_alt_tab(Compositor::Niri, "    Mod+Tab { focus-workspace-previous; }"));
    }
}

#[cfg(test)]
mod live {
    /// Prints what Settings would say on this machine: cargo test advice_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn advice_live() {
        println!("{:?}", super::advice());
    }
}
