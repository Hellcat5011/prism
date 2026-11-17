//! Audio for recordings: PipeWire capture streams for desktop audio (the default sink's
//! monitor) and/or the microphone (the default source), 48 kHz stereo f32, mixed on the
//! recording clock. A source that delivers nothing (an idle sink is suspended) is filled with
//! silence, so audio never drifts from the wall clock and the video.

use pipewire as pw;
use pw::{properties::properties, spa};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Mixing runs this far behind real time so PipeWire's buffers have arrived.
const LATENCY_FRAMES: u64 = RATE as u64 / 10;
/// Keep at most this much unread audio per source (a stalled mixer must not grow forever).
const MAX_RING: usize = RATE as usize * CHANNELS * 2;

type Ring = Arc<Mutex<VecDeque<f32>>>;

pub struct Audio {
    rings: Vec<Ring>,
    /// Frames (per channel) handed out so far.
    written: u64,
}

impl Audio {
    /// Start capturing; returns once the streams are connected (or failed).
    pub fn start(desktop: bool, mic: bool) -> Result<Audio, String> {
        let mut rings = Vec::new();
        let mut wanted = Vec::new();
        if desktop {
            let r: Ring = Default::default();
            rings.push(r.clone());
            wanted.push((true, r));
        }
        if mic {
            let r: Ring = Default::default();
            rings.push(r.clone());
            wanted.push((false, r));
        }
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            if let Err(e) = run(wanted, &tx) {
                let _ = tx.send(Err(e));
            }
        });
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "PipeWire did not answer".to_string())??;
        Ok(Audio { rings, written: 0 })
    }

    /// Drop what was captured before the recording clock started.
    pub fn clear(&mut self) {
        for r in &self.rings {
            r.lock().unwrap().clear();
        }
        self.written = 0;
    }

    /// Mixed interleaved stereo samples due by `elapsed` since the recording started.
    pub fn take(&mut self, elapsed: Duration, flush: bool) -> Vec<f32> {
        let due = (elapsed.as_secs_f64() * RATE as f64) as u64;
        let target = if flush { due } else { due.saturating_sub(LATENCY_FRAMES) };
        if target <= self.written {
            return Vec::new();
        }
        let n = (target - self.written) as usize;
        let mut out = vec![0.0f32; n * CHANNELS];
        for ring in &self.rings {
            let mut r = ring.lock().unwrap();
            let have = r.len().min(n * CHANNELS);
            for (o, s) in out.iter_mut().zip(r.drain(..have)) {
                *o += s;
            }
        }
        if self.rings.len() > 1 {
            for s in out.iter_mut() {
                *s = s.clamp(-1.0, 1.0);
            }
        }
        self.written = target;
        out
    }
}

fn run(wanted: Vec<(bool, Ring)>, ready: &std::sync::mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_rc(None).map_err(|e| format!("PipeWire: {e}"))?;
    let mut keep = Vec::new();
    for (desktop, ring) in wanted {
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::NODE_NAME => if desktop { "prism-rec-desktop" } else { "prism-rec-mic" },
        };
        if desktop {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }
        let stream = pw::stream::StreamBox::new(&core, if desktop { "Prism recording (desktop audio)" } else { "Prism recording (microphone)" }, props).map_err(|e| e.to_string())?;
        let listener = stream
            .add_local_listener_with_user_data(ring)
            .process(|stream, ring| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                let Some(d) = datas.first_mut() else { return };
                let size = d.chunk().size() as usize;
                let Some(bytes) = d.data() else { return };
                let bytes = &bytes[..size.min(bytes.len())];
                let mut r = ring.lock().unwrap();
                r.extend(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
                let excess = r.len().saturating_sub(MAX_RING);
                r.drain(..excess);
            })
            .register()
            .map_err(|e| e.to_string())?;
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.set_format(spa::param::audio::AudioFormat::F32LE);
        info.set_rate(RATE);
        info.set_channels(CHANNELS as u32);
        let obj = spa::pod::Object { type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(), id: spa::param::ParamType::EnumFormat.as_raw(), properties: info.into() };
        let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj)).map_err(|e| format!("{e:?}"))?.0.into_inner();
        let mut params = [spa::pod::Pod::from_bytes(&values).ok_or("bad format pod")?];
        stream
            .connect(spa::utils::Direction::Input, None, pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS, &mut params)
            .map_err(|e| e.to_string())?;
        keep.push((stream, listener));
    }
    let _ = ready.send(Ok(()));
    mainloop.run();
    drop(keep);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_and_pads_on_the_clock() {
        let a: Ring = Default::default();
        let b: Ring = Default::default();
        a.lock().unwrap().extend([0.5f32; 8]);
        b.lock().unwrap().extend([0.75f32; 4]);
        let mut au = Audio { rings: vec![a, b], written: 0 };
        // 6 frames due (flush: no latency hold-back).
        let out = au.take(Duration::from_secs_f64(6.0 / RATE as f64), true);
        assert_eq!(out.len(), 12);
        assert_eq!(out[0], 1.0); // 0.5 + 0.75, clipped
        assert_eq!(out[5], 0.5); // b ran out: silence
        assert_eq!(out[11], 0.0); // both ran out
        assert!(au.take(Duration::from_secs_f64(6.0 / RATE as f64), true).is_empty());
    }
}
