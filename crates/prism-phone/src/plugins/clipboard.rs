//! Clipboard sync (`kdeconnect.clipboard`, `kdeconnect.clipboard.connect`). PC copies are
//! watched with `wl-paste --watch`; text from the phone is set with `wl-copy` (so it also lands
//! in Prism's clipboard history). Content that came from the phone is not sent back.
//! Password managers' copies (`x-kde-passwordManagerHint`) are never sent.

use crate::core::{Core, Shared};
use crate::identity::unix_ms;
use crate::packet::Packet;
use serde_json::json;

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
    prism_common::wlclip::copy_text(text, true);
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

/// Watch the PC clipboard (text only; password-manager secrets are never read).
pub fn start_watcher(shared: Shared) {
    let ok = prism_common::wlclip::watch(move |sel| {
        if let prism_common::wlclip::Content::Text(t) = &sel.content {
            pc_changed(&shared, t.clone());
        }
    });
    if !ok {
        eprintln!("[prism-phoned] no clipboard access; PC clipboard is not synced");
    }
}
