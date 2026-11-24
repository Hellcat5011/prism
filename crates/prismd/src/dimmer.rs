//! The idle dim before the auto-lock: the screen's brightness fades to half of what it was, and
//! back when the user returns. Laptop panels step through brightnessctl in raw units; DDC/CI
//! monitors are written straight over I2C (`hardware::fade`, one step per ~60 ms, as fast as the
//! monitor takes them), else through ddcutil (slow steps, still eased over the same time).
//! Without any brightness control the gamma ramp fades instead. What it was before the dim is
//! kept in `~/.cache/prism/brightness-dimmed` until restored, so a crash or restart mid-dim does
//! not leave the screen dark.

use crate::gamma::GammaHandle;
use prism_common::hardware::fade;
use std::sync::mpsc::{channel, RecvTimeoutError, Receiver, Sender};
use std::time::{Duration, Instant};

/// Brightness while dimmed, as a share of the brightness before.
const DIM_SHARE: f64 = 0.5;
const DOWN: Duration = Duration::from_millis(3000);
const UP: Duration = Duration::from_millis(450);

pub struct Dimmer {
    tx: Sender<bool>,
}

impl Dimmer {
    pub fn start(gamma: Option<GammaHandle>) -> Dimmer {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("prismd-dimmer".into())
            .spawn(move || run(rx, gamma))
            .expect("dimmer thread");
        Dimmer { tx }
    }

    pub fn dim(&self, on: bool) {
        let _ = self.tx.send(on);
    }
}

fn marker() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/prism/brightness-dimmed")
}

/// Smoothstep: no jump at either end.
fn ease(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Move from `from` to `to` over `dur`, calling `set` whenever the value moves by `quantum`.
/// `cur` follows what was last set. Returns a message that arrived meanwhile (the fade stops).
fn fade_to(from: f64, to: f64, dur: Duration, step: Duration, quantum: f64, cur: &mut f64, rx: &Receiver<bool>, mut set: impl FnMut(f64)) -> Option<bool> {
    let start = Instant::now();
    let mut last = f64::NAN;
    loop {
        let t = (start.elapsed().as_secs_f64() / dur.as_secs_f64()).min(1.0);
        let v = ((from + (to - from) * ease(t)) / quantum).round() * quantum;
        if v != last {
            set(v);
            last = v;
            *cur = v;
        }
        if t >= 1.0 {
            return None;
        }
        match rx.recv_timeout(step) {
            Ok(m) => return Some(m),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn run(rx: Receiver<bool>, gamma: Option<GammaHandle>) {
    // Left dimmed by an earlier prismd: put it back.
    if let Some(raw) = std::fs::read_to_string(marker()).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
        eprintln!("[prismd/dim] restoring brightness {raw} left from an unfinished dim");
        let _ = fade::set(raw);
    }
    let _ = std::fs::remove_file(marker());

    // Brightness before the dim (raw units), and where the fade has got to.
    let mut base: Option<f64> = None;
    let mut cur = 0.0;
    // Gamma fallback: factor 1 = normal.
    let mut gamma_cur = 1.0;
    let mut pending: Option<bool> = None;
    loop {
        let on = match pending.take() {
            Some(m) => m,
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => return,
            },
        };
        if base.is_none() && on {
            if let Some((raw, max)) = fade::read().filter(|(_, max)| *max > 0) {
                eprintln!("[prismd/dim] dimming from {raw}/{max}");
                base = Some(raw as f64);
                cur = raw as f64;
                let _ = std::fs::write(marker(), raw.to_string());
            }
        }
        match base {
            Some(b) => {
                // Fast devices step every frame; DDC over I2C paces itself (each write waits for
                // the monitor) and ddcutil takes ~0.4 s a step.
                let step = if fade::fast() { Duration::from_millis(16) } else { Duration::ZERO };
                let set = |v: f64| {
                    if let Err(e) = fade::set(v as u32) {
                        eprintln!("[prismd/dim] {e}");
                    }
                };
                if on {
                    let target = (b * DIM_SHARE).round().max(1.0);
                    pending = fade_to(cur, target, DOWN, step, 1.0, &mut cur, &rx, set);
                } else {
                    pending = fade_to(cur, b, UP, step, 1.0, &mut cur, &rx, set);
                    if pending.is_none() {
                        base = None;
                        let _ = std::fs::remove_file(marker());
                    }
                }
            }
            None => {
                let Some(g) = &gamma else { continue };
                let target = if on { DIM_SHARE } else { 1.0 };
                let dur = if on { DOWN } else { UP };
                let mut c = gamma_cur;
                pending = fade_to(gamma_cur, target, dur, Duration::from_millis(16), 0.005, &mut c, &rx, |v| g.dim(v));
                gamma_cur = c;
            }
        }
    }
}
