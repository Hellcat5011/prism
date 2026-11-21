//! Video wallpaper decoder. Runs as its own process, so a bad file can never take the desktop or
//! the lock screen down with it, and fills frames straight into memory the display side shares.
//!
//! `prism-video play <file> <width> <height> <fd> [smooth]`: loops `file`, cropped to cover the
//! size; up to 30 frames a second with bilinear scaling, or with `smooth` 60 and lanczos.
//! `fd` is shared memory of `SLOTS` frames. Each finished frame is announced on stdout as
//! (slot: u32, time in ms: u64), little endian; a slot may be written again once its number
//! comes back on stdin. Exits when stdin closes.
//!
//! `prism-video still <file> <out.ppm>`: writes one picture from the video.

use ffmpeg_next as ff;
use ff::format::Pixel;
use ff::software::scaling;
use std::collections::VecDeque;
use std::io::Write;

/// Frames in the shared memory (`prism_common::video::SLOTS`).
const SLOTS: u32 = 4;
/// Wallpapers do not need more than 30 frames a second; the rest of a faster video is skipped.
const MIN_FRAME_MS: u64 = 33;
/// `smooth`: up to 60. 16 rather than 16.7 so a 60 fps video (frames 16 or 17 ms apart, in whole
/// ms) keeps every frame.
const SMOOTH_FRAME_MS: u64 = 16;

type Res<T> = Result<T, String>;

fn err(what: &str) -> impl Fn(ff::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// Open a decoder for the stream: the GPU's when it has one for this codec.
fn open_decoder(params: ff::codec::Parameters, gpu: bool) -> Res<ff::decoder::Video> {
    let name = match params.id() {
        ff::codec::Id::H264 => "h264_cuvid",
        ff::codec::Id::HEVC => "hevc_cuvid",
        ff::codec::Id::AV1 => "av1_cuvid",
        ff::codec::Id::VP9 => "vp9_cuvid",
        ff::codec::Id::VP8 => "vp8_cuvid",
        ff::codec::Id::MPEG2VIDEO => "mpeg2_cuvid",
        _ => "",
    };
    if let Some(codec) = ff::decoder::find_by_name(name).filter(|_| gpu) {
        let ctx = ff::codec::context::Context::from_parameters(params.clone()).map_err(err("decoder"))?;
        if let Ok(dec) = ctx.decoder().open_as(codec).and_then(|d| d.video()) {
            return Ok(dec);
        }
    }
    let mut ctx = ff::codec::context::Context::from_parameters(params).map_err(err("decoder"))?;
    ctx.set_threading(ff::threading::Config { kind: ff::threading::Type::Frame, count: 0 });
    ctx.decoder().video().map_err(err("decoder"))
}

/// Size that covers (w, h) keeping the aspect of (sw, sh).
fn cover(sw: u32, sh: u32, w: u32, h: u32) -> (u32, u32) {
    let k = (w as f64 / sw as f64).max(h as f64 / sh as f64);
    (((sw as f64 * k).ceil() as u32).max(w), ((sh as f64 * k).ceil() as u32).max(h))
}

struct Output {
    map: *mut u8,
    w: u32,
    h: u32,
    free: VecDeque<u32>,
}

impl Output {
    /// Wait for a slot to write into. None: the display side has gone.
    fn slot(&mut self) -> Option<u32> {
        while self.free.is_empty() {
            let mut buf = [0u8; 4 * SLOTS as usize];
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                return None;
            }
            for c in buf[..n as usize].chunks_exact(4) {
                self.free.push_back(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }
        self.free.pop_front()
    }

    /// Copy the middle of a BGRA picture into `slot`.
    fn store(&self, slot: u32, picture: &ff::frame::Video) {
        let row = self.w as usize * 4;
        let frame = row * self.h as usize;
        let stride = picture.stride(0);
        let x = (picture.width().saturating_sub(self.w) / 2) as usize * 4;
        let y = (picture.height().saturating_sub(self.h) / 2) as usize;
        let data = picture.data(0);
        for line in 0..self.h as usize {
            let from = (y + line) * stride + x;
            unsafe { std::ptr::copy_nonoverlapping(data[from..from + row].as_ptr(), self.map.add(slot as usize * frame + line * row), row) };
        }
    }
}

fn announce(slot: u32, ms: u64) -> Option<()> {
    let mut record = [0u8; 12];
    record[..4].copy_from_slice(&slot.to_le_bytes());
    record[4..].copy_from_slice(&ms.to_le_bytes());
    std::io::stdout().write_all(&record).ok()?;
    std::io::stdout().flush().ok()
}

fn play(file: &str, w: u32, h: u32, fd: i32, smooth: bool) -> Res<()> {
    let (min_frame, filter) = if smooth { (SMOOTH_FRAME_MS, scaling::Flags::LANCZOS) } else { (MIN_FRAME_MS, scaling::Flags::BILINEAR) };
    let size = SLOTS as usize * (w * h * 4) as usize;
    let map = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0) };
    if map == libc::MAP_FAILED {
        return Err("cannot map the frame memory".into());
    }
    let mut out = Output { map: map as *mut u8, w, h, free: (0..SLOTS).collect() };

    let mut input = ff::format::input(file).map_err(err("open"))?;
    let stream = input.streams().best(ff::media::Type::Video).ok_or("no video stream")?;
    let index = stream.index();
    let base = stream.time_base();
    let ms_per_tick = base.numerator() as f64 * 1000.0 / base.denominator() as f64;
    let params = stream.parameters();
    let mut decoder = open_decoder(params.clone(), true)?;
    let mut scaler: Option<(scaling::Context, Pixel, u32, u32)> = None;
    let mut picture = ff::frame::Video::empty();
    let mut frame = ff::frame::Video::empty();
    // Time of the first frame of this pass, and what earlier passes added up to.
    let (mut first, mut offset, mut last, mut next) = (None, 0u64, 0u64, 0u64);
    let mut decoded_any = false;
    let mut gpu = true;

    loop {
        let mut ended = false;
        let packet = input.packets().find(|(s, _)| s.index() == index).map(|(_, p)| p);
        let sent = match &packet {
            Some(p) => decoder.send_packet(p),
            None => {
                ended = true;
                decoder.send_eof()
            }
        };
        // The GPU cannot play this file: start over on the processor.
        if sent.is_err() && !decoded_any && gpu {
            gpu = false;
            decoder = open_decoder(params.clone(), false)?;
            input.seek(0, ..).map_err(err("seek"))?;
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            decoded_any = true;
            let pts = frame.timestamp().unwrap_or(0).max(0) as f64 * ms_per_tick;
            let start = *first.get_or_insert(pts as u64);
            let ms = offset + (pts as u64).saturating_sub(start);
            last = last.max(ms);
            if ms < next {
                continue;
            }
            next = ms + min_frame;
            let (fmt, sw, sh) = (frame.format(), frame.width(), frame.height());
            if !matches!(&scaler, Some((_, f, a, b)) if *f == fmt && *a == sw && *b == sh) {
                let (cw, ch) = cover(sw, sh, w, h);
                let ctx = scaling::Context::get(fmt, sw, sh, Pixel::BGRA, cw, ch, filter).map_err(err("scaler"))?;
                scaler = Some((ctx, fmt, sw, sh));
            }
            scaler.as_mut().unwrap().0.run(&frame, &mut picture).map_err(err("scale"))?;
            let Some(slot) = out.slot() else { return Ok(()) };
            out.store(slot, &picture);
            if announce(slot, ms).is_none() {
                return Ok(());
            }
        }
        if ended {
            if !decoded_any {
                return Err("the file holds no frames".into());
            }
            // Around again, continuing the clock.
            offset = last + min_frame;
            first = None;
            next = offset;
            input.seek(0, ..).map_err(err("seek"))?;
            decoder.flush();
        }
    }
}

/// One picture from the video, a second in so that a fade from black is over.
fn still(file: &str, out: &str) -> Res<()> {
    let mut input = ff::format::input(file).map_err(err("open"))?;
    let stream = input.streams().best(ff::media::Type::Video).ok_or("no video stream")?;
    let index = stream.index();
    let mut decoder = open_decoder(stream.parameters(), false)?;
    let _ = input.seek(ff::ffi::AV_TIME_BASE as i64, ..);
    let mut frame = ff::frame::Video::empty();
    let mut got = false;
    for (s, p) in input.packets() {
        if s.index() == index && decoder.send_packet(&p).is_ok() && decoder.receive_frame(&mut frame).is_ok() {
            got = true;
            break;
        }
    }
    if !got {
        let _ = decoder.send_eof();
        decoder.receive_frame(&mut frame).map_err(err("decode"))?;
    }
    let (w, h) = (frame.width(), frame.height());
    let mut rgb = ff::frame::Video::empty();
    scaling::Context::get(frame.format(), w, h, Pixel::RGB24, w, h, scaling::Flags::BILINEAR).map_err(err("scaler"))?.run(&frame, &mut rgb).map_err(err("scale"))?;
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    let (stride, data) = (rgb.stride(0), rgb.data(0));
    for y in 0..h as usize {
        bytes.extend_from_slice(&data[y * stride..y * stride + w as usize * 3]);
    }
    std::fs::write(out, bytes).map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Behind the desktop in every way: decoding must never cost the UI a frame.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 5) };
    if let Err(e) = ff::init() {
        eprintln!("[prism-video] {e}");
        std::process::exit(1);
    }
    ff::util::log::set_level(ff::util::log::Level::Fatal);
    let result = match args.as_slice() {
        ["play", file, w, h, fd, rest @ ..] if rest.len() <= 1 => match (w.parse(), h.parse(), fd.parse()) {
            (Ok(w), Ok(h), Ok(fd)) => play(file, w, h, fd, rest == ["smooth"]),
            _ => Err("bad arguments".into()),
        },
        ["still", file, out] => still(file, out),
        _ => Err("usage: prism-video play <file> <w> <h> <fd> [smooth] | still <file> <out.ppm>".into()),
    };
    if let Err(e) = result {
        eprintln!("[prism-video] {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_covers_the_screen_whatever_its_shape() {
        assert_eq!(cover(1920, 1080, 2560, 1440), (2560, 1440));
        // Wider than the screen: full height, sides cropped.
        assert_eq!(cover(2000, 500, 1000, 500), (2000, 500));
        // Taller than the screen: full width, top and bottom cropped.
        let (w, h) = cover(1080, 1920, 2560, 1440);
        assert!(w == 2560 && h >= 1440);
    }
}
