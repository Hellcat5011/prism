use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreeterConfig {
    #[serde(rename = "lockscreenAlignment", default = "default_alignment")]
    pub lockscreen_alignment: String,
    #[serde(rename = "rememberLastUser", default = "default_remember")]
    pub remember_last_user: bool,
    #[serde(rename = "defaultSession", default = "default_session")]
    pub default_session: String,
}

fn default_alignment() -> String {
    "left".to_string()
}
fn default_remember() -> bool {
    true
}

pub fn detect_default_session() -> String {
    // 1. Check /usr/share/wayland-sessions/
    let session_dir = Path::new("/usr/share/wayland-sessions");
    if session_dir.is_dir() {
        let priority = ["mango", "niri", "hyprland", "sway"];
        for name in &priority {
            let desktop_file = session_dir.join(format!("{}.desktop", name));
            if desktop_file.exists() {
                return name.to_string();
            }
        }
        if let Ok(entries) = fs::read_dir(session_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("desktop") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        return stem.to_string();
                    }
                }
            }
        }
    }

    // 2. Check binary paths
    for bin in &["mango", "niri", "hyprland", "sway"] {
        if Path::new(&format!("/usr/bin/{}", bin)).exists() || Path::new(&format!("/usr/local/bin/{}", bin)).exists() {
            return bin.to_string();
        }
    }

    "mango".to_string()
}

fn default_session() -> String {
    detect_default_session()
}

impl Default for GreeterConfig {
    fn default() -> Self {
        Self {
            lockscreen_alignment: default_alignment(),
            remember_last_user: default_remember(),
            default_session: default_session(),
        }
    }
}

pub fn get_state_dir() -> PathBuf {
    let candidates = [
        "/var/lib/greetd/prism",
        "/var/lib/greetd/quickshell-greeter",
    ];
    for dir in &candidates {
        let p = Path::new(dir);
        if p.exists() {
            return p.to_path_buf();
        }
    }
    PathBuf::from("/var/lib/greetd/prism")
}

pub fn load_greeter_config() -> GreeterConfig {
    let state_dir = get_state_dir();
    let conf_file = state_dir.join("config-snapshot.json");
    if let Ok(content) = fs::read_to_string(&conf_file) {
        if let Ok(cfg) = serde_json::from_str::<GreeterConfig>(&content) {
            return cfg;
        }
    }
    GreeterConfig::default()
}

pub fn load_last_user() -> Option<String> {
    let state_dir = get_state_dir();
    let last_user_file = state_dir.join("last-user");
    if let Ok(content) = fs::read_to_string(&last_user_file) {
        if let Ok(v) = serde_json::from_str::<Value>(&content) {
            if let Some(u) = v.get("user").and_then(|u| u.as_str()) {
                if !u.is_empty() {
                    return Some(u.to_string());
                }
            }
        }
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

pub fn save_last_user(username: &str, session_cmd: &[String]) {
    let state_dir = get_state_dir();
    let _ = fs::create_dir_all(&state_dir);
    let last_user_file = state_dir.join("last-user");
    let payload = serde_json::json!({
        "user": username,
        "session": session_cmd
    });
    let _ = fs::write(&last_user_file, serde_json::to_string_pretty(&payload).unwrap_or_default());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&last_user_file, fs::Permissions::from_mode(0o664));
    }
}

fn send_msg(stream: &mut UnixStream, val: &Value) -> Result<(), String> {
    let json_bytes = serde_json::to_vec(val).map_err(|e| e.to_string())?;
    let len = json_bytes.len() as u32;
    stream.write_all(&len.to_ne_bytes()).map_err(|e| format!("Failed to write length: {}", e))?;
    stream.write_all(&json_bytes).map_err(|e| format!("Failed to write payload: {}", e))?;
    stream.flush().map_err(|e| format!("Flush error: {}", e))?;
    Ok(())
}

fn recv_msg(stream: &mut UnixStream) -> Result<Value, String> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).map_err(|e| format!("Greetd closed socket / read len error: {}", e))?;
    let len = u32::from_ne_bytes(len_buf) as usize;
    if len > 1024 * 1024 {
        return Err("Oversized greetd payload".to_string());
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(|e| format!("Read payload error: {}", e))?;
    serde_json::from_slice(&payload).map_err(|e| format!("JSON decode error: {}", e))
}

pub fn authenticate_and_start(
    sock_path: &str,
    username: &str,
    password: &str,
    session_cmd: &[String],
) -> Result<(), String> {
    let mut stream = UnixStream::connect(sock_path)
        .map_err(|e| format!("Failed to connect to greetd socket at '{}': {}", sock_path, e))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    // Helper closure to cancel on failure
    let cancel = |s: &mut UnixStream| {
        let _ = send_msg(s, &serde_json::json!({"type": "cancel_session"}));
    };

    // 1. Create Session
    send_msg(&mut stream, &serde_json::json!({
        "type": "create_session",
        "username": username
    }))?;

    // 2. Auth loop
    let mut prompts = 0;
    loop {
        let resp = match recv_msg(&mut stream) {
            Ok(r) => r,
            Err(e) => {
                cancel(&mut stream);
                return Err(e);
            }
        };

        let msg_type = resp.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match msg_type {
            "auth_message" => {
                prompts += 1;
                if prompts > 5 {
                    cancel(&mut stream);
                    return Err("Too many authentication prompts from greetd".to_string());
                }
                let auth_type = resp.get("auth_message_type").and_then(|t| t.as_str()).unwrap_or("");
                if auth_type == "secret" {
                    send_msg(&mut stream, &serde_json::json!({
                        "type": "post_auth_message_response",
                        "response": password
                    }))?;
                } else {
                    send_msg(&mut stream, &serde_json::json!({
                        "type": "post_auth_message_response"
                    }))?;
                }
            }
            "success" => {
                // Auth succeeded!
                break;
            }
            "error" => {
                let desc = resp.get("description").and_then(|d| d.as_str()).unwrap_or("Authentication failed");
                cancel(&mut stream);
                return Err(desc.to_string());
            }
            other => {
                cancel(&mut stream);
                return Err(format!("Unexpected message from greetd: {}", other));
            }
        }
    }

    // 3. Start Session
    send_msg(&mut stream, &serde_json::json!({
        "type": "start_session",
        "cmd": session_cmd
    }))?;

    let start_resp = recv_msg(&mut stream)?;
    let start_type = start_resp.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if start_type != "success" {
        let desc = start_resp.get("description").and_then(|d| d.as_str()).unwrap_or("Failed to start session");
        return Err(desc.to_string());
    }

    Ok(())
}

/// A login session from /usr/share/wayland-sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// The .desktop file name without the extension (what `defaultSession` names, e.g. "mango").
    pub id: String,
    pub name: String,
    pub cmd: Vec<String>,
}

/// Wayland sessions installed on the system (Name and Exec of each .desktop file), sorted.
pub fn sessions() -> Vec<Session> {
    let mut out = Vec::new();
    for dir in ["/usr/share/wayland-sessions", "/usr/local/share/wayland-sessions"] {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
            if let Some(s) = fs::read_to_string(&p).ok().and_then(|t| parse_session(&id, &t)) {
                if !out.iter().any(|o: &Session| o.name == s.name) {
                    out.push(s);
                }
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

fn parse_session(id: &str, text: &str) -> Option<Session> {
    let mut name = None;
    let mut exec = None;
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            name.get_or_insert_with(|| v.to_string());
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec.get_or_insert_with(|| v.to_string());
        } else if line == "Hidden=true" || line == "NoDisplay=true" {
            return None;
        }
    }
    let cmd: Vec<String> = split_exec(&exec?).into_iter().filter(|a| !(a.len() == 2 && a.starts_with('%'))).collect();
    let name = name.unwrap_or_else(|| if id.is_empty() { cmd[0].clone() } else { id.to_string() });
    (!cmd.is_empty()).then(|| Session { id: id.to_string(), name, cmd })
}

/// Split an `Exec=` value into arguments like a POSIX shell (QML used `shlex.split`): single
/// quotes are literal, double quotes allow `\` escapes, a backslash outside quotes escapes the
/// next character.
fn split_exec(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_arg = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                in_arg = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                if !matches!(e, '"' | '\\' | '$' | '`') {
                                    cur.push('\\');
                                }
                                cur.push(e);
                            }
                        }
                        _ => cur.push(q),
                    }
                }
            }
            '\\' => {
                in_arg = true;
                if let Some(e) = chars.next() {
                    cur.push(e);
                }
            }
            c if c.is_whitespace() => {
                if in_arg {
                    out.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            c => {
                in_arg = true;
                cur.push(c);
            }
        }
    }
    if in_arg {
        out.push(cur);
    }
    out
}

/// The session to preselect: the last one when it is remembered and still installed, else
/// `defaultSession` (matched by .desktop id, name or command, case ignored), else the first.
pub fn initial_session(sessions: &[Session], last: Option<&[String]>, default: &str) -> usize {
    if let Some(i) = last.and_then(|l| sessions.iter().position(|s| s.cmd == l)) {
        return i;
    }
    let d = default.trim();
    sessions
        .iter()
        .position(|s| s.id.eq_ignore_ascii_case(d) || s.name.eq_ignore_ascii_case(d) || s.cmd.first().is_some_and(|c| c.eq_ignore_ascii_case(d)))
        .unwrap_or(0)
}

/// Login users (QML enum-helper.py): uid 1000..=60000 with a real login shell (listed in /etc/shells
/// when that file exists). Reads `getent passwd` so NSS users are included, else /etc/passwd.
pub fn users() -> Vec<String> {
    let passwd = std::process::Command::new("getent")
        .arg("passwd")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .or_else(|| fs::read_to_string("/etc/passwd").ok())
        .unwrap_or_default();
    let shells: Vec<String> = fs::read_to_string("/etc/shells")
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect())
        .unwrap_or_default();
    parse_users(&passwd, &shells)
}

fn parse_users(passwd: &str, shells: &[String]) -> Vec<String> {
    const DENY: [&str; 3] = ["/bin/false", "/sbin/nologin", "/usr/sbin/nologin"];
    let mut out: Vec<String> = Vec::new();
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 {
            continue;
        }
        let (name, shell) = (f[0], f[6]);
        let Ok(uid) = f[2].parse::<u32>() else { continue };
        if (1000..=60000).contains(&uid)
            && !DENY.contains(&shell)
            && (shells.is_empty() || shells.iter().any(|s| s == shell))
            && !out.iter().any(|u| u == name)
        {
            out.push(name.to_string());
        }
    }
    out
}

/// The session the last user picked, if it is still installed.
pub fn last_session() -> Option<Vec<String>> {
    let content = fs::read_to_string(get_state_dir().join("last-user")).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let cmd: Vec<String> = v.get("session")?.as_array()?.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    (!cmd.is_empty()).then_some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_desktop_files() {
        let s = parse_session("mango", "[Desktop Entry]\nName=Mango\nExec=mango %U\nType=Application\n").unwrap();
        assert_eq!(s, Session { id: "mango".into(), name: "Mango".into(), cmd: vec!["mango".into()] });
        assert!(parse_session("x", "[Desktop Entry]\nName=X\nExec=x\nHidden=true\n").is_none());
    }

    #[test]
    fn splits_exec_like_a_shell() {
        assert_eq!(split_exec(r#"sh -c "exec niri --session"  'a b' c\ d"#), vec!["sh", "-c", "exec niri --session", "a b", "c d"]);
        assert_eq!(split_exec(r#"x "" "q\"uote""#), vec!["x", "", "q\"uote"]);
    }

    #[test]
    fn picks_the_initial_session() {
        let mk = |id: &str, name: &str, cmd: &str| Session { id: id.into(), name: name.into(), cmd: vec![cmd.into()] };
        let s = vec![mk("hyprland", "Hyprland", "Hyprland"), mk("mango", "Mango", "mango"), mk("niri", "Niri", "niri-session")];
        assert_eq!(initial_session(&s, Some(&["niri-session".to_string()]), "mango"), 2);
        assert_eq!(initial_session(&s, None, "mango"), 1);
        assert_eq!(initial_session(&s, None, "NIRI"), 2);
        assert_eq!(initial_session(&s, Some(&["gone".to_string()]), "unknown"), 0);
    }

    #[test]
    fn lists_login_users_only() {
        let passwd = "root:x:0:0::/root:/bin/bash\nvic:x:1000:1000::/home/vic:/usr/bin/fish\n\
                      svc:x:1001:1001::/:/usr/sbin/nologin\nnobody:x:65534:65534::/:/bin/bash\n\
                      ann:x:1002:1002::/home/ann:/bin/zsh\n";
        let shells = vec!["/usr/bin/fish".to_string(), "/bin/bash".to_string()];
        assert_eq!(parse_users(passwd, &shells), vec!["vic".to_string()]);
        assert_eq!(parse_users(passwd, &[]), vec!["vic".to_string(), "ann".to_string()]);
    }
}
