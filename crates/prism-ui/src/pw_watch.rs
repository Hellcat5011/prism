//! PipeWire streams: which apps play sound (the per-app mixer in the Control Centre), and which
//! record from the microphone or a camera (the privacy indicator), and the audio devices, cards,
//! profiles and ports for the Control Centre's Sound page. One `pw-dump --monitor` child prints
//! every change as JSON; nothing is polled. Changes go through `wpctl`, `pw-metadata` (moving a
//! stream) and `pw-cli` (balance).

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
    /// media.name (the track or tab title), may be empty.
    pub media: String,
    /// The device node it plays to or records from (from its links).
    pub target: Option<u32>,
}

/// An audio device node: a sink (output) or a source (input).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub id: u32,
    /// node.name (what `target.object` and the default metadata name).
    pub name: String,
    pub description: String,
    pub sink: bool,
    /// The card (PipeWire Device object) and its card.profile.device, for ports and balance.
    pub card: Option<u32>,
    pub card_device: Option<i64>,
    /// 0..1.5 (wpctl scale) and mute.
    pub volume: f64,
    pub muted: bool,
    /// Channel names (FL, FR, ...) and their linear volumes.
    pub channels: Vec<String>,
    pub channel_volumes: Vec<f64>,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub index: u32,
    pub description: String,
    /// Not "no" (unplugged or unsupported profiles are "no").
    pub available: bool,
}

/// A card route (pavucontrol's "port"): speakers, headphones, line out, a microphone jack.
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub index: u32,
    pub description: String,
    pub output: bool,
    /// card.profile.device values it serves and profiles it exists in.
    pub devices: Vec<i64>,
    pub profiles: Vec<u32>,
    pub available: bool,
}

/// A sound card (PipeWire Audio/Device) with its profiles and ports.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u32,
    pub description: String,
    pub profiles: Vec<Profile>,
    pub profile: Option<u32>,
    pub ports: Vec<Port>,
    /// Active routes: (card.profile.device, port index).
    pub active: Vec<(i64, u32)>,
}

impl Card {
    /// Ports a device node can switch between, in the active profile.
    pub fn ports_for(&self, d: &Device) -> Vec<&Port> {
        let Some(dev) = d.card_device else { return Vec::new() };
        self.ports
            .iter()
            .filter(|p| p.output == d.sink && p.devices.contains(&dev) && self.profile.is_none_or(|pr| p.profiles.is_empty() || p.profiles.contains(&pr)))
            .collect()
    }

    /// The port a device node uses now.
    pub fn active_port(&self, d: &Device) -> Option<u32> {
        let dev = d.card_device?;
        self.active.iter().find(|(x, _)| *x == dev).map(|(_, i)| *i)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Audio {
    pub sinks: Vec<Device>,
    pub sources: Vec<Device>,
    pub cards: Vec<Card>,
}

impl Audio {
    pub fn card(&self, id: Option<u32>) -> Option<&Card> {
        let id = id?;
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn device(&self, id: u32) -> Option<&Device> {
        self.sinks.iter().chain(self.sources.iter()).find(|d| d.id == id)
    }
}

struct State {
    generation: u64,
    streams: Vec<Stream>,
    audio: Audio,
}

static STATE: Mutex<State> = Mutex::new(State { generation: 0, streams: Vec::new(), audio: Audio { sinks: Vec::new(), sources: Vec::new(), cards: Vec::new() } });
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
    STATE.lock().unwrap().streams.clone()
}

/// Audio devices and cards.
pub fn audio() -> Audio {
    STATE.lock().unwrap().audio.clone()
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().generation
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
                publish(streams(&objects), audio_state(&objects));
            }
        }
        let _ = child.wait();
        publish(Vec::new(), Audio::default());
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

/// Node ids a node is linked to (either direction).
fn peers(objects: &HashMap<u64, Value>) -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    for o in objects.values().filter(|o| o.get("type").and_then(Value::as_str).is_some_and(|t| t.ends_with(":Link"))) {
        let Some(info) = o.get("info") else { continue };
        let id = |k: &str| info.get(k).and_then(Value::as_u64).map(|v| v as u32);
        if let (Some(a), Some(b)) = (id("output-node-id"), id("input-node-id")) {
            out.insert(a, b);
            out.insert(b, a);
        }
    }
    out
}

fn streams(objects: &HashMap<u64, Value>) -> Vec<Stream> {
    let links = peers(objects);
    let mut out: Vec<Stream> = objects
        .values()
        .filter(|o| o.get("type").and_then(Value::as_str).is_some_and(|t| t.ends_with(":Node")))
        .filter_map(|o| {
            let info = o.get("info")?;
            let props = info.get("props")?;
            let s = |k: &str| props.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            // The Sound page's own level meters are not apps recording.
            if s("node.name") == crate::meters::NODE_NAME {
                return None;
            }
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
            let id = o.get("id")?.as_u64()? as u32;
            Some(Stream {
                id,
                media: s("media.name"),
                target: links.get(&id).copied(),
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

/// Linear channel volumes and mute of a node's Props.
fn props_volume(info: &Value) -> (Vec<f64>, Vec<String>, bool) {
    let p = info.get("params").and_then(|p| p.get("Props")).and_then(Value::as_array).and_then(|a| a.first());
    let vols = p.and_then(|p| p.get("channelVolumes")).and_then(Value::as_array).map(|v| v.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let map = p.and_then(|p| p.get("channelMap")).and_then(Value::as_array).map(|v| v.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let muted = p.and_then(|p| p.get("mute")).and_then(Value::as_bool).unwrap_or(false);
    (vols, map, muted)
}

fn audio_state(objects: &HashMap<u64, Value>) -> Audio {
    let ty = |o: &Value, t: &str| o.get("type").and_then(Value::as_str).is_some_and(|x| x.ends_with(t));
    // Default sink and source names (metadata "default").
    let mut defaults = (String::new(), String::new());
    for o in objects.values().filter(|o| ty(o, ":Metadata")) {
        if o.get("props").and_then(|p| p.get("metadata.name")).and_then(Value::as_str) != Some("default") {
            continue;
        }
        for m in o.get("metadata").and_then(Value::as_array).into_iter().flatten() {
            let name = m.get("value").and_then(|v| v.get("name")).and_then(Value::as_str).unwrap_or("").to_string();
            match m.get("key").and_then(Value::as_str) {
                Some("default.audio.sink") => defaults.0 = name,
                Some("default.audio.source") => defaults.1 = name,
                _ => {}
            }
        }
    }
    let mut audio = Audio::default();
    for o in objects.values() {
        let Some(info) = o.get("info") else { continue };
        let Some(id) = o.get("id").and_then(Value::as_u64).map(|v| v as u32) else { continue };
        let props = info.get("props");
        let s = |k: &str| props.and_then(|p| p.get(k)).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| props.and_then(|p| p.get(k)).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        let class = s("media.class");
        if ty(o, ":Node") && (class.starts_with("Audio/Sink") || class.starts_with("Audio/Source")) {
            let sink = class.starts_with("Audio/Sink");
            let (vols, map, muted) = props_volume(info);
            let name = s("node.name");
            let description = [s("node.description"), s("node.nick"), name.clone()].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            let d = Device {
                id,
                is_default: !name.is_empty() && &name == if sink { &defaults.0 } else { &defaults.1 },
                name,
                description,
                sink,
                card: n("device.id").map(|v| v as u32),
                card_device: n("card.profile.device"),
                volume: vols.iter().copied().fold(0.0f64, f64::max).cbrt(),
                muted,
                channels: map,
                channel_volumes: vols,
            };
            if sink { audio.sinks.push(d) } else { audio.sources.push(d) }
        } else if ty(o, ":Device") && class == "Audio/Device" {
            let params = info.get("params");
            let list = |k: &str| params.and_then(|p| p.get(k)).and_then(Value::as_array).cloned().unwrap_or_default();
            let idx = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).map(|x| x as u32);
            let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let avail = |v: &Value| v.get("available").and_then(Value::as_str) != Some("no");
            let ints = |v: &Value, k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_i64).collect::<Vec<i64>>()).unwrap_or_default();
            let profiles = list("EnumProfile").iter().filter_map(|p| Some(Profile { index: idx(p, "index")?, description: text(p, "description"), available: avail(p) })).collect();
            let profile = list("Profile").first().and_then(|p| idx(p, "index"));
            let ports = list("EnumRoute")
                .iter()
                .filter_map(|r| {
                    Some(Port {
                        index: idx(r, "index")?,
                        description: text(r, "description"),
                        output: text(r, "direction") == "Output",
                        devices: ints(r, "devices"),
                        profiles: ints(r, "profiles").into_iter().map(|x| x as u32).collect(),
                        available: avail(r),
                    })
                })
                .collect();
            let active = list("Route").iter().filter_map(|r| Some((r.get("device")?.as_i64()?, idx(r, "index")?))).collect();
            let description = [s("device.description"), s("device.nick"), s("device.name")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            audio.cards.push(Card { id, description, profiles, profile, ports, active });
        }
    }
    audio.sinks.sort_by(|a, b| a.description.to_lowercase().cmp(&b.description.to_lowercase()).then(a.id.cmp(&b.id)));
    audio.sources.sort_by(|a, b| a.description.to_lowercase().cmp(&b.description.to_lowercase()).then(a.id.cmp(&b.id)));
    audio.cards.sort_by(|a, b| a.description.to_lowercase().cmp(&b.description.to_lowercase()).then(a.id.cmp(&b.id)));
    audio
}

fn publish(list: Vec<Stream>, audio: Audio) {
    let mut st = STATE.lock().unwrap();
    if st.streams != list || st.audio != audio {
        st.generation += 1;
        st.streams = list;
        st.audio = audio;
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

fn run_quiet(cmd: &str, args: Vec<String>) {
    let cmd = cmd.to_string();
    std::thread::spawn(move || {
        let _ = Command::new(&cmd).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    });
}

/// Make a sink or source the default.
pub fn set_default(id: u32) {
    run_quiet("wpctl", vec!["set-default".into(), id.to_string()]);
}

/// Send a stream to a device (its node.name), or back to the default with None.
pub fn move_stream(id: u32, target: Option<&str>) {
    match target {
        Some(name) => run_quiet("pw-metadata", vec![id.to_string(), "target.object".into(), name.to_string()]),
        None => run_quiet("pw-metadata", vec!["-d".into(), id.to_string(), "target.object".into()]),
    }
}

/// Switch a card to a profile.
pub fn set_profile(card: u32, index: u32) {
    run_quiet("wpctl", vec!["set-profile".into(), card.to_string(), index.to_string()]);
}

/// Switch a device node to a port (route index of its card).
pub fn set_port(node: u32, index: u32) {
    run_quiet("wpctl", vec!["set-route".into(), node.to_string(), index.to_string()]);
}

fn left(ch: &str) -> bool {
    matches!(ch, "FL" | "RL" | "SL" | "TFL" | "TRL" | "FLC" | "RLC" | "FLW" | "TSL")
}

fn right(ch: &str) -> bool {
    matches!(ch, "FR" | "RR" | "SR" | "TFR" | "TRR" | "FRC" | "RRC" | "FRW" | "TSR")
}

impl Device {
    /// Has left and right channels (a balance control makes sense).
    pub fn has_balance(&self) -> bool {
        self.channels.len() == self.channel_volumes.len() && self.channels.iter().any(|c| left(c)) && self.channels.iter().any(|c| right(c))
    }

    /// -1 (left only) .. 1 (right only), as pavucontrol shows it.
    pub fn balance(&self) -> f64 {
        let side = |f: fn(&str) -> bool| self.channels.iter().zip(&self.channel_volumes).filter(|(c, _)| f(c)).map(|(_, v)| *v).fold(0.0f64, f64::max);
        let (l, r) = (side(left), side(right));
        let m = l.max(r);
        if m <= 0.0 {
            0.0
        } else if l >= r {
            -(1.0 - r / m)
        } else {
            1.0 - l / m
        }
    }

    /// Channel volumes for `balance` at the current overall level.
    fn balanced(&self, balance: f64) -> Vec<f64> {
        let top = self.channel_volumes.iter().copied().fold(0.0f64, f64::max);
        let b = balance.clamp(-1.0, 1.0);
        self.channels
            .iter()
            .map(|c| {
                let f = if left(c) && b > 0.0 {
                    1.0 - b
                } else if right(c) && b < 0.0 {
                    1.0 + b
                } else {
                    1.0
                };
                top * f
            })
            .collect()
    }
}

/// Set a device's balance: on its card route when it has one (that is what WirePlumber keeps),
/// else on the node.
pub fn set_balance(d: &Device, card: Option<&Card>, balance: f64) {
    if !d.has_balance() {
        return;
    }
    let vols = d.balanced(balance).iter().map(|v| format!("{v:.6}")).collect::<Vec<_>>().join(", ");
    let route = card.zip(d.card_device).and_then(|(c, dev)| c.active_port(d).map(|i| (c.id, dev, i)));
    match route {
        Some((card, dev, index)) => run_quiet("pw-cli", vec!["s".into(), card.to_string(), "Route".into(), format!("{{ index: {index}, device: {dev}, props: {{ channelVolumes: [ {vols} ] }}, save: true }}")]),
        None => run_quiet("pw-cli", vec!["s".into(), d.id.to_string(), "Props".into(), format!("{{ channelVolumes: [ {vols} ] }}")]),
    }
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

    #[test]
    fn reads_devices_cards_and_links() {
        let mut objects: HashMap<u64, Value> = HashMap::new();
        objects.insert(44, serde_json::json!({"id": 44, "type": "PipeWire:Interface:Device", "info": {"props": {"media.class": "Audio/Device", "device.description": "HD Audio"}, "params": {
            "EnumProfile": [{"index": 0, "description": "Off", "available": "yes"}, {"index": 1, "description": "Duplex", "available": "yes"}, {"index": 5, "description": "Surround", "available": "no"}],
            "Profile": [{"index": 1}],
            "EnumRoute": [{"index": 3, "description": "Line Out", "direction": "Output", "devices": [5], "profiles": [1], "available": "no"}, {"index": 4, "description": "Headphones", "direction": "Output", "devices": [5], "profiles": [1], "available": "yes"}, {"index": 0, "description": "Mic", "direction": "Input", "devices": [0], "profiles": [1], "available": "yes"}],
            "Route": [{"index": 4, "device": 5}, {"index": 0, "device": 0}]}}}));
        objects.insert(52, serde_json::json!({"id": 52, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Audio/Sink", "node.name": "out", "node.description": "Speakers", "device.id": 44, "card.profile.device": 5}, "params": {"Props": [{"channelVolumes": [0.125, 0.0625], "channelMap": ["FL", "FR"], "mute": false}]}}}));
        objects.insert(39, serde_json::json!({"id": 39, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"}, "metadata": [{"subject": 0, "key": "default.audio.sink", "value": {"name": "out"}}]}));
        objects.insert(67, serde_json::json!({"id": 67, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Output/Audio", "application.name": "mpv", "media.name": "song"}}}));
        objects.insert(70, serde_json::json!({"id": 70, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 67, "input-node-id": 52}}));
        let a = audio_state(&objects);
        assert_eq!(a.sinks.len(), 1);
        let d = &a.sinks[0];
        assert!(d.is_default);
        assert!((d.volume - 0.5).abs() < 1e-9);
        assert!((d.balance() + 0.5).abs() < 1e-9);
        let b = d.balanced(0.0);
        assert!((b[0] - 0.125).abs() < 1e-9 && (b[1] - 0.125).abs() < 1e-9);
        let card = a.card(d.card).unwrap();
        assert_eq!(card.profile, Some(1));
        assert_eq!(card.ports_for(d).len(), 2);
        assert_eq!(card.active_port(d), Some(4));
        let s = streams(&objects);
        assert_eq!(s[0].target, Some(52));
        assert_eq!(s[0].media, "song");
    }
}
