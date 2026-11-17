//! Blue light schedule (port of the QML BlueLightFilter service): computes the colour
//! temperature for the current local time from the config, and drives the gamma thread.
//!
//! The scheduler thread re-evaluates every few seconds (transitions are gradual) and at once
//! when the config changes. It only talks to the gamma thread when the kelvin value changes;
//! when the filter is off it releases gamma control so the compositor restores the default.

use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use prism_common::Config;
use serde::Serialize;

use crate::gamma;

#[derive(Serialize, Clone, Default)]
pub struct BlueLightStatus {
    pub enabled: bool,
    pub manual_on: bool,
    /// Kelvin currently applied, or the day temperature when the filter is off.
    pub current_temp: u32,
    pub day_temp: u32,
    pub night_temp: u32,
    pub mode: String,
    /// Location in use for sunset/sunrise mode, if any.
    pub location: Option<(f64, f64)>,
}

enum Msg {
    Config(Box<Config>),
    Location(f64, f64),
}

pub struct BlueLight {
    tx: mpsc::Sender<Msg>,
    status: Arc<Mutex<BlueLightStatus>>,
}

impl BlueLight {
    pub fn start(cfg: &Config, gamma: Option<gamma::GammaHandle>) -> BlueLight {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(BlueLightStatus::default()));
        let thread_status = Arc::clone(&status);
        let thread_tx = tx.clone();
        let cfg = cfg.clone();
        thread::spawn(move || run(cfg, rx, thread_tx, gamma, thread_status));
        BlueLight { tx, status }
    }

    pub fn update_config(&self, cfg: &Config) {
        let _ = self.tx.send(Msg::Config(Box::new(cfg.clone())));
    }

    pub fn status(&self) -> BlueLightStatus {
        self.status.lock().unwrap().clone()
    }
}

fn run(
    mut cfg: Config,
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<Msg>,
    gamma: Option<gamma::GammaHandle>,
    status: Arc<Mutex<BlueLightStatus>>,
) {
    let mut ip_location: Option<(f64, f64)> = None;
    let mut fetching = false;
    let mut applied: Option<u32> = None;
    let mut first = true;
    loop {
        if wants_ip_location(&cfg) && ip_location.is_none() && !fetching {
            fetching = true;
            let tx = tx.clone();
            thread::spawn(move || {
                if let Some((lat, lon)) = fetch_ip_location() {
                    let _ = tx.send(Msg::Location(lat, lon));
                }
            });
        }

        let location = location_for(&cfg, ip_location);
        let target = if cfg.blue_light_enabled { Some(target_kelvin(&cfg, location, &LocalTime::now())) } else { None };
        if target != applied || first {
            if let Some(g) = &gamma {
                // Nothing to release at startup when the filter is off.
                if target.is_some() || !first {
                    g.set(target);
                }
            }
            if target != applied {
                eprintln!("[prismd/bluelight] {}", target.map_or("off".to_string(), |k| format!("{k}K")));
            }
            applied = target;
            first = false;
        }
        *status.lock().unwrap() = BlueLightStatus {
            enabled: cfg.blue_light_enabled,
            manual_on: cfg.blue_light_manual_on,
            current_temp: target.unwrap_or(cfg.blue_light_day_temp),
            day_temp: cfg.blue_light_day_temp,
            night_temp: cfg.blue_light_night_temp,
            mode: cfg.blue_light_mode.clone(),
            location,
        };

        let wait = if cfg.blue_light_enabled { Duration::from_secs(5) } else { Duration::from_secs(3600) };
        match rx.recv_timeout(wait) {
            Ok(Msg::Config(c)) => {
                if c.blue_light_use_ip_location != cfg.blue_light_use_ip_location {
                    fetching = false;
                }
                cfg = *c;
            }
            Ok(Msg::Location(lat, lon)) => {
                eprintln!("[prismd/bluelight] IP location {lat:.2},{lon:.2}");
                ip_location = Some((lat, lon));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn is_sun_mode(cfg: &Config) -> bool {
    matches!(cfg.blue_light_mode.as_str(), "sunset_sunrise" | "realtime")
}

fn wants_ip_location(cfg: &Config) -> bool {
    cfg.blue_light_enabled && is_sun_mode(cfg) && cfg.blue_light_use_ip_location
}

fn location_for(cfg: &Config, ip: Option<(f64, f64)>) -> Option<(f64, f64)> {
    if !is_sun_mode(cfg) {
        return None;
    }
    let manual = match (cfg.blue_light_latitude.trim().parse::<f64>(), cfg.blue_light_longitude.trim().parse::<f64>()) {
        (Ok(lat), Ok(lon)) if lat.is_finite() && lon.is_finite() && !(lat == 0.0 && lon == 0.0) => Some((lat, lon)),
        _ => None,
    };
    if cfg.blue_light_use_ip_location {
        ip.or(manual)
    } else {
        manual
    }
}

/// Same service and fields the QML shell used (scripts/get-location.py).
fn fetch_ip_location() -> Option<(f64, f64)> {
    let out = Command::new("curl")
        .args(["-s", "--max-time", "5", "http://ip-api.com/json/?fields=lat,lon"])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let lat = v.get("lat")?.as_f64()?;
    let lon = v.get("lon")?.as_f64()?;
    if lat == 0.0 && lon == 0.0 {
        eprintln!("[prismd/bluelight] IP location lookup returned nothing");
        return None;
    }
    Some((lat, lon))
}

pub struct LocalTime {
    /// Minutes since local midnight, fractional.
    pub minutes: f64,
    /// Day of the year, 1-based.
    pub day_of_year: u32,
    pub hour: u32,
    /// Seconds east of UTC.
    pub utc_offset_secs: i64,
}

impl LocalTime {
    pub fn now() -> LocalTime {
        unsafe {
            let mut ts: libc::timespec = std::mem::zeroed();
            libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&ts.tv_sec, &mut tm);
            LocalTime {
                minutes: tm.tm_hour as f64 * 60.0 + tm.tm_min as f64 + (tm.tm_sec as f64 + ts.tv_nsec as f64 / 1e9) / 60.0,
                day_of_year: tm.tm_yday as u32 + 1,
                hour: tm.tm_hour as u32,
                utc_offset_secs: tm.tm_gmtoff,
            }
        }
    }
}

fn parse_hhmm(s: &str, default: f64) -> f64 {
    let mut it = s.trim().split(':');
    match (it.next().and_then(|h| h.parse::<u32>().ok()), it.next().and_then(|m| m.parse::<u32>().ok()), it.next()) {
        (Some(h), Some(m), None) if h <= 23 && m <= 59 => (h * 60 + m) as f64,
        _ => default,
    }
}

fn is_night(now: f64, start: f64, end: f64) -> bool {
    if start == end {
        false
    } else if start < end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

fn clock_distance(from: f64, to: f64) -> f64 {
    let d = (to - from + 1440.0).rem_euclid(1440.0);
    if d == 0.0 { 1440.0 } else { d }
}

/// Progress 0..1 through a ramp window that ends at `end`, or None outside it.
fn ramp_progress(now: f64, start: f64, end: f64, duration: f64) -> Option<f64> {
    if duration <= 0.0 {
        return None;
    }
    let start = start.rem_euclid(1440.0);
    let end = end.rem_euclid(1440.0);
    let inside = if start < end {
        now >= start && now < end
    } else if start > end {
        now >= start || now < end
    } else {
        true
    };
    inside.then(|| ((now - start + 1440.0).rem_euclid(1440.0) / duration).min(1.0))
}

/// Kelvin for `now` given a night window; the transition ramps finish at the window edges.
pub fn temperature_for_time(now: f64, night_start: f64, night_end: f64, transition: f64, day: u32, night: u32) -> u32 {
    if night_start == night_end {
        return day;
    }
    let (dayf, nightf) = (day as f64, night as f64);
    let dur = transition.max(0.0);
    let night_ramp = dur.min(clock_distance(night_end, night_start));
    let day_ramp = dur.min(clock_distance(night_start, night_end));
    let k = if let Some(p) = ramp_progress(now, night_start - night_ramp, night_start, night_ramp) {
        dayf + (nightf - dayf) * p
    } else if let Some(p) = ramp_progress(now, night_end - day_ramp, night_end, day_ramp) {
        nightf + (dayf - nightf) * p
    } else if is_night(now, night_start, night_end) {
        nightf
    } else {
        dayf
    };
    (k.round() as u32).clamp(day.min(night), day.max(night))
}

/// Local (sunset, sunrise) in minutes since midnight (NOAA approximation, as in QML).
pub fn sun_times(lat: f64, lon: f64, t: &LocalTime) -> (f64, f64) {
    use std::f64::consts::PI;
    let g = 2.0 * PI / 365.0 * (t.day_of_year as f64 - 1.0 + (t.hour as f64 - 12.0) / 24.0);
    let eq_time = 229.18
        * (0.000075 + 0.001868 * g.cos() - 0.032077 * g.sin() - 0.014615 * (2.0 * g).cos() - 0.040849 * (2.0 * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2.0 * g).cos() + 0.000907 * (2.0 * g).sin()
        - 0.002697 * (3.0 * g).cos()
        + 0.00148 * (3.0 * g).sin();
    let latr = lat.to_radians();
    let mut ha = ((90.833f64).to_radians().cos() / (latr.cos() * decl.cos()) - latr.tan() * decl.tan()).acos();
    if ha.is_nan() {
        ha = PI / 2.0;
    }
    let offset = t.utc_offset_secs as f64 / 60.0;
    let sunrise = (720.0 - 4.0 * (lon + ha.to_degrees()) - eq_time + offset).rem_euclid(1440.0);
    let sunset = (720.0 - 4.0 * (lon - ha.to_degrees()) - eq_time + offset).rem_euclid(1440.0);
    (sunset, sunrise)
}

pub fn target_kelvin(cfg: &Config, location: Option<(f64, f64)>, t: &LocalTime) -> u32 {
    if cfg.blue_light_manual_on {
        return cfg.blue_light_night_temp;
    }
    let (start, end) = match location {
        Some((lat, lon)) if is_sun_mode(cfg) => sun_times(lat, lon, t),
        // fixed_time, or sun mode without coordinates: the configured window.
        _ => (parse_hhmm(&cfg.blue_light_night_start, 22.0 * 60.0), parse_hhmm(&cfg.blue_light_night_end, 6.0 * 60.0)),
    };
    temperature_for_time(
        t.minutes,
        start,
        end,
        cfg.blue_light_transition_minutes as f64,
        cfg.blue_light_day_temp,
        cfg.blue_light_night_temp,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u32 = 6500;
    const NIGHT: u32 = 4000;

    fn at(h: f64, m: f64) -> f64 {
        h * 60.0 + m
    }

    #[test]
    fn overnight_window_without_transition() {
        let (s, e) = (at(22.0, 0.0), at(6.0, 0.0));
        assert_eq!(temperature_for_time(at(12.0, 0.0), s, e, 0.0, DAY, NIGHT), DAY);
        assert_eq!(temperature_for_time(at(23.0, 0.0), s, e, 0.0, DAY, NIGHT), NIGHT);
        assert_eq!(temperature_for_time(at(3.0, 0.0), s, e, 0.0, DAY, NIGHT), NIGHT);
        assert_eq!(temperature_for_time(at(6.0, 0.0), s, e, 0.0, DAY, NIGHT), DAY);
    }

    #[test]
    fn transitions_ramp_into_window_edges() {
        let (s, e) = (at(22.0, 0.0), at(6.0, 0.0));
        // Halfway through the 60 min ramp before 22:00.
        assert_eq!(temperature_for_time(at(21.0, 30.0), s, e, 60.0, DAY, NIGHT), (DAY + NIGHT) / 2);
        // Halfway through the ramp back to day before 06:00.
        assert_eq!(temperature_for_time(at(5.0, 30.0), s, e, 60.0, DAY, NIGHT), (DAY + NIGHT) / 2);
        assert_eq!(temperature_for_time(at(20.0, 59.0), s, e, 60.0, DAY, NIGHT), DAY);
    }

    #[test]
    fn same_start_and_end_is_always_day() {
        assert_eq!(temperature_for_time(at(1.0, 0.0), 600.0, 600.0, 30.0, DAY, NIGHT), DAY);
    }

    #[test]
    fn manual_on_forces_night() {
        let mut cfg = Config::default();
        cfg.blue_light_manual_on = true;
        cfg.blue_light_night_temp = NIGHT;
        let t = LocalTime { minutes: at(12.0, 0.0), day_of_year: 100, hour: 12, utc_offset_secs: 0 };
        assert_eq!(target_kelvin(&cfg, None, &t), NIGHT);
    }

    #[test]
    fn sun_times_are_plausible() {
        // London, around the March equinox, UTC: sunrise ~06:00, sunset ~18:15.
        let t = LocalTime { minutes: 720.0, day_of_year: 80, hour: 12, utc_offset_secs: 0 };
        let (sunset, sunrise) = sun_times(51.5, -0.12, &t);
        assert!((sunrise - 360.0).abs() < 30.0, "sunrise {sunrise}");
        assert!((sunset - 1095.0).abs() < 30.0, "sunset {sunset}");
    }

    #[test]
    fn bad_time_strings_fall_back() {
        assert_eq!(parse_hhmm("25:00", 7.0), 7.0);
        assert_eq!(parse_hhmm("7:5", 0.0), 425.0);
        assert_eq!(parse_hhmm("abc", 1.0), 1.0);
    }
}
