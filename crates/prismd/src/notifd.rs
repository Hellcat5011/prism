//! The desktop notification server (org.freedesktop.Notifications, spec 1.2), replacing the old
//! Python bridge. Notify / CloseNotification come in on a zbus connection thread; storing,
//! popups and silent mode are handled by `Handler` (prismd's main.rs). Signals back to apps
//! (NotificationClosed, ActionInvoked) go through `emit_closed` / `emit_action`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

const PATH: &str = "/org/freedesktop/Notifications";
const NAME: &str = "org.freedesktop.Notifications";

/// NotificationClosed reasons (spec).
pub const CLOSED_EXPIRED: u32 = 1;
pub const CLOSED_DISMISSED: u32 = 2;
pub const CLOSED_BY_CALL: u32 = 3;

/// A notification as received, already normalised.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub app_name: String,
    pub replaces_id: u32,
    pub icon: Option<String>,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub desktop_entry: String,
    pub image: Option<String>,
    /// Show the image as a large preview (Prism's private hint "x-prism-preview").
    pub preview: bool,
    pub transient: bool,
    /// The sender asks for no sound (hint "suppress-sound").
    pub suppress_sound: bool,
    /// Popup duration in ms; 0 = stays until dismissed (critical).
    pub timeout_ms: u32,
}

impl Incoming {
    /// A new notification worth a sound: updates of one already shown and passing messages
    /// stay quiet.
    pub fn wants_sound(&self) -> bool {
        self.replaces_id == 0 && !self.transient && !self.suppress_sound
    }
}

pub trait Handler: Send + Sync {
    /// Store / show a notification; returns its id.
    fn notify(&self, n: Incoming) -> u32;
    /// The app withdrew a notification.
    fn close(&self, id: u32);
}

struct Server {
    handler: Arc<dyn Handler>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        ["actions", "body", "body-markup", "icon-static", "persistence"].iter().map(|s| s.to_string()).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let incoming = normalise(app_name, replaces_id, app_icon, summary, body, actions, &hints, expire_timeout);
        self.handler.notify(incoming)
    }

    fn close_notification(&self, id: u32) {
        self.handler.close(id);
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("Prism".into(), "Prism".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: String) -> zbus::Result<()>;
}

static CONN: OnceLock<Connection> = OnceLock::new();

/// Take the bus name and serve. Retries while another server still owns the name (e.g. the
/// old Python bridge being shut down). Runs in the background; returns at once.
pub fn start(handler: Arc<dyn Handler>) {
    std::thread::spawn(move || {
        for attempt in 0..30 {
            let built = zbus::blocking::connection::Builder::session()
                .and_then(|b| b.name(NAME))
                .and_then(|b| b.serve_at(PATH, Server { handler: Arc::clone(&handler) }))
                .and_then(|b| b.build());
            match built {
                Ok(conn) => {
                    eprintln!("[prismd/notif] serving {NAME}");
                    let _ = CONN.set(conn);
                    return; // zbus keeps serving on its own executor thread
                }
                Err(e) => {
                    if attempt == 0 {
                        eprintln!("[prismd/notif] cannot own {NAME} yet ({e}); retrying");
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
        eprintln!("[prismd/notif] giving up: another notification daemon owns {NAME}");
    });
}

fn emitter() -> Option<SignalEmitter<'static>> {
    let conn = CONN.get()?;
    SignalEmitter::new(conn.inner(), PATH).ok().map(|e| e.into_owned())
}

pub fn emit_closed(id: u32, reason: u32) {
    if let Some(e) = emitter() {
        let _ = zbus::block_on(Server::notification_closed(&e, id, reason));
    }
}

pub fn emit_action(id: u32, key: &str) {
    if let Some(e) = emitter() {
        let _ = zbus::block_on(Server::action_invoked(&e, id, key.to_string()));
    }
}

// ── Normalisation ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn normalise(
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: &HashMap<String, OwnedValue>,
    expire_timeout: i32,
) -> Incoming {
    let urgency = hints.get("urgency").and_then(|v| u8::try_from(&**v).ok()).unwrap_or(1).min(2);
    let string_hint = |k: &str| hints.get(k).and_then(|v| <&str>::try_from(&**v).ok()).map(String::from);
    let bool_hint = |k: &str| hints.get(k).and_then(|v| bool::try_from(&**v).ok()).unwrap_or(false);
    let desktop_entry = string_hint("desktop-entry").unwrap_or_default();

    // Picture: image-data (raw pixels) > image-path > app_icon when it is a file.
    let mut image = ["image-data", "image_data", "icon_data"].iter().find_map(|k| hints.get(*k)).and_then(save_image_data);
    if image.is_none() {
        image = string_hint("image-path").or_else(|| string_hint("image_path")).and_then(|p| file_path(&p));
    }
    let icon_path = file_path(&app_icon);
    if image.is_none() {
        image = icon_path.clone().filter(|p| looks_like_picture(p));
    }
    let icon = if app_icon.trim().is_empty() {
        None
    } else {
        Some(icon_path.unwrap_or(app_icon))
    };

    let actions: Vec<(String, String)> = actions
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| {
            let label = if c[0] == "default" && (c[1].is_empty() || c[1].eq_ignore_ascii_case("default")) { "Open".to_string() } else { c[1].clone() };
            (c[0].clone(), label)
        })
        .filter(|(_, l)| !l.is_empty())
        .collect();

    Incoming {
        app_name: if app_name.trim().is_empty() { "System".into() } else { app_name },
        replaces_id,
        icon,
        summary,
        body,
        actions,
        urgency,
        desktop_entry,
        preview: image.is_some() && bool_hint("x-prism-preview"),
        image,
        transient: bool_hint("transient"),
        suppress_sound: bool_hint("suppress-sound"),
        timeout_ms: popup_timeout(expire_timeout, urgency),
    }
}

/// QML NotificationManager rule: the sender's timeout (seconds tolerated when <= 100) clamped
/// to 3-20 s, otherwise 5 s; critical notifications stay until dismissed.
pub fn popup_timeout(expire_timeout: i32, urgency: u8) -> u32 {
    if urgency >= 2 {
        return 0;
    }
    let mut ms = expire_timeout.max(0) as u32;
    if ms > 0 && ms <= 100 {
        ms *= 1000;
    }
    if ms > 0 { ms.clamp(3000, 20000) } else { 5000 }
}

/// file:// URIs and absolute paths that exist; None for icon names.
fn file_path(s: &str) -> Option<String> {
    let p = s.strip_prefix("file://").unwrap_or(s);
    (p.starts_with('/') && std::path::Path::new(p).exists()).then(|| p.to_string())
}

fn looks_like_picture(p: &str) -> bool {
    let l = p.to_lowercase();
    [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"].iter().any(|e| l.ends_with(e)) && !l.contains("/icons/")
}

fn image_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/prism/notifications/images")
}

/// The `image-data` hint, (iiibiiay) = width, height, rowstride, has_alpha, bits, channels,
/// pixels, written as a PNG in the cache. Returns the file path.
fn save_image_data(v: &OwnedValue) -> Option<String> {
    let Value::Structure(st) = &**v else { return None };
    let f = st.fields();
    if f.len() < 7 {
        return None;
    }
    let w = i32::try_from(&f[0]).ok()?;
    let h = i32::try_from(&f[1]).ok()?;
    let stride = i32::try_from(&f[2]).ok()?;
    let alpha = bool::try_from(&f[3]).ok()?;
    let bits = i32::try_from(&f[4]).ok()?;
    let channels = i32::try_from(&f[5]).ok()?;
    let data: Vec<u8> = match &f[6] {
        Value::Array(a) => a.iter().filter_map(|x| u8::try_from(x).ok()).collect(),
        _ => return None,
    };
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 || bits != 8 || !(3..=4).contains(&channels) || (alpha && channels != 4) {
        return None;
    }
    if !image_fits(w, h, stride, channels, data.len()) {
        return None;
    }
    let (w, h, stride, ch) = (w as usize, h as usize, stride as usize, channels as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &data[y * stride..y * stride + w * ch];
        for px in row.chunks(ch) {
            rgba.extend_from_slice(&[px[0], px[1], px[2], if ch == 4 { px[3] } else { 255 }]);
        }
    }
    let dir = image_dir();
    prism_common::clipboard::private_dir(&dir).ok()?;
    prune_images(&dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let path = dir.join(format!("{stamp}.png"));
    prism_common::clipboard::write_private(&path, &encode_png(w as u32, h as u32, &rgba)).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Do rows of `stride` bytes hold `w` pixels each, and `h` of them fit in `len` bytes? The values
/// come from any client on the session bus. `w`, `h` and `channels` are already positive.
fn image_fits(w: i32, h: i32, stride: i32, channels: i32, len: usize) -> bool {
    let row = w as i64 * channels as i64;
    stride as i64 >= row && (stride as i64 * (h as i64 - 1) + row) as u64 <= len as u64
}

/// Keep the image cache small: the newest 200 files.
fn prune_images(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = rd.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if files.len() <= 200 {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - 200) {
        let _ = std::fs::remove_file(p);
    }
}

// ── Minimal PNG writer (RGBA8, zlib "stored" blocks) ─────────────────────

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut c = kind.to_vec();
    c.extend_from_slice(data);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, blk) in blocks.iter().enumerate() {
        z.push(if i + 1 == blocks.len() { 1 } else { 0 });
        let len = blk.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(blk);
    }
    if blocks.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_follow_qml() {
        assert_eq!(popup_timeout(-1, 1), 5000);
        assert_eq!(popup_timeout(0, 1), 5000);
        assert_eq!(popup_timeout(10, 1), 10000); // seconds tolerated
        assert_eq!(popup_timeout(500, 1), 3000);
        assert_eq!(popup_timeout(60000, 1), 20000);
        assert_eq!(popup_timeout(8000, 2), 0);
    }

    #[test]
    fn png_checksums() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]);
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn actions_and_hints() {
        let mut hints = HashMap::new();
        hints.insert("urgency".to_string(), OwnedValue::from(2u8));
        hints.insert("desktop-entry".to_string(), OwnedValue::try_from(Value::from("firefox")).unwrap());
        let n = normalise("".into(), 0, "firefox".into(), "s".into(), "b".into(), vec!["default".into(), "".into(), "reply".into(), "Reply".into()], &hints, -1);
        assert_eq!(n.app_name, "System");
        assert_eq!(n.urgency, 2);
        assert!(n.wants_sound());
        assert_eq!(n.timeout_ms, 0);
        assert_eq!(n.desktop_entry, "firefox");
        assert_eq!(n.actions, vec![("default".into(), "Open".into()), ("reply".into(), "Reply".into())]);
        assert_eq!(n.icon.as_deref(), Some("firefox"));
    }
}

#[cfg(test)]
mod image_tests {
    use super::image_fits;

    #[test]
    fn rejects_rows_that_do_not_fit() {
        assert!(image_fits(2, 2, 8, 4, 16));
        assert!(image_fits(2, 2, 10, 4, 18));
        assert!(!image_fits(2, 2, 8, 4, 15));
        assert!(!image_fits(1, 2, -1, 3, 8));
        assert!(!image_fits(4, 1, 3, 4, 1024));
        assert!(!image_fits(4096, 4096, i32::MAX, 4, 1024));
    }
}
