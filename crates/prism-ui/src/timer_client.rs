//! prismd's timer and stopwatch for the desktop Timer widget: read over IPC on a worker thread
//! (every second while something counts, every few seconds otherwise, only while the widget is
//! shown), and the widget's buttons sent back.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

use prism_common::ipc::{IpcCommand, TimerAction, TimerState};

use crate::waker::Waker;

static STATE: Mutex<(u64, TimerState)> = Mutex::new((0, TimerState { end_ms: None, paused_ms: None, total_ms: 0, sw_start_ms: None, sw_acc_ms: 0 }));
static WANTED: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

pub fn snapshot() -> TimerState {
    STATE.lock().unwrap().1
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

fn store(t: TimerState) {
    let mut s = STATE.lock().unwrap();
    if s.1 != t {
        s.0 += 1;
        s.1 = t;
        drop(s);
        if let Some(w) = WAKER.lock().unwrap().as_ref() {
            w.wake();
        }
    }
}

fn request(a: TimerAction) -> Option<TimerState> {
    let json = prism_common::ipc::request(&IpcCommand::Timer(a)).ok()??;
    serde_json::from_str(&json).ok()
}

/// Follow the timer while the widget is shown.
pub fn set_wanted(on: bool, waker: &Waker) {
    WANTED.store(on, Ordering::Relaxed);
    if !on {
        return;
    }
    *WAKER.lock().unwrap() = Some(waker.clone());
    START.call_once(|| {
        std::thread::Builder::new()
            .name("prism-timer".into())
            .spawn(|| loop {
                let ticking = snapshot().ticking();
                if WANTED.load(Ordering::Relaxed) {
                    if let Some(t) = request(TimerAction::State) {
                        store(t);
                    }
                }
                std::thread::sleep(Duration::from_millis(if ticking { 1000 } else { 3000 }));
            })
            .ok();
    });
}

/// A button on the widget.
pub fn send(a: TimerAction) {
    std::thread::spawn(move || {
        if let Some(t) = request(a) {
            store(t);
        }
    });
}
