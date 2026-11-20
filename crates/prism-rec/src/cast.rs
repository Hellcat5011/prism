//! `prism-rec --cast '<CastJob JSON>'`: screen sharing for the ScreenCast portal (prism-ui's
//! `xdp.rs`). One output or window is captured with prism-capture and published as a PipeWire
//! video source; the portal hands its node to the app. Prints `{"node":N,"width":W,"height":H}`
//! on stdout once the node exists, then streams until SIGINT/SIGTERM, the parent dying, or the
//! source going away.
//!
//! Two threads: capture (its own Wayland connection) keeps the newest frame in a shared slot
//! and wakes the PipeWire loop, which (as the graph's driver) copies it into a buffer.

use pipewire as pw;
use pw::{properties::properties, spa};
use prism_common::capture::CastJob;
use spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Rectangle, SpaTypes};
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_shm;

use crate::STOP;

/// Frames are sent at most this often.
const MAX_FPS: u32 = 60;

struct Latest {
    data: Vec<u8>,
    width: u32,
    height: u32,
    stride: u32,
    format: spa::param::video::VideoFormat,
    fresh: bool,
}

enum Msg {
    /// A new frame is in the slot.
    Frame,
    /// The capture ended (source gone or error).
    Ended(String),
}

pub fn run(job: &CastJob) -> Result<(), String> {
    // End with the portal (prism-ui) if it goes away without stopping us.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
    let slot: Arc<Mutex<Option<Latest>>> = Arc::new(Mutex::new(None));

    // The first frame decides the format and size the stream offers.
    let (first_tx, first_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (pw_tx, pw_rx) = pw::channel::channel::<Msg>();
    {
        let job = job.clone();
        let slot = slot.clone();
        std::thread::spawn(move || {
            let r = capture_loop(&job, &slot, &first_tx, &pw_tx);
            let _ = first_tx.send(r.clone());
            let _ = pw_tx.send(Msg::Ended(r.err().unwrap_or_else(|| "stopped".into())));
        });
    }
    first_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "no frame from the compositor".to_string())??;
    let (width, height, format) = {
        let s = slot.lock().unwrap();
        let l = s.as_ref().ok_or("no frame")?;
        (l.width, l.height, l.format)
    };

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_rc(None).map_err(|e| e.to_string())?;
    let stream = pw::stream::StreamRc::new(
        core,
        "prism-screencast",
        properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "prism-screencast",
        },
    )
    .map_err(|e| e.to_string())?;

    let reported = std::cell::Cell::new(false);
    let size = Arc::new(Mutex::new((width, height)));
    let size_l = size.clone();
    let size_rx = size.clone();
    let slot_p = slot.clone();
    let ml = mainloop.clone();
    let _listener = stream
        .add_local_listener::<()>()
        .state_changed(move |s, _, _old, new| {
            if let pw::stream::StreamState::Error(e) = &new {
                eprintln!("[prism-rec] cast stream error: {e}");
                ml.quit();
            }
            if new == pw::stream::StreamState::Streaming {
                // Send what is on screen now; a still screen gives no new frames.
                let _ = s.trigger_process();
            }
            if matches!(new, pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming) && !reported.get() {
                reported.set(true);
                let (w, h) = *size_l.lock().unwrap();
                let mut out = std::io::stdout();
                let _ = writeln!(out, "{{\"node\":{},\"width\":{w},\"height\":{h}}}", s.node_id());
                let _ = out.flush();
            }
        })
        .param_changed(move |s, _, id, param| {
            if id != spa::param::ParamType::Format.as_raw() || param.is_none() {
                return;
            }
            // Format fixed: say how big the buffers are.
            let (w, h) = *size.lock().unwrap();
            if let Ok(bytes) = buffers_param(w * 4, h) {
                if let Some(pod) = Pod::from_bytes(&bytes) {
                    let _ = s.update_params(&mut [pod]);
                }
            }
        })
        .process(move |s, _| {
            let Some(mut buf) = s.dequeue_buffer() else { return };
            let mut slot = slot_p.lock().unwrap();
            let Some(l) = slot.as_mut() else { return };
            let datas = buf.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let dst_stride = (l.width * 4) as usize;
            let rows = l.height as usize;
            if let Some(dst) = d.data() {
                if dst.len() >= dst_stride * rows {
                    for y in 0..rows {
                        let s0 = y * l.stride as usize;
                        dst[y * dst_stride..(y + 1) * dst_stride].copy_from_slice(&l.data[s0..s0 + dst_stride]);
                    }
                }
            }
            l.fresh = false;
            let c = d.chunk_mut();
            *c.offset_mut() = 0;
            *c.stride_mut() = dst_stride as i32;
            *c.size_mut() = (dst_stride * rows) as u32;
        })
        .register()
        .map_err(|e| e.to_string())?;

    let fmt = format_param(format, width, height)?;
    let mut params = [Pod::from_bytes(&fmt).ok_or("bad format pod")?];
    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;

    let st = stream.clone();
    let ml = mainloop.clone();
    let slot_m = slot.clone();
    let current = std::cell::Cell::new((width, height, format));
    let _rx = pw_rx.attach(mainloop.loop_(), move |msg| match msg {
        Msg::Ended(why) => {
            eprintln!("[prism-rec] cast ended: {why}");
            ml.quit();
        }
        Msg::Frame => {
            let (w, h, f) = match slot_m.lock().unwrap().as_ref() {
                Some(l) => (l.width, l.height, l.format),
                None => return,
            };
            if (w, h, f) != current.get() {
                // A window was resized: offer the new size; frames wait for the new buffers.
                current.set((w, h, f));
                *size_rx.lock().unwrap() = (w, h);
                if let Ok(bytes) = format_param(f, w, h) {
                    if let Some(pod) = Pod::from_bytes(&bytes) {
                        let _ = st.update_params(&mut [pod]);
                    }
                }
                return;
            }
            if st.state() == pw::stream::StreamState::Streaming {
                let _ = st.trigger_process();
            }
        }
    });
    // SIGINT/SIGTERM only set STOP: poll it. While the screen is still, repeat the last frame
    // once a second so consumers that join late (or drop frames) still get a picture.
    let ml = mainloop.clone();
    let st = stream.clone();
    let idle = std::cell::Cell::new(0u32);
    let slot_t = slot.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if STOP.load(Ordering::SeqCst) {
            ml.quit();
        }
        let fresh = slot_t.lock().unwrap().as_ref().is_some_and(|l| l.fresh);
        idle.set(if fresh { 0 } else { idle.get() + 1 });
        if idle.get() >= 10 && st.state() == pw::stream::StreamState::Streaming {
            idle.set(0);
            let _ = st.trigger_process();
        }
    });
    let _ = timer.update_timer(Some(Duration::from_millis(100)), Some(Duration::from_millis(100)));
    mainloop.run();
    STOP.store(true, Ordering::SeqCst);
    let _ = stream.disconnect();
    Ok(())
}

fn capture_loop(job: &CastJob, slot: &Mutex<Option<Latest>>, first: &std::sync::mpsc::Sender<Result<(), String>>, wake: &pw::channel::Sender<Msg>) -> Result<(), String> {
    let conn = wayland_client::Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let mut cap = prism_capture::Capturer::new(&conn)?;
    match (&job.output, &job.window) {
        (Some(name), _) => {
            let outs = cap.outputs();
            let o = outs.iter().find(|o| &o.name == name).cloned().ok_or_else(|| format!("output {name} is gone"))?;
            cap.start_stream(Some(&o), None, job.cursor)?;
        }
        (None, Some(id)) => {
            let w = cap.windows().into_iter().find(|w| &w.id == id).ok_or("the window is gone")?;
            cap.start_stream(None, Some(&w), job.cursor)?;
        }
        _ => return Err("nothing to share".into()),
    }
    let min_gap = Duration::from_secs(1) / MAX_FPS;
    let mut last = Instant::now() - min_gap;
    let mut sent_first = false;
    while !STOP.load(Ordering::SeqCst) {
        let gap = last.elapsed();
        if gap < min_gap {
            std::thread::sleep(min_gap - gap);
        }
        let Some(frame) = cap.next_frame(Duration::from_millis(250))? else { continue };
        last = Instant::now();
        let format = match frame.format {
            wl_shm::Format::Xrgb8888 => spa::param::video::VideoFormat::BGRx,
            wl_shm::Format::Argb8888 => spa::param::video::VideoFormat::BGRA,
            wl_shm::Format::Xbgr8888 => spa::param::video::VideoFormat::RGBx,
            wl_shm::Format::Abgr8888 => spa::param::video::VideoFormat::RGBA,
            other => return Err(format!("unsupported capture format {other:?}")),
        };
        {
            let mut s = slot.lock().unwrap();
            let l = s.get_or_insert_with(|| Latest { data: Vec::new(), width: 0, height: 0, stride: 0, format, fresh: false });
            l.data.clear();
            l.data.extend_from_slice(frame.data);
            (l.width, l.height, l.stride, l.format, l.fresh) = (frame.width, frame.height, frame.stride, format, true);
        }
        if !sent_first {
            sent_first = true;
            let _ = first.send(Ok(()));
        }
        if wake.send(Msg::Frame).is_err() {
            break;
        }
    }
    Ok(())
}

fn serialize(obj: Object) -> Result<Vec<u8>, String> {
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj)).map_err(|e| format!("{e:?}"))?.0.into_inner())
}

fn format_param(format: spa::param::video::VideoFormat, width: u32, height: u32) -> Result<Vec<u8>, String> {
    use spa::param::format::{FormatProperties as F, MediaSubtype, MediaType};
    let obj = spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(F::MediaType, Id, MediaType::Video),
        spa::pod::property!(F::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(F::VideoFormat, Id, format),
        spa::pod::property!(F::VideoSize, Rectangle, Rectangle { width, height }),
        spa::pod::property!(F::VideoFramerate, Fraction, Fraction { num: 0, denom: 1 }),
        spa::pod::property!(
            F::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: MAX_FPS, denom: 1 },
            Fraction { num: 1, denom: 1 },
            Fraction { num: MAX_FPS, denom: 1 }
        ),
    );
    serialize(obj)
}

fn buffers_param(stride: u32, height: u32) -> Result<Vec<u8>, String> {
    let int = |v: i32| Value::Int(v);
    let range = |d: i32, min: i32, max: i32| Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), ChoiceEnum::Range { default: d, min, max })));
    let data_types = (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_MemPtr);
    let obj = Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa::sys::SPA_PARAM_Buffers,
        properties: vec![
            Property::new(spa::sys::SPA_PARAM_BUFFERS_buffers, range(4, 2, 8)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_blocks, int(1)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_size, int((stride * height) as i32)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_stride, int(stride as i32)),
            Property::new(spa::sys::SPA_PARAM_BUFFERS_dataType, int(data_types)),
        ],
    };
    serialize(obj)
}
