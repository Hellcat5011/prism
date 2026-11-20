//! NetworkManager secret agent (replaces nm-applet's password dialogs). While
//! `network_agent_enabled` is on, prism-ui serves org.freedesktop.NetworkManager.SecretAgent
//! and NetworkManager asks it for secrets it does not have (a new or changed Wi-Fi password,
//! 802.1X, VPN, mobile broadband, PPPoE). Each request opens the prompt card (prompt.rs).
//! Secrets are not stored here: NetworkManager saves system-owned secrets itself.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::prompt::{self, Field, Request, Verdict};

const NM: &str = "org.freedesktop.NetworkManager";
const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
const IDENTIFIER: &str = "org.prism.secret-agent";
/// NM_SECRET_AGENT_GET_SECRETS_FLAG_ALLOW_INTERACTION.
const ALLOW_INTERACTION: u32 = 0x1;
/// NM_SECRET_AGENT_CAPABILITY_VPN_HINTS.
const CAP_VPN_HINTS: u32 = 0x1;

type Setting = HashMap<String, OwnedValue>;
type ConnSettings = HashMap<String, Setting>;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.NetworkManager.SecretAgent")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    UserCanceled(String),
    NoSecrets(String),
}

/// Requests NetworkManager withdrew: (connection path, setting name).
static CANCELLED: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);

struct Agent;

#[zbus::interface(name = "org.freedesktop.NetworkManager.SecretAgent")]
impl Agent {
    async fn get_secrets(
        &self,
        connection: ConnSettings,
        connection_path: OwnedObjectPath,
        setting_name: String,
        hints: Vec<String>,
        flags: u32,
    ) -> Result<HashMap<String, HashMap<String, OwnedValue>>, AgentError> {
        if flags & ALLOW_INTERACTION == 0 {
            return Err(AgentError::NoSecrets("no stored secrets".into()));
        }
        let path = connection_path.to_string();
        blocking::unblock(move || ask(&connection, &path, &setting_name, &hints)).await
    }

    fn cancel_get_secrets(&self, connection_path: OwnedObjectPath, setting_name: String) {
        CANCELLED.lock().unwrap().get_or_insert_with(HashSet::new).insert((connection_path.to_string(), setting_name));
    }

    fn save_secrets(&self, _connection: ConnSettings, _connection_path: OwnedObjectPath) {}

    fn delete_secrets(&self, _connection: ConnSettings, _connection_path: OwnedObjectPath) {}
}

fn s(set: Option<&Setting>, key: &str) -> String {
    match set.and_then(|m| m.get(key)).map(|v| &**v) {
        Some(Value::Str(x)) => x.to_string(),
        _ => String::new(),
    }
}

fn ssid(conn: &ConnSettings) -> String {
    match conn.get("802-11-wireless").and_then(|m| m.get("ssid")).map(|v| &**v) {
        Some(Value::Array(a)) => {
            let bytes: Vec<u8> = a.iter().filter_map(|v| if let Value::U8(b) = v { Some(*b) } else { None }).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => String::new(),
    }
}

/// VPN `data` / `secrets` dictionaries are a{ss}.
fn vpn_dict(conn: &ConnSettings, key: &str) -> HashMap<String, String> {
    conn.get("vpn").and_then(|v| v.get(key)).and_then(|v| HashMap::<String, String>::try_from(v.try_clone().ok()?).ok()).unwrap_or_default()
}

/// A field to ask for: (secret key, label, is secret, initial value).
type Ask = (String, String, bool, String);

/// What to ask: (title, message, fields).
fn plan(conn: &ConnSettings, setting: &str, hints: &[String]) -> Option<(String, String, Vec<Ask>)> {
    let id = s(conn.get("connection"), "id");
    let sec = conn.get("802-11-wireless-security");
    let f = |k: &str, label: &str, secret: bool, v: String| (k.to_string(), label.to_string(), secret, v);
    Some(match setting {
        "802-11-wireless-security" => {
            let name = Some(ssid(conn)).filter(|x| !x.is_empty()).unwrap_or(id.clone());
            let key = match s(sec, "key-mgmt").as_str() {
                "none" => f("wep-key0", "WEP key", true, String::new()),
                "ieee8021x" => f("leap-password", "Password", true, String::new()),
                _ => f("psk", "Password", true, String::new()),
            };
            (format!("Wi-Fi network \u{201C}{name}\u{201D}"), "A password is required to connect.".into(), vec![key])
        }
        "802-1x" => {
            let x = conn.get("802-1x");
            let name = Some(ssid(conn)).filter(|x| !x.is_empty()).unwrap_or(id.clone());
            let mut fields = vec![f("identity", "User name", false, s(x, "identity"))];
            if hints.iter().any(|h| h == "private-key-password") {
                fields = vec![f("private-key-password", "Private key password", true, String::new())];
            } else {
                fields.push(f("password", "Password", true, String::new()));
            }
            (format!("Network \u{201C}{name}\u{201D}"), "Enterprise authentication is required.".into(), fields)
        }
        "vpn" => {
            let data = vpn_dict(conn, "data");
            // Hints name the secrets the VPN plugin needs ("x-vpn-message:" carries a message).
            let msg = hints.iter().find_map(|h| h.strip_prefix("x-vpn-message:")).map(str::to_string);
            let mut keys: Vec<String> = hints.iter().filter(|h| !h.starts_with("x-vpn-")).cloned().collect();
            if keys.is_empty() {
                keys.push("password".into());
            }
            let mut fields = Vec::new();
            // Password logins need a user name in the profile (imported .ovpn files have none).
            let user = data.get("username").cloned().unwrap_or_default();
            let ctype = data.get("connection-type").map(String::as_str).unwrap_or("");
            if !user.is_empty() || matches!(ctype, "password" | "password-tls") {
                fields.push(f("x-prism-username", "User name", false, user));
            }
            for k in keys {
                let label = match k.as_str() {
                    "password" => "Password".to_string(),
                    "cert-pass" => "Certificate password".to_string(),
                    "http-proxy-password" => "Proxy password".to_string(),
                    other => other.replace(['-', '_'], " "),
                };
                fields.push((k, label, true, String::new()));
            }
            (format!("VPN \u{201C}{id}\u{201D}"), msg.unwrap_or_else(|| "The VPN needs your credentials.".into()), fields)
        }
        "gsm" | "cdma" => {
            let key = if hints.iter().any(|h| h == "pin") { f("pin", "SIM PIN", true, String::new()) } else { f("password", "Password", true, String::new()) };
            (format!("Mobile broadband \u{201C}{id}\u{201D}"), "A secret is required to connect.".into(), vec![key])
        }
        "pppoe" => (format!("DSL \u{201C}{id}\u{201D}"), "A password is required to connect.".into(), vec![f("password", "Password", true, String::new())]),
        _ => return None,
    })
}

fn cancelled(path: &str, setting: &str) -> bool {
    CANCELLED.lock().unwrap().as_mut().is_some_and(|c| c.remove(&(path.to_string(), setting.to_string())))
}

fn ask(conn: &ConnSettings, path: &str, setting: &str, hints: &[String]) -> Result<HashMap<String, HashMap<String, OwnedValue>>, AgentError> {
    let (title, message, fields) = plan(conn, setting, hints).ok_or_else(|| AgentError::NoSecrets(format!("cannot ask for {setting} secrets")))?;
    let session = prompt::ask(Request {
        title,
        message,
        detail: String::new(),
        icon: if setting == "vpn" { "lock" } else { "wifi" },
        fields: fields.iter().map(|(_, label, secret, v)| Field { label: label.clone(), secret: *secret, value: v.clone() }).collect(),
        ok_label: "Connect".into(),
        choices: Vec::new(),
    })
    .ok_or_else(|| AgentError::NoSecrets("prompt unavailable".into()))?;
    let answer = loop {
        match session.answers.recv_timeout(Duration::from_millis(250)) {
            Ok(a) => break a,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancelled(path, setting) => {
                session.verdict(Verdict::Cancel);
                return Err(AgentError::UserCanceled("cancelled by NetworkManager".into()));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break None,
        }
    };
    let Some(values) = answer else {
        return Err(AgentError::UserCanceled("dismissed".into()));
    };
    // NetworkManager reports a wrong password by asking again, so the card closes here.
    session.verdict(Verdict::Done);
    if setting == "vpn" {
        let cfg = prism_common::Config::load();
        let typed_user = fields.iter().zip(&values).find(|((k, ..), _)| k == "x-prism-username").map(|(_, v)| v.trim().to_string()).filter(|u| !u.is_empty());
        let had_user = !vpn_dict(conn, "data").get("username").is_none_or(|u| u.is_empty());
        // A missing user name is always stored: the login cannot work without it.
        let user = typed_user.filter(|_| cfg.vpn_save_username || !had_user);
        let secrets: Option<HashMap<String, String>> = cfg.vpn_save_password.then(|| {
            fields.iter().zip(&values).filter(|((k, _, secret, _), _)| *secret && k != "x-prism-username").map(|((k, ..), v)| (k.clone(), v.clone())).collect()
        });
        if user.is_some() || secrets.is_some() {
            let path = path.to_string();
            std::thread::spawn(move || {
                if let Err(e) = save_vpn(&path, user, secrets) {
                    eprintln!("[prism-ui/nm-agent] saving VPN credentials: {e}");
                }
            });
        }
    }
    let ov = |v: &str| OwnedValue::try_from(Value::from(v.to_string())).expect("string value");
    let mut out: HashMap<String, OwnedValue> = HashMap::new();
    if setting == "vpn" {
        let mut secrets: HashMap<String, String> = HashMap::new();
        for ((k, ..), v) in fields.iter().zip(&values) {
            if k != "x-prism-username" {
                secrets.insert(k.clone(), v.clone());
            }
        }
        out.insert("secrets".into(), OwnedValue::try_from(Value::from(secrets)).expect("dict value"));
    } else {
        for ((k, ..), v) in fields.iter().zip(&values) {
            out.insert(k.clone(), ov(v));
        }
    }
    Ok(HashMap::from([(setting.to_string(), out)]))
}

/// Store a VPN user name and/or secrets in the connection profile. Secrets get flags 0
/// (system-owned): NetworkManager keeps them itself, as nm-applet's "store for all users".
fn save_vpn(path: &str, user: Option<String>, secrets: Option<HashMap<String, String>>) -> Result<(), String> {
    let sys = Connection::system().map_err(|e| e.to_string())?;
    const IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
    let m = sys.call_method(Some(NM), path, Some(IFACE), "GetSettings", &()).map_err(|e| e.to_string())?;
    let mut settings: ConnSettings = m.body().deserialize().map_err(|e| e.to_string())?;
    let mut data = vpn_dict(&settings, "data");
    if let Some(u) = user {
        data.insert("username".into(), u);
    }
    let vpn = settings.entry("vpn".into()).or_default();
    if let Some(sec) = &secrets {
        for k in sec.keys() {
            data.insert(format!("{k}-flags"), "0".into());
        }
        vpn.insert("secrets".into(), OwnedValue::try_from(Value::from(sec.clone())).map_err(|e| e.to_string())?);
    }
    vpn.insert("data".into(), OwnedValue::try_from(Value::from(data)).map_err(|e| e.to_string())?);
    sys.call_method(Some(NM), path, Some(IFACE), "Update", &(settings,)).map(|_| ()).map_err(|e| e.to_string())
}

// ── Registration ─────────────────────────────────────────────────────────

fn register() -> Result<Connection, String> {
    let conn = zbus::blocking::connection::Builder::system()
        .and_then(|b| b.serve_at(AGENT_PATH, Agent))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    conn.call_method(Some(NM), "/org/freedesktop/NetworkManager/AgentManager", Some("org.freedesktop.NetworkManager.AgentManager"), "RegisterWithCapabilities", &(IDENTIFIER, CAP_VPN_HINTS))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

static WANTED: AtomicBool = AtomicBool::new(false);
static SUPERVISOR: std::sync::Once = std::sync::Once::new();

pub fn apply(cfg: &prism_common::Config) {
    WANTED.store(cfg.network_agent_enabled, Ordering::SeqCst);
    if cfg.network_agent_enabled {
        SUPERVISOR.call_once(|| {
            std::thread::Builder::new().name("prism-nm-agent".into()).spawn(supervise).ok();
        });
    }
}

fn owner(c: &Connection, name: &str) -> Option<String> {
    let d = zbus::blocking::fdo::DBusProxy::new(c).ok()?;
    d.get_name_owner(name.try_into().ok()?).ok().map(|o| o.to_string())
}

fn supervise() {
    let mut reg: Option<(Connection, Option<String>)> = None;
    let mut logged = false;
    loop {
        let wanted = WANTED.load(Ordering::SeqCst);
        match (&reg, wanted) {
            (None, true) => match register() {
                Ok(c) => {
                    eprintln!("[prism-ui/nm-agent] secret agent registered");
                    let o = owner(&c, NM);
                    reg = Some((c, o));
                    logged = false;
                }
                Err(e) => {
                    if !logged {
                        eprintln!("[prism-ui/nm-agent] cannot register ({e}); retrying");
                        logged = true;
                    }
                }
            },
            (Some((c, _)), false) => {
                let _ = c.call_method(Some(NM), "/org/freedesktop/NetworkManager/AgentManager", Some("org.freedesktop.NetworkManager.AgentManager"), "Unregister", &());
                reg = None;
                eprintln!("[prism-ui/nm-agent] secret agent unregistered");
            }
            (Some((c, o)), true) => {
                // NetworkManager restarted: its new instance does not know us.
                if owner(c, NM) != *o {
                    reg = None;
                }
            }
            (None, false) => {}
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
