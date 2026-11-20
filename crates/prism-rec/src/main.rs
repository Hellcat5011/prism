//! prism-rec: Prism's screen recorder (SCREEN_CAPTURE_PLAN.md). prismd starts it with a JSON
//! `RecordJob` as the only argument and stops it with SIGINT; it writes the file as it goes
//! and exits when done. Capture is ext-image-copy-capture (prism-capture), encoding is the
//! system FFmpeg (NVENC when available).

mod audio;
mod cast;
mod encode;

use ffmpeg_next::format::Pixel;
use prism_common::capture::{RecordJob, RecordTarget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_shm;

pub(crate) static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--cast") {
        unsafe {
            libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
        let job: prism_common::capture::CastJob = match std::env::args().nth(2).map(|a| serde_json::from_str(&a)) {
            Some(Ok(j)) => j,
            _ => {
                eprintln!("usage: prism-rec --cast '<CastJob JSON>' (started by the ScreenCast portal)");
                std::process::exit(2);
            }
        };
        if let Err(e) = cast::run(&job) {
            eprintln!("[prism-rec] cast: {e}");
            std::process::exit(1);
        }
        return;
    }
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: prism-rec '<RecordJob JSON>' (started by prismd)");
        std::process::exit(2);
    };
    let job: RecordJob = match serde_json::from_str(&arg) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("[prism-rec] bad job: {e}");
            std::process::exit(2);
        }
    };
    unsafe {
        libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    match record(&job) {
        Ok(()) => eprintln!("[prism-rec] saved {}", job.path),
        Err(e) => {
            eprintln!("[prism-rec] {e}");
            std::process::exit(1);
        }
    }
}

fn record(job: &RecordJob) -> Result<(), String> {
    if let Some(dir) = std::path::Path::new(&job.path).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let conn = wayland_client::Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let mut cap = prism_capture::Capturer::new(&conn)?;
    // What to capture, and which part of each frame to keep.
    let mut crop: Option<(u32, u32, u32, u32)> = None;
    match &job.target {
        RecordTarget::Output { name } | RecordTarget::Region { output: name, .. } => {
            let outs = cap.outputs();
            let o = outs.iter().find(|o| &o.name == name).or(outs.first()).cloned().ok_or("no outputs")?;
            cap.start_stream(Some(&o), None, job.cursor)?;
            if let RecordTarget::Region { x, y, w, h, .. } = job.target {
                crop = Some((x.max(0) as u32, y.max(0) as u32, w.max(0) as u32, h.max(0) as u32));
            }
        }
        RecordTarget::Window { id, title } => {
            let w = cap.windows().into_iter().find(|w| &w.id == id).ok_or_else(|| format!("window {title:?} is gone"))?;
            cap.start_stream(None, Some(&w), job.cursor)?;
        }
    }

    let mut audio = if job.audio_desktop || job.audio_mic { Some(audio::Audio::start(job.audio_desktop, job.audio_mic)?) } else { None };
    // Recording clock: CLOCK_MONOTONIC at the first frame (frame times use the same clock).
    
    let interval = Duration::from_nanos(1_000_000_000 / job.fps.clamp(1, 240) as u64);
    // Open the encoder before the first frame (NVENC takes a moment), so the audio clock and
    // the first frame start together.
    let (sw, sh) = cap.stream_size().ok_or("no stream size")?;
    let clamp_area = |c: Option<(u32, u32, u32, u32)>, fw: u32, fh: u32| {
        c.map(|(x, y, w, h)| {
            let x = x.min(fw.saturating_sub(16));
            let y = y.min(fh.saturating_sub(16));
            (x, y, w.min(fw - x), h.min(fh - y))
        })
        .unwrap_or((0, 0, fw, fh))
    };
    let first = clamp_area(crop, sw, sh);
    let opts = encode::VideoOpts { width: first.2, height: first.3, fps: job.fps, codec: job.codec.clone(), quality: job.quality.clone() };
    let mut rec: Option<encode::Recorder> = Some(encode::Recorder::new(&job.path, &opts, audio.is_some())?);
    if let Some(a) = audio.as_mut() {
        a.clear();
    }
    let clock0: Option<Instant> = Some(Instant::now());
    let mut t0: Option<u64> = None;
    let mut next = Instant::now();
    let mut frames = 0u64;
    let started = Instant::now();
    while !STOP.load(Ordering::SeqCst) {
        // Pace requests to the fps cap.
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next = Instant::now() + interval;
        let frame = match cap.next_frame(Duration::from_millis(250)) {
            Ok(Some(f)) => f,
            Ok(None) => {
                // Still screen: keep the audio flowing.
                if let (Some(a), Some(c0), Some(r)) = (audio.as_mut(), clock0, rec.as_mut()) {
                    let s = a.take(c0.elapsed(), false);
                    r.push_audio(&s)?;
                }
                continue;
            }
            Err(e) => {
                eprintln!("[prism-rec] capture ended: {e}");
                break;
            }
        };
        let fmt = match frame.format {
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Pixel::BGRZ,
            wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Pixel::RGBZ,
            other => return Err(format!("unsupported capture format {other:?}")),
        };
        let area = clamp_area(crop, frame.width, frame.height);
        // Video time 0 = the recording clock's start, on the compositor's monotonic clock.
        let t = *t0.get_or_insert_with(|| {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
            let now_ns = ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64;
            let since = clock0.map(|c| c.elapsed().as_nanos() as u64).unwrap_or(0);
            now_ns.saturating_sub(since).min(frame.time_ns)
        });
        let pts = (frame.time_ns.saturating_sub(t) / 1_000_000) as i64;
        rec.as_mut().unwrap().push_video(frame.data, frame.stride as usize, fmt, area, pts)?;
        frames += 1;
        if let (Some(a), Some(c0)) = (audio.as_mut(), clock0) {
            let s = a.take(c0.elapsed(), false);
            rec.as_mut().unwrap().push_audio(&s)?;
        }
    }
    cap.stop_stream();
    let mut rec = rec.ok_or("no frame was captured")?;
    if let (Some(a), Some(c0)) = (audio.as_mut(), clock0) {
        let s = a.take(c0.elapsed(), true);
        rec.push_audio(&s)?;
    }
    let secs = started.elapsed().as_secs_f64();
    eprintln!("[prism-rec] {frames} frames in {secs:.1} s with {}", rec.encoder_name);
    rec.finish()
}
