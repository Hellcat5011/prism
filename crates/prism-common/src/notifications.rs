use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_STORED_NOTIFICATIONS: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    /// Themed icon name or image path (app_icon from the sender).
    pub icon: Option<String>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    pub timeout_ms: u32,
    pub timestamp: u64,
    /// (key, label) pairs; "default" is the activate-the-notification action.
    #[serde(default)]
    pub actions: Vec<(String, String)>,
    /// The sending app's desktop entry id (hint "desktop-entry").
    #[serde(default)]
    pub desktop_entry: String,
    /// Picture to show instead of the icon (hint "image-path", or "image-data" saved as PNG).
    #[serde(default)]
    pub image: Option<String>,
    /// Show `image` as a large preview inside the notification, not as its icon (Prism's
    /// private hint "x-prism-preview", sent by the screenshot notification).
    #[serde(default)]
    pub preview: bool,
    /// Placeholder text of the inline reply field (for notifications with an "inline-reply"
    /// action).
    #[serde(default)]
    pub reply_placeholder: String,
}

impl Notification {
    pub fn new(id: u32, app_name: String, summary: String, body: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            id,
            app_name,
            summary,
            body,
            icon: None,
            urgency: 1, // normal
            timeout_ms: 5000,
            timestamp,
            actions: Vec::new(),
            desktop_entry: String::new(),
            image: None,
            preview: false,
            reply_placeholder: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationStore {
    pub notifications: Vec<Notification>,
    next_id: u32,
}

impl NotificationStore {
    pub fn new() -> Self {
        Self {
            notifications: Vec::new(),
            next_id: 1,
        }
    }

    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let path = PathBuf::from(home).join(".cache/prism/notifications/history.json");

        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str::<NotificationStore>(&content) {
                    return store;
                }
            }
        }

        Self::new()
    }

    pub fn save(&self) {
        let home = std::env::var("HOME").unwrap_or_default();
        let cache_dir = PathBuf::from(home).join(".cache/prism/notifications");
        let _ = crate::clipboard::private_dir(&cache_dir);

        let path = cache_dir.join("history.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if crate::clipboard::write_private(&tmp, json.as_bytes()).is_ok() {
                let _ = fs::rename(tmp, path);
            }
        }
    }

    pub fn add(&mut self, mut notif: Notification) -> u32 {
        if notif.id == 0 {
            let max = self.notifications.iter().map(|n| n.id).max().unwrap_or(0);
            self.next_id = self.next_id.max(max + 1).max(1);
            notif.id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
        }

        let assigned_id = notif.id;

        // Insert at the front (newest first)
        self.notifications.insert(0, notif);

        // Keep at most MAX_STORED_NOTIFICATIONS
        if self.notifications.len() > MAX_STORED_NOTIFICATIONS {
            self.notifications.truncate(MAX_STORED_NOTIFICATIONS);
        }

        self.save();
        assigned_id
    }

    /// Store a notification from D-Bus: replaces `replaces_id` in place when it is still stored
    /// (keeping its id, moved to the top), otherwise assigns a new id. Returns the id.
    pub fn upsert(&mut self, replaces_id: u32, mut notif: Notification) -> u32 {
        if replaces_id != 0 {
            if let Some(pos) = self.notifications.iter().position(|n| n.id == replaces_id) {
                notif.id = replaces_id;
                self.notifications.remove(pos);
                self.notifications.insert(0, notif);
                self.save();
                return replaces_id;
            }
        }
        notif.id = 0;
        self.add(notif)
    }

    /// An id for a notification that is shown but not kept (the "transient" hint).
    pub fn reserve_id(&mut self) -> u32 {
        let max = self.notifications.iter().map(|n| n.id).max().unwrap_or(0);
        self.next_id = self.next_id.max(max + 1).max(1);
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.notifications.iter().find(|n| n.id == id)
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let before_len = self.notifications.len();
        self.notifications.retain(|n| n.id != id);
        let removed = self.notifications.len() < before_len;
        if removed {
            self.save();
        }
        removed
    }

    pub fn clear(&mut self) {
        self.notifications.clear();
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_store_add_and_remove() {
        let mut store = NotificationStore::new();
        let n1 = Notification::new(0, "test".to_string(), "Hello".to_string(), "World".to_string());
        let id1 = store.add(n1);
        assert_eq!(id1, 1);
        assert_eq!(store.notifications.len(), 1);
        assert_eq!(store.notifications[0].summary, "Hello");

        let n2 = Notification::new(0, "test".to_string(), "Second".to_string(), "Body".to_string());
        let id2 = store.add(n2);
        assert_eq!(id2, 2);
        assert_eq!(store.notifications.len(), 2);
        assert_eq!(store.notifications[0].summary, "Second"); // newest first

        assert!(store.remove(id1));
        assert_eq!(store.notifications.len(), 1);
        assert_eq!(store.notifications[0].id, id2);

        store.clear();
        assert!(store.notifications.is_empty());
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn old_history_files_still_load() {
        let old = r#"{"notifications":[{"id":3,"app_name":"Firefox","summary":"s","body":"b","icon":null,"urgency":1,"timeout_ms":5000,"timestamp":1}],"next_id":4}"#;
        let store: NotificationStore = serde_json::from_str(old).expect("old format must parse");
        assert_eq!(store.notifications.len(), 1);
    }
}
