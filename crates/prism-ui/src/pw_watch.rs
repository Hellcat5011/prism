//! PipeWire streams: which apps play sound (the per-app mixer in the Control Centre), and which
//! record from the microphone or a camera (the privacy indicator). One `pw-dump --monitor`
//! child prints every change as JSON; nothing is polled. Volumes are set with `wpctl`.

use std::collections::HashMap;
use std::io::BufReader;
use std::process::{Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::Duration;

use serde_json::Value;

use crate::waker::Waker;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StreamKind {
    /// An app playing audio.
    Playback,
    /// An app recording audio (microphone, or a monitor).
    Record,
    /// An app reading a camera / video source.
    Video,
    /// A camera device node (v4l2 / libcamera); `active` while something reads it.
    Camera,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub id: u32,
    pub kind: StreamKind,
    /// application.name, else the binary, else the node name.
    pub app: String,
    /// Icon name hint (application.icon-name or the binary).
    pub icon: String,
    /// Running (moving data), not just open.
    pub active: bool,
    /// 0..1.5, the scale wpctl shows (cube root of the linear gain).
    pub volume: f64,
    pub muted: bool,
    /// Records from a sink's monitor (screen recorders): not the microphone.
    pub monitor: bool,
}

static STATE: Mutex<(u64, Vec<Stream>)> = Mutex::new((0, Vec::new()));
static START: Once = Once::new();
static WAKERS: Mutex<Vec<Waker>> = Mutex::new(Vec::new());

/// Follow PipeWire for as long as the process runs; `waker` is woken on every change.
pub fn start(waker: Waker) {
    WAKERS.lock().unwrap().push(waker);
    START.call_once(|| {
        std::thread::Builder::new().name("prism-pw-watch".into()).spawn(run).ok();
    });
}

pub fn snapshot() -> Vec<Stream> {
    STATE.lock().unwrap().1.clone()
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0
}

fn run() {
    loop {
        let mut cmd = Command::new("pw-dump");
        cmd.args(["--monitor", "--no-colors"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let Ok(mut child) = cmd.spawn() else {
            eprintln!("[prism-ui] pw-dump not found: no per-app volume or privacy indicator");
            return;
        };
        let mut objects: HashMap<u64, Value> = HashMap::new();
        if let Some(out) = child.stdout.take() {
            for batch in serde_json::Deserializer::from_reader(BufReader::new(out)).into_iter::<Value>() {
                let Ok(Value::Array(items)) = batch else { break };
                for o in items {
                    let Some(id) = o.get("id").and_then(Value::as_u64) else { continue };
                    if o.get("info").is_some_and(Value::is_null) {
                        objects.remove(&id);
                        continue;
                    }
                    match objects.get_mut(&id) {
                        Some(old) => merge(old, o),
                        None => {
                            objects.insert(id, o);
                        }
                    }
                }
                publish(streams(&objects));
            }
        }
        let _ = child.wait();
        publish(Vec::new());
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Updates carry the changed parts of an object: merge them in.
fn merge(old: &mut Value, new: Value) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

fn streams(objects: &HashMap<u64, Value>) -> Vec<Stream> {
    let mut out: Vec<Stream> = objects
        .values()
        .filter(|o| o.get("type").and_then(Value::as_str).is_some_and(|t| t.ends_with(":Node")))
        .filter_map(|o| {
            let info = o.get("info")?;
            let props = info.get("props")?;
            let s = |k: &str| props.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let kind = match s("media.class").as_str() {
                "Stream/Output/Audio" => StreamKind::Playback,
                "Stream/Input/Audio" => StreamKind::Record,
                "Stream/Input/Video" => StreamKind::Video,
                "Video/Source" if !s("device.api").is_empty() => StreamKind::Camera,
                _ => return None,
            };
            let binary = s("application.process.binary");
            let app = [s("application.name"), binary.clone(), s("node.description"), s("node.name")].into_iter().find(|x| !x.is_empty()).unwrap_or_else(|| "Unknown".into());
            let icon = [s("application.icon-name"), binary.clone()].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            let p = info.get("params").and_then(|p| p.get("Props")).and_then(Value::as_array).and_then(|a| a.first());
            let volume = p
                .and_then(|p| p.get("channelVolumes"))
                .and_then(Value::as_array)
                .map(|v| v.iter().filter_map(Value::as_f64).fold(0.0f64, f64::max))
                .map(|lin| lin.cbrt())
                .unwrap_or(1.0);
            let muted = p.and_then(|p| p.get("mute")).and_then(Value::as_bool).unwrap_or(false);
            let monitor = s("stream.capture.sink") == "true" || s("target.object").ends_with(".monitor");
            Some(Stream {
                id: o.get("id")?.as_u64()? as u32,
                kind,
                app,
                icon,
                active: info.get("state").and_then(Value::as_str) == Some("running"),
                volume,
                muted,
                monitor,
            })
        })
        .collect();
    out.sort_by(|a, b| a.app.to_lowercase().cmp(&b.app.to_lowercase()).then(a.id.cmp(&b.id)));
    out
}

fn publish(list: Vec<Stream>) {
    let mut st = STATE.lock().unwrap();
    if st.1 != list {
        st.0 += 1;
        st.1 = list;
        drop(st);
        for w in WAKERS.lock().unwrap().iter() {
            w.wake();
        }
    }
}

/// Set the volume (0..1.5, as wpctl shows it) of every stream in `ids`.
pub fn set_volume(ids: &[u32], volume: f64) {
    let ids = ids.to_vec();
    let v = format!("{:.3}", volume.clamp(0.0, 1.5));
    std::thread::spawn(move || {
        for id in ids {
            let _ = Command::new("wpctl").args(["set-volume", &id.to_string(), &v]).stdin(Stdio::null()).status();
        }
    });
}

pub fn set_mute(ids: &[u32], muted: bool) {
    let ids = ids.to_vec();
    std::thread::spawn(move || {
        for id in ids {
            let _ = Command::new("wpctl").args(["set-mute", &id.to_string(), if muted { "1" } else { "0" }]).stdin(Stdio::null()).status();
        }
    });
}

/// Apps playing sound, one entry per app: (name, icon, stream ids, volume, muted).
pub fn mixer() -> Vec<(String, String, Vec<u32>, f64, bool)> {
    let mut out: Vec<(String, String, Vec<u32>, f64, bool)> = Vec::new();
    for s in snapshot().into_iter().filter(|s| s.kind == StreamKind::Playback) {
        match out.iter_mut().find(|e| e.0 == s.app) {
            Some(e) => {
                e.2.push(s.id);
                e.3 = e.3.max(s.volume);
                e.4 &= s.muted;
            }
            None => out.push((s.app.clone(), s.icon.clone(), vec![s.id], s.volume, s.muted)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_streams_and_merges_updates() {
        let mut objects: HashMap<u64, Value> = HashMap::new();
        let full: Value = serde_json::json!({"id": 40, "type": "PipeWire:Interface:Node", "info": {"state": "running", "props": {"media.class": "Stream/Output/Audio", "application.name": "Firefox", "application.process.binary": "firefox"}, "params": {"Props": [{"channelVolumes": [0.125, 0.125], "mute": false}]}}});
        objects.insert(40, full);
        let s = streams(&objects);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].app, "Firefox");
        assert!((s[0].volume - 0.5).abs() < 1e-9);
        assert!(s[0].active);
        // An update with only the state changes just that.
        merge(objects.get_mut(&40).unwrap(), serde_json::json!({"id": 40, "info": {"state": "idle"}}));
        let s = streams(&objects);
        assert!(!s[0].active);
        assert_eq!(s[0].app, "Firefox");
    }
}
