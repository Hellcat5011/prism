//! Clipboard sync (`kdeconnect.clipboard`, `kdeconnect.clipboard.connect`). PC copies are
//! watched with `wl-paste --watch`; text from the phone is set with `wl-copy` (so it also lands
//! in Prism's clipboard history). Content that came from the phone is not sent back.
//! Password managers' copies (`x-kde-passwordManagerHint`) are never sent.

use crate::core::{Core, Shared};
use crate::identity::unix_ms;
use crate::packet::Packet;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Larger texts are not synced.
const MAX_TEXT: usize = 512 * 1024;

#[derive(Default)]
pub struct ClipState {
    /// The PC clipboard text as last seen, and when it changed (Unix ms).
    pub current: String,
    pub changed_ms: i64,
    /// Text we set from the phone (do not echo it back).
    from_phone: String,
}

pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    if !core.cfg.phone_clipboard_sync {
        return;
    }
    let Some(text) = p.str("content").filter(|t| !t.is_empty() && t.len() <= MAX_TEXT) else { return };
    if p.kind == "kdeconnect.clipboard.connect" {
        // Only take the phone's clipboard when it is newer than ours.
        let ts = p.int("timestamp").unwrap_or(0);
        if ts == 0 || ts <= core.plugins.clip.changed_ms {
            return;
        }
    }
    if text == core.plugins.clip.current {
        return;
    }
    let st = &mut core.plugins.clip;
    st.from_phone = text.to_string();
    st.current = text.to_string();
    st.changed_ms = unix_ms();
    let _ = id;
    set_pc_clipboard(text);
}

/// Text shared from the phone: onto the PC clipboard (not echoed back).
pub fn handle_shared_text(core: &mut Core, text: &str) {
    let st = &mut core.plugins.clip;
    st.from_phone = text.to_string();
    st.current = text.to_string();
    st.changed_ms = unix_ms();
    set_pc_clipboard(text);
}

fn set_pc_clipboard(text: &str) {
    let text = text.to_string();
    std::thread::spawn(move || {
        if let Ok(mut c) = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(text.as_bytes());
            }
            let _ = c.wait();
        }
    });
}

/// The PC clipboard changed to `text`: send it to every connected paired device.
fn pc_changed(shared: &Shared, text: String) {
    let mut core = shared.lock().unwrap();
    if text.is_empty() || text.len() > MAX_TEXT || text == core.plugins.clip.current {
        return;
    }
    let echo = text == core.plugins.clip.from_phone;
    core.plugins.clip.current = text.clone();
    core.plugins.clip.changed_ms = unix_ms();
    if echo || !core.cfg.phone_clipboard_sync {
        return;
    }
    let ids: Vec<String> = core.devices.values().filter(|d| d.view.paired && d.link.is_some() && d.view.supports("kdeconnect.clipboard")).map(|d| d.view.id.clone()).collect();
    for id in ids {
        core.send(&id, Packet::new("kdeconnect.clipboard", json!({ "content": text })));
    }
}

/// A device connected: offer our clipboard; it takes it when newer than its own.
pub fn on_connected(core: &mut Core, id: &str) {
    let st = &core.plugins.clip;
    if core.cfg.phone_clipboard_sync && !st.current.is_empty() {
        let p = Packet::new("kdeconnect.clipboard.connect", json!({ "content": st.current, "timestamp": st.changed_ms }));
        core.send(id, p);
    }
}

/// Send `text` (or the current PC clipboard) to the device now.
pub fn send_now(core: &mut Core, id: &str, text: &str) -> Result<Option<String>, String> {
    let text = if text.is_empty() { core.plugins.clip.current.clone() } else { text.to_string() };
    if text.is_empty() {
        return Err("the clipboard is empty".into());
    }
    core.send(id, Packet::new("kdeconnect.clipboard", json!({ "content": text })));
    Ok(Some("clipboard sent".into()))
}

/// Watch the PC clipboard. Each change arrives as one base64 line (empty for secrets).
pub fn start_watcher(shared: Shared) {
    std::thread::spawn(move || loop {
        let script = "if wl-paste -l 2>/dev/null | grep -q x-kde-passwordManagerHint; then cat >/dev/null; echo; else base64 -w0; echo; fi";
        let child = Command::new("wl-paste")
            .args(["--no-newline", "--type", "text", "--watch", "sh", "-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            eprintln!("[prism-phoned] wl-paste not available; PC clipboard is not synced");
            return;
        };
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(text) = decode_b64(line.trim()).and_then(|b| String::from_utf8(b).ok()) {
                    pc_changed(&shared, text);
                }
            }
        }
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_secs(5));
    });
}

fn decode_b64(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            acc |= val(*c)? << (18 - 6 * i);
        }
        let n = chunk.len() * 6 / 8;
        out.extend_from_slice(&acc.to_be_bytes()[1..1 + n]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_base64() {
        assert_eq!(super::decode_b64("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(super::decode_b64("w6k=").unwrap(), "é".as_bytes());
        assert_eq!(super::decode_b64(""), None);
    }
}
