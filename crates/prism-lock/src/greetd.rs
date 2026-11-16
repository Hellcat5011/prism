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
