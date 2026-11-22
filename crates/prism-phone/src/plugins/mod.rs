//! KDE Connect plugins: which packet types we take and send, and their handlers.

pub mod clipboard;
pub mod media;
pub mod mousepad;
pub mod mpris;
pub mod notifications;
pub mod share;

use crate::core::{Core, Shared};
use crate::notify::{self, Note};
use crate::packet::Packet;
use prism_common::phone::{PhoneBattery, PhoneCommand, PhoneNetwork, PhoneReply};
use serde_json::json;

/// Packet types we accept (the phone shows features for these).
pub const INCOMING: &[&str] = &[
    "kdeconnect.ping",
    "kdeconnect.battery",
    "kdeconnect.connectivity_report",
    "kdeconnect.mousepad.request",
    "kdeconnect.presenter",
    "kdeconnect.clipboard",
    "kdeconnect.clipboard.connect",
    "kdeconnect.mpris",
    "kdeconnect.mpris.request",
    "kdeconnect.notification",
    "kdeconnect.share.request",
    "kdeconnect.share.request.update",
];

/// Packet types we send (the phone enables its matching receivers).
pub const OUTGOING: &[&str] = &[
    "kdeconnect.ping",
    "kdeconnect.findmyphone.request",
    "kdeconnect.mousepad.keyboardstate",
    "kdeconnect.mousepad.echo",
    "kdeconnect.clipboard",
    "kdeconnect.clipboard.connect",
    "kdeconnect.mpris",
    "kdeconnect.mpris.request",
    "kdeconnect.notification.request",
    "kdeconnect.notification.action",
    "kdeconnect.notification.reply",
    "kdeconnect.share.request",
];

/// Plugin state that is not part of the per-device view.
#[derive(Default)]
pub struct PluginState {
    pub input: mousepad::InputState,
    pub clip: clipboard::ClipState,
    pub share: share::ShareState,
    /// Queue of the worker that serves the PC's media players.
    pub mpris: Option<std::sync::mpsc::Sender<mpris::Msg>>,
}

/// A paired device just connected (or just got paired).
pub fn on_connected(core: &mut Core, id: &str) {
    mousepad::on_connected(core, id);
    clipboard::on_connected(core, id);
    media::on_connected(core, id);
    mpris::on_connected(core, id);
    notifications::on_connected(core, id);
}

pub fn on_disconnected(core: &mut Core, id: &str) {
    if let Some(d) = core.devices.get_mut(id) {
        d.view.network = None;
        d.view.media = None;
        d.view.notifications.clear();
    }
}

pub fn tick(_core: &mut Core) {}

pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    match p.kind.as_str() {
        "kdeconnect.mousepad.request" | "kdeconnect.presenter" => return mousepad::handle(core, id, p),
        "kdeconnect.clipboard" | "kdeconnect.clipboard.connect" => return clipboard::handle(core, id, p),
        "kdeconnect.mpris" => return media::handle(core, id, p),
        "kdeconnect.mpris.request" => return mpris::handle(core, id, p),
        "kdeconnect.notification" => return notifications::handle(core, id, p),
        "kdeconnect.share.request" | "kdeconnect.share.request.update" => return share::handle(core, id, p),
        _ => {}
    }
    let Some(dev) = core.devices.get_mut(id) else { return };
    match p.kind.as_str() {
        "kdeconnect.ping" => {
            let name = dev.view.name.clone();
            notify::show(Note { key: format!("ping:{id}"), app: name, summary: "Ping!".into(), body: p.str("message").unwrap_or_default().to_string(), actions: Vec::new(), icon: "phone".into(), urgency: 1 });
        }
        "kdeconnect.battery" => {
            let percent = p.int("currentCharge").unwrap_or(-1);
            if percent >= 0 {
                let low = p.int("thresholdEvent") == Some(1);
                dev.view.battery = Some(PhoneBattery { percent: percent.min(100) as i32, charging: p.bool("isCharging") });
                if low && !p.bool("isCharging") {
                    let name = dev.view.name.clone();
                    notify::show(Note { key: format!("battery:{id}"), app: name.clone(), summary: format!("{name}: battery low"), body: format!("{percent}% left"), actions: Vec::new(), icon: "battery-low".into(), urgency: 1 });
                }
                core.publish();
            }
        }
        "kdeconnect.connectivity_report" => {
            // {signalStrengths: {"<subscription id>": {networkType, signalStrength}}}; show the first.
            let first = p.body.get("signalStrengths").and_then(|s| s.as_object()).and_then(|o| o.values().next().cloned());
            dev.view.network = first.map(|v| PhoneNetwork {
                // The app says "Unknown" when it cannot tell (no Phone permission, no mobile
                // data): nothing to show then.
                kind: v.get("networkType").and_then(|t| t.as_str()).filter(|t| !t.eq_ignore_ascii_case("unknown")).unwrap_or_default().to_string(),
                strength: v.get("signalStrength").and_then(|t| t.as_i64()).unwrap_or(0).clamp(0, 4) as i32,
            });
            core.publish();
        }
        other => eprintln!("[prism-phoned] unhandled {other} from {id}"),
    }
}

/// Device commands that are not about the link or pairing. `id` is a resolved, connected,
/// paired device (empty for commands without one).
pub fn command(core: &mut Core, id: &str, cmd: PhoneCommand) -> Result<Option<String>, String> {
    match cmd {
        PhoneCommand::Ring { .. } => {
            core.send(id, Packet::new("kdeconnect.findmyphone.request", json!({})));
            Ok(None)
        }
        PhoneCommand::SendClipboard { text, .. } => clipboard::send_now(core, id, &text),
        PhoneCommand::Media { action, .. } => media::command(core, id, Some(action), None),
        PhoneCommand::Seek { position_ms, .. } => media::command(core, id, None, Some(position_ms)),
        PhoneCommand::NotificationAction { id: nid, action, .. } => notifications::action(core, id, &nid, &action),
        PhoneCommand::NotificationReply { reply_id, text, .. } => notifications::reply(core, id, &reply_id, &text),
        PhoneCommand::NotificationDismiss { id: nid, .. } => notifications::dismiss(core, id, &nid),
        PhoneCommand::SendText { text, .. } => share::send_text(core, id, Some(&text), None),
        PhoneCommand::SendUrl { url, .. } => share::send_text(core, id, None, Some(&url)),
        PhoneCommand::CancelTransfer { id: tid } => share::cancel(core, tid),
        other => Err(format!("not supported: {other:?}")),
    }
}

pub fn notify_pair_request(core: &mut Core, id: &str) {
    let d = &core.devices[id].view;
    notify::show(Note {
        key: format!("pair:{id}"),
        app: "Phone".into(),
        summary: format!("Pair with {}?", d.name),
        body: format!("Check that {} shows the key {}", d.name, d.verification_key.clone().unwrap_or_default()),
        actions: vec![("accept".into(), "Accept".into()), ("reject".into(), "Reject".into())],
        icon: "phone".into(),
        urgency: 2,
    });
}

/// The user picked an action on one of our desktop notifications.
pub fn on_desktop_action(shared: &Shared, key: &str, action: &str) {
    let mut core = shared.lock().unwrap();
    if let Some(dev) = key.strip_prefix("pair:") {
        let device = Some(dev.to_string());
        let cmd = if action == "accept" { PhoneCommand::Accept { device } } else { PhoneCommand::Reject { device } };
        if let PhoneReply::Error { message } = core.command(cmd) {
            eprintln!("[prism-phoned] {message}");
        }
    } else if key.starts_with("notif:") {
        notifications::desktop_action(&mut core, key, action);
    } else {
        share::desktop_action(key, action);
    }
}

/// The user typed a reply into one of our desktop notifications.
pub fn on_desktop_reply(shared: &Shared, key: &str, text: &str) {
    if key.starts_with("notif:") {
        notifications::desktop_reply(&mut shared.lock().unwrap(), key, text);
    }
}

/// The user dismissed one of our desktop notifications.
pub fn on_desktop_closed(shared: &Shared, key: &str) {
    if key.starts_with("notif:") {
        notifications::desktop_closed(&mut shared.lock().unwrap(), key);
    }
}
