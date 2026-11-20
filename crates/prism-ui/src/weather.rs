//! Weather for the desktop widget, from Open-Meteo (no API key): the current temperature and
//! conditions and three days ahead, refreshed every 30 minutes on a worker thread (through curl).
//! Location: `weather_location` (a place name, looked up with Open-Meteo's geocoding), else the
//! blue light filter's latitude/longitude, else an IP lookup.

use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::waker::Waker;

const REFRESH: Duration = Duration::from_secs(30 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    /// Short weekday, e.g. "Mon".
    pub name: String,
    pub code: u32,
    pub max: f64,
    pub min: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weather {
    pub place: String,
    pub temp: f64,
    pub feels: f64,
    pub code: u32,
    pub is_day: bool,
    /// "°C" or "°F".
    pub unit: String,
    pub days: Vec<Day>,
}

/// (generation, last result, last error).
static STATE: Mutex<(u64, Option<Weather>, Option<String>)> = Mutex::new((0, None, None));
static START: Once = Once::new();
/// Ask the worker for a refresh now (settings changed).
static AGAIN: Mutex<bool> = Mutex::new(false);

pub fn start(waker: Waker) {
    START.call_once(|| {
        std::thread::Builder::new().name("prism-weather".into()).spawn(move || run(waker)).ok();
    });
}

pub fn snapshot() -> (Option<Weather>, Option<String>) {
    let s = STATE.lock().unwrap();
    (s.1.clone(), s.2.clone())
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

/// Fetch again soon (the location or units changed).
pub fn refresh() {
    *AGAIN.lock().unwrap() = true;
}

fn run(waker: Waker) {
    let mut next = Instant::now();
    let mut last_key = String::new();
    loop {
        let cfg = prism_common::Config::load();
        let key = format!("{}|{}", cfg.weather_location, cfg.weather_fahrenheit);
        let wanted = cfg.desktop_layout.weather.visible;
        if wanted && (Instant::now() >= next || key != last_key || std::mem::take(&mut *AGAIN.lock().unwrap())) {
            last_key = key;
            let r = fetch(&cfg);
            let ok = r.is_ok();
            let mut s = STATE.lock().unwrap();
            match r {
                Ok(w) => {
                    s.1 = Some(w);
                    s.2 = None;
                }
                Err(e) => {
                    eprintln!("[prism-ui] weather: {e}");
                    s.2 = Some(e);
                }
            }
            s.0 += 1;
            drop(s);
            waker.wake();
            next = Instant::now() + if ok { REFRESH } else { RETRY };
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn curl(url: &str) -> Result<Value, String> {
    let out = std::process::Command::new("curl").args(["-s", "--max-time", "10", url]).stdin(std::process::Stdio::null()).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err("no connection to the weather service".into());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("bad answer: {e}"))
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// (latitude, longitude, place name).
fn locate(cfg: &prism_common::Config) -> Result<(f64, f64, String), String> {
    let name = cfg.weather_location.trim();
    if !name.is_empty() {
        let v = curl(&format!("https://geocoding-api.open-meteo.com/v1/search?count=1&name={}", url_encode(name)))?;
        let r = v.get("results").and_then(|r| r.get(0)).ok_or_else(|| format!("place \u{201c}{name}\u{201d} not found"))?;
        let f = |k: &str| r.get(k).and_then(Value::as_f64);
        let place = r.get("name").and_then(Value::as_str).unwrap_or(name).to_string();
        return Ok((f("latitude").ok_or("no latitude")?, f("longitude").ok_or("no longitude")?, place));
    }
    if let (Ok(lat), Ok(lon)) = (cfg.blue_light_latitude.trim().parse::<f64>(), cfg.blue_light_longitude.trim().parse::<f64>()) {
        return Ok((lat, lon, String::new()));
    }
    let v = curl("http://ip-api.com/json/?fields=lat,lon,city")?;
    let lat = v.get("lat").and_then(Value::as_f64).ok_or("location unknown: set a place in Settings")?;
    let lon = v.get("lon").and_then(Value::as_f64).ok_or("location unknown")?;
    Ok((lat, lon, v.get("city").and_then(Value::as_str).unwrap_or("").to_string()))
}

fn fetch(cfg: &prism_common::Config) -> Result<Weather, String> {
    let (lat, lon, place) = locate(cfg)?;
    let unit = if cfg.weather_fahrenheit { "&temperature_unit=fahrenheit" } else { "" };
    let v = curl(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}&current=temperature_2m,apparent_temperature,weather_code,is_day&daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=4{unit}"
    ))?;
    let cur = v.get("current").ok_or("no current weather")?;
    let num = |o: &Value, k: &str| o.get(k).and_then(Value::as_f64);
    let daily = v.get("daily").ok_or("no forecast")?;
    let arr = |k: &str| daily.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let (dates, codes, maxs, mins) = (arr("time"), arr("weather_code"), arr("temperature_2m_max"), arr("temperature_2m_min"));
    let days = (1..dates.len().min(4))
        .map(|i| Day {
            name: dates[i].as_str().map(weekday).unwrap_or_default(),
            code: codes.get(i).and_then(Value::as_u64).unwrap_or(0) as u32,
            max: maxs.get(i).and_then(Value::as_f64).unwrap_or(0.0),
            min: mins.get(i).and_then(Value::as_f64).unwrap_or(0.0),
        })
        .collect();
    Ok(Weather {
        place,
        temp: num(cur, "temperature_2m").ok_or("no temperature")?,
        feels: num(cur, "apparent_temperature").unwrap_or(0.0),
        code: num(cur, "weather_code").unwrap_or(0.0) as u32,
        is_day: num(cur, "is_day").unwrap_or(1.0) > 0.5,
        unit: if cfg.weather_fahrenheit { "\u{b0}F".into() } else { "\u{b0}C".into() },
        days,
    })
}

/// "2026-11-21" -> "Sat" (Zeller-style day of week).
fn weekday(date: &str) -> String {
    let p: Vec<i64> = date.split('-').filter_map(|x| x.parse().ok()).collect();
    if p.len() != 3 {
        return String::new();
    }
    let (mut y, mut m, d) = (p[0], p[1], p[2]);
    if m < 3 {
        m += 12;
        y -= 1;
    }
    let h = (d + 13 * (m + 1) / 5 + y + y / 4 - y / 100 + y / 400) % 7;
    ["Sat", "Sun", "Mon", "Tue", "Wed", "Thu", "Fri"][h as usize].into()
}

/// WMO weather code -> (description, glyph kind).
pub fn describe(code: u32) -> (&'static str, Glyph) {
    match code {
        0 => ("Clear", Glyph::Clear),
        1 => ("Mostly clear", Glyph::Clear),
        2 => ("Partly cloudy", Glyph::PartCloud),
        3 => ("Overcast", Glyph::Cloud),
        45 | 48 => ("Fog", Glyph::Fog),
        51..=57 => ("Drizzle", Glyph::Rain),
        61..=67 | 80..=82 => ("Rain", Glyph::Rain),
        71..=77 | 85 | 86 => ("Snow", Glyph::Snow),
        95..=99 => ("Thunderstorm", Glyph::Storm),
        _ => ("Unknown", Glyph::Cloud),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph {
    Clear,
    PartCloud,
    Cloud,
    Fog,
    Rain,
    Snow,
    Storm,
}

/// SVG path (24 px grid, stroked) of a glyph; `day` picks sun or moon.
pub fn glyph_path(g: Glyph, day: bool) -> &'static str {
    const SUN: &str = "M12 7a5 5 0 1 0 0 10 5 5 0 0 0 0-10zM12 1v2M12 21v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M1 12h2M21 12h2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42";
    const MOON: &str = "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z";
    match g {
        Glyph::Clear if day => SUN,
        Glyph::Clear => MOON,
        Glyph::PartCloud if day => "M12 2v2M4.93 4.93l1.41 1.41M20 12h2M19.07 4.93l-1.41 1.41M15.95 12.48A4 4 0 0 0 8.03 10M13 22H7a5 5 0 1 1 4.9-6H13a3 3 0 0 1 0 6z",
        Glyph::PartCloud => "M13 22H7a5 5 0 1 1 4.9-6H13a3 3 0 0 1 0 6zM10.08 9.06A6 6 0 0 1 17.5 4a6 6 0 0 0 3.5 9",
        Glyph::Cloud => "M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9z",
        Glyph::Fog => "M4 14.9A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.24M16 17H7M17 21H9",
        Glyph::Rain => "M4 14.9A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.24M16 14v6M8 14v6M12 16v6",
        Glyph::Snow => "M4 14.9A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.24M8 15h.01M8 19h.01M12 17h.01M12 21h.01M16 15h.01M16 19h.01",
        Glyph::Storm => "M19 16.9A5 5 0 0 0 18 7h-1.26a8 8 0 1 0-11.62 9M13 11l-4 6h6l-4 6",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekdays() {
        assert_eq!(weekday("2026-11-20"), "Fri");
        assert_eq!(weekday("2024-02-29"), "Thu");
        assert_eq!(weekday("2000-01-01"), "Sat");
    }
}
