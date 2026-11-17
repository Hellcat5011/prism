//! Shared daemon state: known devices, their links, pairing, and the state stream to
//! subscribers (prism-ui). One `Mutex<Core>`; handlers keep it briefly and never block on the
//! network while holding it (links are written through channels).

use crate::identity::{self, Identity};
use crate::packet::{Packet, PeerInfo, PAIR};
use crate::tls::TlsConfigs;
use crate::trust::{self, Trusted};
use prism_common::phone::{PairStatus, PhoneCommand, PhoneDevice, PhoneReply, PhoneState};
use prism_common::Config;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Shared = Arc<Mutex<Core>>;

/// Pair requests expire after this long (KDE Connect uses 30 s).
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);

/// Sending half of a link. Dropping it closes the wake pipe, which ends the link thread.
pub struct LinkHandle {
    pub serial: u64,
    pub tx: Sender<Packet>,
    pub wake: OwnedFd,
    pub cert: Vec<u8>,
}

impl LinkHandle {
    pub fn send(&self, p: Packet) {
        if self.tx.send(p).is_ok() {
            unsafe { libc::write(self.wake.as_raw_fd(), b"x".as_ptr() as *const libc::c_void, 1) };
        }
    }
}

pub struct Dev {
    pub view: PhoneDevice,
    pub info: PeerInfo,
    pub trusted: Option<Trusted>,
    pub link: Option<LinkHandle>,
    pub addr: Option<std::net::IpAddr>,
    pair_ts: u64,
    pair_deadline: Option<Instant>,
    /// Last connect attempt started from discovery (rate limit).
    pub last_attempt: Option<Instant>,
}

pub struct Core {
    pub ident: Identity,
    pub tls: TlsConfigs,
    pub cfg: Config,
    pub data_dir: PathBuf,
    pub tcp_port: u16,
    pub udp_port: u16,
    pub error: Option<String>,
    pub devices: BTreeMap<String, Dev>,
    subscribers: Vec<UnixStream>,
    next_serial: u64,
    /// Plugin state that is not per-device view data.
    pub plugins: crate::plugins::PluginState,
    /// Ourselves, for handlers that start threads (payload transfers).
    pub me: std::sync::Weak<Mutex<Core>>,
}

impl Core {
    pub fn new(ident: Identity, tls: TlsConfigs, cfg: Config, data_dir: PathBuf) -> Core {
        let mut devices = BTreeMap::new();
        for t in trust::load_all(&data_dir) {
            let view = PhoneDevice { id: t.id.clone(), name: t.name.clone(), device_type: t.device_type.clone(), paired: true, pair: PairStatus::Paired, ..Default::default() };
            let info = PeerInfo { id: t.id.clone(), name: t.name.clone(), device_type: t.device_type.clone(), ..Default::default() };
            let addr = t.last_addr.as_deref().and_then(|a| a.parse().ok());
            devices.insert(t.id.clone(), Dev { view, info, trusted: Some(t), link: None, addr, pair_ts: 0, pair_deadline: None, last_attempt: None });
        }
        Core {
            ident,
            tls,
            cfg,
            data_dir,
            tcp_port: 0,
            udp_port: 1716,
            error: None,
            devices,
            subscribers: Vec::new(),
            next_serial: 1,
            plugins: Default::default(),
            me: std::sync::Weak::new(),
        }
    }

    pub fn shared(&self) -> Option<Shared> {
        self.me.upgrade()
    }

    pub fn name(&self) -> String {
        let n = self.cfg.phone_device_name.trim();
        if n.is_empty() { identity::hostname() } else { n.to_string() }
    }

    pub fn identity_packet(&self, with_port: bool) -> Packet {
        crate::packet::identity_packet(&self.ident.id, &self.name(), with_port.then_some(self.tcp_port), crate::plugins::INCOMING, crate::plugins::OUTGOING)
    }

    pub fn next_serial(&mut self) -> u64 {
        self.next_serial += 1;
        self.next_serial
    }

    /// Discovery saw `info` at `addr`: true when a connection attempt should start.
    pub fn should_connect(&mut self, info: &PeerInfo, addr: std::net::IpAddr) -> bool {
        if info.id == self.ident.id || info.protocol < crate::packet::PROTOCOL_VERSION || !identity::valid_device_id(&info.id) {
            return false;
        }
        let dev = self.devices.entry(info.id.clone()).or_insert_with(|| Dev {
            view: PhoneDevice { id: info.id.clone(), name: info.name.clone(), device_type: info.device_type.clone(), ..Default::default() },
            info: info.clone(),
            trusted: None,
            link: None,
            addr: None,
            pair_ts: 0,
            pair_deadline: None,
            last_attempt: None,
        });
        dev.addr = Some(addr);
        dev.view.last_seen = identity::unix_secs();
        if dev.link.is_some() || dev.last_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return false;
        }
        dev.last_attempt = Some(Instant::now());
        true
    }

    /// A TLS link to `info` is up. Err = refuse it (the caller closes the socket).
    pub fn link_up(&mut self, info: PeerInfo, cert: Vec<u8>, addr: std::net::IpAddr, handle: LinkHandle) -> Result<(), String> {
        if info.id == self.ident.id {
            return Err("that is us".into());
        }
        if identity::cert_common_name(&cert).as_deref() != Some(info.id.as_str()) {
            return Err(format!("certificate does not belong to {}", info.id));
        }
        if let Some(t) = self.devices.get(&info.id).and_then(|d| d.trusted.as_ref()) {
            if t.cert_der() != cert {
                return Err(format!("{} ({}) presented a different certificate than when it was paired; refusing", info.name, info.id));
            }
        }
        let id = info.id.clone();
        let dev = self.devices.entry(id.clone()).or_insert_with(|| Dev {
            view: PhoneDevice::default(),
            info: info.clone(),
            trusted: None,
            link: None,
            addr: None,
            pair_ts: 0,
            pair_deadline: None,
            last_attempt: None,
        });
        dev.view.id = id.clone();
        dev.view.name = info.name.clone();
        dev.view.device_type = info.device_type.clone();
        dev.view.can_receive = info.incoming.clone();
        dev.view.reachable = true;
        dev.view.last_seen = identity::unix_secs();
        dev.info = info;
        dev.addr = Some(addr);
        dev.link = Some(handle);
        if let Some(t) = dev.trusted.as_mut() {
            let a = addr.to_string();
            if t.name != dev.view.name || t.last_addr.as_deref() != Some(a.as_str()) {
                t.name = dev.view.name.clone();
                t.last_addr = Some(a);
                trust::save(&self.data_dir, t);
            }
        }
        eprintln!("[prism-phoned] connected: {} ({id}), paired={}", self.devices[&id].view.name, self.devices[&id].view.paired);
        if self.devices[&id].view.paired {
            crate::plugins::on_connected(self, &id);
        }
        self.publish();
        Ok(())
    }

    pub fn link_down(&mut self, id: &str, serial: u64) {
        let Some(dev) = self.devices.get_mut(id) else { return };
        if dev.link.as_ref().map(|l| l.serial) != Some(serial) {
            return;
        }
        dev.link = None;
        dev.view.reachable = false;
        if !dev.view.paired {
            dev.view.pair = PairStatus::NotPaired;
            dev.view.verification_key = None;
            dev.pair_deadline = None;
        }
        eprintln!("[prism-phoned] disconnected: {} ({id})", dev.view.name);
        crate::plugins::on_disconnected(self, id);
        // Unpaired devices nobody is pairing with are forgotten when they go away.
        if !self.devices[id].view.paired {
            self.devices.remove(id);
        }
        self.publish();
    }

    pub fn send(&self, id: &str, p: Packet) -> bool {
        match self.devices.get(id).and_then(|d| d.link.as_ref()) {
            Some(l) => {
                l.send(p);
                true
            }
            None => false,
        }
    }

    pub fn on_packet(&mut self, id: &str, p: Packet) {
        let Some(dev) = self.devices.get_mut(id) else { return };
        dev.view.last_seen = identity::unix_secs();
        if p.kind == PAIR {
            self.on_pair(id, &p);
            return;
        }
        if p.kind == crate::packet::IDENTITY {
            // The identity sent again inside TLS (it can arrive after other packets): refresh
            // name and capabilities. The id is bound to the certificate and cannot change.
            if let Some(info) = PeerInfo::from_identity(&p).filter(|i| i.id == id) {
                dev.view.name = info.name.clone();
                dev.view.device_type = info.device_type.clone();
                dev.view.can_receive = info.incoming.clone();
                dev.info = info;
                self.publish();
            }
            return;
        }
        if !self.devices[id].view.paired {
            // Unpaired peers may only pair.
            return;
        }
        crate::plugins::handle(self, id, &p);
    }

    // ── Pairing ──────────────────────────────────────────────────────

    fn on_pair(&mut self, id: &str, p: &Packet) {
        let want = p.bool("pair");
        let status = self.devices[id].view.pair;
        match (want, status) {
            (true, PairStatus::Requested) => self.set_paired(id),
            (true, PairStatus::Paired) => {
                // The peer forgot us but we still trust its (unchanged) certificate.
                self.send(id, Packet::new(PAIR, json!({"pair": true})));
            }
            (true, _) => {
                let ts = p.int("timestamp").map(|t| t.max(0) as u64).unwrap_or_else(identity::unix_secs);
                let key = self.devices[id].link.as_ref().and_then(|l| identity::verification_key(&self.ident.cert, &l.cert, ts));
                let dev = self.devices.get_mut(id).unwrap();
                dev.pair_ts = ts;
                dev.pair_deadline = Some(Instant::now() + PAIR_TIMEOUT);
                dev.view.pair = PairStatus::RequestedByPeer;
                dev.view.verification_key = key;
                eprintln!("[prism-phoned] pair request from {} ({id}), key {:?}", dev.view.name, dev.view.verification_key);
                crate::plugins::notify_pair_request(self, id);
                self.publish();
            }
            (false, PairStatus::Paired) => {
                eprintln!("[prism-phoned] {id} unpaired us");
                self.forget(id);
            }
            (false, _) => {
                let dev = self.devices.get_mut(id).unwrap();
                dev.view.pair = PairStatus::NotPaired;
                dev.view.verification_key = None;
                dev.pair_deadline = None;
                self.publish();
            }
        }
    }

    fn set_paired(&mut self, id: &str) {
        let Some(dev) = self.devices.get_mut(id) else { return };
        let Some(cert) = dev.link.as_ref().map(|l| l.cert.clone()) else { return };
        let t = Trusted {
            id: id.to_string(),
            name: dev.view.name.clone(),
            device_type: dev.view.device_type.clone(),
            cert: trust::hex(&cert),
            last_addr: dev.addr.map(|a| a.to_string()),
        };
        trust::save(&self.data_dir, &t);
        dev.trusted = Some(t);
        dev.view.paired = true;
        dev.view.pair = PairStatus::Paired;
        dev.view.verification_key = None;
        dev.pair_deadline = None;
        eprintln!("[prism-phoned] paired with {} ({id})", dev.view.name);
        crate::plugins::on_connected(self, id);
        self.publish();
    }

    fn forget(&mut self, id: &str) {
        trust::remove(&self.data_dir, id);
        if let Some(dev) = self.devices.get_mut(id) {
            dev.trusted = None;
            dev.view.paired = false;
            dev.view.pair = PairStatus::NotPaired;
            dev.view.verification_key = None;
            dev.view.battery = None;
            dev.view.network = None;
            dev.view.media = None;
            dev.view.notifications.clear();
            if dev.link.is_none() {
                self.devices.remove(id);
            }
        }
        self.publish();
    }

    /// Once a second: pair timeouts.
    pub fn tick(&mut self) {
        let expired: Vec<String> = self.devices.iter().filter(|(_, d)| d.pair_deadline.is_some_and(|t| Instant::now() >= t)).map(|(k, _)| k.clone()).collect();
        for id in expired {
            let dev = self.devices.get_mut(&id).unwrap();
            dev.pair_deadline = None;
            if !dev.view.paired {
                dev.view.pair = PairStatus::NotPaired;
                dev.view.verification_key = None;
                eprintln!("[prism-phoned] pair request with {id} timed out");
            }
            self.publish();
        }
        crate::plugins::tick(self);
    }

    // ── State stream ─────────────────────────────────────────────────

    pub fn state(&self) -> PhoneState {
        PhoneState { error: self.error.clone(), name: self.name(), devices: self.devices.values().map(|d| d.view.clone()).collect() }
    }

    pub fn subscribe(&mut self, s: UnixStream) {
        s.set_write_timeout(Some(Duration::from_millis(500))).ok();
        let mut s = s;
        if write_reply(&mut s, &PhoneReply::State { state: self.state() }) {
            self.subscribers.push(s);
        }
    }

    pub fn publish(&mut self) {
        if self.subscribers.is_empty() {
            return;
        }
        let reply = PhoneReply::State { state: self.state() };
        self.subscribers.retain_mut(|s| write_reply(s, &reply));
    }

    // ── Commands ─────────────────────────────────────────────────────

    /// The device a command is about: by id or name, else the primary one.
    pub fn resolve(&self, device: &Option<String>) -> Result<String, String> {
        if let Some(d) = device {
            if self.devices.contains_key(d) {
                return Ok(d.clone());
            }
            return self.devices.values().find(|x| x.view.name.eq_ignore_ascii_case(d)).map(|x| x.view.id.clone()).ok_or_else(|| format!("no device {d:?}"));
        }
        self.state().primary().map(|d| d.id.clone()).ok_or_else(|| "no paired phone".to_string())
    }

    fn resolve_connected(&self, device: &Option<String>) -> Result<String, String> {
        let id = self.resolve(device)?;
        let d = &self.devices[&id];
        if !d.view.paired {
            return Err(format!("{} is not paired", d.view.name));
        }
        if d.link.is_none() {
            return Err(format!("{} is not connected", d.view.name));
        }
        Ok(id)
    }

    pub fn command(&mut self, cmd: PhoneCommand) -> PhoneReply {
        match self.run(cmd) {
            Ok(message) => PhoneReply::Ok { message },
            Err(message) => PhoneReply::Error { message },
        }
    }

    fn run(&mut self, cmd: PhoneCommand) -> Result<Option<String>, String> {
        match cmd {
            PhoneCommand::Subscribe | PhoneCommand::Status => Ok(Some(serde_json::to_string(&self.state()).unwrap_or_default())),
            PhoneCommand::ReloadConfig => {
                self.cfg = Config::load();
                crate::tray::sync(self);
                self.publish();
                Ok(None)
            }
            PhoneCommand::Pair { device } => {
                let id = match &device {
                    Some(_) => self.resolve(&device)?,
                    None => self.devices.values().find(|d| !d.view.paired && d.link.is_some()).map(|d| d.view.id.clone()).ok_or("no unpaired device is connected")?,
                };
                let ts = identity::unix_secs();
                let dev = self.devices.get_mut(&id).unwrap();
                if dev.view.paired {
                    return Err(format!("{} is already paired", dev.view.name));
                }
                let Some(link) = dev.link.as_ref() else { return Err(format!("{} is not connected", dev.view.name)) };
                link.send(Packet::new(PAIR, json!({"pair": true, "timestamp": ts})));
                dev.pair_ts = ts;
                dev.pair_deadline = Some(Instant::now() + PAIR_TIMEOUT);
                dev.view.pair = PairStatus::Requested;
                dev.view.verification_key = identity::verification_key(&self.ident.cert, &link.cert, ts);
                let msg = format!("pair request sent to {}; verification key {}", dev.view.name, dev.view.verification_key.clone().unwrap_or_default());
                self.publish();
                Ok(Some(msg))
            }
            PhoneCommand::Accept { device } => {
                let id = match &device {
                    Some(_) => self.resolve(&device)?,
                    None => self.devices.values().find(|d| d.view.pair == PairStatus::RequestedByPeer).map(|d| d.view.id.clone()).ok_or("no pair request is waiting")?,
                };
                if self.devices[&id].view.pair != PairStatus::RequestedByPeer {
                    return Err("that device has not asked to pair".into());
                }
                self.send(&id, Packet::new(PAIR, json!({"pair": true})));
                self.set_paired(&id);
                Ok(None)
            }
            PhoneCommand::Reject { device } => {
                let id = match &device {
                    Some(_) => self.resolve(&device)?,
                    None => self.devices.values().find(|d| matches!(d.view.pair, PairStatus::RequestedByPeer | PairStatus::Requested)).map(|d| d.view.id.clone()).ok_or("no pair request is pending")?,
                };
                self.send(&id, Packet::new(PAIR, json!({"pair": false})));
                let dev = self.devices.get_mut(&id).unwrap();
                if !dev.view.paired {
                    dev.view.pair = PairStatus::NotPaired;
                    dev.view.verification_key = None;
                    dev.pair_deadline = None;
                }
                self.publish();
                Ok(None)
            }
            PhoneCommand::Unpair { device } => {
                let id = self.resolve(&device)?;
                self.send(&id, Packet::new(PAIR, json!({"pair": false})));
                self.forget(&id);
                Ok(None)
            }
            PhoneCommand::Ping { device } => {
                let id = self.resolve_connected(&device)?;
                self.send(&id, Packet::new("kdeconnect.ping", json!({})));
                Ok(None)
            }
            other => {
                let id = match &other {
                    PhoneCommand::CancelTransfer { .. } => String::new(),
                    PhoneCommand::Refresh | PhoneCommand::Connect { .. } => String::new(),
                    PhoneCommand::Ring { device }
                    | PhoneCommand::SendClipboard { device, .. }
                    | PhoneCommand::Media { device, .. }
                    | PhoneCommand::Seek { device, .. }
                    | PhoneCommand::NotificationAction { device, .. }
                    | PhoneCommand::NotificationReply { device, .. }
                    | PhoneCommand::NotificationDismiss { device, .. }
                    | PhoneCommand::SendFiles { device, .. }
                    | PhoneCommand::SendText { device, .. }
                    | PhoneCommand::SendUrl { device, .. } => self.resolve_connected(device)?,
                    _ => String::new(),
                };
                crate::plugins::command(self, &id, other)
            }
        }
    }
}

fn write_reply(s: &mut UnixStream, r: &PhoneReply) -> bool {
    let mut line = serde_json::to_string(r).unwrap_or_default();
    line.push('\n');
    s.write_all(line.as_bytes()).is_ok()
}
