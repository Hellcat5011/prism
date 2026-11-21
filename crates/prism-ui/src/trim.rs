//! Trim a recording (the "Trim…" action of the recording notification): a preview of the
//! frame under the active handle, a filmstrip with start and end handles, and Save, which
//! writes `<name>_trimmed.<ext>` next to the recording with ffmpeg (re-encoded, so the cut is
//! frame-exact). Frames come from ffmpeg on worker threads into the runtime directory.
//!
//! Keys: Left/Right move the active handle by 0.1 s (Shift: 1 s), Tab switches handles,
//! Space shows the other handle's frame, Enter saves, Esc cancels.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{ENTER, ESC, LEFT, RIGHT};
use crate::ui::Style;
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 920.0;
pub const CARD_H: f64 = 640.0;
const PAD: f64 = 24.0;
const THUMBS: usize = 10;
const STRIP_H: f64 = 64.0;
const TAB: u32 = 15;
const SPACE: u32 = 57;

/// The recording the next Trim card opens.
static PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_pending(path: PathBuf) {
    *PENDING.lock().unwrap() = Some(path);
}

enum Msg {
    Duration(f64),
    Thumbs(Vec<PathBuf>),
    Frame(u64, PathBuf),
    Saved(Result<PathBuf, String>),
}

pub struct TrimView {
    path: PathBuf,
    work: PathBuf,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    waker: Waker,
    duration: f64,
    start: f64,
    end: f64,
    /// 0 = start handle, 1 = end handle.
    active: usize,
    dragging: Option<usize>,
    thumbs: Vec<PathBuf>,
    frame: Option<PathBuf>,
    /// Frame requests: the newest asked for, the newest shown.
    frame_seq: u64,
    frame_shown: u64,
    frame_due: Option<Instant>,
    saving: bool,
    error: Option<String>,
    done: bool,
    strip: Rect,
    save_btn: Rect,
    cancel_btn: Rect,
    pointer: (f64, f64),
    images_seen: u64,
}

fn ffmpeg(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("ffmpeg failed").to_string())
    }
}

fn probe_duration(path: &Path) -> Option<f64> {
    let out = std::process::Command::new("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(path).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0)
}

impl TrimView {
    pub fn new(waker: Waker) -> Self {
        let path = PENDING.lock().unwrap().take().unwrap_or_default();
        let work = prism_common::runtime::dir().join(format!("prism-trim-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&work);
        let (tx, rx) = channel();
        let v = TrimView {
            path: path.clone(),
            work: work.clone(),
            tx: tx.clone(),
            rx,
            waker: waker.clone(),
            duration: 0.0,
            start: 0.0,
            end: 0.0,
            active: 0,
            dragging: None,
            thumbs: Vec::new(),
            frame: None,
            frame_seq: 0,
            frame_shown: 0,
            frame_due: None,
            saving: false,
            error: None,
            done: false,
            strip: Rect::default(),
            save_btn: Rect::default(),
            cancel_btn: Rect::default(),
            pointer: (0.0, 0.0),
            images_seen: cb::images_generation(),
        };
        std::thread::spawn(move || {
            let Some(d) = probe_duration(&path) else {
                let _ = tx.send(Msg::Saved(Err("Cannot read this recording".into())));
                waker.wake();
                return;
            };
            let _ = tx.send(Msg::Duration(d));
            waker.wake();
            let pattern = work.join("thumb-%02d.png");
            let rate = format!("fps={}/{d:.3},scale=-2:{}", THUMBS, (STRIP_H * 2.0) as i32);
            if ffmpeg(&["-i", &path.to_string_lossy(), "-vf", &rate, "-frames:v", &THUMBS.to_string(), &pattern.to_string_lossy()]).is_ok() {
                let thumbs = (1..=THUMBS).map(|i| work.join(format!("thumb-{i:02}.png"))).filter(|p| p.exists()).collect();
                let _ = tx.send(Msg::Thumbs(thumbs));
                waker.wake();
            }
        });
        v
    }

    fn handle_time(&self) -> f64 {
        if self.active == 0 { self.start } else { self.end }
    }

    /// Ask for the frame under the active handle (soon: drags ask often).
    fn want_frame(&mut self) {
        self.frame_due = Some(Instant::now() + Duration::from_millis(120));
    }

    fn request_frame(&mut self) {
        self.frame_seq += 1;
        let (seq, t, path, out, tx, w) = (self.frame_seq, self.handle_time(), self.path.clone(), self.work.join(format!("frame-{}.png", self.frame_seq)), self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let ts = format!("{:.3}", (t - 0.001).max(0.0));
            if ffmpeg(&["-ss", &ts, "-i", &path.to_string_lossy(), "-frames:v", "1", "-vf", "scale=-2:720", &out.to_string_lossy()]).is_ok() {
                let _ = tx.send(Msg::Frame(seq, out));
                w.wake();
            }
        });
    }

    fn set_handle(&mut self, h: usize, t: f64) {
        const MIN: f64 = 0.2;
        if h == 0 {
            self.start = t.clamp(0.0, (self.end - MIN).max(0.0));
        } else {
            self.end = t.clamp((self.start + MIN).min(self.duration), self.duration);
        }
        self.active = h;
        self.want_frame();
    }

    fn time_at(&self, x: f64) -> f64 {
        ((x - self.strip.x) / self.strip.w).clamp(0.0, 1.0) * self.duration
    }

    fn x_of(&self, t: f64) -> f64 {
        self.strip.x + if self.duration > 0.0 { t / self.duration * self.strip.w } else { 0.0 }
    }

    fn save(&mut self) {
        if self.saving || self.duration <= 0.0 {
            return;
        }
        self.saving = true;
        self.error = None;
        let (src, s, e, tx, w) = (self.path.clone(), self.start, self.end, self.tx.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Recording".into());
            let ext = src.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mkv".into());
            let mut out = src.with_file_name(format!("{stem}_trimmed.{ext}"));
            let mut n = 2;
            while out.exists() {
                out = src.with_file_name(format!("{stem}_trimmed_{n}.{ext}"));
                n += 1;
            }
            // Audio is re-encoded too: a stream copy keeps its packets from before the cut.
            let audio: &[&str] = if ext == "mp4" { &["-c:a", "aac", "-b:a", "192k"] } else { &["-c:a", "libopus", "-b:a", "160k"] };
            let (ss, len) = (format!("{s:.3}"), format!("{:.3}", e - s));
            let (src_s, out_s) = (src.to_string_lossy().to_string(), out.to_string_lossy().to_string());
            let mut args: Vec<&str> = vec!["-ss", &ss, "-i", &src_s, "-t", &len, "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"];
            args.extend(audio);
            args.push(&out_s);
            let r = ffmpeg(&args).map(|_| out.clone());
            if r.is_err() {
                let _ = std::fs::remove_file(&out);
            }
            let _ = tx.send(Msg::Saved(r));
            w.wake();
        });
    }

    fn palette(style: &Style, theme: &Theme) -> Palette {
        Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: cb::hex_to_rgba(&theme.on_primary, 1.0),
            outline: cb::hex_to_rgba(&theme.outline_variant, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        }
    }
}

impl Drop for TrimView {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

/// "Trimmed recording saved" with Open and Show in folder (fire and forget).
fn notify_saved(path: PathBuf) {
    std::thread::spawn(move || {
        let Ok(conn) = zbus::blocking::Connection::session() else { return };
        const D: &str = "org.freedesktop.Notifications";
        let mut hints: std::collections::HashMap<&str, zbus::zvariant::Value> = std::collections::HashMap::new();
        hints.insert("desktop-entry", zbus::zvariant::Value::from("prism-capture"));
        let body = path.to_string_lossy().to_string();
        let _ = conn.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Screen Recording", 0u32, "video-x-generic", "Trimmed recording saved", body.as_str(), Vec::<&str>::new(), hints, 6000i32));
    });
}

impl CardView for TrimView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;
        cb::draw_pango_text(cr, x, y, "Trim Recording", &format!("{} Bold 18px", p.family), fg, None);
        let name = self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        cb::draw_pango_text(cr, x, y + 26.0, &name, &format!("{} 12px", p.family), (fg.0, fg.1, fg.2, 0.65), Some(w as i32));
        y += 54.0;

        // Preview.
        let ph = card.y + card.h - PAD - 44.0 - 16.0 - 24.0 - 12.0 - STRIP_H - 16.0 - y;
        cb::fill_rounded_rect(cr, x, y, w, ph, 12.0, (0.0, 0.0, 0.0, 0.35));
        if let Some(f) = self.frame.as_ref().and_then(|f| cb::cached_image(&f.to_string_lossy(), w as i32, ph as i32, true)) {
            let (iw, ih) = cb::image_size(f);
            cb::paint_cached(cr, f, x + ((w - iw) / 2.0).round(), y + ((ph - ih) / 2.0).round());
        } else {
            let msg = if self.duration <= 0.0 { "Reading the recording\u{2026}" } else { "Loading frame\u{2026}" };
            crate::launcher::draw_text_caret(cr, "", None, msg, x + w / 2.0 - 70.0, y, 200.0, ph, &format!("{} 13px", p.family), fg, (fg.0, fg.1, fg.2, 0.6));
        }
        let label = format!("{} {}", if self.active == 0 { "Start" } else { "End" }, clock(self.handle_time()));
        let (lw, lh) = cb::measure_pango_text(cr, &label, &format!("{} Semi-Bold 12px", p.family), 400, Some(1));
        cb::fill_rounded_rect(cr, x + 10.0, y + 10.0, lw as f64 + 16.0, lh as f64 + 8.0, 6.0, (0.0, 0.0, 0.0, 0.6));
        cb::draw_pango_text(cr, x + 18.0, y + 14.0, &label, &format!("{} Semi-Bold 12px", p.family), (1.0, 1.0, 1.0, 1.0), None);
        y += ph + 16.0;

        // Filmstrip with the kept range.
        self.strip = Rect { x: x + 10.0, y, w: w - 20.0, h: STRIP_H };
        let s = self.strip;
        cb::fill_rounded_rect(cr, s.x, s.y, s.w, s.h, 8.0, (0.0, 0.0, 0.0, 0.3));
        let tw = s.w / THUMBS as f64;
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, s.x, s.y, s.w, s.h);
            cb::cairo_clip(cr);
        }
        for (i, t) in self.thumbs.iter().enumerate() {
            if let Some(img) = cb::cached_image_cover(&t.to_string_lossy(), tw.ceil() as i32, s.h as i32) {
                let (iw, ih) = cb::image_size(img);
                cb::paint_cached(cr, img, s.x + i as f64 * tw + (tw - iw) / 2.0, s.y + (s.h - ih) / 2.0);
            }
        }
        let (sx, ex) = (self.x_of(self.start), self.x_of(self.end));
        cb::fill_rounded_rect(cr, s.x, s.y, sx - s.x, s.h, 0.0, (0.0, 0.0, 0.0, 0.6));
        cb::fill_rounded_rect(cr, ex, s.y, s.x + s.w - ex, s.h, 0.0, (0.0, 0.0, 0.0, 0.6));
        unsafe { cb::cairo_restore(cr) };
        cb::stroke_rounded_rect(cr, sx, s.y + 1.0, ex - sx, s.h - 2.0, 4.0, style.accent, 3.0);
        for (h, hx) in [(0usize, sx), (1, ex)] {
            let on = h == self.active;
            cb::fill_rounded_rect(cr, hx - 5.0, s.y - 6.0, 10.0, s.h + 12.0, 5.0, if on { style.accent } else { (fg.0, fg.1, fg.2, 0.85) });
        }
        y += STRIP_H + 12.0;
        let span = format!("{}  \u{2013}  {}   ({:.1} s of {:.1} s)", clock(self.start), clock(self.end), self.end - self.start, self.duration);
        cb::draw_pango_text(cr, x + 10.0, y, &span, &format!("{} 13px", p.family), fg, None);

        // Buttons.
        let by = card.y + card.h - PAD - 44.0;
        self.save_btn = Rect { x: x + w - 200.0, y: by, w: 200.0, h: 44.0 };
        self.cancel_btn = Rect { x: self.save_btn.x - 12.0 - 120.0, y: by, w: 120.0, h: 44.0 };
        let hov = |r: Rect| if r.contains(self.pointer.0, self.pointer.1) { 1.0 } else { 0.0 };
        uikit::button(cr, self.cancel_btn, "Cancel", ButtonKind::Outline, hov(self.cancel_btn), !self.saving, &p);
        uikit::button(cr, self.save_btn, if self.saving { "Saving\u{2026}" } else { "Save Trimmed Copy" }, ButtonKind::Primary, hov(self.save_btn), !self.saving && self.duration > 0.0, &p);
        let hint = self.error.clone().unwrap_or_else(|| "Drag the handles or use \u{2190} \u{2192} (Shift: 1 s), Tab to switch".into());
        let hc = if self.error.is_some() { p.error } else { (fg.0, fg.1, fg.2, 0.6) };
        cb::draw_pango_text(cr, x, by + 13.0, &hint, &format!("{} 12px", p.family), hc, Some((self.cancel_btn.x - x - 12.0) as i32));
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        if let Some(h) = self.dragging {
            let t = self.time_at(x);
            self.set_handle(h, t);
        }
        true
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        if self.cancel_btn.contains(x, y) && !self.saving {
            return Effect::Close;
        }
        if self.save_btn.contains(x, y) {
            self.save();
            return Effect::Redraw;
        }
        let s = self.strip;
        if self.duration > 0.0 && x >= s.x - 12.0 && x <= s.x + s.w + 12.0 && y >= s.y - 8.0 && y <= s.y + s.h + 8.0 {
            // The nearer handle follows the pointer.
            let h = if (x - self.x_of(self.start)).abs() <= (x - self.x_of(self.end)).abs() { 0 } else { 1 };
            self.dragging = Some(h);
            let t = self.time_at(x);
            self.set_handle(h, t);
            return Effect::Redraw;
        }
        Effect::None
    }

    fn release(&mut self) -> Effect {
        if self.dragging.take().is_some() {
            self.request_frame();
            self.frame_due = None;
        }
        Effect::Redraw
    }

    fn key(&mut self, special: u32, _typed: Option<char>, shift: bool, _ctrl: bool) -> Effect {
        let step = if shift { 1.0 } else { 0.1 };
        match special {
            ESC if !self.saving => return Effect::Close,
            ENTER => self.save(),
            TAB => {
                self.active = 1 - self.active;
                self.want_frame();
            }
            SPACE => {
                self.active = 1 - self.active;
                self.want_frame();
            }
            LEFT => self.set_handle(self.active, self.handle_time() - step),
            RIGHT => self.set_handle(self.active, self.handle_time() + step),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(m) = self.rx.try_recv() {
            changed = true;
            match m {
                Msg::Duration(d) => {
                    self.duration = d;
                    self.end = d;
                    self.request_frame();
                }
                Msg::Thumbs(t) => self.thumbs = t,
                Msg::Frame(seq, path) => {
                    if seq >= self.frame_shown {
                        if let Some(old) = self.frame.replace(path) {
                            let _ = std::fs::remove_file(old);
                        }
                        self.frame_shown = seq;
                    }
                }
                Msg::Saved(Ok(p)) => {
                    notify_saved(p);
                    self.done = true;
                }
                Msg::Saved(Err(e)) => {
                    self.saving = false;
                    self.error = Some(e);
                }
            }
        }
        if self.frame_due.is_some_and(|t| Instant::now() >= t) {
            self.frame_due = None;
            self.request_frame();
        }
        let g = cb::images_generation();
        if g != self.images_seen {
            self.images_seen = g;
            changed = true;
        }
        changed
    }

    fn wake_in(&self) -> Option<Duration> {
        self.frame_due.map(|t| t.saturating_duration_since(Instant::now()))
    }

    fn wants_close(&self) -> bool {
        self.done
    }

    fn clickable(&self) -> bool {
        let (x, y) = self.pointer;
        self.save_btn.contains(x, y) || self.cancel_btn.contains(x, y) || (self.strip.contains(x, y) && self.duration > 0.0)
    }
}
