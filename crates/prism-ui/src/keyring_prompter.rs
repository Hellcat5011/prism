//! gnome-keyring's password prompts (replaces gcr-prompter): while `keyring_prompter_enabled`
//! is on, prism-ui owns `org.gnome.keyring.SystemPrompter` and shows "unlock keyring" and
//! "choose a password" requests as prompt cards (prompt.rs).
//!
//! Protocol (gcr's GcrSystemPrompter): the caller calls `BeginPrompting(callback)`; we answer
//! on its callback object with `PromptReady("", {}, exchange)`. Each `PerformPrompt(callback,
//! "password" | "confirm", properties, exchange)` shows a card and is answered with
//! `PromptReady("yes" | "no", changed properties, exchange)`; `StopPrompting` ends with
//! `PromptDone`. Passwords never travel in the clear: the exchange strings carry a
//! Diffie-Hellman key agreement (RFC 3526 1536-bit group) and the password AES-128-CBC
//! encrypted with an HKDF-SHA256 key (gcr's "sx-aes-1").

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use num_bigint::BigUint;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::prompt::{self, Canceller, Field, Request, Verdict};

const NAME: &str = "org.gnome.keyring.SystemPrompter";
const PATH: &str = "/org/gnome/keyring/Prompter";
const CALLBACK: &str = "org.gnome.keyring.internal.Prompter.Callback";

/// RFC 3526 group 5 (gcr's "ietf-ike-grp-modp-1536"), generator 2.
const PRIME: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA237327FFFFFFFFFFFFFFFF";
const GROUP: &str = "[sx-aes-1]";

// ── Secret exchange ─────────────────────────────────────────────────────

struct Exchange {
    private: BigUint,
    public: BigUint,
    key: Option<[u8; 16]>,
}

fn prime() -> BigUint {
    BigUint::parse_bytes(PRIME.as_bytes(), 16).expect("prime")
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

impl Exchange {
    fn new() -> Exchange {
        let p = prime();
        let mut raw = [0u8; 192];
        let _ = getrandom::getrandom(&mut raw);
        // A private exponent in [2, p - 2].
        let private = BigUint::from_bytes_be(&raw) % (&p - 3u32) + 2u32;
        let public = BigUint::from(2u32).modpow(&private, &p);
        Exchange { private, public, key: None }
    }

    /// Our public key only (begin, and answers without a secret).
    fn begin(&self) -> String {
        format!("{GROUP}\npublic={}\n", b64(&self.public.to_bytes_be()))
    }

    /// Take the peer's public key from its exchange string and derive the shared key.
    fn receive(&mut self, text: &str) -> bool {
        let fields = parse(text);
        let Some(peer) = fields.get("public").and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok()) else { return false };
        let p = prime();
        let peer = BigUint::from_bytes_be(&peer);
        if peer < BigUint::from(2u32) || peer >= &p - 1u32 {
            return false;
        }
        let shared = peer.modpow(&self.private, &p).to_bytes_be();
        // Left-padded to the prime's size, as gcr does.
        let mut ikm = vec![0u8; 192 - shared.len().min(192)];
        ikm.extend_from_slice(&shared);
        let mut key = [0u8; 16];
        if hkdf::Hkdf::<sha2::Sha256>::new(None, &ikm).expand(&[], &mut key).is_err() {
            return false;
        }
        self.key = Some(key);
        true
    }

    /// Our public key and `secret`, encrypted with the shared key.
    fn send(&self, secret: &[u8]) -> String {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        let Some(key) = self.key else { return self.begin() };
        let mut iv = [0u8; 16];
        let _ = getrandom::getrandom(&mut iv);
        let ct = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(secret);
        format!("{GROUP}\npublic={}\nsecret={}\niv={}\n", b64(&self.public.to_bytes_be()), b64(&ct), b64(&iv))
    }
}

/// The key=value lines of an exchange string's group.
fn parse(text: &str) -> HashMap<String, String> {
    let mut in_group = false;
    let mut out = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == GROUP;
        } else if let (true, Some((k, v))) = (in_group, line.split_once('=')) {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

// ── Prompter ────────────────────────────────────────────────────────────

struct Prompt {
    exchange: Exchange,
    /// The card open for this caller, if any.
    open: Option<Canceller>,
}

type Prompts = Arc<Mutex<HashMap<(String, String), Prompt>>>;

struct Prompter {
    prompts: Prompts,
}

fn caller(hdr: &zbus::message::Header<'_>) -> String {
    hdr.sender().map(|s| s.to_string()).unwrap_or_default()
}

#[zbus::interface(name = "org.gnome.keyring.internal.Prompter")]
impl Prompter {
    fn begin_prompting(&self, callback: OwnedObjectPath, #[zbus(header)] hdr: zbus::message::Header<'_>) {
        let key = (caller(&hdr), callback.to_string());
        let exchange = Exchange::new();
        let first = exchange.begin();
        self.prompts.lock().unwrap().insert(key.clone(), Prompt { exchange, open: None });
        std::thread::spawn(move || ready(&key, "", HashMap::new(), &first));
    }

    fn perform_prompt(&self, callback: OwnedObjectPath, kind: String, properties: HashMap<String, OwnedValue>, exchange: String, #[zbus(header)] hdr: zbus::message::Header<'_>) {
        let key = (caller(&hdr), callback.to_string());
        {
            let mut map = self.prompts.lock().unwrap();
            let Some(p) = map.get_mut(&key) else { return };
            if !p.exchange.receive(&exchange) {
                eprintln!("[prism-ui/keyring] invalid secret exchange from {}", key.0);
            }
        }
        let prompts = self.prompts.clone();
        std::thread::spawn(move || run_prompt(&prompts, key, &kind, &properties));
    }

    fn stop_prompting(&self, callback: OwnedObjectPath, #[zbus(header)] hdr: zbus::message::Header<'_>) {
        let key = (caller(&hdr), callback.to_string());
        if let Some(p) = self.prompts.lock().unwrap().remove(&key) {
            if let Some(c) = p.open {
                c.cancel();
            }
        }
        std::thread::spawn(move || {
            if let Some(conn) = CONN.get() {
                let _ = conn.call_method(Some(key.0.as_str()), key.1.as_str(), Some(CALLBACK), "PromptDone", &());
            }
        });
    }
}

/// Answer the caller.
fn ready(key: &(String, String), reply: &str, props: HashMap<&str, Value<'_>>, exchange: &str) {
    let Some(conn) = CONN.get() else { return };
    if let Err(e) = conn.call_method(Some(key.0.as_str()), key.1.as_str(), Some(CALLBACK), "PromptReady", &(reply, props, exchange)) {
        eprintln!("[prism-ui/keyring] PromptReady: {e}");
    }
}

fn text(props: &HashMap<String, OwnedValue>, k: &str) -> String {
    props.get(k).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default()
}

fn flag(props: &HashMap<String, OwnedValue>, k: &str) -> bool {
    props.get(k).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

/// Show one prompt and send the answer.
fn run_prompt(prompts: &Prompts, key: (String, String), kind: &str, props: &HashMap<String, OwnedValue>) {
    let password = kind == "password";
    let new = password && flag(props, "password-new");
    let mut title = text(props, "title");
    if title.is_empty() {
        title = if password { "Password Needed".into() } else { "Confirm".into() };
    }
    let mut message = text(props, "message");
    let description = text(props, "description");
    let warning = text(props, "warning");
    if message.is_empty() {
        message = description.clone();
    }
    let detail = if message == description { String::new() } else { description };
    let mut fields = Vec::new();
    if password {
        fields.push(Field { label: if new { "New password".into() } else { "Password".into() }, secret: true, value: String::new() });
        if new {
            fields.push(Field { label: "Confirm password".into(), secret: true, value: String::new() });
        }
    }
    let mut ok = text(props, "continue-label").replace('_', "");
    if ok.is_empty() {
        ok = if password { "Unlock".into() } else { "Continue".into() };
    }
    let Some(session) = prompt::ask(Request { title, message, detail, icon: "key", fields, ok_label: ok, choices: Vec::new() }) else {
        return answer(prompts, &key, None);
    };
    if !warning.is_empty() {
        // Shown like a wrong password (the keyring asks again with a warning).
        session.verdict(Verdict::Retry(warning));
    }
    if let Some(p) = prompts.lock().unwrap().get_mut(&key) {
        p.open = Some(session.canceller());
    } else {
        session.verdict(Verdict::Cancel);
        return;
    }
    loop {
        let Ok(Some(mut vals)) = session.answers.recv() else {
            return answer(prompts, &key, None);
        };
        if new && vals.first() != vals.get(1) {
            prism_common::secret::wipe_all(&mut vals);
            session.verdict(Verdict::Retry("The passwords do not match.".into()));
            continue;
        }
        session.verdict(Verdict::Done);
        let secret = vals.first().cloned().unwrap_or_default();
        prism_common::secret::wipe_all(&mut vals);
        return answer(prompts, &key, Some(secret));
    }
}

/// "yes" with the password (or none for a confirmation), or "no".
fn answer(prompts: &Prompts, key: &(String, String), secret: Option<String>) {
    let exchange = {
        let mut map = prompts.lock().unwrap();
        let Some(p) = map.get_mut(key) else { return };
        p.open = None;
        match &secret {
            Some(s) if !s.is_empty() => p.exchange.send(s.as_bytes()),
            _ => p.exchange.begin(),
        }
    };
    let chosen = if secret.is_some() { "yes" } else { "no" };
    if let Some(mut s) = secret {
        prism_common::secret::wipe(&mut s);
    }
    let mut props: HashMap<&str, Value> = HashMap::new();
    props.insert("choice-chosen", Value::from(false));
    ready(key, chosen, props, &exchange);
}

// ── Service ─────────────────────────────────────────────────────────────

static CONN: OnceLock<zbus::blocking::Connection> = OnceLock::new();
static WANTED: AtomicBool = AtomicBool::new(false);
static SUPERVISOR: std::sync::Once = std::sync::Once::new();

pub fn apply(cfg: &prism_common::Config) {
    WANTED.store(cfg.keyring_prompter_enabled, Ordering::SeqCst);
    if cfg.keyring_prompter_enabled {
        SUPERVISOR.call_once(|| {
            std::thread::Builder::new().name("prism-keyring".into()).spawn(supervise).ok();
        });
    }
}

/// Own the prompter name (gcr-prompter may hold it for a while after a prompt: retry).
fn supervise() {
    let mut logged = false;
    loop {
        if WANTED.load(Ordering::SeqCst) {
            let conn = zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at(PATH, Prompter { prompts: Arc::default() }))
                .and_then(|b| b.build())
                .and_then(|c| {
                    c.request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::ReplaceExisting | zbus::fdo::RequestNameFlags::DoNotQueue)
                        .and_then(|r| if r == zbus::fdo::RequestNameReply::PrimaryOwner || r == zbus::fdo::RequestNameReply::AlreadyOwner { Ok(c) } else { Err(zbus::Error::NameTaken) })
                });
            match conn {
                Ok(c) => {
                    eprintln!("[prism-ui/keyring] serving {NAME}");
                    let _ = CONN.set(c);
                    return;
                }
                Err(e) => {
                    if !logged {
                        eprintln!("[prism-ui/keyring] cannot own {NAME} ({e}); retrying");
                        logged = true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against gcr itself: PRISM_SX_IN holds a GcrSecretExchange begin() string; the answer
    /// (as the prompter, with the secret "hunter2") is written to PRISM_SX_OUT.
    #[test]
    #[ignore]
    fn gcr_interop() {
        let input = std::fs::read_to_string(std::env::var("PRISM_SX_IN").unwrap()).unwrap();
        let mut ours = Exchange::new();
        assert!(ours.receive(&input));
        std::fs::write(std::env::var("PRISM_SX_OUT").unwrap(), ours.send(b"hunter2")).unwrap();
    }

    #[test]
    fn both_sides_agree_and_decrypt() {
        use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
        let mut a = Exchange::new();
        let mut b = Exchange::new();
        assert!(a.receive(&b.begin()));
        assert!(b.receive(&a.begin()));
        assert_eq!(a.key, b.key);
        let sent = a.send(b"hunter2");
        let f = parse(&sent);
        let d = |k: &str| base64::engine::general_purpose::STANDARD.decode(&f[k]).unwrap();
        let iv: [u8; 16] = d("iv").try_into().unwrap();
        let plain = cbc::Decryptor::<aes::Aes128>::new(&b.key.unwrap().into(), &iv.into()).decrypt_padded_vec_mut::<Pkcs7>(&d("secret")).unwrap();
        assert_eq!(plain, b"hunter2");
    }
}
