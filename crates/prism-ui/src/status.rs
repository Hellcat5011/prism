// ─────────────────────────────────────────────────────────────────────────
// status.rs — background system-state worker for the control centre
//
// The render path must never block. Everything that shells out (nmcli,
// bluetoothctl, wpctl, ddcutil) or talks to prismd runs here, on one worker
// thread, and is published to the UI as an immutable snapshot. The UI wakes
// up through `Waker` when a new snapshot arrives and only reads the copy.
//
// User actions are sent as `StatusRequest`s. Slider requests are coalesced
// to the latest value. Slow actions (Wi-Fi connect, Bluetooth pairing,
// scans) run on helper threads that record their progress in `Ops` and ask
// the worker to refresh when they finish, so the worker never blocks on them.
// ─────────────────────────────────────────────────────────────────────────

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prism_common::ipc::{DaemonFlags, IpcCommand, NotifAction};
use prism_common::network::{AudioSink, BatteryStatus};
use prism_common::notifications::{Notification, NotificationStore};

use crate::nc::dbus::Bus;
use crate::nc::model::{BtBusy, BtState, NetState};
use crate::waker::Waker;

#[derive(Clone, Debug, Default)]
pub struct StatusSnapshot {
    pub net: NetState,
    pub bt: BtState,
    pub volume: u32,
    pub muted: bool,
    /// None until read (DDC/CI reads are slow) or when there is no brightness control.
    pub brightness: Option<u32>,
    pub sinks: Vec<AudioSink>,
    pub battery: Option<BatteryStatus>,
    /// power-profiles-daemon: (active, available), None when it is not running.
    pub power_profile: Option<(String, Vec<String>)>,
    /// Newest first.
    pub notifications: Vec<Notification>,
    pub flags: DaemonFlags,
    /// False until the first full refresh has completed.
    pub loaded: bool,
}

#[derive(Debug)]
pub enum StatusRequest {
    SetVolume(u32),
    SetBrightness(u32),
    SetDefaultSink(String),
    SetWifi(bool),
    ScanWifi,
    ConnectWifi { ssid: String, password: Option<String> },
    DisconnectWifi,
    ForgetWifi(String),
    SetBluetooth(bool),
    SetDiscoverable(bool),
    SetPowerProfile(String),
    /// Activate the VPN with this settings path.
    VpnUp(String),
    /// Deactivate: (settings path, active connection path).
    VpnDown(String, String),
    /// Import VPN profiles (.ovpn / WireGuard .conf) with nmcli.
    ImportVpn(Vec<std::path::PathBuf>),
    ScanBluetooth,
    PairBluetooth(String),
    CancelPair(String),
    ConnectBluetooth(String),
    DisconnectBluetooth(String),
    ForgetBluetooth(String),
    /// Forwarded to prismd (silent, caffeinate, dismiss, clear).
    Daemon(IpcCommand),
    /// Copy text to the clipboard without recording it in clipboard history.
    Copy(String),
    RefreshAll,
}

pub struct StatusHandle {
    pub rx: Receiver<StatusSnapshot>,
    pub tx: Sender<StatusRequest>,
}

/// Fallback refresh; NetworkManager / BlueZ signals trigger refreshes as things change.
const INTERVAL: Duration = Duration::from_secs(5);
/// QML NcNetwork scanTimer: the list is re-read this long after a rescan starts.
const WIFI_SCAN_MS: u64 = 3000;
/// QML NcBluetoothPage scanTimer.
const BT_SCAN_SECS: u64 = 15;

/// Progress of background operations, shared with helper threads.
#[derive(Default)]
struct Ops {
    wifi_scan_until: Option<Instant>,
    connecting: Option<String>,
    wifi_error: Option<(String, String)>,
    bt_busy: Vec<(String, BtBusy)>,
    bt_failed: Vec<String>,
    vpn_busy: Vec<String>,
    vpn_msg: Option<(String, bool)>,
}

pub fn spawn(waker: Waker) -> StatusHandle {
    let (snap_tx, snap_rx) = std::sync::mpsc::channel::<StatusSnapshot>();
    let (req_tx, req_rx) = std::sync::mpsc::channel::<StatusRequest>();
    let again = req_tx.clone();
    std::thread::Builder::new()
        .name("prism-status".into())
        .spawn(move || worker(waker, snap_tx, req_rx, again))
        .expect("failed to spawn status worker");
    StatusHandle { rx: snap_rx, tx: req_tx }
}

fn refresh(snap: &mut StatusSnapshot, ops: &Ops, bus: Option<&Bus>) {
    if let Some(bus) = bus {
        bus.refresh_network(&mut snap.net);
        bus.refresh_bluetooth(&mut snap.bt);
        snap.power_profile = bus.power_profile();
        bus.refresh_vpns(&mut snap.net);
    }
    let (vol, muted) = prism_common::hardware::get_volume().unwrap_or((snap.volume, snap.muted));
    snap.volume = vol;
    snap.muted = muted;
    snap.sinks = prism_common::network::list_audio_sinks();
    snap.battery = prism_common::network::get_battery_status();
    snap.notifications = NotificationStore::load().notifications;
    if let Ok(Some(json)) = prism_common::ipc::request(&IpcCommand::Notif(NotifAction::State)) {
        snap.flags = serde_json::from_str(&json).unwrap_or_default();
    }
    apply_ops(snap, ops);
}

fn apply_ops(snap: &mut StatusSnapshot, ops: &Ops) {
    snap.net.scanning = ops.wifi_scan_until.map(|t| Instant::now() < t).unwrap_or(false);
    snap.net.connecting = ops.connecting.clone();
    snap.net.error = ops.wifi_error.clone();
    snap.bt.busy = ops.bt_busy.clone();
    snap.bt.failed = ops.bt_failed.clone();
    snap.net.vpn_busy = ops.vpn_busy.clone();
    snap.net.vpn_msg = ops.vpn_msg.clone();
}

fn worker(waker: Waker, out: Sender<StatusSnapshot>, req_rx: Receiver<StatusRequest>, again: Sender<StatusRequest>) {
    let ops = Arc::new(Mutex::new(Ops::default()));
    let bus = Bus::system().map(Arc::new);
    if let Some(b) = &bus {
        b.register_agent();
        let again2 = again.clone();
        Bus::watch(move || again2.send(StatusRequest::RefreshAll).is_ok());
    } else {
        eprintln!("[prism-ui/status] no system D-Bus: network and Bluetooth unavailable");
    }
    let mut snap = StatusSnapshot::default();
    let mut last = Instant::now() - INTERVAL;
    let mut brightness_read = false;
    loop {
        if last.elapsed() >= INTERVAL {
            last = Instant::now();
            refresh(&mut snap, &ops.lock().unwrap(), bus.as_deref());
            snap.loaded = true;
            if out.send(snap.clone()).is_err() {
                return; // UI is gone
            }
            waker.wake();
            if !brightness_read {
                brightness_read = true;
                // Never read DDC here: it stalls the compositor while the card animates in.
                snap.brightness = prism_common::hardware::cached_brightness();
                if out.send(snap.clone()).is_err() {
                    return;
                }
                waker.wake();
            }
        }

        let first = match req_rx.recv_timeout(INTERVAL.saturating_sub(last.elapsed())) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Drain everything queued and keep only the latest slider values.
        let mut volume = None;
        let mut brightness = None;
        let mut others = Vec::new();
        for req in std::iter::once(first).chain(req_rx.try_iter()) {
            match req {
                StatusRequest::SetVolume(v) => volume = Some(v),
                StatusRequest::SetBrightness(v) => brightness = Some(v),
                other => others.push(other),
            }
        }
        if let Some(v) = volume {
            let _ = prism_common::hardware::set_volume(v);
            snap.volume = v;
        }
        if let Some(v) = brightness {
            if prism_common::hardware::set_brightness(v).is_ok() {
                snap.brightness = Some(v);
            }
        }
        let mut full_refresh = false;
        for req in others {
            full_refresh |= handle(req, &ops, &again, bus.as_ref());
        }
        if full_refresh {
            last = Instant::now() - INTERVAL; // refresh at the top of the loop
        } else {
            apply_ops(&mut snap, &ops.lock().unwrap());
            if out.send(snap.clone()).is_err() {
                return;
            }
            waker.wake();
        }
    }
}

/// Run `cmd` in the background and refresh when it finishes.
fn background(again: &Sender<StatusRequest>, f: impl FnOnce() + Send + 'static) {
    let again = again.clone();
    std::thread::spawn(move || {
        f();
        let _ = again.send(StatusRequest::RefreshAll);
    });
}

/// Handle one request; true when a full refresh should follow.
fn handle(req: StatusRequest, ops: &Arc<Mutex<Ops>>, again: &Sender<StatusRequest>, bus: Option<&Arc<Bus>>) -> bool {
    match req {
        StatusRequest::SetVolume(_) | StatusRequest::SetBrightness(_) => false,
        StatusRequest::RefreshAll => true,
        StatusRequest::SetDefaultSink(id) => {
            let _ = prism_common::network::set_default_sink(&id);
            true
        }
        StatusRequest::Daemon(cmd) => {
            if let Err(e) = prism_common::ipc::request(&cmd) {
                eprintln!("[prism-ui/status] prismd: {e}");
            }
            true
        }
        StatusRequest::Copy(text) => {
            prism_common::wlclip::copy_text(&text, false);
            false
        }
        other => match bus {
            Some(bus) => handle_bus(other, ops, again, Arc::clone(bus)),
            None => false,
        },
    }
}

/// Network and Bluetooth requests (NetworkManager / BlueZ).
fn handle_bus(req: StatusRequest, ops: &Arc<Mutex<Ops>>, again: &Sender<StatusRequest>, bus: Arc<Bus>) -> bool {
    let log = |what: &str, r: Result<(), String>| {
        if let Err(e) = r {
            eprintln!("[prism-ui/status] {what}: {e}");
        }
    };
    match req {
        StatusRequest::SetWifi(on) => {
            log("wifi radio", bus.set_wifi(on));
            true
        }
        StatusRequest::ScanWifi => {
            {
                let mut o = ops.lock().unwrap();
                if o.wifi_scan_until.map(|t| Instant::now() < t).unwrap_or(false) {
                    return false;
                }
                o.wifi_scan_until = Some(Instant::now() + Duration::from_millis(WIFI_SCAN_MS));
            }
            bus.scan_wifi();
            let again2 = again.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(WIFI_SCAN_MS));
                let _ = again2.send(StatusRequest::RefreshAll);
            });
            false
        }
        StatusRequest::ConnectWifi { ssid, password } => {
            {
                let mut o = ops.lock().unwrap();
                if o.connecting.is_some() {
                    return false;
                }
                o.connecting = Some(ssid.clone());
                o.wifi_error = None;
            }
            let ops = Arc::clone(ops);
            background(again, move || {
                let result = bus.connect_wifi(&ssid, password.as_deref());
                let mut o = ops.lock().unwrap();
                o.connecting = None;
                if let Err(e) = result {
                    o.wifi_error = Some((ssid, e));
                }
            });
            true
        }
        StatusRequest::DisconnectWifi => {
            bus.disconnect_wifi();
            true
        }
        StatusRequest::ForgetWifi(ssid) => {
            bus.forget_wifi(&ssid);
            true
        }
        StatusRequest::SetBluetooth(on) => {
            log("bluetooth power", bus.set_bluetooth(on));
            true
        }
        StatusRequest::SetDiscoverable(on) => {
            log("discoverable", bus.set_discoverable(on));
            true
        }
        StatusRequest::SetPowerProfile(p) => {
            log("power profile", bus.set_power_profile(&p));
            true
        }
        StatusRequest::VpnUp(path) | StatusRequest::VpnDown(path, _) if ops.lock().unwrap().vpn_busy.contains(&path) => false,
        StatusRequest::VpnUp(path) => {
            {
                let mut o = ops.lock().unwrap();
                o.vpn_busy.push(path.clone());
                o.vpn_msg = None;
            }
            let ops = Arc::clone(ops);
            background(again, move || {
                // Activation returns at once; the state then follows NetworkManager's signals.
                let r = bus.vpn_up(&path);
                let mut o = ops.lock().unwrap();
                o.vpn_busy.retain(|p| *p != path);
                if let Err(e) = r {
                    o.vpn_msg = Some((e, true));
                }
            });
            true
        }
        StatusRequest::VpnDown(path, active) => {
            ops.lock().unwrap().vpn_busy.push(path.clone());
            let ops = Arc::clone(ops);
            background(again, move || {
                let r = bus.vpn_down(&active);
                let mut o = ops.lock().unwrap();
                o.vpn_busy.retain(|p| *p != path);
                if let Err(e) = r {
                    o.vpn_msg = Some((e, true));
                }
            });
            true
        }
        StatusRequest::ImportVpn(files) => {
            ops.lock().unwrap().vpn_msg = Some((format!("Importing {} file{}\u{2026}", files.len(), if files.len() == 1 { "" } else { "s" }), false));
            let ops = Arc::clone(ops);
            background(again, move || {
                let msg = import_vpns(&files);
                ops.lock().unwrap().vpn_msg = Some(msg);
            });
            true
        }
        StatusRequest::ScanBluetooth => {
            bus.scan_bluetooth(BT_SCAN_SECS);
            true
        }
        StatusRequest::PairBluetooth(mac) => {
            {
                let mut o = ops.lock().unwrap();
                o.bt_failed.retain(|m| *m != mac);
                o.bt_busy.retain(|(m, _)| *m != mac);
                o.bt_busy.push((mac.clone(), BtBusy::Pairing));
            }
            let ops = Arc::clone(ops);
            background(again, move || {
                let result = bus.pair(&mac);
                let mut o = ops.lock().unwrap();
                let cancelled = !o.bt_busy.iter().any(|(m, _)| *m == mac);
                o.bt_busy.retain(|(m, _)| *m != mac);
                if let Err(e) = result {
                    eprintln!("[prism-ui/status] pairing {mac}: {e}");
                    if !cancelled {
                        o.bt_failed.push(mac.clone());
                    }
                }
            });
            true
        }
        StatusRequest::CancelPair(mac) => {
            ops.lock().unwrap().bt_busy.retain(|(m, _)| *m != mac);
            bus.cancel_pair(&mac);
            true
        }
        StatusRequest::ConnectBluetooth(mac) => {
            ops.lock().unwrap().bt_busy.push((mac.clone(), BtBusy::Connecting));
            let ops = Arc::clone(ops);
            background(again, move || {
                let r = bus.connect_bt(&mac);
                ops.lock().unwrap().bt_busy.retain(|(m, _)| *m != mac);
                if let Err(e) = r {
                    eprintln!("[prism-ui/status] connecting {mac}: {e}");
                }
            });
            true
        }
        StatusRequest::DisconnectBluetooth(mac) => {
            log("disconnect", bus.disconnect_bt(&mac));
            true
        }
        StatusRequest::ForgetBluetooth(mac) => {
            log("forget", bus.forget_bt(&mac));
            true
        }
        _ => false,
    }
}

/// Import VPN profiles with nmcli (the VPN plugins' importers run in the client); WireGuard
/// files are recognised by their [Interface] section. Returns (summary, any failed).
pub fn import_vpns(files: &[std::path::PathBuf]) -> (String, bool) {
    let mut ok = 0;
    let mut failed: Vec<String> = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let kind = if text.contains("[Interface]") { "wireguard" } else { "openvpn" };
        let out = std::process::Command::new("nmcli").args(["connection", "import", "type", kind, "file"]).arg(f).output();
        match out {
            Ok(o) if o.status.success() => ok += 1,
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("").trim().to_string();
                eprintln!("[prism-ui/vpn] import {}: {err}", f.display());
                failed.push(f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            }
            Err(e) => {
                return (format!("nmcli: {e}"), true);
            }
        }
    }
    if failed.is_empty() {
        (format!("Imported {ok} VPN profile{}", if ok == 1 { "" } else { "s" }), false)
    } else {
        (format!("Imported {ok}, failed {}: {}", failed.len(), failed.join(", ")), true)
    }
}
