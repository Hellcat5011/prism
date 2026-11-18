//! CPU, memory and temperature for the bar's System Monitor module, read from /proc and /sys on
//! a worker thread that only runs while the module is shown.

use crate::waker::Waker;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

const INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub cpu: u8,
    pub memory: u8,
    pub temperature: Option<i32>,
}

static STATE: Mutex<(u64, Stats)> = Mutex::new((0, Stats { cpu: 0, memory: 0, temperature: None }));
static WANTED: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();

/// (busy, total) jiffies from the first line of /proc/stat.
fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let fields: Vec<u64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if fields.len() < 4 {
        return None;
    }
    // Only user to steal: guest time is already counted in user.
    let total: u64 = fields.iter().take(8).sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

fn percent(part: u64, whole: u64) -> u8 {
    if whole == 0 {
        return 0;
    }
    ((part as f64 / whole as f64 * 100.0).round() as u64).min(100) as u8
}

/// Share of memory in use, from /proc/meminfo.
fn memory_percent(meminfo: &str) -> u8 {
    let value = |key: &str| meminfo.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok());
    match (value("MemTotal:"), value("MemAvailable:")) {
        (Some(total), Some(available)) => percent(total.saturating_sub(available), total),
        _ => 0,
    }
}

/// The hwmon input of the CPU package, if the machine has a known sensor.
fn find_temperature_sensor() -> Option<PathBuf> {
    const CPU_SENSORS: [&str; 4] = ["k10temp", "coretemp", "zenpower", "cpu_thermal"];
    let mut sensors: Vec<PathBuf> = std::fs::read_dir("/sys/class/hwmon").ok()?.flatten().map(|e| e.path()).collect();
    sensors.sort();
    sensors.into_iter().find(|p| std::fs::read_to_string(p.join("name")).is_ok_and(|n| CPU_SENSORS.contains(&n.trim()))).map(|p| p.join("temp1_input"))
}

pub fn start(waker: Waker) {
    START.call_once(|| {
        std::thread::spawn(move || {
            let sensor = find_temperature_sensor();
            let mut last = None;
            loop {
                if !WANTED.load(Ordering::Relaxed) {
                    last = None;
                    std::thread::sleep(INTERVAL);
                    continue;
                }
                let times = std::fs::read_to_string("/proc/stat").ok().and_then(|s| cpu_times(&s));
                let cpu = match (last, times) {
                    (Some((b0, t0)), Some((b1, t1))) => percent(u64::saturating_sub(b1, b0), u64::saturating_sub(t1, t0)),
                    _ => STATE.lock().unwrap().1.cpu,
                };
                last = times;
                let stats = Stats {
                    cpu,
                    memory: std::fs::read_to_string("/proc/meminfo").map(|s| memory_percent(&s)).unwrap_or(0),
                    temperature: sensor.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.trim().parse::<i32>().ok()).map(|m| m / 1000),
                };
                let mut st = STATE.lock().unwrap();
                if st.1 != stats {
                    st.0 += 1;
                    st.1 = stats;
                    drop(st);
                    waker.wake();
                }
                std::thread::sleep(INTERVAL);
            }
        });
    });
}

/// Tell the worker whether anything shows its numbers.
pub fn set_wanted(wanted: bool) {
    WANTED.store(wanted, Ordering::Relaxed);
}

pub fn snapshot() -> Stats {
    STATE.lock().unwrap().1
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_time_leaves_out_idle_and_iowait() {
        let stat = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4\n";
        assert_eq!(cpu_times(stat), Some((150, 1000)));
        assert_eq!(cpu_times("nonsense"), None);
    }

    #[test]
    fn memory_in_use_is_what_is_not_available() {
        assert_eq!(memory_percent("MemTotal: 1000 kB\nMemFree: 10 kB\nMemAvailable: 250 kB\n"), 75);
        assert_eq!(memory_percent(""), 0);
    }

    #[test]
    fn percentages_stay_in_range() {
        assert_eq!(percent(5, 0), 0);
        assert_eq!(percent(200, 100), 100);
    }
}
