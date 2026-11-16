use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ClipboardItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // "text" or "image"
    pub content: String,
    pub timestamp: u64,
    #[serde(rename = "isImage", default)]
    pub is_image: bool,
    #[serde(rename = "imagePath", default)]
    pub image_path: Option<String>,
    #[serde(rename = "lineCount", default)]
    pub line_count: usize,
    #[serde(rename = "charCount", default)]
    pub char_count: usize,
}

impl ClipboardItem {
    pub fn new_text(content: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let id = format!("{}-{}", timestamp, content.len());
        let line_count = content.lines().count().max(1);
        let char_count = content.chars().count();
        Self {
            id,
            kind: "text".to_string(),
            content,
            timestamp,
            is_image: false,
            image_path: None,
            line_count,
            char_count,
        }
    }

    pub fn new_image(image_path: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let id = format!("{}-img", timestamp);
        Self {
            id,
            kind: "image".to_string(),
            content: "[Image]".to_string(),
            timestamp,
            is_image: true,
            image_path: Some(image_path),
            line_count: 1,
            char_count: 7,
        }
    }

    /// Comprehensive security heuristics checking if text appears to be credentials or sensitive keys.
    pub fn is_sensitive(text: &str) -> bool {
        let t = text.trim();
        if t.is_empty() {
            return false;
        }

        // 1. SSH / PGP / OpenSSH private keys
        if t.contains("PRIVATE KEY-----") || t.contains("BEGIN OPENSSH PRIVATE KEY") || t.contains("BEGIN PGP PRIVATE KEY") {
            return true;
        }

        // 2. Bearer tokens & JWTs
        if t.starts_with("Bearer ") || t.contains("Bearer eyJ") {
            return true;
        }
        if is_jwt(t) {
            return true;
        }

        // 3. Password / Secret assignment patterns (key=value, key: value)
        let lower = t.to_lowercase();
        let sensitive_keys = [
            "password", "passwd", "pass", "pwd", "secret", "token", "api_key",
            "apikey", "api_secret", "auth_token", "access_token", "refresh_token",
        ];
        for k in sensitive_keys {
            if let Some(pos) = lower.find(k) {
                let rest = &lower[pos + k.len()..];
                let trimmed_rest = rest.trim_start();
                if trimmed_rest.starts_with('=') || trimmed_rest.starts_with(':') {
                    // Check if there is a non-empty value following
                    let val = trimmed_rest[1..].trim();
                    if !val.is_empty() {
                        return true;
                    }
                }
            }
        }

        // 4. Well-known API key prefixes
        // AWS access key ID: AKIA... (20 chars)
        if lower.contains("akia") {
            for word in t.split_whitespace() {
                if word.starts_with("AKIA") && word.len() == 20 && word.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return true;
                }
            }
        }
        // GitHub PATs: ghp_, gho_, ghs_, ghr_
        if t.contains("ghp_") || t.contains("gho_") || t.contains("ghs_") || t.contains("ghr_") {
            return true;
        }
        // GitLab PATs: glpat-
        if t.contains("glpat-") {
            return true;
        }
        // Slack tokens: xoxb-, xoxp-, xoxa-, xoxs-
        if t.contains("xoxb-") || t.contains("xoxp-") || t.contains("xoxa-") || t.contains("xoxs-") {
            return true;
        }
        // Stripe keys: sk_live_, pk_live_
        if t.contains("sk_live_") || t.contains("pk_live_") {
            return true;
        }
        // OpenAI keys: sk-
        if t.contains("sk-proj-") || t.contains("sk-None-") || (t.starts_with("sk-") && t.len() >= 24) {
            return true;
        }

        // 5. Database connection strings
        let db_schemes = [
            "postgres://", "postgresql://", "mysql://", "mariadb://",
            "mongodb://", "mongodb+srv://", "redis://", "rediss://",
            "amqp://", "amqps://",
        ];
        for scheme in db_schemes {
            if lower.contains(scheme) {
                return true;
            }
        }

        // 6. Generated-password-shaped tokens.
        if looks_like_password(t) {
            return true;
        }

        // 7. Credit card numbers (13 to 19 digits with optional spaces or dashes, validated with Luhn algorithm)
        if contains_valid_card_number(t) {
            return true;
        }

        false
    }
}

/// Helper detecting JWT tokens: three dot-separated base64url strings with length >= 10.
fn is_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() == 3 {
        let (h, p, sig) = (parts[0], parts[1], parts[2]);
        if h.starts_with("eyJ") && h.len() >= 10 && p.len() >= 10 && sig.len() >= 10 {
            return true;
        }
    }
    false
}

/// Helper to detect if a string contains a 13-19 digit credit card number passing the Luhn check.
fn contains_valid_card_number(s: &str) -> bool {
    // Direct stripped check first
    let stripped: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if (13..=19).contains(&stripped.len()) && luhn_check(&stripped) {
        return true;
    }

    // Word-by-word scan for card patterns (e.g. 4532 1234 5678 9010)
    for word in s.split(|c: char| c.is_whitespace() || c == ',') {
        let digits: String = word.chars().filter(|c| c.is_ascii_digit()).collect();
        if (13..=19).contains(&digits.len()) && luhn_check(&digits) {
            return true;
        }
    }

    false
}

/// Standard Luhn algorithm implementation.
pub fn luhn_check(digits_str: &str) -> bool {
    let digits: Vec<u32> = digits_str.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let mut sum = 0;
    for (i, &d) in digits.iter().rev().enumerate() {
        let mut val = d;
        if i % 2 == 1 {
            val *= 2;
            if val > 9 {
                val -= 9;
            }
        }
        sum += val;
    }
    sum % 10 == 0
}

/// Self-copy lockfile path helper
pub fn get_self_copy_lock_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("prism-clipboard-self.lock")
    } else {
        std::env::temp_dir().join("prism-clipboard-self.lock")
    }
}

/// Creates the lockfile before `wl-copy` so the watcher skips self-initiated copies.
pub fn touch_self_copy_lock() {
    let path = get_self_copy_lock_path();
    let _ = fs::File::create(&path);
    // Also touch legacy quickshell lockfile for backward compatibility
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let _ = fs::File::create(PathBuf::from(runtime_dir).join("qs-clipboard-self.lock"));
    }
}

/// Checks and consumes the lockfile if it exists. Returns true if self-copy was active.
pub fn consume_self_copy_lock() -> bool {
    let path = get_self_copy_lock_path();
    let mut locked = false;
    if path.exists() {
        let _ = fs::remove_file(&path);
        locked = true;
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let legacy = PathBuf::from(runtime_dir).join("qs-clipboard-self.lock");
        if legacy.exists() {
            let _ = fs::remove_file(legacy);
            locked = true;
        }
    }
    locked
}

/// Directory for storing captured PNG image clipboard thumbnails.
pub fn get_image_cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache/prism/clipboard/images")
    } else {
        std::env::temp_dir().join("prism/clipboard/images")
    }
}

pub struct ClipboardStore {
    path: PathBuf,
    pub items: Vec<ClipboardItem>,
    pub max_items: usize,
}

impl ClipboardStore {
    pub fn default_path() -> PathBuf {
        match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home).join(".cache/prism/clipboard/history.json"),
            Err(_) => std::env::temp_dir().join("prism-clipboard.json"),
        }
    }

    /// Move the QML shell's history (~/.cache/quickshell-clipboard) to the Prism path once.
    fn migrate_legacy(path: &std::path::Path) {
        let Ok(home) = std::env::var("HOME") else { return };
        let legacy = PathBuf::from(home).join(".cache/quickshell-clipboard/history.json");
        if path.exists() || !legacy.exists() {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = private_dir(dir);
        }
        if fs::rename(&legacy, path).is_ok() {
            let _ = set_mode(path, 0o600);
        }
    }

    pub fn load() -> Self {
        let path = Self::default_path();
        Self::migrate_legacy(&path);
        let mut items = Vec::new();

        if path.exists() {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(parsed) = serde_json::from_str::<Vec<ClipboardItem>>(&data) {
                    items = parsed;
                }
            }
        }

        Self {
            path,
            items,
            max_items: 50,
        }
    }

    pub fn add_text(&mut self, text: String) -> bool {
        if text.trim().is_empty() || ClipboardItem::is_sensitive(&text) {
            return false;
        }

        // Deduplicate against latest item
        if let Some(first) = self.items.first() {
            if !first.is_image && first.content == text {
                return false;
            }
        }

        self.items.retain(|item| item.is_image || item.content != text);
        self.items.insert(0, ClipboardItem::new_text(text));

        if self.items.len() > self.max_items {
            self.items.truncate(self.max_items);
        }

        let _ = self.save();
        true
    }

    pub fn add_image(&mut self, image_bytes: &[u8]) -> bool {
        if image_bytes.is_empty() {
            return false;
        }

        let img_dir = get_image_cache_dir();
        if let Err(e) = private_dir(&img_dir) {
            eprintln!("[clipboard] Failed to create image cache directory: {}", e);
            return false;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let filename = format!("{}.png", timestamp);
        let file_path = img_dir.join(filename);

        if let Err(e) = write_private(&file_path, image_bytes) {
            eprintln!("[clipboard] Failed to write image thumbnail: {}", e);
            return false;
        }

        let path_str = format!("file://{}", file_path.display());
        self.items.insert(0, ClipboardItem::new_image(path_str));

        if self.items.len() > self.max_items {
            // Check if any removed items were images and remove the orphaned files
            let removed = self.items.split_off(self.max_items);
            for it in removed {
                if it.is_image {
                    if let Some(p) = it.image_path {
                        let local_path = p.replace("file://", "");
                        let _ = fs::remove_file(local_path);
                    }
                }
            }
        }

        let _ = self.save();
        true
    }

    pub fn clear(&mut self) {
        // Remove cached image files
        for it in &self.items {
            if it.is_image {
                if let Some(p) = &it.image_path {
                    let local_path = p.replace("file://", "");
                    let _ = fs::remove_file(local_path);
                }
            }
        }
        self.items.clear();
        let _ = self.save();
    }

    pub fn prune_older_than(&mut self, max_age_secs: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut kept = Vec::new();
        for item in self.items.drain(..) {
            if now.saturating_sub(item.timestamp) <= max_age_secs {
                kept.push(item);
            } else if item.is_image {
                if let Some(p) = item.image_path {
                    let local_path = p.replace("file://", "");
                    let _ = fs::remove_file(local_path);
                }
            }
        }
        self.items = kept;
        let _ = self.save();
    }

    /// Write the history readable by the owner only (atomically, via a temp file).
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            private_dir(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.items)?;
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, data.as_bytes())?;
        fs::rename(&tmp, &self.path)
    }

    /// Drop entries that the sensitivity filter would reject today (filters improve over time).
    /// Returns how many were removed.
    pub fn purge_sensitive(&mut self) -> usize {
        let before = self.items.len();
        self.items.retain(|i| i.is_image || !ClipboardItem::is_sensitive(&i.content));
        let removed = before - self.items.len();
        if removed > 0 {
            let _ = self.save();
        }
        removed
    }
}

fn set_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Create `dir` (and parents) and make it owner-only.
pub fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)
}

/// Write a file created with mode 0600.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    set_mode(path, 0o600)
}

/// True when the current Wayland selection is marked secret by a password manager
/// (KeePassXC, Bitwarden, ... offer the `x-kde-passwordManagerHint` MIME type).
pub fn selection_is_secret() -> bool {
    std::process::Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|t| t.trim() == "x-kde-passwordManagerHint"))
        .unwrap_or(false)
}

/// A lone token that looks like a generated password: 8-64 characters, no whitespace, upper and
/// lower case letters, digits and symbols, and not a URL or path.
fn looks_like_password(t: &str) -> bool {
    let n = t.chars().count();
    if !(8..=64).contains(&n) || t.chars().any(char::is_whitespace) {
        return false;
    }
    if t.contains("://") || t.starts_with('/') || t.starts_with("~/") || t.starts_with("./") {
        return false;
    }
    let upper = t.chars().any(|c| c.is_uppercase());
    let lower = t.chars().any(|c| c.is_lowercase());
    let digit = t.chars().any(|c| c.is_ascii_digit());
    let symbol = t.chars().any(|c| !c.is_alphanumeric());
    upper && lower && digit && symbol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sensitive_filtering() {
        assert!(ClipboardItem::is_sensitive("-----BEGIN RSA PRIVATE KEY-----\nMIIE..."));
        assert!(ClipboardItem::is_sensitive("Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."));
        assert!(ClipboardItem::is_sensitive("export api_key=abc123secret"));
        assert!(ClipboardItem::is_sensitive("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
        assert!(ClipboardItem::is_sensitive("postgres://user:secret@localhost:5432/db"));
        assert!(!ClipboardItem::is_sensitive("hello world this is regular text"));
        assert!(!ClipboardItem::is_sensitive("https://github.com/vic/prism"));
        assert!(ClipboardItem::is_sensitive("Xq7#mP2v!Lr9"));
        assert!(!ClipboardItem::is_sensitive("hello-world"));
        assert!(!ClipboardItem::is_sensitive("README.md"));
        assert!(!ClipboardItem::is_sensitive("/home/u/Some-Dir/File2.txt"));
        assert!(!ClipboardItem::is_sensitive("cargo build --release"));
    }

    #[test]
    fn test_luhn_algorithm() {
        // Valid Visa test card number (16 digits)
        assert!(luhn_check("4532015112830366"));
        // Invalid card number
        assert!(!luhn_check("4532015112830367"));
    }

    #[test]
    fn test_clipboard_deduplication() {
        let mut store = ClipboardStore {
            path: PathBuf::from("/tmp/test_clipboard.json"),
            items: Vec::new(),
            max_items: 10,
        };

        assert!(store.add_text("first entry".to_string()));
        assert!(!store.add_text("first entry".to_string())); // duplicate ignored
        assert!(store.add_text("second entry".to_string()));
        assert_eq!(store.items.len(), 2);
        assert_eq!(store.items[0].content, "second entry");
    }
}
