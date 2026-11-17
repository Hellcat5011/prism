//! Paired devices, one JSON file each in `<data dir>/trusted/<deviceId>.json`, holding the
//! certificate pinned at pairing time.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Trusted {
    pub id: String,
    pub name: String,
    pub device_type: String,
    /// The peer's certificate (DER, hex).
    pub cert: String,
    /// Last address the device was reached at (unicast identity on start).
    #[serde(default)]
    pub last_addr: Option<String>,
}

impl Trusted {
    pub fn cert_der(&self) -> Vec<u8> {
        (0..self.cert.len() / 2).filter_map(|i| u8::from_str_radix(&self.cert[2 * i..2 * i + 2], 16).ok()).collect()
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn dir(base: &Path) -> PathBuf {
    base.join("trusted")
}

pub fn load_all(base: &Path) -> Vec<Trusted> {
    let Ok(entries) = fs::read_dir(dir(base)) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<Trusted>(&s).ok())
        .filter(|t| crate::identity::valid_device_id(&t.id))
        .collect()
}

pub fn save(base: &Path, t: &Trusted) {
    let d = dir(base);
    let _ = fs::create_dir_all(&d);
    if let Ok(s) = serde_json::to_string_pretty(t) {
        let tmp = d.join(format!(".{}.tmp", t.id));
        if fs::write(&tmp, s).is_ok() {
            let _ = fs::rename(&tmp, d.join(format!("{}.json", t.id)));
        }
    }
}

pub fn remove(base: &Path, id: &str) {
    let _ = fs::remove_file(dir(base).join(format!("{id}.json")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_remove() {
        let base = std::env::temp_dir().join(format!("prism-trust-test-{}", std::process::id()));
        let t = Trusted { id: "a".repeat(32), name: "Pixel".into(), device_type: "phone".into(), cert: hex(&[0, 1, 254, 255]), last_addr: None };
        save(&base, &t);
        assert_eq!(load_all(&base), vec![t.clone()]);
        assert_eq!(t.cert_der(), vec![0, 1, 254, 255]);
        remove(&base, &t.id);
        assert!(load_all(&base).is_empty());
        let _ = fs::remove_dir_all(&base);
    }
}
