use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use wayland_client::protocol::{wl_registry, wl_seat};
use crate::wake;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

/// What the idle monitor should do; sent again whenever the config changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdleSettings {
    /// Lock after `timeout_mins` of inactivity (Config::manage_idle).
    pub auto_lock: bool,
    pub timeout_mins: u32,
}

pub struct IdleHandle {
    tx: wake::Sender<IdleSettings>,
}

impl IdleHandle {
    pub fn update(&self, settings: IdleSettings) {
        self.tx.send(settings);
    }
}

struct IdleState {
    notifier: Option<ExtIdleNotifierV1>,
    seat: Option<wl_seat::WlSeat>,
    notification: Option<ExtIdleNotificationV1>,
    settings: IdleSettings,
    is_idle: Arc<AtomicBool>,
    on_idle: Arc<dyn Fn() + Send + Sync>,
}

impl IdleState {
    /// (Re)create the idle notification for the current settings.
    fn rearm(&mut self, qh: &QueueHandle<Self>) {
        if let Some(n) = self.notification.take() {
            n.destroy();
        }
        self.is_idle.store(false, Ordering::SeqCst);
        if !self.settings.auto_lock {
            eprintln!("[prismd/idle] auto-lock off");
            return;
        }
        if let (Some(notifier), Some(seat)) = (&self.notifier, &self.seat) {
            let ms = self.settings.timeout_mins.max(1).saturating_mul(60_000);
            eprintln!("[prismd/idle] auto-lock after {} min", self.settings.timeout_mins.max(1));
            self.notification = Some(notifier.get_idle_notification(ms, seat, qh, ()));
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for IdleState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version: _,
        } = event
        {
            if interface == "wl_seat" && state.seat.is_none() {
                state.seat = Some(registry.bind::<wl_seat::WlSeat, _, _>(name, 1, qh, ()));
            } else if interface == "ext_idle_notifier_v1" && state.notifier.is_none() {
                eprintln!("[prismd/idle] Bound ext_idle_notifier_v1");
                state.notifier = Some(registry.bind::<ExtIdleNotifierV1, _, _>(name, 1, qh, ()));
            }
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for IdleState {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for IdleState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: <wl_seat::WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<ExtIdleNotificationV1, ()> for IdleState {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                eprintln!("[prismd/idle] idle for {} min", state.settings.timeout_mins.max(1));
                state.is_idle.store(true, Ordering::SeqCst);
                (state.on_idle)();
            }
            ext_idle_notification_v1::Event::Resumed => {
                eprintln!("[prismd/idle] User activity resumed.");
                state.is_idle.store(false, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

/// Spawns the Wayland idle monitor thread if a Wayland display is available. `on_idle`
/// runs on the monitor thread when the timeout passes while auto-lock is on.
pub fn start_idle_monitor(settings: IdleSettings, is_idle_flag: Arc<AtomicBool>, on_idle: Box<dyn Fn() + Send + Sync>) -> Option<IdleHandle> {
    if env::var("WAYLAND_DISPLAY").is_err() && env::var("WAYLAND_SOCKET").is_err() {
        eprintln!("[prismd/idle] No active WAYLAND_DISPLAY found; idle monitoring disabled.");
        return None;
    }
    let (tx, rx) = wake::channel::<IdleSettings>();
    let on_idle: Arc<dyn Fn() + Send + Sync> = Arc::from(on_idle);

    // Reconnects when the compositor goes away (session restart), keeping the settings.
    thread::spawn(move || {
        let mut settings = settings;
        let mut last_error = String::new();
        loop {
            if let Err(e) = session(&rx, &mut settings, &is_idle_flag, &on_idle) {
                if e != last_error {
                    eprintln!("[prismd/idle] {e}; retrying until the compositor is back");
                    last_error = e;
                }
            }
            is_idle_flag.store(false, Ordering::SeqCst);
            thread::sleep(std::time::Duration::from_secs(2));
        }
    });
    Some(IdleHandle { tx })
}

/// One connection's lifetime; Err when it is lost or cannot be made.
fn session(
    rx: &wake::Receiver<IdleSettings>,
    settings: &mut IdleSettings,
    is_idle: &Arc<AtomicBool>,
    on_idle: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("cannot connect to Wayland: {e}"))?;
    let mut event_queue: EventQueue<IdleState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let mut state = IdleState {
        notifier: None,
        seat: None,
        notification: None,
        settings: *settings,
        is_idle: Arc::clone(is_idle),
        on_idle: Arc::clone(on_idle),
    };
    let _registry = conn.display().get_registry(&qh, ());
    event_queue.roundtrip(&mut state).map_err(|e| format!("registry roundtrip failed: {e}"))?;
    event_queue.roundtrip(&mut state).map_err(|e| format!("registry roundtrip failed: {e}"))?;
    if state.notifier.is_none() || state.seat.is_none() {
        eprintln!("[prismd/idle] Compositor does not support ext_idle_notifier_v1 or no wl_seat found.");
    }
    eprintln!("[prismd/idle] connected");
    state.rearm(&qh);
    loop {
        if let Some(new) = rx.drain().into_iter().last() {
            if new != state.settings {
                state.settings = new;
                *settings = new;
                state.rearm(&qh);
            }
        }
        wake::dispatch(&mut event_queue, &mut state, rx.fd(), -1).map_err(|e| format!("Wayland connection lost: {e}"))?;
    }
}
