//! Reading and editing the compositor's key bindings and window rules (the Shortcuts and
//! Window Rules editor), for MangoWM, niri and Hyprland.
//!
//! * MangoWM: `bind*=MODS,KEY,action,args` and `window_rule=app_id:X,title:Y,key:value,...` lines
//!   anywhere in config.conf and the files it sources; edited in place, new ones go to the file
//!   that already holds most of them (keys.conf / wrules.conf in a modular config).
//! * niri: single-line binds inside `binds { }` (edited in place, new ones before the closing
//!   brace of the first block); window rules Prism writes live in `prism-rules.kdl`, included
//!   from config.kdl (rules elsewhere are shown, not edited).
//! * Hyprland (Lua config only; hyprlang is gone): single-line
//!   `hl.bind("COMBO", hl.dsp.exec_cmd("cmd"))` calls (`local` strings resolved; new ones in
//!   `prism-binds.lua`, required from hyprland.lua) and `hl.window_rule { ... }` blocks (new ones
//!   after the last rule).
//!
//! Every file is copied to `<file>.prism-bak` before Prism changes it the first time in a
//! session, and the compositor is told to reload.

use std::collections::HashMap;
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
    /// Mango: written as `window_rule_once` (applied only when the window opens).
    pub once: bool,
    /// Hyprland Lua: the rule's `name`.
    pub name: String,
    /// Hyprland Lua: what comes before `hl.window_rule` (`local r = `).
    pub lead: String,
    /// niri and Hyprland Lua: the indentation of the block's lines (empty: the default).
    pub indent: String,
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

/// Lua: the quoted string at the start of `s` (`"..."` or `'...'`, backslash escapes) and what
/// follows it.
fn lua_string(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    let q = s.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let mut out = String::new();
    let mut it = s.char_indices().skip(1);
    while let Some((i, c)) = it.next() {
        if c == '\\' {
            if let Some((_, n)) = it.next() {
                out.push(match n {
                    'n' => '\n',
                    't' => '\t',
                    o => o,
                });
            }
        } else if c == q {
            return Some((out, &s[i + 1..]));
        } else {
            out.push(c);
        }
    }
    None
}

/// Walk `s` outside Lua strings and comments: `f(index, char, depth)` for every such char, where
/// depth counts open `(`/`{`/`[` before it. Stops when `f` returns false or at a `--` comment;
/// returns where it stopped.
fn lua_walk(s: &str, mut f: impl FnMut(usize, char, i32) -> bool) -> usize {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if let Some(q) = quote {
            if c == '\\' {
                it.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '-' && it.peek().is_some_and(|(_, n)| *n == '-') {
            return i;
        }
        if !f(i, c, depth) {
            return i;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            _ => {}
        }
    }
    s.len()
}

/// Open minus closed brackets on a line of Lua (strings and comments ignored).
fn lua_depth(s: &str) -> i32 {
    let mut d = 0;
    let _ = lua_walk(s, |_, c, _| {
        match c {
            '(' | '{' | '[' => d += 1,
            ')' | '}' | ']' => d -= 1,
            _ => {}
        }
        true
    });
    d
}

/// Split `s` at top-level commas; with `close`, stop at the first unmatched `)`/`}`
/// (None if it never comes).
fn lua_split(s: &str, close: bool) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut end = None;
    let stop = lua_walk(s, |i, c, depth| {
        if depth == 0 && c == ',' {
            parts.push(&s[start..i]);
            start = i + 1;
        } else if depth == 0 && close && matches!(c, ')' | '}') {
            end = Some(i);
            return false;
        }
        true
    });
    let end = match (close, end) {
        (true, None) => return None,
        (_, e) => e.unwrap_or(stop.max(start)),
    };
    let last = s[start..end].trim();
    if !last.is_empty() {
        parts.push(&s[start..end]);
    }
    Some(parts.into_iter().map(str::trim).collect())
}

/// A Lua string expression: quoted strings and `local` string variables joined with `..`.
fn lua_concat(expr: &str, vars: &HashMap<String, String>) -> Option<String> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut skip = false;
    let end = lua_walk(expr, |i, c, _| {
        if skip {
            skip = false;
        } else if c == '.' && expr[i..].starts_with("..") {
            pieces.push(&expr[start..i]);
            start = i + 2;
            skip = true;
        }
        true
    });
    pieces.push(&expr[start..end.max(start)]);
    let mut out = String::new();
    for p in pieces {
        let p = p.trim();
        if p.starts_with(['"', '\'']) {
            let (v, rest) = lua_string(p)?;
            if !rest.trim().is_empty() {
                return None;
            }
            out += &v;
        } else {
            out += vars.get(p)?;
        }
    }
    Some(out)
}

/// `local name = "value"` (or a concatenation of known ones) lines of a Lua file.
fn lua_vars(text: &str, vars: &mut HashMap<String, String>) {
    for l in text.lines() {
        let Some(rest) = l.trim().strip_prefix("local ") else { continue };
        let Some((name, expr)) = rest.split_once('=') else { continue };
        let name = name.trim();
        if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            if let Some(v) = lua_concat(expr, vars) {
                vars.insert(name.to_string(), v);
            }
        }
    }
}

/// Hyprland Lua: `hl.bind("SUPER + T", hl.dsp.exec_cmd("cmd"))` on one line; the keys and the
/// command may use `local` strings (`mainMod .. " + T"`, `exec_cmd(terminal)`). Returns mods,
/// key, action (the command, else the dispatcher as written) and whether Prism can rewrite it.
fn lua_bind(line: &str, vars: &HashMap<String, String>) -> Option<(String, String, String, bool)> {
    let l = line.trim();
    if l.starts_with("--") {
        return None;
    }
    let at = l.find("hl.bind(")?;
    let args = lua_split(&l[at + "hl.bind(".len()..], true)?;
    // Kept as a variable (`local b = hl.bind(...)`) or with options: not Prism's to rewrite.
    let mut editable = l[..at].trim().is_empty() && args.len() == 2;
    let (mods, key) = match lua_concat(args.first()?, vars) {
        Some(combo) => {
            let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
            let (key, mods) = parts.split_last()?;
            (norm_mods(&mods.join("+")), key.to_string())
        }
        None => {
            editable = false;
            (String::new(), args[0].to_string())
        }
    };
    let disp = args.get(1).copied().unwrap_or("");
    let cmd = disp.find("exec_cmd(").and_then(|i| lua_split(&disp[i + "exec_cmd(".len()..], true)).and_then(|a| lua_concat(a.first()?, vars));
    let action = match cmd {
        Some(c) => c,
        None => {
            editable = false;
            disp.to_string()
        }
    };
    Some((mods, key, action, editable))
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
    let mut vars = HashMap::new();
    for f in files(c).iter().filter(|f| is_lua(f)) {
        lua_vars(&read(f), &mut vars);
    }
    for f in files(c) {
        let text = read(&f);
        let lua = is_lua(&f);
        let mut in_binds = 0i32;
        for (i, line) in text.lines().enumerate() {
            let b = match c {
                Compositor::Mango => mango_bind(line).map(|(f, m, k, a)| (f, m, k, a, true)),
                Compositor::Hyprland if lua => lua_bind(line, &vars).map(|(m, k, a, e)| (String::new(), m, k, a, e)),
                Compositor::Hyprland => None,
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
                        b.map(|(m, k, a)| (String::new(), m, k, a, true))
                    } else {
                        None
                    }
                }
            };
            if let Some((flags, mods, key, action, editable)) = b {
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

/// Mango: `window_rule=app_id:pavucontrol,is_floating:1,width:800` (also the older
/// `windowrule=isfloating:1,appid:pavucontrol` spelling).
fn mango_rule(line: &str) -> Option<Rule> {
    let l = line.trim();
    if l.starts_with('#') {
        return None;
    }
    let (k, v) = l.split_once('=')?;
    if !matches!(k.trim(), "window_rule" | "window_rule_once" | "windowrule") {
        return None;
    }
    let mut r = Rule { once: k.trim() == "window_rule_once", ..Rule::default() };
    for part in v.split(',') {
        let Some((k, v)) = kv(part) else { continue };
        match k.as_str() {
            "app_id" | "appid" => r.app_id = v,
            "title" => r.title = v,
            "is_floating" | "isfloating" => r.floating = Some(v == "1"),
            "width" => r.width = v,
            "height" => r.height = v,
            "tags" => r.workspace = v,
            "focused_opacity" => r.opacity = v,
            _ => r.other.push(format!("{k}:{v}")),
        }
    }
    Some(r)
}

/// KDL properties (`app-id=r#"^x$"# title="y"`); None if `s` has anything else (`{}`,
/// arguments).
fn kdl_props(s: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let eq = rest.find('=')?;
        let key = &rest[..eq];
        if key.is_empty() || key.contains(char::is_whitespace) || key.contains(['{', '"']) {
            return None;
        }
        let v = &rest[eq + 1..];
        let (value, after) = if let Some(raw) = v.strip_prefix('r') {
            let hashes = raw.len() - raw.trim_start_matches('#').len();
            let body = raw[hashes..].strip_prefix('"')?;
            let close = format!("\"{}", "#".repeat(hashes));
            let e = body.find(&close)?;
            (body[..e].to_string(), &body[e + close.len()..])
        } else if let Some(body) = v.strip_prefix('"') {
            let mut val = String::new();
            let mut it = body.char_indices();
            let mut end = None;
            while let Some((i, c)) = it.next() {
                match c {
                    '\\' => val.push(it.next()?.1),
                    '"' => {
                        end = Some(i);
                        break;
                    }
                    c => val.push(c),
                }
            }
            (val, &body[end? + 1..])
        } else {
            let e = v.find(char::is_whitespace).unwrap_or(v.len());
            (v[..e].to_string(), &v[e..])
        };
        if !after.is_empty() && !after.starts_with(char::is_whitespace) {
            return None;
        }
        out.push((key.to_string(), value));
        rest = after.trim_start();
    }
    Some(out)
}

/// A KDL string for a regex: raw (`r#"..."#`) so backslashes stay as written.
fn kdl_raw(s: &str) -> String {
    if s.contains("\"#") {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("r#\"{s}\"#")
    }
}

/// niri: `window-rule { ... }` blocks. The first `match` with only `app-id` / `title` is the
/// rule's app and title (regexes as written); other matches, nested blocks, comments and
/// settings Prism does not edit are kept as written. `/* */` comments are skipped.
fn niri_rules(text: &str, file: &Path, editable: bool) -> Vec<Rule> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_comment = false;
    while i < lines.len() {
        let t = lines[i].trim();
        if in_comment {
            in_comment = !t.contains("*/");
            i += 1;
            continue;
        }
        if t.starts_with("/*") {
            in_comment = !t.contains("*/");
            i += 1;
            continue;
        }
        if !(t.starts_with("window-rule") && t.ends_with('{')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut depth = 1;
        let mut r = Rule { file: file.to_path_buf(), line: start, editable, ..Default::default() };
        let mut matched = false;
        let mut indent_set = false;
        i += 1;
        while i < lines.len() && depth > 0 {
            let raw = lines[i].trim_end();
            let t = raw.trim();
            let before = depth;
            depth += t.matches('{').count() as i32 - t.matches('}').count() as i32;
            i += 1;
            if depth <= 0 || t.is_empty() {
                continue;
            }
            if !indent_set {
                r.indent = raw[..raw.len() - raw.trim_start().len()].to_string();
                indent_set = true;
            }
            if before != 1 || depth != 1 {
                r.other.push(raw.to_string());
                continue;
            }
            let (key, val) = t.split_once(char::is_whitespace).map(|(k, v)| (k, v.trim())).unwrap_or((t, ""));
            let fixed = || val.split("fixed").nth(1).map(|s| s.trim().trim_end_matches(['}', ';', ' ']).trim().to_string());
            match key {
                "match" if !matched => match kdl_props(val) {
                    Some(p) if !p.is_empty() && p.iter().all(|(k, _)| k == "app-id" || k == "title") => {
                        for (k, v) in p {
                            if k == "app-id" {
                                r.app_id = v;
                            } else {
                                r.title = v;
                            }
                        }
                        matched = true;
                    }
                    _ => r.other.push(raw.to_string()),
                },
                "open-floating" => r.floating = Some(val.trim_end_matches(';').trim() == "true"),
                "open-on-workspace" => r.workspace = first_quoted(val).unwrap_or("").to_string(),
                "opacity" => r.opacity = val.trim_end_matches(';').trim().to_string(),
                "default-column-width" if fixed().is_some() => r.width = fixed().unwrap_or_default(),
                "default-window-height" if fixed().is_some() => r.height = fixed().unwrap_or_default(),
                _ => r.other.push(raw.to_string()),
            }
        }
        r.lines = i - start;
        out.push(r);
    }
    out
}

/// Hyprland Lua: `hl.window_rule { ... }` / `hl.window_rule({ ... })` blocks (`user/wrules.lua`):
/// `name`, `match = { class = ..., title = ... }`, `float`, `size = { w, h }`, `workspace`,
/// `opacity`; other fields are kept as written. A match on more than class and title, or a rule
/// on one line, is shown but not changed.
fn lua_rules(text: &str, file: &Path) -> Vec<Rule> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let unquote = |v: &str| lua_string(v).map(|(s, _)| s).unwrap_or_else(|| v.to_string());
    while i < lines.len() {
        let t = lines[i].trim();
        let Some(p) = t.find("hl.window_rule").filter(|_| !t.starts_with("--")) else {
            i += 1;
            continue;
        };
        let start = i;
        let mut r = Rule { file: file.to_path_buf(), line: start, editable: true, lead: t[..p].to_string(), ..Default::default() };
        let mut depth = lua_depth(t);
        i += 1;
        if depth <= 0 || !t.ends_with('{') {
            r.other.push(t.to_string());
            r.editable = false;
            r.lines = i - start;
            out.push(r);
            continue;
        }
        let base = depth;
        while i < lines.len() && depth > 0 {
            let raw = lines[i].trim_end();
            let t = raw.trim();
            let before = depth;
            depth += lua_depth(t);
            i += 1;
            if depth <= 0 || t.is_empty() {
                continue;
            }
            if r.indent.is_empty() {
                r.indent = raw[..raw.len() - raw.trim_start().len()].to_string();
            }
            if before != base || depth != base {
                // Inside, or the start of, a table over several lines.
                if before == base && t.starts_with("match") {
                    r.editable = false;
                }
                r.other.push(raw.to_string());
                continue;
            }
            let Some((k, v)) = t.split_once('=').filter(|_| !t.starts_with("--")) else {
                r.other.push(raw.to_string());
                continue;
            };
            let v = v.trim().trim_end_matches(',').trim();
            let table = || v.strip_prefix('{').and_then(|v| v.strip_suffix('}')).and_then(|v| lua_split(v, false));
            match k.trim() {
                "name" => r.name = unquote(v),
                "match" => {
                    for f in table().unwrap_or_default() {
                        match f.split_once('=').map(|(a, b)| (a.trim(), unquote(b.trim()))) {
                            Some(("class", c)) => r.app_id = c,
                            Some(("title", c)) => r.title = c,
                            _ => r.editable = false,
                        }
                    }
                    if !r.editable {
                        r.other.push(raw.to_string());
                    }
                }
                "float" => r.floating = Some(v == "true"),
                "size" => match table().as_deref() {
                    Some([w, h]) => (r.width, r.height) = (unquote(w), unquote(h)),
                    _ => r.other.push(raw.to_string()),
                },
                "workspace" => r.workspace = unquote(v),
                "opacity" => r.opacity = unquote(v),
                _ => r.other.push(raw.to_string()),
            }
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
            Compositor::Hyprland if is_lua(&f) => out.extend(lua_rules(&text, &f)),
            _ => {
                for (i, l) in text.lines().enumerate() {
                    let r = if c == Compositor::Mango { mango_rule(l) } else { None };
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
pub fn bind_text(c: Compositor, flags: &str, mods: &str, key: &str, action: &str) -> String {
    let mods = norm_mods(mods);
    match c {
        Compositor::Mango => format!("bind{flags}={},{key},{action}", if mods.is_empty() { "NONE".into() } else { mods }),
        Compositor::Hyprland => {
            let combo = if mods.is_empty() { key.to_string() } else { format!("{} + {key}", mods.replace('+', " + ")) };
            format!("hl.bind(\"{combo}\", hl.dsp.exec_cmd(\"{}\"))", action.replace('\\', "\\\\").replace('"', "\\\""))
        }
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
            if !r.app_id.is_empty() {
                parts.push(format!("app_id:{}", r.app_id));
            }
            if !r.title.is_empty() {
                parts.push(format!("title:{}", r.title));
            }
            if let Some(f) = r.floating {
                parts.push(format!("is_floating:{}", if f { 1 } else { 0 }));
            }
            for (k, v) in [("width", &r.width), ("height", &r.height), ("tags", &r.workspace), ("focused_opacity", &r.opacity)] {
                if !v.is_empty() {
                    parts.push(format!("{k}:{v}"));
                }
            }
            parts.extend(r.other.iter().cloned());
            vec![format!("window_rule{}={}", if r.once { "_once" } else { "" }, parts.join(","))]
        }
        Compositor::Hyprland => lua_rule_text(r),
        Compositor::Niri => {
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let ind = if r.indent.is_empty() { "    " } else { r.indent.as_str() };
            let mut out = vec!["window-rule {".to_string()];
            let mut m = Vec::new();
            if !r.app_id.is_empty() {
                m.push(format!("app-id={}", kdl_raw(&r.app_id)));
            }
            if !r.title.is_empty() {
                m.push(format!("title={}", kdl_raw(&r.title)));
            }
            if !m.is_empty() {
                out.push(format!("{ind}match {}", m.join(" ")));
            }
            // Other matches right after it.
            let (matches, rest): (Vec<&String>, Vec<&String>) = r.other.iter().partition(|o| o.trim_start().starts_with("match"));
            out.extend(matches.into_iter().cloned());
            if let Some(f) = r.floating {
                out.push(format!("{ind}open-floating {f}"));
            }
            if !r.width.is_empty() {
                out.push(format!("{ind}default-column-width {{ fixed {}; }}", r.width));
            }
            if !r.height.is_empty() {
                out.push(format!("{ind}default-window-height {{ fixed {}; }}", r.height));
            }
            if !r.workspace.is_empty() {
                out.push(format!("{ind}open-on-workspace \"{}\"", esc(&r.workspace)));
            }
            if !r.opacity.is_empty() {
                out.push(format!("{ind}opacity {}", r.opacity));
            }
            out.extend(rest.into_iter().map(|o| if o.starts_with(char::is_whitespace) { o.clone() } else { format!("{ind}{o}") }));
            out.push("}".to_string());
            out
        }
    }
}

/// A Hyprland Lua `hl.window_rule { ... }` block (tabs, as in Hyprland's example config).
fn lua_rule_text(r: &Rule) -> Vec<String> {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let val = |s: &str| if s.parse::<f64>().is_ok() { s.to_string() } else { q(s) };
    let name = if r.name.is_empty() {
        let base = if r.app_id.is_empty() { &r.title } else { &r.app_id };
        let slug: String = base.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let slug = slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
        format!("prism-{slug}")
    } else {
        r.name.clone()
    };
    let ind = if r.indent.is_empty() { "\t" } else { r.indent.as_str() };
    let mut out = vec![format!("{}hl.window_rule {{", r.lead), format!("{ind}name = {},", q(&name))];
    let mut m = Vec::new();
    if !r.app_id.is_empty() {
        m.push(format!("class = {}", q(&r.app_id)));
    }
    if !r.title.is_empty() {
        m.push(format!("title = {}", q(&r.title)));
    }
    if !m.is_empty() {
        out.push(format!("{ind}match = {{ {} }},", m.join(", ")));
    }
    if let Some(f) = r.floating {
        out.push(format!("{ind}float = {f},"));
    }
    if !r.width.is_empty() && !r.height.is_empty() {
        out.push(format!("{ind}size = {{ {}, {} }},", val(&r.width), val(&r.height)));
    }
    if !r.workspace.is_empty() {
        out.push(format!("{ind}workspace = {},", q(&r.workspace)));
    }
    if !r.opacity.is_empty() {
        out.push(format!("{ind}opacity = {},", val(&r.opacity)));
    }
    out.extend(r.other.iter().map(|o| if o.starts_with(char::is_whitespace) { o.clone() } else { format!("{ind}{o}") }));
    out.push("}".to_string());
    out
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
        Compositor::Hyprland => format!("require(\"{}\")", name.trim_end_matches(".lua")),
        _ => format!("source=./{name}"),
    };
    let comment = match c {
        Compositor::Niri => "//",
        Compositor::Hyprland => "--",
        Compositor::Mango => "#",
    };
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
        Compositor::Hyprland => (expand_home("~/.config/hypr/prism-binds.lua"), None),
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
            let text = bind_text(c, &o.flags, mods, key, action);
            splice(&o.file, o.line, 1, &[text])
        }
        (Some(o), None) => splice(&o.file, o.line, 1, &[]),
        (None, Some((mods, key, action))) => {
            let (f, at) = bind_target(c);
            let text = bind_text(c, "", mods, key, action);
            match at {
                Some(i) => splice(&f, i, 0, &[text]),
                None => {
                    append(&f, &[text])?;
                    if c != Compositor::Mango {
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
    let target = match (old, c) {
        (Some(o), _) => o.file.clone(),
        (None, Compositor::Niri) => niri_rules_file(),
        (None, _) => {
            // After the last rule, else in a rules file of a modular config (wrules.conf / wrules.lua).
            let all = rules(c);
            all.last().map(|r| r.file.clone()).or_else(|| files(c).into_iter().find(|f| f.file_name().is_some_and(|n| n.to_string_lossy().contains("rule")))).unwrap_or_else(|| c.main_config())
        }
    };
    let text = new.map(|r| rule_text(c, &Rule { file: target.clone(), ..r.clone() })).unwrap_or_default();
    match old {
        Some(o) => splice(&o.file, o.line, o.lines.max(1), &text),
        None => {
            let block = c != Compositor::Mango;
            let text = if block && target.exists() { [vec![String::new()], text].concat() } else { text };
            append(&target, &text)?;
            if c == Compositor::Niri {
                ensure_included(c, &target)?;
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
        assert_eq!(bind_text(Compositor::Mango, "l", "", "XF86AudioMute", "spawn,x"), "bindl=NONE,XF86AudioMute,spawn,x");
        let r = mango_rule("windowrule=isfloating:1,width:800,height:600,appid:pavucontrol").unwrap();
        assert_eq!((r.floating, r.width.as_str(), r.app_id.as_str()), (Some(true), "800", "pavucontrol"));
        assert_eq!(rule_text(Compositor::Mango, &r), vec!["window_rule=app_id:pavucontrol,is_floating:1,width:800,height:600"]);
        let r = mango_rule("window_rule_once=app_id:(^mpv$|^qView$),no_blur:1,focused_opacity:1.0,title:x").unwrap();
        assert_eq!((r.app_id.as_str(), r.title.as_str(), r.opacity.as_str(), r.once), ("(^mpv$|^qView$)", "x", "1.0", true));
        assert_eq!(rule_text(Compositor::Mango, &r), vec!["window_rule_once=app_id:(^mpv$|^qView$),title:x,focused_opacity:1.0,no_blur:1"]);
    }

    #[test]
    fn niri_and_hyprland_lines() {
        assert_eq!(niri_bind(r#"    Mod+Shift+T hotkey-overlay-title="x" { spawn "ghostty"; }"#), Some(("SUPER+SHIFT".into(), "T".into(), r#"spawn "ghostty";"#.into())));
        assert_eq!(bind_text(Compositor::Niri, "", "SUPER+SHIFT", "T", r#"spawn "ghostty""#), r#"    Mod+Shift+T { spawn "ghostty"; }"#);
        let mut vars = HashMap::new();
        lua_vars("local mainMod = \"SUPER\" -- Sets \"Windows\" key\nlocal terminal = \"ghostty\"\nlocal qs = \"qs -c x \"\n", &mut vars);
        let b = |l: &str| lua_bind(l, &vars);
        assert_eq!(b(r#"hl.bind("SUPER + SHIFT + Q", hl.dsp.exec_cmd("kitty"))"#), Some(("SUPER+SHIFT".into(), "Q".into(), "kitty".into(), true)));
        assert_eq!(b(r#"hl.bind(mainMod .. " + return", hl.dsp.exec_cmd(terminal))"#), Some(("SUPER".into(), "return".into(), "ghostty".into(), true)));
        assert_eq!(b(r#"hl.bind(mainMod .. " + A", hl.dsp.exec_cmd(qs .. "notif toggle"))"#), Some(("SUPER".into(), "A".into(), "qs -c x notif toggle".into(), true)));
        assert_eq!(b(r#"local c = hl.bind(mainMod .. " + escape", hl.dsp.window.close())"#), Some(("SUPER".into(), "escape".into(), "hl.dsp.window.close()".into(), false)));
        assert_eq!(b(r#"hl.bind(mainMod .. " + V", hl.dsp.window.float({ action = "toggle" }))"#).map(|x| (x.2, x.3)), Some((r#"hl.dsp.window.float({ action = "toggle" })"#.into(), false)));
        assert_eq!(b(r#"    hl.bind(mainMod .. " + " .. key, hl.dsp.focus({ workspace = i}))"#).map(|x| x.3), Some(false));
        assert_eq!(bind_text(Compositor::Hyprland, "", "SUPER+SHIFT", "Q", "kitty"), r#"hl.bind("SUPER + SHIFT + Q", hl.dsp.exec_cmd("kitty"))"#);
        let kdl = "window-rule {\n    match app-id=r#\"^firefox$\"#\n    open-floating true\n    default-column-width { fixed 900; }\n}\n";
        let r = &niri_rules(kdl, Path::new("x"), true)[0];
        assert_eq!((r.app_id.as_str(), r.floating, r.width.as_str(), r.lines), ("^firefox$", Some(true), "900", 5));
        assert_eq!(rule_text(Compositor::Niri, r).join("\n") + "\n", kdl);
    }

    #[test]
    fn niri_rules_as_written() {
        let kdl = r##"window-rule {
    match app-id=r#"zen$"# title="^Picture-in-Picture$"
    open-floating true
}
// Global settings
window-rule {
	match {}
	opacity 0.8
	background-effect {
		blur true
	}
}
window-rule {
	match app-id=r#"^org.gnome.[Ll]oupe$"#
	match app-id=r#"^[Mm]pv$"#
	opacity 1.0
	default-column-width { proportion 0.5; }
}
/* Workspaces
window-rule {
	match app-id=r#"^[Ss]team$"#
	open-on-workspace "games"
} */
"##;
        let r = niri_rules(kdl, Path::new("x"), true);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].app_id.as_str(), r[0].title.as_str(), r[0].floating), ("zen$", "^Picture-in-Picture$", Some(true)));
        assert_eq!(rule_text(Compositor::Niri, &r[0]), vec!["window-rule {", r##"    match app-id=r#"zen$"# title=r#"^Picture-in-Picture$"#"##, "    open-floating true", "}"]);
        assert_eq!((r[1].app_id.as_str(), r[1].opacity.as_str()), ("", "0.8"));
        assert_eq!(rule_text(Compositor::Niri, &r[1]).join("\n"), "window-rule {\n\tmatch {}\n\topacity 0.8\n\tbackground-effect {\n\t\tblur true\n\t}\n}");
        assert_eq!(rule_text(Compositor::Niri, &r[2]).join("\n"), "window-rule {\n\tmatch app-id=r#\"^org.gnome.[Ll]oupe$\"#\n\tmatch app-id=r#\"^[Mm]pv$\"#\n\topacity 1.0\n\tdefault-column-width { proportion 0.5; }\n}");
    }

    #[test]
    fn hyprland_lua_rules() {
        let lua = r#"local suppressMaximizeRule = hl.window_rule({
    -- Ignore maximize requests from all apps.
    name  = "suppress-maximize-events",
    match = { class = ".*" },
    suppress_event = "maximize",
})
hl.window_rule({
    name  = "fix-xwayland-drags",
    match = {
        class      = "^$",
        xwayland   = true,
    },
    no_focus = true,
})
hl.window_rule {
	name = "calculator",
	match = { class = "^(org.gnome.[cC]alculator)$" },
	float = true,
	size = { 360, 616 },
}
hl.window_rule {
	name = "rmpc",
	match = { class = "com.example.rmpc" },
	workspace = "special:magic",
	opacity = 0.8,
	move = { 3, 3 },
}
hl.layer_rule {
	name = "wl-eww",
	blur = true,
}
"#;
        let r = lua_rules(lua, Path::new("w.lua"));
        assert_eq!(r.len(), 4);
        assert_eq!((r[0].name.as_str(), r[0].app_id.as_str(), r[0].editable, r[0].lines), ("suppress-maximize-events", ".*", true, 6));
        assert_eq!(rule_text(Compositor::Hyprland, &r[0]), vec!["local suppressMaximizeRule = hl.window_rule {", "    name = \"suppress-maximize-events\",", "    match = { class = \".*\" },", "    -- Ignore maximize requests from all apps.", "    suppress_event = \"maximize\",", "}"]);
        assert!(!r[1].editable);
        assert_eq!((r[2].app_id.as_str(), r[2].floating, r[2].width.as_str(), r[2].height.as_str()), ("^(org.gnome.[cC]alculator)$", Some(true), "360", "616"));
        assert_eq!(rule_text(Compositor::Hyprland, &r[2]).join("\n") + "\n", lua.lines().skip(14).take(6).collect::<Vec<_>>().join("\n") + "\n");
        assert_eq!((r[3].workspace.as_str(), r[3].opacity.as_str(), r[3].other.clone()), ("special:magic", "0.8", vec!["\tmove = { 3, 3 },".to_string()]));
        let new = Rule { app_id: "pavucontrol".into(), floating: Some(true), file: "w.lua".into(), ..Default::default() };
        assert_eq!(rule_text(Compositor::Hyprland, &new), vec!["hl.window_rule {", "\tname = \"prism-pavucontrol\",", "\tmatch = { class = \"pavucontrol\" },", "\tfloat = true,", "}"]);
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
