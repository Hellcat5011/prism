//! Reading and editing the compositor's key bindings and window rules (the Shortcuts and
//! Window Rules editor), for MangoWM, niri and Hyprland.
//!
//! * MangoWM: `bind*=MODS,KEY,action,args` and `windowrule=key:value,...,appid:X,title:Y` lines
//!   anywhere in config.conf and the files it sources; edited in place, new ones go to the file
//!   that already holds most of them (keys.conf / wrules.conf in a modular config).
//! * niri: single-line binds inside `binds { }` (edited in place, new ones before the closing
//!   brace of the first block); window rules Prism writes live in `prism-rules.kdl`, included
//!   from config.kdl (rules elsewhere are shown, not edited).
//! * Hyprland: hyprlang `bind = MODS, KEY, dispatcher, args` and `windowrule = rule, match`
//!   lines in place; in a Lua config single-line `hl.bind("COMBO", hl.dsp.exec_cmd("cmd"))`
//!   calls (new ones in `prism-binds.lua`, required from hyprland.lua). Lua window rules are
//!   shown read-only.
//!
//! Every file is copied to `<file>.prism-bak` before Prism changes it the first time in a
//! session, and the compositor is told to reload.

use std::path::{Path, PathBuf};

use crate::alt_tab::{files, is_lua, Compositor};
use crate::config::expand_home;

/// A key binding as found in a config file.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    /// "SUPER+SHIFT" style (normalised, upper case; empty for none).
    pub mods: String,
    pub key: String,
    /// The action in the compositor's own syntax (Mango `spawn,firefox`, niri
    /// `spawn "firefox";`, Hyprland `exec, firefox` or a Lua command string).
    pub action: String,
    pub file: PathBuf,
    /// 0-based line.
    pub line: usize,
    /// Prism can rewrite this line.
    pub editable: bool,
    /// Mango: the bind flags (`bindl` -> "l"); kept when the line is rewritten.
    pub flags: String,
}

/// A window rule: what it matches and what it sets.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rule {
    pub app_id: String,
    pub title: String,
    pub floating: Option<bool>,
    pub width: String,
    pub height: String,
    /// Mango tag / niri workspace / Hyprland workspace.
    pub workspace: String,
    pub opacity: String,
    /// Other settings of the rule (kept as written).
    pub other: Vec<String>,
    pub file: PathBuf,
    /// 0-based first line (niri: the `window-rule {` line).
    pub line: usize,
    /// Lines the rule spans.
    pub lines: usize,
    pub editable: bool,
}

pub fn current() -> Option<Compositor> {
    Compositor::current()
}

fn read(f: &Path) -> String {
    std::fs::read_to_string(f).unwrap_or_default()
}

// ── Parsing ─────────────────────────────────────────────────────────────

fn norm_mods(m: &str) -> String {
    m.split(['+', ' ', '_'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))
        .map(|s| match s.to_uppercase().as_str() {
            "MOD" | "MOD4" | "LOGO" | "WIN" => "SUPER".to_string(),
            "MOD1" => "ALT".to_string(),
            "CONTROL" => "CTRL".to_string(),
            o => o.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Mango: `bind[flags]=MODS,KEY,action...`.
fn mango_bind(line: &str) -> Option<(String, String, String, String)> {
    let l = line.trim();
    if l.starts_with('#') {
        return None;
    }
    let (k, v) = l.split_once('=')?;
    let flags = k.trim().strip_prefix("bind")?;
    if !flags.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    let mut parts = v.splitn(3, ',');
    let mods = parts.next()?.trim();
    let key = parts.next()?.trim();
    let action = parts.next().unwrap_or("").trim();
    Some((flags.to_string(), norm_mods(mods), key.to_string(), action.to_string()))
}

/// Hyprland hyprlang: `bind[flags] = MODS, KEY, dispatcher, args`.
fn hypr_bind(line: &str) -> Option<(String, String, String, String)> {
    let l = line.trim();
    if l.starts_with('#') {
        return None;
    }
    let (k, v) = l.split_once('=')?;
    let flags = k.trim().strip_prefix("bind")?;
    if !flags.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    let mut parts = v.splitn(3, ',');
    let mods = parts.next()?.trim();
    let key = parts.next()?.trim();
    let action = parts.next().unwrap_or("").trim();
    Some((flags.to_string(), norm_mods(mods), key.to_string(), action.to_string()))
}

/// Hyprland Lua: `hl.bind("SUPER + T", hl.dsp.exec_cmd("cmd"))` on one line.
fn lua_bind(line: &str) -> Option<(String, String, String)> {
    let l = line.trim();
    let rest = l.strip_prefix("hl.bind(")?;
    let combo = first_quoted(rest)?;
    let after = &rest[rest.find(combo)? + combo.len() + 1..];
    let cmd = after.find("exec_cmd(").and_then(|i| first_quoted(&after[i..])).unwrap_or("");
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last()?;
    Some((norm_mods(&mods.join("+")), key.to_string(), cmd.to_string()))
}

fn first_quoted(s: &str) -> Option<&str> {
    let a = s.find('"')?;
    let b = s[a + 1..].find('"')?;
    Some(&s[a + 1..a + 1 + b])
}

/// niri: `Mod+Shift+T [props] { action; }` on one line (inside `binds { }`).
fn niri_bind(line: &str) -> Option<(String, String, String)> {
    let l = line.trim();
    if l.starts_with("//") || !l.ends_with('}') {
        return None;
    }
    let open = l.find('{')?;
    let head = l[..open].trim();
    let combo = head.split_whitespace().next()?;
    if combo.contains('=') {
        return None;
    }
    let action = l[open + 1..l.len() - 1].trim().to_string();
    let parts: Vec<&str> = combo.split('+').collect();
    let (key, mods) = parts.split_last()?;
    Some((norm_mods(&mods.join("+")), key.to_string(), action))
}

/// All bindings of the compositor, in file order.
pub fn bindings(c: Compositor) -> Vec<Binding> {
    let mut out = Vec::new();
    for f in files(c) {
        let text = read(&f);
        let lua = is_lua(&f);
        let mut in_binds = 0i32;
        for (i, line) in text.lines().enumerate() {
            let b = match c {
                Compositor::Mango => mango_bind(line),
                Compositor::Hyprland if lua => lua_bind(line).map(|(m, k, a)| (String::new(), m, k, a)),
                Compositor::Hyprland => hypr_bind(line),
                Compositor::Niri => {
                    let t = line.trim();
                    if in_binds == 0 && t.starts_with("binds") && t.ends_with('{') {
                        in_binds = 1;
                        continue;
                    }
                    if in_binds > 0 {
                        let b = niri_bind(line);
                        if b.is_none() {
                            in_binds += t.matches('{').count() as i32 - t.matches('}').count() as i32;
                        }
                        b.map(|(m, k, a)| (String::new(), m, k, a))
                    } else {
                        None
                    }
                }
            };
            if let Some((flags, mods, key, action)) = b {
                // Lua binds made in loops (tables) are not single lines Prism can rewrite.
                let editable = !(lua && action.is_empty());
                out.push(Binding { mods, key, action, file: f.clone(), line: i, editable, flags });
            }
        }
    }
    out
}

fn kv(s: &str) -> Option<(String, String)> {
    let (k, v) = s.split_once(':')?;
    Some((k.trim().to_string(), v.trim().to_string()))
}

/// Mango: `windowrule=isfloating:1,width:800,appid:pavucontrol`.
fn mango_rule(line: &str) -> Option<Rule> {
    let l = line.trim();
    if l.starts_with('#') {
        return None;
    }
    let (k, v) = l.split_once('=')?;
    if k.trim() != "windowrule" {
        return None;
    }
    let mut r = Rule::default();
    for part in v.split(',') {
        let Some((k, v)) = kv(part) else { continue };
        match k.as_str() {
            "appid" => r.app_id = v,
            "title" => r.title = v,
            "isfloating" => r.floating = Some(v == "1"),
            "width" => r.width = v,
            "height" => r.height = v,
            "tags" => r.workspace = v,
            "focused_opacity" => r.opacity = v,
            _ => r.other.push(format!("{k}:{v}")),
        }
    }
    Some(r)
}

/// Hyprland hyprlang: `windowrule = float, class:^(pavucontrol)$` (one setting per line).
fn hypr_rule(line: &str) -> Option<Rule> {
    let l = line.trim();
    if l.starts_with('#') {
        return None;
    }
    let (k, v) = l.split_once('=')?;
    if !matches!(k.trim(), "windowrule" | "windowrulev2") {
        return None;
    }
    let mut parts = v.split(',').map(str::trim);
    let setting = parts.next()?.to_string();
    let mut r = Rule::default();
    for p in parts {
        if let Some((k, v)) = kv(p) {
            match k.as_str() {
                "class" | "initialClass" => r.app_id = v.trim_start_matches("^(").trim_end_matches(")$").to_string(),
                "title" | "initialTitle" => r.title = v,
                _ => r.other.push(p.to_string()),
            }
        }
    }
    let mut words = setting.split_whitespace();
    match words.next().unwrap_or("") {
        "float" => r.floating = Some(true),
        "tile" => r.floating = Some(false),
        "size" => {
            r.width = words.next().unwrap_or("").to_string();
            r.height = words.next().unwrap_or("").to_string();
        }
        "workspace" => r.workspace = words.next().unwrap_or("").to_string(),
        "opacity" => r.opacity = words.next().unwrap_or("").to_string(),
        _ => r.other.push(setting.clone()),
    }
    Some(r)
}

/// niri: `window-rule { ... }` blocks (match app-id / title; a few settings understood).
fn niri_rules(text: &str, file: &Path, editable: bool) -> Vec<Rule> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if !(t.starts_with("window-rule") && t.ends_with('{')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut depth = 1;
        let mut r = Rule { file: file.to_path_buf(), line: start, editable, ..Default::default() };
        i += 1;
        while i < lines.len() && depth > 0 {
            let t = lines[i].trim();
            depth += t.matches('{').count() as i32 - t.matches('}').count() as i32;
            if let Some(m) = t.strip_prefix("match") {
                for (k, field) in [("app-id=", &mut r.app_id), ("title=", &mut r.title)] {
                    if let Some(p) = m.find(k) {
                        let v = &m[p + k.len()..];
                        let v = v.trim_start_matches('r').trim_start_matches('#');
                        if let Some(q) = first_quoted(v) {
                            *field = q.trim_start_matches('^').trim_end_matches('$').to_string();
                        }
                    }
                }
            } else if let Some(v) = t.strip_prefix("open-floating") {
                r.floating = Some(v.trim().trim_end_matches(';').trim() == "true");
            } else if let Some(v) = t.strip_prefix("open-on-workspace") {
                r.workspace = first_quoted(v).unwrap_or("").to_string();
            } else if let Some(v) = t.strip_prefix("opacity") {
                r.opacity = v.trim().trim_end_matches(';').trim().to_string();
            } else if t.starts_with("default-column-width") {
                r.width = t.split("fixed").nth(1).map(|s| s.trim().trim_end_matches(['}', ';', ' ']).trim().to_string()).unwrap_or_default();
            } else if t.starts_with("default-window-height") {
                r.height = t.split("fixed").nth(1).map(|s| s.trim().trim_end_matches(['}', ';', ' ']).trim().to_string()).unwrap_or_default();
            } else if depth > 0 && !t.is_empty() {
                r.other.push(t.to_string());
            }
            i += 1;
        }
        r.lines = i - start;
        out.push(r);
    }
    out
}

/// All window rules, in file order.
pub fn rules(c: Compositor) -> Vec<Rule> {
    let managed = niri_rules_file();
    let mut out = Vec::new();
    for f in files(c) {
        let text = read(&f);
        match c {
            Compositor::Niri => out.extend(niri_rules(&text, &f, f == managed)),
            Compositor::Hyprland if is_lua(&f) => {
                for (i, l) in text.lines().enumerate() {
                    let t = l.trim();
                    if t.starts_with("hl.window_rule") {
                        out.push(Rule { other: vec![t.to_string()], file: f.clone(), line: i, lines: 1, editable: false, ..Default::default() });
                    }
                }
            }
            _ => {
                for (i, l) in text.lines().enumerate() {
                    let r = if c == Compositor::Mango { mango_rule(l) } else { hypr_rule(l) };
                    if let Some(mut r) = r {
                        r.file = f.clone();
                        r.line = i;
                        r.lines = 1;
                        r.editable = true;
                        out.push(r);
                    }
                }
            }
        }
    }
    out
}

// ── Writing ─────────────────────────────────────────────────────────────

/// The line(s) for a binding.
pub fn bind_text(c: Compositor, lua: bool, flags: &str, mods: &str, key: &str, action: &str) -> String {
    let mods = norm_mods(mods);
    match c {
        Compositor::Mango => format!("bind{flags}={},{key},{action}", if mods.is_empty() { "NONE".into() } else { mods }),
        Compositor::Hyprland if lua => {
            let combo = if mods.is_empty() { key.to_string() } else { format!("{} + {key}", mods.replace('+', " + ")) };
            format!("hl.bind(\"{combo}\", hl.dsp.exec_cmd(\"{}\"))", action.replace('\\', "\\\\").replace('"', "\\\""))
        }
        Compositor::Hyprland => format!("bind{flags} = {}, {key}, {action}", mods.replace('+', " ")),
        Compositor::Niri => {
            let m: Vec<String> = mods.split('+').filter(|s| !s.is_empty()).map(|s| if s == "SUPER" { "Mod".to_string() } else { titlecase(s) }).collect();
            let combo = if m.is_empty() { key.to_string() } else { format!("{}+{key}", m.join("+")) };
            let action = action.trim();
            let action = if action.ends_with(';') { action.to_string() } else { format!("{action};") };
            format!("    {combo} {{ {action} }}")
        }
    }
}

fn titlecase(s: &str) -> String {
    let l = s.to_lowercase();
    let mut c = l.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// The text of a window rule.
pub fn rule_text(c: Compositor, r: &Rule) -> Vec<String> {
    match c {
        Compositor::Mango => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(f) = r.floating {
                parts.push(format!("isfloating:{}", if f { 1 } else { 0 }));
            }
            for (k, v) in [("width", &r.width), ("height", &r.height), ("tags", &r.workspace), ("focused_opacity", &r.opacity)] {
                if !v.is_empty() {
                    parts.push(format!("{k}:{v}"));
                }
            }
            parts.extend(r.other.iter().cloned());
            if !r.app_id.is_empty() {
                parts.push(format!("appid:{}", r.app_id));
            }
            if !r.title.is_empty() {
                parts.push(format!("title:{}", r.title));
            }
            vec![format!("windowrule={}", parts.join(","))]
        }
        Compositor::Hyprland => {
            let mut m = Vec::new();
            if !r.app_id.is_empty() {
                m.push(format!("class:^({})$", r.app_id));
            }
            if !r.title.is_empty() {
                m.push(format!("title:{}", r.title));
            }
            let m = m.join(", ");
            let mut out = Vec::new();
            if let Some(f) = r.floating {
                out.push(format!("windowrule = {}, {m}", if f { "float" } else { "tile" }));
            }
            if !r.width.is_empty() && !r.height.is_empty() {
                out.push(format!("windowrule = size {} {}, {m}", r.width, r.height));
            }
            if !r.workspace.is_empty() {
                out.push(format!("windowrule = workspace {}, {m}", r.workspace));
            }
            if !r.opacity.is_empty() {
                out.push(format!("windowrule = opacity {}, {m}", r.opacity));
            }
            out
        }
        Compositor::Niri => {
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let mut out = vec!["window-rule {".to_string()];
            let mut m = String::from("    match");
            if !r.app_id.is_empty() {
                m.push_str(&format!(" app-id=\"^{}$\"", esc(&r.app_id)));
            }
            if !r.title.is_empty() {
                m.push_str(&format!(" title=\"{}\"", esc(&r.title)));
            }
            out.push(m);
            if let Some(f) = r.floating {
                out.push(format!("    open-floating {f}"));
            }
            if !r.width.is_empty() {
                out.push(format!("    default-column-width {{ fixed {}; }}", r.width));
            }
            if !r.height.is_empty() {
                out.push(format!("    default-window-height {{ fixed {}; }}", r.height));
            }
            if !r.workspace.is_empty() {
                out.push(format!("    open-on-workspace \"{}\"", esc(&r.workspace)));
            }
            if !r.opacity.is_empty() {
                out.push(format!("    opacity {}", r.opacity));
            }
            out.extend(r.other.iter().map(|o| format!("    {o}")));
            out.push("}".to_string());
            out
        }
    }
}

fn backup_once(f: &Path) {
    static DONE: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut done = DONE.lock().unwrap();
    if done.iter().any(|d| d == f) {
        return;
    }
    let mut bak = f.as_os_str().to_owned();
    bak.push(".prism-bak");
    if std::fs::copy(f, PathBuf::from(bak)).is_ok() {
        done.push(f.to_path_buf());
    }
}

/// Replace `count` lines from `line` of `f` with `with` (none: delete), or insert `with` at
/// `line` when `count` is 0.
fn splice(f: &Path, line: usize, count: usize, with: &[String]) -> Result<(), String> {
    let text = read(f);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if line > lines.len() || line + count > lines.len() {
        return Err(format!("{} changed on disk; reopen the editor", f.display()));
    }
    lines.splice(line..line + count, with.iter().cloned());
    backup_once(f);
    let mut out = lines.join("\n");
    out.push('\n');
    let tmp = f.with_extension("prism-tmp");
    std::fs::write(&tmp, out).map_err(|e| format!("{}: {e}", f.display()))?;
    std::fs::rename(&tmp, f).map_err(|e| format!("{}: {e}", f.display()))
}

/// Append lines at the end of `f` (created if missing).
fn append(f: &Path, with: &[String]) -> Result<(), String> {
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut text = read(f);
    if f.exists() {
        backup_once(f);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for l in with {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(f, text).map_err(|e| format!("{}: {e}", f.display()))
}

/// Make the main config pull in `file` (once).
fn ensure_included(c: Compositor, file: &Path) -> Result<(), String> {
    let main = c.main_config();
    if files(c).iter().any(|f| f == file) {
        return Ok(());
    }
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let line = match c {
        Compositor::Niri => format!("include \"{name}\""),
        Compositor::Hyprland if is_lua(&main) => format!("require(\"{}\")", name.trim_end_matches(".lua")),
        _ => format!("source=./{name}"),
    };
    let comment = if c == Compositor::Niri { "//" } else if is_lua(&main) { "--" } else { "#" };
    append(&main, &[String::new(), format!("{comment} Added by Prism (Shortcuts and Window Rules editor)"), line])
}

fn niri_rules_file() -> PathBuf {
    expand_home("~/.config/niri/prism-rules.kdl")
}

/// Where new bindings go.
fn bind_target(c: Compositor) -> (PathBuf, Option<usize>) {
    let all = bindings(c);
    match c {
        Compositor::Niri => {
            // Before the closing brace of the first binds block.
            for f in files(c) {
                let text = read(&f);
                let mut depth = 0i32;
                let mut inside = false;
                for (i, l) in text.lines().enumerate() {
                    let t = l.trim();
                    if !inside && t.starts_with("binds") && t.ends_with('{') {
                        inside = true;
                        depth = 1;
                        continue;
                    }
                    if inside {
                        depth += t.matches('{').count() as i32 - t.matches('}').count() as i32;
                        if depth == 0 {
                            return (f, Some(i));
                        }
                    }
                }
            }
            (c.main_config(), None)
        }
        Compositor::Hyprland if is_lua(&c.main_config()) => (expand_home("~/.config/hypr/prism-binds.lua"), None),
        _ => {
            // The file with the most bindings.
            let mut counts: Vec<(PathBuf, usize)> = Vec::new();
            for b in &all {
                match counts.iter_mut().find(|(f, _)| *f == b.file) {
                    Some(e) => e.1 += 1,
                    None => counts.push((b.file.clone(), 1)),
                }
            }
            (counts.into_iter().max_by_key(|e| e.1).map(|e| e.0).unwrap_or_else(|| c.main_config()), None)
        }
    }
}

/// Change, add (`old` None) or delete (`new` None) a binding.
pub fn save_binding(c: Compositor, old: Option<&Binding>, new: Option<(&str, &str, &str)>) -> Result<(), String> {
    match (old, new) {
        (Some(o), _) if !o.editable => Err("This binding is not a single line Prism can change; edit it in the config file".into()),
        (Some(o), Some((mods, key, action))) => {
            let text = bind_text(c, is_lua(&o.file), &o.flags, mods, key, action);
            splice(&o.file, o.line, 1, &[text])
        }
        (Some(o), None) => splice(&o.file, o.line, 1, &[]),
        (None, Some((mods, key, action))) => {
            let (f, at) = bind_target(c);
            let lua = is_lua(&f);
            let text = bind_text(c, lua, "", mods, key, action);
            match at {
                Some(i) => splice(&f, i, 0, &[text]),
                None => {
                    append(&f, &[text])?;
                    if lua || c == Compositor::Niri {
                        ensure_included(c, &f)?;
                    }
                    Ok(())
                }
            }
        }
        (None, None) => Ok(()),
    }
}

/// Change, add (`old` None) or delete (`new` None) a window rule.
pub fn save_rule(c: Compositor, old: Option<&Rule>, new: Option<&Rule>) -> Result<(), String> {
    if old.is_some_and(|o| !o.editable) {
        return Err("Prism cannot change this rule; edit it in the config file".into());
    }
    if c == Compositor::Hyprland && is_lua(&c.main_config()) && new.is_some() && old.is_none() {
        return Err("Window rules for Hyprland's Lua config are not supported yet".into());
    }
    let text = new.map(|r| rule_text(c, r)).unwrap_or_default();
    match old {
        Some(o) => splice(&o.file, o.line, o.lines.max(1), &text),
        None => {
            let f = match c {
                Compositor::Niri => niri_rules_file(),
                _ => {
                    // After the last rule, else in a rules file of a modular config (wrules.conf).
                    let all = rules(c);
                    all.last().map(|r| r.file.clone()).or_else(|| files(c).into_iter().find(|f| f.file_name().is_some_and(|n| n.to_string_lossy().contains("rule")))).unwrap_or_else(|| c.main_config())
                }
            };
            append(&f, &text)?;
            if c == Compositor::Niri {
                ensure_included(c, &f)?;
            }
            Ok(())
        }
    }
}

/// Ask the compositor to read its config again (niri and Hyprland notice by themselves).
pub fn reload(c: Compositor) {
    let cmd: &[&str] = match c {
        Compositor::Mango => &["mmsg", "dispatch", "reload_config"],
        Compositor::Hyprland => &["hyprctl", "reload"],
        Compositor::Niri => return,
    };
    let _ = std::process::Command::new(cmd[0]).args(&cmd[1..]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mango_lines() {
        assert_eq!(mango_bind("bindl=NONE,XF86AudioMute,spawn,prism-ctl osd volume mute"), Some(("l".into(), "".into(), "XF86AudioMute".into(), "spawn,prism-ctl osd volume mute".into())));
        assert_eq!(mango_bind("bind=SUPER+SHIFT,q,killclient"), Some(("".into(), "SUPER+SHIFT".into(), "q".into(), "killclient".into())));
        assert!(mango_bind("#bind=SUPER,a,x").is_none());
        assert_eq!(bind_text(Compositor::Mango, false, "l", "", "XF86AudioMute", "spawn,x"), "bindl=NONE,XF86AudioMute,spawn,x");
        let r = mango_rule("windowrule=isfloating:1,width:800,height:600,appid:pavucontrol").unwrap();
        assert_eq!((r.floating, r.width.as_str(), r.app_id.as_str()), (Some(true), "800", "pavucontrol"));
        assert_eq!(rule_text(Compositor::Mango, &r), vec!["windowrule=isfloating:1,width:800,height:600,appid:pavucontrol"]);
    }

    #[test]
    fn niri_and_hyprland_lines() {
        assert_eq!(niri_bind(r#"    Mod+Shift+T hotkey-overlay-title="x" { spawn "ghostty"; }"#), Some(("SUPER+SHIFT".into(), "T".into(), r#"spawn "ghostty";"#.into())));
        assert_eq!(bind_text(Compositor::Niri, false, "", "SUPER+SHIFT", "T", r#"spawn "ghostty""#), r#"    Mod+Shift+T { spawn "ghostty"; }"#);
        assert_eq!(lua_bind(r#"hl.bind("SUPER + SHIFT + Q", hl.dsp.exec_cmd("kitty"))"#), Some(("SUPER+SHIFT".into(), "Q".into(), "kitty".into())));
        assert_eq!(bind_text(Compositor::Hyprland, true, "", "SUPER+SHIFT", "Q", "kitty"), r#"hl.bind("SUPER + SHIFT + Q", hl.dsp.exec_cmd("kitty"))"#);
        assert_eq!(hypr_bind("bind = SUPER SHIFT, Q, exec, kitty"), Some(("".into(), "SUPER+SHIFT".into(), "Q".into(), "exec, kitty".into())));
        let kdl = "window-rule {\n    match app-id=\"^firefox$\"\n    open-floating true\n    default-column-width { fixed 900; }\n}\n";
        let r = &niri_rules(kdl, Path::new("x"), true)[0];
        assert_eq!((r.app_id.as_str(), r.floating, r.width.as_str(), r.lines), ("firefox", Some(true), "900", 5));
        assert_eq!(rule_text(Compositor::Niri, r).join("\n") + "\n", kdl);
    }

    /// `cargo test -p prism-common compconf_live -- --ignored --nocapture` (reads only).
    #[test]
    #[ignore]
    fn compconf_live() {
        let Some(c) = current() else { return };
        let b = bindings(c);
        let r = rules(c);
        println!("{} bindings ({} editable), {} rules", b.len(), b.iter().filter(|b| b.editable).count(), r.len());
        for x in b.iter().take(3) {
            println!("{x:?}");
        }
        for x in r.iter().take(3) {
            println!("{x:?}");
        }
    }

    #[test]
    fn splice_and_append_edit_lines() {
        let dir = std::env::temp_dir().join(format!("prism-compconf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("keys.conf");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        splice(&f, 1, 1, &["B".into()]).unwrap();
        splice(&f, 0, 0, &["z".into()]).unwrap();
        append(&f, &["d".into()]).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "z\na\nB\nc\nd\n");
        assert!(dir.join("keys.conf.prism-bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
