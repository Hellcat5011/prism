//! Applies Settings > Display (`Config::outputs`) through wlr-output-management. At login the
//! saved settings are applied as they are; a change made later asks to keep it and reverts
//! after 15 seconds otherwise (a mode the monitor cannot show leaves the screen black).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use prism_common::outputs::{self, OutputSettings};
use prism_common::Config;

static LAST: Mutex<Option<Vec<OutputSettings>>> = Mutex::new(None);
const CONFIRM_SECS: u64 = 15;
/// The latest change; an older one still waiting for its answer leaves the decision to it.
static LATEST: AtomicU64 = AtomicU64::new(0);
/// What an unconfirmed change goes back to: (live outputs, saved settings). Changes made while
/// one is unconfirmed share it, so a revert returns to the last state that was kept.
static BASELINE: Mutex<Option<(Vec<OutputSettings>, Vec<OutputSettings>)>> = Mutex::new(None);

pub fn apply(cfg: &Config) {
    let mut last = LAST.lock().unwrap();
    if last.as_ref() == Some(&cfg.outputs) {
        return;
    }
    let startup = last.is_none();
    let saved_before = last.replace(cfg.outputs.clone()).unwrap_or_default();
    if cfg.outputs.is_empty() {
        return;
    }
    let new = cfg.outputs.clone();
    let change = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
    let is_latest = move || LATEST.load(Ordering::SeqCst) == change;
    std::thread::spawn(move || {
        let (live, saved_before) = {
            // Held while applying, so changes reach the compositor one at a time and in order.
            let mut baseline = BASELINE.lock().unwrap();
            if !is_latest() {
                return;
            }
            let base = baseline.get_or_insert_with(|| (live_outputs(), saved_before)).clone();
            if let Err(e) = outputs::apply(&new) {
                eprintln!("[prismd/displays] {e}");
                *baseline = None;
                drop(baseline);
                if !startup {
                    restore_config(base.1);
                    crate::obex::ask_detached("Display settings not applied", &e, "dialog-error");
                }
                return;
            }
            if startup {
                *baseline = None;
                return;
            }
            base
        };
        let conn = zbus::blocking::Connection::session().ok();
        let keep = conn.as_ref().and_then(|c| {
            crate::obex::ask(c, "Keep these display settings?", &format!("They are undone in {CONFIRM_SECS} seconds."), "video-display", &["keep", "Keep", "revert", "Revert"], Some(Duration::from_secs(CONFIRM_SECS)))
        });
        let mut baseline = BASELINE.lock().unwrap();
        if !is_latest() {
            return;
        }
        *baseline = None;
        if keep.as_deref() != Some("keep") {
            if let Err(e) = outputs::apply(&live) {
                eprintln!("[prismd/displays] revert: {e}");
            }
            drop(baseline);
            restore_config(saved_before);
        }
    });
}

/// The outputs as they are now.
fn live_outputs() -> Vec<OutputSettings> {
    outputs::list()
        .unwrap_or_default()
        .into_iter()
        .map(|o| OutputSettings {
            mode: o.current_mode().map(|m| m.key()).unwrap_or_default(),
            name: o.name,
            enabled: o.enabled,
            scale: o.scale,
            transform: o.transform,
            position: Some(o.position),
            adaptive_sync: o.adaptive_sync,
        })
        .collect()
}

/// Put the previous saved settings back in config.json (the watcher then finds them applied).
fn restore_config(prev: Vec<OutputSettings>) {
    *LAST.lock().unwrap() = Some(prev.clone());
    let mut cfg = Config::load();
    cfg.outputs = prev;
    if let Err(e) = cfg.save() {
        eprintln!("[prismd/displays] saving config: {e}");
    }
}
