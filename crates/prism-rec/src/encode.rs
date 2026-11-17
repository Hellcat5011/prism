//! Encoding and muxing with the system FFmpeg libraries. Video prefers NVENC (BGR0 frames go
//! straight in; the GPU converts colour) and falls back to VAAPI-less software x264 (with a
//! swscale conversion to YUV 4:2:0). Timestamps are milliseconds; frames arrive when the screen
//! changes, so the stream is variable frame rate with exact timing. MKV is written as it goes;
//! MP4 is fragmented, so a killed recording is still playable.

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

pub struct Recorder {
    audio: Option<AudioTrack>,
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
        let video_tb = octx.stream(video_idx).map(|s| s.time_base()).unwrap_or(TIME_BASE);
        let audio = audio.map(|mut a| {
            a.tb = octx.stream(a.idx).map(|s| s.time_base()).unwrap_or(a.tb);
            a
        });
        eprintln!("[prism-rec] {w}x{h} with {name} into {path}");
        Ok(Recorder {
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
                Self::send(&mut self.video, &mut self.octx, self.video_idx, self.video_tb, Some(conv))
            } else {
                self.staging.set_pts(Some(pts_ms));
                Self::send(&mut self.video, &mut self.octx, self.video_idx, self.video_tb, Some(&self.staging))
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
            Self::send(&mut self.video, &mut self.octx, self.video_idx, self.video_tb, Some(&out))
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
            Self::drain_audio(a, &mut self.octx)?;
        }
        Ok(())
    }

    fn drain_audio(a: &mut AudioTrack, octx: &mut format::context::Output) -> Result<(), String> {
        let mut pkt = Packet::empty();
        while a.enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(a.idx);
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

    fn send(enc: &mut encoder::Video, octx: &mut format::context::Output, idx: usize, tb: Rational, frame: Option<&frame::Video>) -> Result<(), String> {
        match frame {
            Some(f) => enc.send_frame(f).map_err(|e| e.to_string())?,
            None => enc.send_eof().map_err(|e| e.to_string())?,
        }
        let mut pkt = Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(idx);
            pkt.rescale_ts(TIME_BASE, tb);
            pkt.write_interleaved(octx).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Flush the encoder and close the file.
    pub fn finish(mut self) -> Result<(), String> {
        Self::send(&mut self.video, &mut self.octx, self.video_idx, self.video_tb, None)?;
        if let Some(a) = self.audio.as_mut() {
            a.enc.send_eof().map_err(|e| e.to_string())?;
            Self::drain_audio(a, &mut self.octx)?;
        }
        self.octx.write_trailer().map_err(|e| e.to_string())
    }
}
