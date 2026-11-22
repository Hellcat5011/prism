//! Phone notifications (`kdeconnect.notification`): kept in the device view for the Phone card
//! and posted as desktop notifications (prismd shows them as toasts and in the notification
//! centre). Actions, replies and dismissals go back to the phone.

use crate::core::{Core, Shared};
use crate::notify::{self, Note};
use crate::packet::Packet;
use prism_common::phone::{phone_runtime_dir, PhoneNotification};
use serde_json::{json, Value};

const MAX_KEPT: usize = 50;

pub fn on_connected(core: &mut Core, id: &str) {
    if core.cfg.phone_notifications {
        core.send(id, Packet::new("kdeconnect.notification.request", json!({ "request": true })));
    }
}

pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    let shared = core.shared();
    if !core.cfg.phone_notifications {
        return;
    }
    let Some(nid) = p.str("id").map(String::from) else { return };
    let key = format!("notif:{id}:{nid}");
    let Some(dev) = core.devices.get_mut(id) else { return };
    if p.bool("isCancel") {
        dev.view.notifications.retain(|n| n.id != nid);
        notify::close(&key);
        core.publish();
        return;
    }
    let app = p.str("appName").unwrap_or("Phone").to_string();
    let title = p.str("title").map(String::from).unwrap_or_else(|| p.str("ticker").unwrap_or_default().to_string());
    let text = p.str("text").unwrap_or_default().to_string();
    let actions: Vec<String> = p.body.get("actions").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let icon_path = p.str("payloadHash").map(icon_file).filter(|f| std::path::Path::new(f).exists());
    let n = PhoneNotification {
        id: nid.clone(),
        app: app.clone(),
        title: title.clone(),
        text: text.clone(),
        time_ms: p.int("time").unwrap_or_else(crate::identity::unix_ms),
        dismissable: p.body.get("isClearable").and_then(Value::as_bool).unwrap_or(true),
        reply_id: p.str("requestReplyId").unwrap_or_default().to_string(),
        actions: actions.clone(),
        icon_path: icon_path.clone(),
    };
    let existed = dev.view.notifications.iter().any(|x| x.id == nid);
    dev.view.notifications.retain(|x| x.id != nid);
    dev.view.notifications.insert(0, n.clone());
    dev.view.notifications.truncate(MAX_KEPT);
    let phone = dev.view.name.clone();

    // Icon as a payload, once per hash.
    if icon_path.is_none() {
        if let (Some(port), Some(hash), Some(shared)) = (p.payload_port(), p.str("payloadHash"), shared.as_ref()) {
            fetch_icon(shared, id, &nid, hash, port, p.payload_size.unwrap_or(-1));
        }
    }
    // Popups: not for silent updates or ones the phone says to show once and already did.
    let silent = p.bool("silent") || (existed && p.bool("onlyOnce"));
    if core.cfg.phone_notification_popups && !silent {
        post(&key, &phone, &n);
    }
    core.publish();
}

/// The standard action key for a reply typed into the notification itself.
pub const INLINE_REPLY: &str = "inline-reply";

fn post(key: &str, phone: &str, n: &PhoneNotification) {
    let mut actions: Vec<(String, String)> = n.actions.iter().map(|a| (format!("a:{a}"), a.clone())).collect();
    if !n.reply_id.is_empty() {
        // Prism's notifications answer this one with a reply field in the popup
        // (NotificationReplied); the text goes back to the phone.
        actions.insert(0, (INLINE_REPLY.into(), "Reply".into()));
    }
    notify::show(Note {
        key: key.to_string(),
        app: format!("{} · {phone}", n.app),
        summary: if n.title.is_empty() { n.app.clone() } else { n.title.clone() },
        body: n.text.clone(),
        actions,
        icon: n.icon_path.clone().unwrap_or_else(|| "phone".into()),
        urgency: 1,
    });
}

fn icon_file(hash: &str) -> String {
    let safe: String = hash.chars().filter(|c| c.is_ascii_alphanumeric()).take(64).collect();
    phone_runtime_dir().join(format!("icon-{safe}.png")).to_string_lossy().into_owned()
}

fn fetch_icon(shared: &Shared, id: &str, nid: &str, hash: &str, port: u16, size: i64) {
    if size > 2 * 1024 * 1024 {
        return;
    }
    let (shared, id, nid, path) = (shared.clone(), id.to_string(), nid.to_string(), icon_file(hash));
    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(phone_runtime_dir());
        let tmp = format!("{path}.part");
        let Ok(mut f) = std::fs::File::create(&tmp) else { return };
        if crate::payload::download(&shared, &id, port, size, &mut f, |_| true).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
            let mut core = shared.lock().unwrap();
            if let Some(n) = core.devices.get_mut(&id).and_then(|d| d.view.notifications.iter_mut().find(|n| n.id == nid)) {
                n.icon_path = Some(path);
            }
            core.publish();
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    });
}

pub fn action(core: &mut Core, id: &str, nid: &str, action: &str) -> Result<Option<String>, String> {
    core.send(id, Packet::new("kdeconnect.notification.action", json!({ "key": nid, "action": action })));
    Ok(None)
}

pub fn reply(core: &mut Core, id: &str, reply_id: &str, text: &str) -> Result<Option<String>, String> {
    if reply_id.is_empty() || text.trim().is_empty() {
        return Err("nothing to reply to".into());
    }
    core.send(id, Packet::new("kdeconnect.notification.reply", json!({ "requestReplyId": reply_id, "message": text })));
    Ok(Some("reply sent".into()))
}

pub fn dismiss(core: &mut Core, id: &str, nid: &str) -> Result<Option<String>, String> {
    core.send(id, Packet::new("kdeconnect.notification.request", json!({ "cancel": nid })));
    if let Some(d) = core.devices.get_mut(id) {
        d.view.notifications.retain(|n| n.id != nid);
    }
    notify::close(&format!("notif:{id}:{nid}"));
    core.publish();
    Ok(None)
}

/// A desktop action on a phone notification (`notif:<device>:<id>`).
pub fn desktop_action(core: &mut Core, key: &str, action: &str) {
    let Some((dev, nid)) = key.strip_prefix("notif:").and_then(|r| r.split_once(':')) else { return };
    if action == "reply" || action == INLINE_REPLY {
        // A notification server without inline replies sends the action itself: reply in
        // the Phone card then.
        open_card_for_reply();
        return;
    }
    if let Some(a) = action.strip_prefix("a:") {
        let _ = self::action(core, dev, nid, a);
    }
}

/// Text typed into the reply field of one of our phone notifications: send it to the phone.
pub fn desktop_reply(core: &mut Core, key: &str, text: &str) {
    let Some((dev, nid)) = key.strip_prefix("notif:").and_then(|r| r.split_once(':')) else { return };
    let reply_id = core.devices.get(dev).and_then(|d| d.view.notifications.iter().find(|n| n.id == nid)).map(|n| n.reply_id.clone());
    match reply_id {
        Some(r) if !r.is_empty() => {
            if let Err(e) = reply(core, dev, &r, text) {
                eprintln!("[prism-phoned] reply: {e}");
            }
        }
        _ => eprintln!("[prism-phoned] reply: that notification is gone from the phone"),
    }
}

pub fn desktop_closed(core: &mut Core, key: &str) {
    let Some((dev, nid)) = key.strip_prefix("notif:").and_then(|r| r.split_once(':')) else { return };
    let dismissable = core.devices.get(dev).and_then(|d| d.view.notifications.iter().find(|n| n.id == nid)).is_some_and(|n| n.dismissable);
    if dismissable {
        let _ = dismiss(core, dev, nid);
    }
}

fn open_card_for_reply() {
    std::thread::spawn(|| {
        use std::io::Write;
        if let Ok(mut s) = std::os::unix::net::UnixStream::connect(prism_common::get_socket_path()) {
            let cmd = prism_common::IpcCommand::PhoneCard("open".into());
            let _ = writeln!(s, "{}", serde_json::to_string(&cmd).unwrap_or_default());
        }
    });
}
