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
    pub icon: Option<String>,
    pub urgency: u8,
    pub timeout_ms: u32,
    pub timestamp: u64,
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
        let _ = fs::create_dir_all(&cache_dir);

        let path = cache_dir.join("history.json");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }

    pub fn add(&mut self, mut notif: Notification) -> u32 {
        if notif.id == 0 {
            notif.id = self.next_id;
            self.next_id += 1;
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
