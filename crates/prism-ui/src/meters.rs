//! Live peak levels of audio devices for the NC Sound page (pavucontrol's meters).
//!
//! The page calls `want` with the devices it shows on every frame it draws them; a worker
//! thread keeps one PipeWire capture stream per device (a sink's monitor, or the source
//! itself) and drops them about a second after the page stops asking, so nothing is captured
//! while the page is closed. The streams are named `prism-meter` and `pw_watch` hides them
//! (they are not apps recording, and the privacy pill must not count them).

use pipewire as pw;
use pw::{properties::properties, spa};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Node name of the meter streams.
pub const NODE_NAME: &str = "prism-meter";
/// Streams stop this long after the last `want`.
const LINGER: Duration = Duration::from_secs(1);
/// The worker (and its PipeWire connection) exits after this long without meters.
const EXIT_AFTER: Duration = Duration::from_secs(30);
/// How fast a shown peak falls (per second, exponential).
const DECAY: f64 = 6.0;

/// A device to meter: its node id (key of `level`), node.name, and whether it is a sink.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Want {
    pub id: u32,
    pub name: String,
    pub sink: bool,
}

struct Shared {
    wanted: Vec<Want>,
    asked: Instant,
    running: bool,
    /// Peak (linear, 0..1) and when it was taken.
    peaks: HashMap<u32, (f64, Instant)>,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Shared { wanted: Vec::new(), asked: Instant::now(), running: false, peaks: HashMap::new() }))
}

fn decayed(peak: f64, at: Instant, now: Instant) -> f64 {
    peak * (-(now.saturating_duration_since(at).as_secs_f64()) * DECAY).exp()
}

/// Meter these devices now (call on every frame that shows them).
pub fn want(devices: Vec<Want>) {
    let mut s = shared().lock().unwrap();
    s.wanted = devices;
    s.asked = Instant::now();
    if !s.running {
        s.running = true;
        std::thread::Builder::new()
            .name("prism-meters".into())
            .spawn(|| {
                if let Err(e) = run() {
                    eprintln!("[prism-ui] level meters: {e}");
                }
                shared().lock().unwrap().running = false;
            })
            .ok();
    }
}

/// Level of device `id` for display (0..1, on the same cubic scale as volumes); None while
/// it has no meter yet.
pub fn level(id: u32) -> Option<f64> {
    let s = shared().lock().unwrap();
    s.peaks.get(&id).map(|&(p, at)| decayed(p, at, Instant::now()).cbrt().clamp(0.0, 1.0))
}

/// True while any meter shows a level (it rises or falls: the page must redraw).
pub fn moving() -> bool {
    let s = shared().lock().unwrap();
    let now = Instant::now();
    s.peaks.values().any(|&(p, at)| decayed(p, at, now) > 1e-6)
}

fn record(id: u32, peak: f64) {
    let now = Instant::now();
    let mut s = shared().lock().unwrap();
    let e = s.peaks.entry(id).or_insert((0.0, now));
    let shown = decayed(e.0, e.1, now);
    *e = (peak.max(shown), now);
}

/// Largest |sample| in a buffer of f32 samples (any channel count).
fn peak_of(bytes: &[u8]) -> f64 {
    bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c).abs()).fold(0.0f32, f32::max) as f64
}

fn run() -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_rc(None).map_err(|e| format!("PipeWire: {e}"))?;
    let mut streams: HashMap<Want, (pw::stream::StreamBox, pw::stream::StreamListener<u32>)> = HashMap::new();
    let mut idle_since: Option<Instant> = None;
    loop {
        mainloop.loop_().iterate(pw::loop_::Timeout::Finite(Duration::from_millis(100)));
        let wanted = {
            let mut s = shared().lock().unwrap();
            let active = s.asked.elapsed() < LINGER;
            if !active && streams.is_empty() && idle_since.is_some_and(|t| t.elapsed() > EXIT_AFTER) {
                // Exits under the lock, so a `want` now starts a new worker.
                s.running = false;
                s.peaks.clear();
                return Ok(());
            }
            if active { s.wanted.clone() } else { Vec::new() }
        };
        streams.retain(|w, (st, _)| {
            let keep = wanted.contains(w);
            if !keep {
                let _ = st.disconnect();
                shared().lock().unwrap().peaks.remove(&w.id);
            }
            keep
        });
        for w in wanted {
            if streams.contains_key(&w) {
                continue;
            }
            match open(&core, &w) {
                Ok(s) => {
                    streams.insert(w, s);
                }
                Err(e) => eprintln!("[prism-ui] level meter for {}: {e}", w.name),
            }
        }
        if streams.is_empty() {
            idle_since.get_or_insert_with(Instant::now);
        } else {
            idle_since = None;
        }
    }
}

fn open<'c>(core: &'c pw::core::CoreRc, w: &Want) -> Result<(pw::stream::StreamBox<'c>, pw::stream::StreamListener<u32>), String> {
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Monitor",
        *pw::keys::NODE_NAME => NODE_NAME,
        *pw::keys::APP_NAME => "Prism",
        "target.object" => w.name.as_str(),
        "stream.monitor" => "true",
        "node.dont-reconnect" => "true",
    };
    if w.sink {
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        // A sink's monitor must not keep the sink awake.
        props.insert("node.passive", "true");
    }
    let stream = pw::stream::StreamBox::new(core, "Prism level meter", props).map_err(|e| e.to_string())?;
    let listener = stream
        .add_local_listener_with_user_data(w.id)
        .process(|stream, id| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let size = d.chunk().size() as usize;
            let Some(bytes) = d.data() else { return };
            record(*id, peak_of(&bytes[..size.min(bytes.len())]));
        })
        .register()
        .map_err(|e| e.to_string())?;
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    let obj = spa::pod::Object { type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(), id: spa::param::ParamType::EnumFormat.as_raw(), properties: info.into() };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj)).map_err(|e| format!("{e:?}"))?.0.into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).ok_or("bad format pod")?];
    stream
        .connect(spa::utils::Direction::Input, None, pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS, &mut params)
        .map_err(|e| e.to_string())?;
    Ok((stream, listener))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_and_decay() {
        let bytes: Vec<u8> = [0.1f32, -0.5, 0.25].iter().flat_map(|f| f.to_le_bytes()).collect();
        assert_eq!(peak_of(&bytes), 0.5);
        let t = Instant::now();
        assert_eq!(decayed(0.5, t, t), 0.5);
        assert!(decayed(0.5, t, t + Duration::from_secs(1)) < 0.01);
    }

    /// `PRISM_METER_SINK=<node.name>` (or `PRISM_METER_SOURCE`) `cargo test --release -p prism-ui meters_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn meters_live() {
        // A sink is only metered while it plays (the monitor is passive); a source always.
        let (name, sink) = match (std::env::var("PRISM_METER_SINK"), std::env::var("PRISM_METER_SOURCE")) {
            (Ok(n), _) => (n, true),
            (_, Ok(n)) => (n, false),
            _ => panic!("set PRISM_METER_SINK or PRISM_METER_SOURCE"),
        };
        let w = Want { id: 1, name, sink };
        let mut top: f64 = 0.0;
        for _ in 0..30 {
            want(vec![w.clone()]);
            std::thread::sleep(Duration::from_millis(100));
            top = top.max(level(1).unwrap_or(0.0));
        }
        println!("highest level: {top:.3}");
    }
}
