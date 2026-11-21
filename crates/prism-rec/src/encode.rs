//! Encoding and muxing with the system FFmpeg libraries. Video prefers NVENC (BGR0 frames go
//! straight in; the GPU converts colour) and falls back to VAAPI-less software x264 (with a
//! swscale conversion to YUV 4:2:0). Timestamps are milliseconds; frames arrive when the screen
//! changes, so the stream is variable frame rate with exact timing. MKV is written as it goes;
//! MP4 is fragmented, so a killed recording is still playable.
//!
//! Replay buffer (`Recorder::new_replay`): packets stay in memory instead, the last few
//! seconds from a keyframe, and `save_replay` writes them to a file on request.

use ffmpeg_next as ff;
use ff::format::Pixel;
use ff::{codec, encoder, format, frame, Dictionary, Packet, Rational};

pub const TIME_BASE: Rational = Rational(1, 1000);

pub struct VideoOpts {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// "auto", "h264", "hevc", "av1".
    pub codec: String,
    /// "low", "medium", "high", "lossless".
    pub quality: String,
}

/// Audio track: Opus in MKV, AAC in MP4; 48 kHz stereo.
struct AudioTrack {
    enc: encoder::Audio,
    idx: usize,
    tb: Rational,
    planar: bool,
    frame_size: usize,
    pending: Vec<f32>,
    /// Next frame's pts in samples.
    pts: i64,
}

/// Replay buffer: encoded packets of the last `keep_ms`, starting at a video keyframe.
struct Ring {
    keep_ms: i64,
    /// (stream index, packet in its encoder time base, its time in ms).
    packets: std::collections::VecDeque<(usize, Packet, i64)>,
    /// Encoder time base of each stream index.
    tbs: Vec<Rational>,
    video_idx: usize,
}

impl Ring {
    fn push(&mut self, idx: usize, pkt: &Packet) {
        let tb = self.tbs.get(idx).copied().unwrap_or(TIME_BASE);
        let ms = pkt.pts().unwrap_or(0) * 1000 * tb.numerator() as i64 / tb.denominator().max(1) as i64;
        self.packets.push_back((idx, pkt.clone(), ms));
        let newest = self.packets.iter().map(|p| p.2).max().unwrap_or(ms);
        while self.packets.front().is_some_and(|p| newest - p.2 > self.keep_ms) {
            self.packets.pop_front();
        }
        // Start at a keyframe; while none is left yet keep everything.
        if self.packets.iter().any(|(i, p, _)| *i == self.video_idx && p.is_key()) {
            while self.packets.front().is_some_and(|(i, p, _)| !(*i == self.video_idx && p.is_key())) {
                self.packets.pop_front();
            }
        }
    }
}

pub struct Recorder {
    audio: Option<AudioTrack>,
    ring: Option<Ring>,
    /// (encoder name, parameters) per stream, for the replay file.
    stream_info: Vec<(String, codec::Parameters)>,
    octx: format::context::Output,
    video: encoder::Video,
    video_idx: usize,
    video_tb: Rational,
    /// Encoder input format; NVENC takes BGR0, software takes YUV420P.
    in_fmt: Pixel,
    scaler: Option<(Pixel, u32, u32, ff::software::scaling::Context)>,
    staging: frame::Video,
    converted: Option<frame::Video>,
    pub width: u32,
    pub height: u32,
    pub encoder_name: String,
    last_pts: i64,
}

/// Candidate encoders for a codec choice, best first.
fn candidates(codec: &str) -> &'static [&'static str] {
    match codec {
        "hevc" | "h265" => &["hevc_nvenc", "libx265"],
        "av1" => &["av1_nvenc", "libsvtav1", "libaom-av1"],
        _ => &["h264_nvenc", "libx264"],
    }
}

fn quality_opts(name: &str, quality: &str) -> Dictionary<'static> {
    let mut d = Dictionary::new();
    let level = match quality {
        "low" => 0,
        "high" => 2,
        "lossless" => 3,
        _ => 1,
    };
    if name.ends_with("_nvenc") {
        d.set("preset", "p4");
        d.set("tune", "hq");
        d.set("rc", "vbr");
        d.set("cq", ["32", "26", "21", "16"][level]);
        d.set("b", "0");
    } else if name == "libx264" || name == "libx265" {
        d.set("preset", "veryfast");
        d.set("crf", ["30", "23", "18", "12"][level]);
    } else if name == "libsvtav1" {
        d.set("preset", "10");
        d.set("crf", ["40", "32", "26", "20"][level]);
    }
    d
}

impl Recorder {
    pub fn new(path: &str, v: &VideoOpts, audio: bool) -> Result<Recorder, String> {
        ff::init().map_err(|e| e.to_string())?;
        ff::log::set_level(ff::log::Level::Error);
        let (w, h) = (v.width & !1, v.height & !1);
        if w < 16 || h < 16 {
            return Err(format!("{w}x{h} is too small to record"));
        }
        let mut last_err = String::from("no encoder found");
        for name in candidates(&v.codec) {
            let Some(c) = encoder::find_by_name(name) else { continue };
            match Self::open(path, v, w, h, c, name, audio) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    eprintln!("[prism-rec] {name}: {e}");
                    last_err = format!("{name}: {e}");
                }
            }
        }
        Err(last_err)
    }

    fn open(path: &str, v: &VideoOpts, w: u32, h: u32, c: ff::Codec, name: &str, with_audio: bool) -> Result<Recorder, String> {
        let mut octx = format::output(&path).map_err(|e| format!("{path}: {e}"))?;
        let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
        let in_fmt = if name.ends_with("_nvenc") { Pixel::BGRZ } else { Pixel::YUV420P };
        let mut enc = codec::context::Context::new_with_codec(c).encoder().video().map_err(|e| e.to_string())?;
        enc.set_width(w);
        enc.set_height(h);
        enc.set_format(in_fmt);
        enc.set_time_base(TIME_BASE);
        enc.set_frame_rate(Some(Rational(v.fps as i32, 1)));
        enc.set_gop(v.fps * 2);
        enc.set_max_b_frames(0);
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let video = enc.open_with(quality_opts(name, &v.quality)).map_err(|e| e.to_string())?;
        let video_idx = {
            let mut ost = octx.add_stream(c).map_err(|e| e.to_string())?;
            ost.set_parameters(&video);
            ost.set_time_base(TIME_BASE);
            ost.index()
        };
        let audio = if with_audio { Some(Self::audio_track(&mut octx, path, global_header)?) } else { None };
        let mut hopts = Dictionary::new();
        if path.ends_with(".mp4") {
            hopts.set("movflags", "frag_keyframe+empty_moov+default_base_moof");
        }
        octx.write_header_with(hopts).map_err(|e| e.to_string())?;
        let mut stream_info = vec![(name.to_string(), codec::Parameters::from(&video))];
        if let Some(a) = &audio {
            stream_info.push((a.enc.codec().map(|c| c.name().to_string()).unwrap_or_default(), codec::Parameters::from(&a.enc)));
        }
        let video_tb = octx.stream(video_idx).map(|s| s.time_base()).unwrap_or(TIME_BASE);
        let audio = audio.map(|mut a| {
            a.tb = octx.stream(a.idx).map(|s| s.time_base()).unwrap_or(a.tb);
            a
        });
        eprintln!("[prism-rec] {w}x{h} with {name} into {path}");
        Ok(Recorder {
            ring: None,
            stream_info,
            audio,
            octx,
            video,
            video_idx,
            video_tb,
            in_fmt,
            scaler: None,
            staging: frame::Video::new(Pixel::BGRZ, w, h),
            converted: (in_fmt != Pixel::BGRZ).then(|| frame::Video::new(in_fmt, w, h)),
            width: w,
            height: h,
            encoder_name: name.to_string(),
            last_pts: -1,
        })
    }

    /// Encode one captured frame: `data` is `src_w x src_h` 32-bit pixels (`fmt`: BGRZ for
    /// XRGB8888, RGBZ for XBGR8888) with `stride`; the `crop` rectangle (x, y, w, h) of it is
    /// recorded (scaled if its size differs from the encoder's, e.g. a resized window).
    #[allow(clippy::too_many_arguments)]
    pub fn push_video(&mut self, data: &[u8], stride: usize, fmt: Pixel, crop: (u32, u32, u32, u32), pts_ms: i64) -> Result<(), String> {
        if pts_ms <= self.last_pts {
            return Ok(());
        }
        self.last_pts = pts_ms;
        let (cx, cy, cw, ch) = crop;
        let same_size = cw & !1 == self.width && ch & !1 == self.height;
        if same_size && fmt == Pixel::BGRZ {
            // Copy the crop rows into the staging frame (BGR0).
            let (w, h) = (self.width as usize, self.height as usize);
            let dst_stride = self.staging.stride(0);
            let dst = self.staging.data_mut(0);
            for row in 0..h {
                let s = (cy as usize + row) * stride + cx as usize * 4;
                dst[row * dst_stride..row * dst_stride + w * 4].copy_from_slice(&data[s..s + w * 4]);
            }
            if let Some(conv) = self.converted.as_mut() {
                let sc = Self::scaler_for(&mut self.scaler, Pixel::BGRZ, self.width, self.height, self.in_fmt, self.width, self.height)?;
                sc.run(&self.staging, conv).map_err(|e| e.to_string())?;
                conv.set_pts(Some(pts_ms));
                Self::send(&mut self.video, &mut self.octx, &mut self.ring, self.video_idx, self.video_tb, Some(conv))
            } else {
                self.staging.set_pts(Some(pts_ms));
                Self::send(&mut self.video, &mut self.octx, &mut self.ring, self.video_idx, self.video_tb, Some(&self.staging))
            }
        } else {
            // Different size or channel order: wrap the crop and let swscale convert.
            let mut src = frame::Video::new(fmt, cw, ch);
            let s_stride = src.stride(0);
            let sd = src.data_mut(0);
            for row in 0..ch as usize {
                let s = (cy as usize + row) * stride + cx as usize * 4;
                sd[row * s_stride..row * s_stride + cw as usize * 4].copy_from_slice(&data[s..s + cw as usize * 4]);
            }
            let mut out = frame::Video::new(self.in_fmt, self.width, self.height);
            let sc = Self::scaler_for(&mut self.scaler, fmt, cw, ch, self.in_fmt, self.width, self.height)?;
            sc.run(&src, &mut out).map_err(|e| e.to_string())?;
            out.set_pts(Some(pts_ms));
            Self::send(&mut self.video, &mut self.octx, &mut self.ring, self.video_idx, self.video_tb, Some(&out))
        }
    }

    fn audio_track(octx: &mut format::context::Output, path: &str, global_header: bool) -> Result<AudioTrack, String> {
        let names: &[&str] = if path.ends_with(".mp4") { &["aac"] } else { &["libopus", "aac"] };
        let c = names.iter().find_map(|n| encoder::find_by_name(n)).ok_or("no audio encoder (libopus/aac)")?;
        let mut enc = codec::context::Context::new_with_codec(c).encoder().audio().map_err(|e| e.to_string())?;
        let rate = crate::audio::RATE as i32;
        enc.set_rate(rate);
        enc.set_channel_layout(ff::ChannelLayout::STEREO);
        // Opus takes packed float, AAC planar float.
        let planar = c.name() == "aac";
        enc.set_format(if planar { ff::format::Sample::F32(ff::format::sample::Type::Planar) } else { ff::format::Sample::F32(ff::format::sample::Type::Packed) });
        enc.set_time_base(Rational(1, rate));
        enc.set_bit_rate(160_000);
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let enc = enc.open_with(Dictionary::new()).map_err(|e| format!("{}: {e}", c.name()))?;
        let idx = {
            let mut ost = octx.add_stream(c).map_err(|e| e.to_string())?;
            ost.set_parameters(&enc);
            ost.set_time_base(Rational(1, rate));
            ost.index()
        };
        let frame_size = (enc.frame_size() as usize).max(960);
        Ok(AudioTrack { enc, idx, tb: Rational(1, rate), planar, frame_size, pending: Vec::new(), pts: 0 })
    }

    /// Append mixed interleaved stereo samples (from the recording start, contiguous).
    pub fn push_audio(&mut self, samples: &[f32]) -> Result<(), String> {
        let Some(a) = self.audio.as_mut() else { return Ok(()) };
        a.pending.extend_from_slice(samples);
        let n = a.frame_size;
        while a.pending.len() >= n * 2 {
            let chunk: Vec<f32> = a.pending.drain(..n * 2).collect();
            let fmt = if a.planar { ff::format::Sample::F32(ff::format::sample::Type::Planar) } else { ff::format::Sample::F32(ff::format::sample::Type::Packed) };
            let mut f = frame::Audio::new(fmt, n, ff::ChannelLayout::STEREO);
            f.set_rate(crate::audio::RATE);
            if a.planar {
                for ch in 0..2 {
                    let plane: &mut [f32] = f.plane_mut(ch);
                    for (i, s) in plane.iter_mut().take(n).enumerate() {
                        *s = chunk[i * 2 + ch];
                    }
                }
            } else {
                let bytes = f.data_mut(0);
                for (i, s) in chunk.iter().enumerate() {
                    bytes[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
                }
            }
            f.set_pts(Some(a.pts));
            a.pts += n as i64;
            a.enc.send_frame(&f).map_err(|e| e.to_string())?;
            Self::drain_audio(a, &mut self.octx, &mut self.ring)?;
        }
        Ok(())
    }

    fn drain_audio(a: &mut AudioTrack, octx: &mut format::context::Output, ring: &mut Option<Ring>) -> Result<(), String> {
        let mut pkt = Packet::empty();
        while a.enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(a.idx);
            if let Some(r) = ring.as_mut() {
                r.push(a.idx, &pkt);
                continue;
            }
            pkt.rescale_ts(Rational(1, crate::audio::RATE as i32), a.tb);
            pkt.write_interleaved(octx).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn scaler_for(slot: &mut Option<(Pixel, u32, u32, ff::software::scaling::Context)>, sf: Pixel, sw: u32, sh: u32, df: Pixel, dw: u32, dh: u32) -> Result<&mut ff::software::scaling::Context, String> {
        if !slot.as_ref().is_some_and(|(f, w, h, _)| *f == sf && *w == sw && *h == sh) {
            let ctx = ff::software::scaling::Context::get(sf, sw, sh, df, dw, dh, ff::software::scaling::Flags::BILINEAR).map_err(|e| e.to_string())?;
            *slot = Some((sf, sw, sh, ctx));
        }
        Ok(&mut slot.as_mut().unwrap().3)
    }

    fn send(enc: &mut encoder::Video, octx: &mut format::context::Output, ring: &mut Option<Ring>, idx: usize, tb: Rational, frame: Option<&frame::Video>) -> Result<(), String> {
        match frame {
            Some(f) => enc.send_frame(f).map_err(|e| e.to_string())?,
            None => enc.send_eof().map_err(|e| e.to_string())?,
        }
        let mut pkt = Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(idx);
            if let Some(r) = ring.as_mut() {
                r.push(idx, &pkt);
                continue;
            }
            pkt.rescale_ts(TIME_BASE, tb);
            pkt.write_interleaved(octx).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Replay buffer: an encoder for `keep_secs` seconds in memory (`ext`: "mkv" or "mp4").
    pub fn new_replay(v: &VideoOpts, audio: bool, ext: &str, keep_secs: u32) -> Result<Recorder, String> {
        ff::init().map_err(|e| e.to_string())?;
        ff::log::set_level(ff::log::Level::Error);
        let (w, h) = (v.width & !1, v.height & !1);
        let mut last_err = String::from("no encoder found");
        for name in candidates(&v.codec) {
            let Some(c) = encoder::find_by_name(name) else { continue };
            match Self::open_replay(v, w, h, c, name, audio, ext) {
                Ok(mut r) => {
                    if let Some(ring) = r.ring.as_mut() {
                        ring.keep_ms = keep_secs.clamp(5, 600) as i64 * 1000;
                    }
                    return Ok(r);
                }
                Err(e) => last_err = format!("{name}: {e}"),
            }
        }
        Err(last_err)
    }

    fn open_replay(v: &VideoOpts, w: u32, h: u32, c: ff::Codec, name: &str, audio: bool, ext: &str) -> Result<Recorder, String> {
        // The muxer only decides global headers here; nothing is written to /dev/null.
        let fmt = if ext == "mp4" { "mp4" } else { "matroska" };
        let mut octx = format::output_as("/dev/null", fmt).map_err(|e| e.to_string())?;
        let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
        let in_fmt = if name.ends_with("_nvenc") { Pixel::BGRZ } else { Pixel::YUV420P };
        let mut enc = codec::context::Context::new_with_codec(c).encoder().video().map_err(|e| e.to_string())?;
        enc.set_width(w);
        enc.set_height(h);
        enc.set_format(in_fmt);
        enc.set_time_base(TIME_BASE);
        enc.set_frame_rate(Some(Rational(v.fps as i32, 1)));
        enc.set_gop(v.fps * 2);
        enc.set_max_b_frames(0);
        if global_header {
            enc.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let video = enc.open_with(quality_opts(name, &v.quality)).map_err(|e| e.to_string())?;
        let video_idx = 0;
        let path = if ext == "mp4" { "x.mp4" } else { "x.mkv" };
        let audio = if audio { Some(Self::audio_track(&mut octx, path, global_header)?) } else { None };
        let audio = audio.map(|mut a| {
            a.idx = 1;
            a
        });
        let mut stream_info = vec![(name.to_string(), codec::Parameters::from(&video))];
        if let Some(a) = &audio {
            stream_info.push((a.enc.codec().map(|c| c.name().to_string()).unwrap_or_default(), codec::Parameters::from(&a.enc)));
        }
        eprintln!("[prism-rec] replay buffer {w}x{h} with {name}");
        Ok(Recorder {
            ring: Some(Ring { keep_ms: 30_000, packets: Default::default(), tbs: vec![TIME_BASE, Rational(1, crate::audio::RATE as i32)], video_idx }),
            stream_info,
            audio,
            octx,
            video,
            video_idx,
            video_tb: TIME_BASE,
            in_fmt,
            scaler: None,
            staging: frame::Video::new(Pixel::BGRZ, w, h),
            converted: (in_fmt != Pixel::BGRZ).then(|| frame::Video::new(in_fmt, w, h)),
            width: w,
            height: h,
            encoder_name: name.to_string(),
            last_pts: -1,
        })
    }

    /// Replay buffer: write what is in memory to `path` (times start at 0). Returns seconds.
    pub fn save_replay(&self, path: &str) -> Result<f64, String> {
        let ring = self.ring.as_ref().ok_or("not a replay buffer")?;
        let (first_ms, last_ms) = match (ring.packets.front(), ring.packets.back()) {
            (Some(a), Some(b)) => (a.2, b.2),
            _ => return Err("the replay buffer is still empty".into()),
        };
        let mut octx = format::output(&path).map_err(|e| format!("{path}: {e}"))?;
        for (name, par) in &self.stream_info {
            let c = encoder::find_by_name(name).ok_or_else(|| format!("no encoder {name}"))?;
            let mut ost = octx.add_stream(c).map_err(|e| e.to_string())?;
            ost.set_parameters(par.clone());
        }
        let mut hopts = Dictionary::new();
        if path.ends_with(".mp4") {
            hopts.set("movflags", "+faststart");
        }
        octx.write_header_with(hopts).map_err(|e| e.to_string())?;
        let out_tbs: Vec<Rational> = (0..self.stream_info.len()).map(|i| octx.stream(i).map(|s| s.time_base()).unwrap_or(TIME_BASE)).collect();
        for (idx, pkt, ms) in &ring.packets {
            if *ms < first_ms || *idx >= out_tbs.len() {
                continue;
            }
            let tb = ring.tbs[*idx];
            let base = first_ms * tb.denominator() as i64 / (1000 * tb.numerator().max(1) as i64);
            let mut p = pkt.clone();
            p.set_pts(p.pts().map(|t| t - base));
            p.set_dts(p.dts().map(|t| t - base));
            if p.dts().is_some_and(|t| t < 0) {
                continue;
            }
            p.set_stream(*idx);
            p.rescale_ts(tb, out_tbs[*idx]);
            p.write_interleaved(&mut octx).map_err(|e| e.to_string())?;
        }
        octx.write_trailer().map_err(|e| e.to_string())?;
        Ok((last_ms - first_ms) as f64 / 1000.0)
    }

    /// Flush the encoder and close the file.
    pub fn finish(mut self) -> Result<(), String> {
        if self.ring.is_some() {
            return Ok(());
        }
        Self::send(&mut self.video, &mut self.octx, &mut self.ring, self.video_idx, self.video_tb, None)?;
        if let Some(a) = self.audio.as_mut() {
            a.enc.send_eof().map_err(|e| e.to_string())?;
            Self::drain_audio(a, &mut self.octx, &mut self.ring)?;
        }
        self.octx.write_trailer().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(pts_ms: i64, key: bool) -> Packet {
        let mut p = Packet::new(4);
        p.set_pts(Some(pts_ms));
        if key {
            p.set_flags(ff::packet::Flags::KEY);
        }
        p
    }

    #[test]
    fn ring_keeps_the_last_seconds_from_a_keyframe() {
        let mut r = Ring { keep_ms: 5000, packets: Default::default(), tbs: vec![TIME_BASE, TIME_BASE], video_idx: 0 };
        // A keyframe every 2 s, a frame every 500 ms, for 12 s.
        for i in 0..=24 {
            let t = i * 500;
            r.push(0, &pkt(t, t % 2000 == 0));
        }
        let first = r.packets.front().unwrap();
        assert!(first.1.is_key());
        assert_eq!(first.2, 8000); // newest 12000: the window starts at 7000, the next keyframe is at 8000
        assert_eq!(r.packets.back().unwrap().2, 12000);
    }
}
