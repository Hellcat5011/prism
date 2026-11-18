//! Output list for Settings > Display (wlr-output-management via prism_common::outputs).
//! Read on a worker thread and cached, so drawing never waits for the compositor.

use prism_common::outputs::{OutputInfo, OutputSettings};
use prism_common::Config;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static CACHE: Mutex<(Vec<OutputInfo>, Option<Instant>)> = Mutex::new((Vec::new(), None));
static LOADING: AtomicBool = AtomicBool::new(false);
/// Output the Display rows edit (set by their getters; options() has no config).
static SELECTED: Mutex<String> = Mutex::new(String::new());

/// Cached outputs; refreshed in the background when older than two seconds.
pub fn outputs() -> Vec<OutputInfo> {
    let c = CACHE.lock().unwrap();
    let stale = c.1.map(|t| t.elapsed() > Duration::from_secs(2)).unwrap_or(true);
    if stale && !LOADING.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            match prism_common::outputs::list() {
                Ok(l) => *CACHE.lock().unwrap() = (l, Some(Instant::now())),
                Err(e) => {
                    eprintln!("[prism-ui/displays] {e}");
                    CACHE.lock().unwrap().1 = Some(Instant::now());
                }
            }
            LOADING.store(false, Ordering::SeqCst);
        });
    }
    c.0.clone()
}

pub fn count() -> usize {
    outputs().len()
}

/// The output being edited.
pub fn selected(c: &Config) -> Option<OutputInfo> {
    let list = outputs();
    let o = list.iter().find(|o| o.name == c.display_output).or(list.first()).cloned();
    if let Some(o) = &o {
        *SELECTED.lock().unwrap() = o.name.clone();
    }
    o
}

fn selected_info() -> Option<OutputInfo> {
    let name = SELECTED.lock().unwrap().clone();
    let list = outputs();
    list.iter().find(|o| o.name == name).or(list.first()).cloned()
}

/// Saved settings for the edited output, or its live state.
pub fn entry(c: &Config) -> Option<OutputSettings> {
    let o = selected(c)?;
    Some(c.outputs.iter().find(|s| s.name == o.name).cloned().unwrap_or_else(|| OutputSettings {
        name: o.name.clone(),
        enabled: o.enabled,
        mode: o.current_mode().map(|m| m.key()).unwrap_or_default(),
        scale: o.scale,
        transform: o.transform.clone(),
        position: Some(o.position),
        adaptive_sync: o.adaptive_sync,
    }))
}

/// Change the edited output's saved settings.
pub fn update(c: &mut Config, f: impl FnOnce(&mut OutputSettings)) {
    let Some(mut e) = entry(c) else { return };
    f(&mut e);
    match c.outputs.iter_mut().find(|s| s.name == e.name) {
        Some(s) => *s = e,
        None => c.outputs.push(e),
    }
}

pub fn monitor_options() -> Vec<(String, String)> {
    outputs().iter().map(|o| (if o.description.is_empty() { o.name.clone() } else { format!("{} ({})", o.name, o.description) }, o.name.clone())).collect()
}

pub fn mode_options() -> Vec<(String, String)> {
    let Some(o) = selected_info() else { return Vec::new() };
    let mut seen = std::collections::HashSet::new();
    o.modes
        .iter()
        .filter(|m| seen.insert(m.key()))
        .map(|m| (if m.preferred { format!("{} (native)", m.label()) } else { m.label() }, m.key()))
        .collect()
}

pub const SCALES: [(&str, &str); 7] = [("100%", "1.00"), ("125%", "1.25"), ("150%", "1.50"), ("175%", "1.75"), ("200%", "2.00"), ("250%", "2.50"), ("300%", "3.00")];

pub fn scale_options() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = SCALES.iter().map(|(l, s)| (l.to_string(), s.to_string())).collect();
    if let Some(o) = selected_info() {
        let cur = format!("{:.2}", o.scale);
        if !v.iter().any(|(_, s)| *s == cur) {
            v.push((format!("{:.0}%", o.scale * 100.0), cur));
        }
    }
    v
}
