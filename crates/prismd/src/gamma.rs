//! wlr-gamma-control: applies a colour temperature to every output (port of the QML shell's
//! gamma-ctl helper, now a thread inside prismd). `set(Some(k))` applies k kelvin;
//! `set(None)` releases gamma control so the compositor restores the original ramps.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::thread;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1;
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_v1::{self, ZwlrGammaControlV1};

use crate::wake;

/// Computes RGB multiplier ratios for a given color temperature in Kelvin (1000K - 10000K).
/// Based on Tanner Helland's algorithm.
pub fn color_temp_to_rgb(kelvin: f64) -> (f64, f64, f64) {
    let temp = (kelvin / 100.0).clamp(10.0, 100.0);

    let r = if temp <= 66.0 {
        255.0
    } else {
        let r = temp - 60.0;
        let r = 329.698727446 * r.powf(-0.1332047592);
        r.clamp(0.0, 255.0)
    };

    let g = if temp <= 66.0 {
        let g = temp;
        let g = 99.4708025861 * g.ln() - 161.1195681661;
        g.clamp(0.0, 255.0)
    } else {
        let g = temp - 60.0;
        let g = 288.1221695283 * g.powf(-0.0755148492);
        g.clamp(0.0, 255.0)
    };

    let b = if temp >= 66.0 {
        255.0
    } else if temp <= 19.0 {
        0.0
    } else {
        let b = temp - 10.0;
        let b = 138.5177312231 * b.ln() - 305.0447927307;
        b.clamp(0.0, 255.0)
    };

    (r / 255.0, g / 255.0, b / 255.0)
}

/// Retry interval for outputs whose gamma control failed (another client held it).
const RETRY: Duration = Duration::from_secs(10);

pub struct GammaHandle {
    tx: wake::Sender<Option<u32>>,
}

impl GammaHandle {
    pub fn set(&self, kelvin: Option<u32>) {
        self.tx.send(kelvin);
    }
}

struct Output {
    output: wl_output::WlOutput,
    name: String,
    control: Option<ZwlrGammaControlV1>,
    size: u32,
    applied: Option<u32>,
    failed_at: Option<Instant>,
}

struct State {
    manager: Option<ZwlrGammaControlManagerV1>,
    outputs: HashMap<u32, Output>,
    target: Option<u32>,
}

/// Starts the gamma thread; None without Wayland.
pub fn start() -> Option<GammaHandle> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("WAYLAND_SOCKET").is_none() {
        return None;
    }
    let (tx, rx) = wake::channel::<Option<u32>>();
    thread::spawn(move || {
        let conn = match Connection::connect_to_env() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[prismd/gamma] cannot connect to Wayland: {e}");
                return;
            }
        };
        let mut queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let _registry = conn.display().get_registry(&qh, ());
        let mut st = State { manager: None, outputs: HashMap::new(), target: None };
        if queue.roundtrip(&mut st).is_err() {
            return;
        }
        if st.manager.is_none() {
            eprintln!("[prismd/gamma] compositor lacks zwlr_gamma_control_manager_v1; blue light unavailable");
            // Keep draining requests so senders never block on anything.
            loop {
                let _ = wake::dispatch(&mut queue, &mut st, rx.fd(), -1);
                rx.drain();
            }
        }
        loop {
            if let Some(last) = rx.drain().into_iter().last() {
                st.target = last;
            }
            st.sync(&qh);
            let retry_pending = st.outputs.values().any(|o| o.failed_at.is_some());
            let timeout = if retry_pending && st.target.is_some() { RETRY.as_millis() as i32 } else { -1 };
            if let Err(e) = wake::dispatch(&mut queue, &mut st, rx.fd(), timeout) {
                eprintln!("[prismd/gamma] Wayland connection lost: {e}");
                return;
            }
        }
    });
    Some(GammaHandle { tx })
}

impl State {
    /// Bring every output in line with `target`.
    fn sync(&mut self, qh: &QueueHandle<State>) {
        let Some(manager) = self.manager.clone() else { return };
        for (id, o) in self.outputs.iter_mut() {
            match self.target {
                None => {
                    if let Some(c) = o.control.take() {
                        c.destroy();
                    }
                    o.size = 0;
                    o.applied = None;
                    o.failed_at = None;
                }
                Some(k) => {
                    if o.control.is_none() {
                        if o.failed_at.is_some_and(|t| t.elapsed() < RETRY) {
                            continue;
                        }
                        o.control = Some(manager.get_gamma_control(&o.output, qh, *id));
                        continue; // applied once gamma_size arrives
                    }
                    if o.size > 0 && o.applied != Some(k) {
                        apply(o, k);
                    }
                }
            }
        }
    }
}

fn apply(o: &mut Output, kelvin: u32) {
    let Some(control) = &o.control else { return };
    let size = o.size as usize;
    if size < 2 {
        return;
    }
    let (rm, gm, bm) = color_temp_to_rgb(kelvin as f64);
    let mut ramp: Vec<u16> = Vec::with_capacity(size * 3);
    for mult in [rm, gm, bm] {
        ramp.extend((0..size).map(|i| (i as f64 / (size - 1) as f64 * mult * u16::MAX as f64).round() as u16));
    }
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ramp.as_ptr() as *const u8, ramp.len() * 2) };
    let fd = unsafe { libc::memfd_create(c"prism-gamma".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        eprintln!("[prismd/gamma] memfd_create failed: {}", std::io::Error::last_os_error());
        return;
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    use std::io::{Seek, Write};
    if file.write_all(bytes).and_then(|_| file.rewind()).is_err() {
        eprintln!("[prismd/gamma] writing gamma ramp failed");
        return;
    }
    control.set_gamma(file.as_fd());
    o.applied = Some(kelvin);
    eprintln!("[prismd/gamma] {} -> {kelvin}K", o.name);
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(st: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } => {
                if interface == "wl_output" {
                    let output = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, name);
                    st.outputs.insert(name, Output { output, name: format!("output {name}"), control: None, size: 0, applied: None, failed_at: None });
                    st.sync(qh);
                } else if interface == "zwlr_gamma_control_manager_v1" {
                    st.manager = Some(registry.bind(name, 1, qh, ()));
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(o) = st.outputs.remove(&name) {
                    if let Some(c) = o.control {
                        c.destroy();
                    }
                    if o.output.version() >= 3 {
                        o.output.release();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(st: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event, id: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Name { name } = event {
            if let Some(o) = st.outputs.get_mut(id) {
                o.name = name;
            }
        }
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrGammaControlManagerV1, _: <ZwlrGammaControlManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrGammaControlV1, u32> for State {
    fn event(st: &mut Self, control: &ZwlrGammaControlV1, event: zwlr_gamma_control_v1::Event, id: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let target = st.target;
        let Some(o) = st.outputs.get_mut(id) else { return };
        if o.control.as_ref() != Some(control) {
            return; // a control we already replaced
        }
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => {
                o.size = size;
                o.failed_at = None;
                if let Some(k) = target {
                    apply(o, k);
                }
            }
            zwlr_gamma_control_v1::Event::Failed => {
                eprintln!("[prismd/gamma] gamma control failed on {} (another client may hold it); retrying in {}s", o.name, RETRY.as_secs());
                if let Some(c) = o.control.take() {
                    c.destroy();
                }
                o.size = 0;
                o.applied = None;
                o.failed_at = Some(Instant::now());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_temp_bounds() {
        for k in [1000.0, 4000.0, 6500.0, 10000.0] {
            let (r, g, b) = color_temp_to_rgb(k);
            for c in [r, g, b] {
                assert!((0.0..=1.0).contains(&c), "{k}K gave {c}");
            }
        }
        // Warmer means less blue.
        assert!(color_temp_to_rgb(3000.0).2 < color_temp_to_rgb(6500.0).2);
    }
}
