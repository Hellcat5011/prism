//! Desktop notifications from prism-phoned (pair requests, received files, phone
//! notifications) through org.freedesktop.Notifications, which prismd serves. Actions and
//! closes come back as signals and are routed to `plugins::on_desktop_action` /
//! `plugins::on_desktop_closed` by the key the notification was posted with.

use crate::core::Shared;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use zbus::blocking::Connection;
use zbus::zvariant::Value;

pub struct Note {
    /// Our key ("pair:<device>", "notif:<device>:<id>", "file:<path>").
    pub key: String,
    pub app: String,
    pub summary: String,
    pub body: String,
    /// (action id, label)
    pub actions: Vec<(String, String)>,
    pub icon: String,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
}

enum Msg {
    Show(Note),
    Close(String),
}

static TX: OnceLock<Mutex<Sender<Msg>>> = OnceLock::new();

pub fn show(n: Note) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(Msg::Show(n));
    }
}

pub fn close(key: &str) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap().send(Msg::Close(key.to_string()));
    }
}

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

pub fn start(shared: Shared) {
    let (tx, rx) = channel::<Msg>();
    let _ = TX.set(Mutex::new(tx));
    let ids: std::sync::Arc<Mutex<HashMap<u32, String>>> = Default::default();
    // Sender thread.
    let ids_s = ids.clone();
    std::thread::spawn(move || {
        let Ok(conn) = Connection::session() else { return };
        let mut by_key: HashMap<String, u32> = HashMap::new();
        for msg in rx {
            match msg {
                Msg::Show(n) => {
                    let replaces = by_key.get(&n.key).copied().unwrap_or(0);
                    let actions: Vec<&str> = n.actions.iter().flat_map(|(a, l)| [a.as_str(), l.as_str()]).collect();
                    let mut hints: HashMap<&str, Value> = HashMap::new();
                    hints.insert("urgency", Value::U8(n.urgency));
                    hints.insert("desktop-entry", Value::from("prism-phone"));
                    let placeholder = format!("Reply to {}", n.summary);
                    if n.actions.iter().any(|(a, _)| a == crate::plugins::notifications::INLINE_REPLY) {
                        hints.insert("x-kde-reply-placeholder-text", Value::from(placeholder.as_str()));
                    }
                    let r = conn.call_method(Some(DEST), PATH, Some(DEST), "Notify", &(n.app.as_str(), replaces, n.icon.as_str(), n.summary.as_str(), n.body.as_str(), actions, hints, -1i32));
                    if let Ok(id) = r.and_then(|m| m.body().deserialize::<u32>()) {
                        by_key.insert(n.key.clone(), id);
                        ids_s.lock().unwrap().insert(id, n.key);
                    }
                }
                Msg::Close(key) => {
                    if let Some(id) = by_key.remove(&key) {
                        ids_s.lock().unwrap().remove(&id);
                        let _ = conn.call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(id,));
                    }
                }
            }
        }
    });
    // Signal thread.
    std::thread::spawn(move || {
        let Ok(conn) = Connection::session() else { return };
        let Ok(proxy) = zbus::blocking::Proxy::new(&conn, DEST, PATH, DEST) else { return };
        let Ok(signals) = proxy.receive_all_signals() else { return };
        for m in signals {
            let header = m.header();
            let member = header.member().map(|m| m.to_string()).unwrap_or_default();
            match member.as_str() {
                "ActionInvoked" => {
                    if let Ok((id, action)) = m.body().deserialize::<(u32, String)>() {
                        let key = ids.lock().unwrap().get(&id).cloned();
                        if let Some(key) = key {
                            crate::plugins::on_desktop_action(&shared, &key, &action);
                        }
                    }
                }
                "NotificationReplied" => {
                    if let Ok((id, text)) = m.body().deserialize::<(u32, String)>() {
                        let key = ids.lock().unwrap().get(&id).cloned();
                        if let Some(key) = key {
                            crate::plugins::on_desktop_reply(&shared, &key, &text);
                        }
                    }
                }
                "NotificationClosed" => {
                    if let Ok((id, reason)) = m.body().deserialize::<(u32, u32)>() {
                        let key = ids.lock().unwrap().remove(&id);
                        // 2 = dismissed by the user.
                        if let (Some(key), 2) = (key, reason) {
                            crate::plugins::on_desktop_closed(&shared, &key);
                        }
                    }
                }
                _ => {}
            }
        }
    });
}
