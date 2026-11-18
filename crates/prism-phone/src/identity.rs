//! This computer's KDE Connect identity: a device id and a long-lived self-signed certificate,
//! made like kdeconnectd's (EC P-256, `CN=<deviceId>, O=KDE, OU=KDE Connect`, valid from a
//! year ago for ten years). Stored in `~/.local/share/prism/phone/` (key file mode 0600).

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct Identity {
    pub id: String,
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Identity { id: self.id.clone(), cert: self.cert.clone(), key: self.key.clone_key() }
    }
}

/// `~/.local/share/prism/phone` (or `$XDG_DATA_HOME/prism/phone`).
pub fn data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())).join(".local/share"));
    base.join("prism/phone")
}

/// KDE Connect v8 device ids: 32 to 38 characters of `[a-zA-Z0-9_]`.
pub fn valid_device_id(id: &str) -> bool {
    (32..=38).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn load_or_create(dir: &Path) -> Result<Identity, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (id_p, cert_p, key_p) = (dir.join("device_id"), dir.join("certificate.pem"), dir.join("private_key.pem"));
    if let (Ok(id), Ok(cert), Ok(key)) = (fs::read_to_string(&id_p), CertificateDer::from_pem_file(&cert_p), PrivateKeyDer::from_pem_file(&key_p)) {
        let id = id.trim().to_string();
        if valid_device_id(&id) {
            return Ok(Identity { id, cert, key });
        }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let (cert_pem, key_pem) = generate(&id)?;
    write_private(&key_p, key_pem.as_bytes())?;
    fs::write(&cert_p, &cert_pem).map_err(|e| format!("{}: {e}", cert_p.display()))?;
    fs::write(&id_p, &id).map_err(|e| format!("{}: {e}", id_p.display()))?;
    eprintln!("[prism-phoned] created identity {id}");
    Ok(Identity {
        id,
        cert: CertificateDer::from_pem_slice(cert_pem.as_bytes()).map_err(|e| e.to_string())?,
        key: PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| e.to_string())?,
    })
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// A self-signed certificate for `id`: (certificate PEM, PKCS#8 key PEM).
pub(crate) fn generate(id: &str) -> Result<(String, String), String> {
    use rcgen::{CertificateParams, DnType, KeyPair, SerialNumber, PKCS_ECDSA_P256_SHA256};
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params.distinguished_name.push(DnType::CommonName, id);
    params.distinguished_name.push(DnType::OrganizationName, "KDE");
    params.distinguished_name.push(DnType::OrganizationalUnitName, "KDE Connect");
    let year = 1970 + (unix_secs() / 31_556_952) as i32;
    params.not_before = rcgen::date_time_ymd(year - 1, 1, 1);
    params.not_after = rcgen::date_time_ymd(year + 10, 1, 1);
    let serial: [u8; 8] = *uuid::Uuid::new_v4().as_bytes().first_chunk().unwrap();
    params.serial_number = Some(SerialNumber::from_slice(&serial));
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    Ok((cert.pem(), key.serialize_pem()))
}

pub fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Common name of a certificate (the peer's device id).
pub fn cert_common_name(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let cn = cert.subject().iter_common_name().next()?.as_str().ok()?.to_string();
    Some(cn)
}

/// DER SubjectPublicKeyInfo of a certificate (what Qt's `publicKey().toDer()` gives).
pub fn cert_public_key(der: &[u8]) -> Option<Vec<u8>> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    Some(cert.public_key().raw.to_vec())
}

/// The pairing verification key both devices show: SHA-256 over both public keys (larger
/// first) and the pairing timestamp, first 8 hex digits upper-case.
pub fn verification_key(ours: &[u8], theirs: &[u8], timestamp: u64) -> Option<String> {
    use sha2::{Digest, Sha256};
    let (mut a, mut b) = (cert_public_key(ours)?, cert_public_key(theirs)?);
    if a < b {
        std::mem::swap(&mut a, &mut b);
    }
    let mut h = Sha256::new();
    h.update(&a);
    h.update(&b);
    h.update(timestamp.to_string().as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{b:02X}")).collect();
    Some(hex[..8].to_string())
}

pub fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "Prism".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_kdeconnect_style_identity() {
        let dir = std::env::temp_dir().join(format!("prism-phone-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let a = load_or_create(&dir).unwrap();
        assert!(valid_device_id(&a.id));
        assert_eq!(cert_common_name(&a.cert).as_deref(), Some(a.id.as_str()));
        let b = load_or_create(&dir).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.cert, b.cert);
        let mode = fs::metadata(dir.join("private_key.pem")).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        let k1 = verification_key(&a.cert, &a.cert, 1700000000).unwrap();
        assert_eq!(k1.len(), 8);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_id_rules() {
        assert!(valid_device_id("f68e2af095db4113a9c3623dc2c1635b"));
        assert!(valid_device_id("_f68e2af0_95db_4113_a9c3_623dc2c1635b_"));
        assert!(!valid_device_id("short"));
        assert!(!valid_device_id("f68e2af0-95db-4113-a9c3-623dc2c1635b"));
    }
}
