// ─────────────────────────────────────────────────────────────────────────
// status.rs — background system-state worker
//
// The render path must never block. Everything that shells out (nmcli,
// bluetoothctl, wpctl, brightnessctl, playerctl) or reads stores from disk
// runs here, on one worker thread, and is published to the UI as an
// immutable snapshot. The UI wakes up through `Waker` when a new snapshot
// arrives and only ever reads the cached copy while drawing.
//
// User actions that change state (volume / brightness drags, toggles,
// rescans) are sent as `StatusRequest`s. Slider requests are coalesced to
// the latest value, so a 1000 Hz mouse drag results in at most one
// `wpctl` / `brightnessctl` call per worker cycle instead of one per event.
// ─────────────────────────────────────────────────────────────────────────

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use prism_common::network::{AudioSink, BatteryStatus, BluetoothDevice, BluetoothStatus, NetworkStatus, WifiNetwork};
use prism_common::notifications::NotificationStore;

use crate::waker::Waker;

/// What the worker should keep fresh. Chosen once per UI mode.
#[derive(Clone, Copy, Debug)]
pub struct StatusScope {
    /// Notification-center data: network, bluetooth, audio, brightness, battery, notifications.
    pub control_center: bool,
}

#[derive(Clone, Debug, Default)]
pub struct StatusSnapshot {
    pub battery: Option<BatteryStatus>,
    pub network: NetworkStatus,
    pub bluetooth: BluetoothStatus,
    pub volume: u32,
    pub muted: bool,
    pub brightness: u32,
    pub notifications: NotificationStore,
    pub wifi_networks: Vec<WifiNetwork>,
    pub bt_devices: Vec<BluetoothDevice>,
    pub audio_sinks: Vec<AudioSink>,
    /// False until the first full refresh has completed.
    pub loaded: bool,
}

#[derive(Debug)]
pub enum StatusRequest {
    SetVolume(u32),
    SetBrightness(u32),
    SetDefaultSink(String),
    ToggleWifi,
    ToggleBluetooth,
    RescanWifi,
    RescanBluetooth,
    RefreshSinks,
    ConnectWifi { ssid: String, password: Option<String> },
    ConnectBluetooth(String),
    DisconnectBluetooth(String),
    /// Refresh everything now (e.g. after an action performed elsewhere).
    RefreshAll,
}

pub struct StatusHandle {
    pub rx: Receiver<StatusSnapshot>,
    pub tx: Sender<StatusRequest>,
}

const FAST_INTERVAL: Duration = Duration::from_secs(2);
const SLOW_INTERVAL: Duration = Duration::from_secs(15);

pub fn spawn(scope: StatusScope, waker: Waker) -> StatusHandle {
    let (snap_tx, snap_rx) = std::sync::mpsc::channel::<StatusSnapshot>();
    let (req_tx, req_rx) = std::sync::mpsc::channel::<StatusRequest>();

    std::thread::Builder::new()
        .name("prism-status".into())
        .spawn(move || worker(scope, waker, snap_tx, req_rx))
        .expect("failed to spawn status worker");

    StatusHandle { rx: snap_rx, tx: req_tx }
}

fn worker(scope: StatusScope, waker: Waker, out: Sender<StatusSnapshot>, req_rx: Receiver<StatusRequest>) {
    let mut snap = StatusSnapshot::default();
    let mut last_fast = Instant::now() - FAST_INTERVAL;
    let mut last_slow = Instant::now() - SLOW_INTERVAL;

    loop {
        let now = Instant::now();
        let mut changed = false;

        if scope.control_center && now.duration_since(last_fast) >= FAST_INTERVAL {
            last_fast = now;
            refresh_fast(&mut snap);
            changed = true;
        }
        if scope.control_center && now.duration_since(last_slow) >= SLOW_INTERVAL {
            last_slow = now;
            snap.wifi_networks = prism_common::network::scan_wifi_networks();
            snap.bt_devices = prism_common::network::scan_bluetooth_devices();
            snap.audio_sinks = prism_common::network::list_audio_sinks();
            changed = true;
        }

        if changed {
            snap.loaded = true;
            if out.send(snap.clone()).is_err() {
                return; // UI is gone
            }
            waker.wake();
        }

        // Sleep until the next periodic refresh, waking early for requests.
        let mut next = Duration::from_secs(60);
        if scope.control_center {
            next = next.min(FAST_INTERVAL.saturating_sub(last_fast.elapsed()));
        }

        let first = match req_rx.recv_timeout(next) {
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
            let _ = prism_common::hardware::set_brightness(v);
            snap.brightness = v;
        }
        for req in others {
            match req {
                StatusRequest::SetDefaultSink(id) => {
                    let _ = prism_common::network::set_default_sink(&id);
                    snap.audio_sinks = prism_common::network::list_audio_sinks();
                }
                StatusRequest::ToggleWifi => {
                    let _ = prism_common::network::toggle_wifi();
                    snap.wifi_networks = prism_common::network::scan_wifi_networks();
                    snap.network = prism_common::network::get_network_status();
                }
                StatusRequest::ToggleBluetooth => {
                    let _ = prism_common::network::toggle_bluetooth();
                    snap.bt_devices = prism_common::network::scan_bluetooth_devices();
                    snap.bluetooth = prism_common::network::get_bluetooth_status();
                }
                StatusRequest::RescanWifi => snap.wifi_networks = prism_common::network::scan_wifi_networks(),
                StatusRequest::RescanBluetooth => snap.bt_devices = prism_common::network::scan_bluetooth_devices(),
                StatusRequest::RefreshSinks => snap.audio_sinks = prism_common::network::list_audio_sinks(),
                StatusRequest::ConnectWifi { ssid, password } => {
                    let _ = prism_common::network::connect_wifi(&ssid, password.as_deref());
                    snap.wifi_networks = prism_common::network::scan_wifi_networks();
                    snap.network = prism_common::network::get_network_status();
                }
                StatusRequest::ConnectBluetooth(mac) => {
                    let _ = prism_common::network::connect_bluetooth_device(&mac);
                    snap.bt_devices = prism_common::network::scan_bluetooth_devices();
                    snap.bluetooth = prism_common::network::get_bluetooth_status();
                }
                StatusRequest::DisconnectBluetooth(mac) => {
                    let _ = prism_common::network::disconnect_bluetooth_device(&mac);
                    snap.bt_devices = prism_common::network::scan_bluetooth_devices();
                    snap.bluetooth = prism_common::network::get_bluetooth_status();
                }
                StatusRequest::RefreshAll => {
                    refresh_fast(&mut snap);
                    last_fast = Instant::now();
                }
                StatusRequest::SetVolume(_) | StatusRequest::SetBrightness(_) => unreachable!(),
            }
        }

        snap.loaded = true;
        if out.send(snap.clone()).is_err() {
            return;
        }
        waker.wake();
    }
}

fn refresh_fast(snap: &mut StatusSnapshot) {
    snap.battery = prism_common::network::get_battery_status();
    snap.network = prism_common::network::get_network_status();
    snap.bluetooth = prism_common::network::get_bluetooth_status();
    let (vol, muted) = prism_common::hardware::get_volume().unwrap_or((snap.volume, snap.muted));
    snap.volume = vol;
    snap.muted = muted;
    snap.brightness = prism_common::hardware::get_brightness().unwrap_or(snap.brightness);
    snap.notifications = NotificationStore::load();
}
