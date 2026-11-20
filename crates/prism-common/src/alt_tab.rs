//! Does the compositor already use Alt+Tab? Before Prism's window switcher takes the key, the
//! compositor's own config (MangoWM, Hyprland or Niri, following its `source` / `include` lines,
//! and for Hyprland's Lua config its `require` / `dofile` calls)
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

    /// Hyprland 0.55+ reads `hyprland.lua` when it exists, else the older `hyprland.conf`.
    fn main_config(self) -> PathBuf {
        match self {
            Compositor::Mango => expand_home("~/.config/mango/config.conf"),
            Compositor::Hyprland => {
                let lua = expand_home("~/.config/hypr/hyprland.lua");
                if lua.exists() { lua } else { expand_home("~/.config/hypr/hyprland.conf") }
            }
            Compositor::Niri => expand_home("~/.config/niri/config.kdl"),
        }
    }

    /// The lines that make Alt+Tab (and Alt+Shift+Tab) open Prism's switcher.
    fn prism_binds(self, lua: bool) -> &'static str {
        if lua {
            return "hl.bind(\"ALT + Tab\", hl.dsp.exec_cmd(\"prism-ctl switcher next\"))\nhl.bind(\"ALT + SHIFT + Tab\", hl.dsp.exec_cmd(\"prism-ctl switcher prev\"))";
        }
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

/// Whether a config file is Hyprland Lua (as opposed to hyprlang, Mango or KDL).
fn is_lua(f: &Path) -> bool {
    f.extension().is_some_and(|e| e == "lua")
}

/// The config files: the main one and everything it sources, in order, each once.
fn files(c: Compositor) -> Vec<PathBuf> {
    let main = c.main_config();
    // Lua's `require` resolves module names against the config directory, not the requiring file.
    let root = main.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut todo = vec![main];
    while let Some(f) = todo.pop() {
        if out.contains(&f) || out.len() > 64 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
        let lua = is_lua(&f);
        for line in text.lines() {
            if lua {
                if let Some(p) = lua_include_of(line, &root) {
                    todo.push(p);
                }
            } else if let Some(p) = include_of(c, line) {
                let p = if p.starts_with('~') { expand_home(&p) } else { dir.join(p.trim_start_matches("./")) };
                todo.push(p);
            }
        }
        out.push(f);
    }
    out
}

/// The first string literal in `s` (double or single quoted).
fn first_string(s: &str) -> Option<&str> {
    let start = s.find(['"', '\''])?;
    let q = s[start..].chars().next()?;
    let rest = &s[start + 1..];
    rest.find(q).map(|end| &rest[..end])
}

/// The file a Lua `require("a.b")` (`<root>/a/b.lua`) or `dofile("path")` line pulls in.
fn lua_include_of(line: &str, root: &Path) -> Option<PathBuf> {
    let l = line.trim();
    if l.starts_with("--") {
        return None;
    }
    if let Some(i) = l.find("require") {
        let name = first_string(&l[i + "require".len()..])?;
        return Some(root.join(format!("{}.lua", name.replace('.', "/"))));
    }
    if let Some(i) = l.find("dofile") {
        let p = first_string(&l[i + "dofile".len()..])?;
        return Some(if p.starts_with('~') { expand_home(p) } else { root.join(p.trim_start_matches("./")) });
    }
    None
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
    if l.starts_with('#') || l.starts_with("//") || l.starts_with("--") || l.contains("prism-ctl") {
        return false;
    }
    let lower = l.to_lowercase();
    match c {
        // Lua: `hl.bind("ALT + Tab", ...)`. A key string built from variables (`mainMod .. " + Tab"`)
        // is not resolved; its modifier counts as unknown, so it does not match.
        Compositor::Hyprland if lower.contains("hl.bind(") => {
            let args = &lower[lower.find("hl.bind(").unwrap_or(0) + "hl.bind(".len()..];
            let keys = args.split(',').next().unwrap_or("").trim();
            if !(keys.starts_with('"') || keys.starts_with('\'')) || keys.contains("..") {
                return false;
            }
            let keys = first_string(keys).unwrap_or("");
            let mut parts: Vec<&str> = keys.split(['+', ' ']).filter(|m| !m.is_empty()).collect();
            let key = parts.pop().unwrap_or("");
            key == "tab" && parts.contains(&"alt") && parts.iter().all(|m| matches!(*m, "alt" | "shift"))
        }
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
    files(c).iter().filter_map(|f| std::fs::read_to_string(f).ok()).any(|t| t.lines().any(|l| {
        let l = l.trim_start();
        l.contains("prism-ctl switcher") && !l.starts_with('#') && !l.starts_with("--")
    }))
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
    let lua = is_lua(&c.main_config());
    let add = c.prism_binds(lua).replace('\n', "   ");
    if binds.is_empty() {
        if prism_bound() {
            return None;
        }
        return Some(format!("{} does not bind Alt+Tab yet. Add to {}:   {add}", c.name(), short(&c.main_config())));
    }
    let lines: Vec<String> = binds.iter().map(|b| format!("{}:{}  {}", short(&b.file), b.line, b.text)).collect();
    let (comment, reload) = match c {
        Compositor::Mango => ("#", "then reload (Super+Ctrl+R or \u{201c}mmsg dispatch reload_config\u{201d})"),
        Compositor::Hyprland if lua => ("--", "Hyprland reloads by itself"),
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
        assert!(binds_alt_tab(Compositor::Hyprland, r#"hl.bind("ALT + Tab", hl.dsp.window.cycle_next())"#));
        assert!(binds_alt_tab(Compositor::Hyprland, r#"hl.bind("ALT + SHIFT + Tab", hl.dsp.exec_cmd("x"))"#));
        assert!(!binds_alt_tab(Compositor::Hyprland, r#"-- hl.bind("ALT + Tab", hl.dsp.exec_cmd("x"))"#));
        assert!(!binds_alt_tab(Compositor::Hyprland, r#"hl.bind(mainMod .. " + Tab", hl.dsp.exec_cmd("x"))"#));
        assert!(!binds_alt_tab(Compositor::Hyprland, r#"hl.bind("ALT + Tab", hl.dsp.exec_cmd("prism-ctl switcher next"))"#));
        assert!(binds_alt_tab(Compositor::Niri, "    Alt+Tab { focus-window-previous; }"));
        assert!(!binds_alt_tab(Compositor::Niri, "    Mod+Tab { focus-workspace-previous; }"));
    }

    #[test]
    fn follows_lua_includes() {
        let root = Path::new("/c/hypr");
        assert_eq!(lua_include_of(r#"require("user.keybinds")"#, root), Some(root.join("user/keybinds.lua")));
        assert_eq!(lua_include_of("require 'prism'", root), Some(root.join("prism.lua")));
        assert_eq!(lua_include_of(r#"dofile("extra.lua")"#, root), Some(root.join("extra.lua")));
        assert_eq!(lua_include_of(r#"-- require("off")"#, root), None);
        assert_eq!(lua_include_of(r#"hl.bind("ALT + Tab", x)"#, root), None);
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
