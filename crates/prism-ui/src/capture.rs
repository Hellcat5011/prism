//! Screenshot overlay (SCREEN_CAPTURE_PLAN.md). Every output is captured first through
//! `prism_capture` (cursor excluded unless configured), then a full-screen overlay shows the
//! frozen frame of the output it lands on. Modes:
//!
//! * Region: drag a rectangle; move it by dragging inside, resize by its handles; Enter, the
//!   Capture button or a double click takes it.
//! * Window: a list of open windows (ext-foreign-toplevel-list) with search; picking one
//!   captures that window's own buffer (also when covered).
//! * Screen: click or Enter takes the whole output.
//!
//! The result (an `Image` in buffer pixels, never resampled) is saved as PNG, put on the
//! clipboard and announced by `finish`.

use prism_capture::{Capturer, Image, OutputInfo, WindowInfo};
use prism_common::Config;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_keyboard, wl_pointer, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xkbcommon::xkb;

use crate::cairo_backend::{self as cb, Cairo, CairoSurface};
use crate::desktop::layout::Rect;
use crate::desktop::ShmBuf;
use crate::Globals;

type Rgba = (f64, f64, f64, f64);
const WHITE: Rgba = (1.0, 1.0, 1.0, 1.0);
const BAR_BG: Rgba = (0.08, 0.08, 0.10, 0.88);
const DIM: f64 = 0.5;
const HANDLE: f64 = 10.0;
const ICON_VIDEO: &str = "M16 10l6-4v12l-6-4zM2 6h14v12H2z";
const ICON_SPEAKER: &str = "M11 5 6 9H2v6h4l5 4V5zM15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14";
const ICON_MIC: &str = "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v3";
const ICON_CAMERA: &str = "M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3zM12 17a4 4 0 1 0 0-8 4 4 0 0 0 0 8z";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Region,
    Window,
    Screen,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        match s {
            "window" => Kind::Window,
            "screen" | "output" => Kind::Screen,
            _ => Kind::Region,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Region => "region",
            Kind::Window => "window",
            Kind::Screen => "screen",
        }
    }
}

/// What the overlay produced.
pub enum Outcome {
    Image(Image),
    /// Start recording this (prismd runs prism-rec); also the options to remember.
    Record(prism_common::capture::RecordJob),
    Cancelled,
}

/// One frozen output: pixels plus a cairo surface over them.
struct Shot {
    info: OutputInfo,
    img: Image,
    surf: *mut CairoSurface,
}

impl Drop for Shot {
    fn drop(&mut self) {
        unsafe { cb::cairo_surface_destroy(self.surf) };
    }
}

fn shot_surface(img: &mut Image) -> *mut CairoSurface {
    unsafe { cb::cairo_image_surface_create_for_data(img.data.as_mut_ptr(), cb::CAIRO_FORMAT_ARGB32, img.width as i32, img.height as i32, img.stride as i32) }
}

#[derive(Clone, PartialEq, Debug)]
enum Ui {
    Kind(Kind),
    Capture,
    Window(usize),
    /// Screenshot (false) or Record (true).
    Mode(bool),
    Fps,
    AudioDesktop,
    AudioMic,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    New(f64, f64),
    Move(f64, f64),
    /// Which edges move: (left, top, right, bottom).
    Resize(bool, bool, bool, bool),
}

pub struct Overlay {
    queue: EventQueue<State>,
    st: State,
}

struct State {
    cap: Capturer,
    shots: Vec<Shot>,
    cur: usize,
    windows: Vec<WindowInfo>,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    enter_serial: u32,
    shm: wayland_client::protocol::wl_shm::WlShm,
    bufs: Vec<ShmBuf>,
    size: (i32, i32),
    configured: bool,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,
    xkb_ctx: xkb::Context,
    xkb: Option<xkb::State>,
    kind: Kind,
    sel: Option<Rect>,
    drag: Drag,
    ptr: (f64, f64),
    hits: Vec<(Rect, Ui)>,
    hover: Option<Ui>,
    query: String,
    win_sel: usize,
    last_click: Option<std::time::Instant>,
    cursor: bool,
    record: bool,
    fps: u32,
    audio_desktop: bool,
    audio_mic: bool,
    codec: String,
    quality: String,
    outcome: Option<Outcome>,
    error: Option<String>,
}

impl Overlay {
    /// Freeze every output and show the overlay. Err when nothing could be captured.
    pub fn open(conn: &Connection, g: &Globals, kind: Kind, record: bool, cfg: &Config) -> Result<Overlay, String> {
        let mut cap = Capturer::new(conn)?;
        let mut shots = Vec::new();
        for o in cap.outputs() {
            match cap.capture_output(&o, cfg.capture_cursor) {
                Ok(mut img) => {
                    let surf = shot_surface(&mut img);
                    shots.push(Shot { info: o, img, surf });
                }
                Err(e) => eprintln!("[prism-ui] capture {}: {e}", o.name),
            }
        }
        if shots.is_empty() {
            return Err("no output could be captured".into());
        }
        let windows = cap.windows();
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let surface = g.compositor.as_ref().ok_or("no compositor")?.create_surface(&qh, ());
        let layer = g.layer_shell.as_ref().ok_or("no layer shell")?.get_layer_surface(&surface, None, Layer::Overlay, "prism-capture".into(), &qh, ());
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();
        let seat = g.seat.as_ref().ok_or("no seat")?;
        let pointer = seat.get_pointer(&qh, ());
        let cursor_dev = g.cursor_shape_manager.as_ref().map(|m| m.get_pointer(&pointer, &qh, ()));
        Ok(Overlay {
            queue,
            st: State {
                cap,
                shots,
                cur: 0,
                windows,
                surface,
                layer,
                keyboard: Some(seat.get_keyboard(&qh, ())),
                pointer: Some(pointer),
                cursor_dev,
                enter_serial: 0,
                shm: g.shm.clone().ok_or("no shm")?,
                bufs: Vec::new(),
                size: (0, 0),
                configured: false,
                frame_cb: None,
                dirty: true,
                xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
                xkb: None,
                kind,
                sel: None,
                drag: Drag::None,
                ptr: (0.0, 0.0),
                hits: Vec::new(),
                hover: None,
                query: String::new(),
                win_sel: 0,
                last_click: None,
                cursor: cfg.capture_cursor,
                record,
                fps: if cfg.capture_fps >= 45 { 60 } else { 30 },
                audio_desktop: cfg.capture_audio_desktop,
                audio_mic: cfg.capture_audio_mic,
                codec: cfg.capture_codec.clone(),
                quality: cfg.capture_quality.clone(),
                outcome: None,
                error: None,
            },
        })
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    pub fn finished(&self) -> bool {
        self.st.outcome.is_some()
    }

    pub fn close(&mut self) {
        if self.st.outcome.is_none() {
            self.st.outcome = Some(Outcome::Cancelled);
        }
    }

    pub fn tick(&mut self) {
        let qh = self.queue.handle();
        if self.st.dirty && self.st.configured && self.st.frame_cb.is_none() && self.st.outcome.is_none() {
            self.st.draw(&qh);
        }
    }

    /// The selection kind last used (remembered in config).
    pub fn kind(&self) -> Kind {
        self.st.kind
    }

    pub fn teardown(mut self) -> Outcome {
        let s = &mut self.st;
        if let Some(d) = s.cursor_dev.take() {
            d.destroy();
        }
        if let Some(k) = s.keyboard.take() {
            k.release();
        }
        if let Some(p) = s.pointer.take() {
            p.release();
        }
        s.bufs.clear();
        s.layer.destroy();
        s.surface.destroy();
        let _ = self.queue.dispatch_pending(&mut self.st);
        self.st.outcome.take().unwrap_or(Outcome::Cancelled)
    }
}

impl State {
    fn shot(&self) -> &Shot {
        &self.shots[self.cur.min(self.shots.len() - 1)]
    }

    /// Surface (logical) to buffer pixels of the current shot.
    fn scale(&self) -> (f64, f64) {
        let s = self.shot();
        (s.img.width as f64 / self.size.0.max(1) as f64, s.img.height as f64 / self.size.1.max(1) as f64)
    }

    fn filtered_windows(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        (0..self.windows.len()).filter(|i| q.is_empty() || self.windows[*i].title.to_lowercase().contains(&q) || self.windows[*i].app_id.to_lowercase().contains(&q)).collect()
    }

    fn set_kind(&mut self, k: Kind) {
        self.kind = k;
        self.drag = Drag::None;
        if k == Kind::Window {
            self.cap.refresh();
            self.windows = self.cap.windows();
            self.win_sel = 0;
        }
        self.set_cursor_shape();
        self.dirty = true;
    }

    fn set_cursor_shape(&self) {
        if let Some(d) = &self.cursor_dev {
            let shape = if self.kind == Kind::Region && self.hover.is_none() { CursorShape::Crosshair } else { CursorShape::Default };
            d.set_shape(self.enter_serial, shape);
        }
    }

    /// Take the result for the current mode.
    fn confirm(&mut self) {
        if self.record {
            return self.confirm_record();
        }
        match self.kind {
            Kind::Screen => self.outcome = Some(Outcome::Image(self.shot().img.clone())),
            Kind::Region => {
                let Some(r) = self.sel.filter(|r| r.w >= 2.0 && r.h >= 2.0) else { return };
                let (sx, sy) = self.scale();
                let img = self.shot().img.crop((r.x * sx).round() as i32, (r.y * sy).round() as i32, (r.w * sx).round() as i32, (r.h * sy).round() as i32);
                self.outcome = Some(Outcome::Image(img));
            }
            Kind::Window => {
                let list = self.filtered_windows();
                let Some(&i) = list.get(self.win_sel) else { return };
                let w = self.windows[i].clone();
                match self.cap.capture_window(&w, self.cursor) {
                    Ok(img) => self.outcome = Some(Outcome::Image(img)),
                    Err(e) => {
                        self.error = Some(format!("Could not capture {}: {e}", if w.title.is_empty() { &w.app_id } else { &w.title }));
                        self.dirty = true;
                    }
                }
            }
        }
    }

    /// Build the recording job for the current selection.
    fn confirm_record(&mut self) {
        use prism_common::capture::{RecordJob, RecordTarget};
        let output = self.shot().info.name.clone();
        let target = match self.kind {
            Kind::Screen => RecordTarget::Output { name: output },
            Kind::Region => {
                let Some(r) = self.sel.filter(|r| r.w >= 16.0 && r.h >= 16.0) else { return };
                let (sx, sy) = self.scale();
                RecordTarget::Region { output, x: (r.x * sx).round() as i32, y: (r.y * sy).round() as i32, w: (r.w * sx).round() as i32, h: (r.h * sy).round() as i32 }
            }
            Kind::Window => {
                let list = self.filtered_windows();
                let Some(&i) = list.get(self.win_sel) else { return };
                let w = &self.windows[i];
                RecordTarget::Window { id: w.id.clone(), title: if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() } }
            }
        };
        self.outcome = Some(Outcome::Record(RecordJob {
            target,
            fps: self.fps,
            cursor: self.cursor,
            audio_desktop: self.audio_desktop,
            audio_mic: self.audio_mic,
            codec: self.codec.clone(),
            quality: self.quality.clone(),
            path: String::new(),
        }));
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        self.bufs.retain(|b| b.w == w && b.h == h);
        let idx = match self.bufs.iter().position(|b| !b.busy) {
            Some(i) => i,
            None if self.bufs.len() < 3 => match ShmBuf::new(&self.shm, qh, w, h, ()) {
                Some(b) => {
                    self.bufs.push(b);
                    self.bufs.len() - 1
                }
                None => return,
            },
            None => return,
        };
        self.bufs[idx].busy = true;
        unsafe {
            let surf = self.bufs[idx].cairo();
            let cr = cb::cairo_create(surf);
            self.paint(cr, w as f64, h as f64);
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, w, h);
        self.frame_cb = Some(self.surface.frame(qh, ()));
        self.surface.commit();
        self.dirty = false;
    }

    unsafe fn paint(&mut self, cr: *mut Cairo, w: f64, h: f64) {
        self.hits.clear();
        let (surf, iw, ih) = {
            let s = self.shot();
            (s.surf, s.img.width as f64, s.img.height as f64)
        };
        // Frozen screen.
        cb::cairo_save(cr);
        cb::cairo_scale(cr, w / iw, h / ih);
        cb::cairo_set_source_surface(cr, surf, 0.0, 0.0);
        cb::cairo_paint(cr);
        cb::cairo_restore(cr);

        // Dim everything except the selection.
        let hole = match self.kind {
            Kind::Screen => Some(Rect { x: 0.0, y: 0.0, w, h }),
            Kind::Region => self.sel,
            Kind::Window => None,
        };
        cb::cairo_save(cr);
        cb::cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, DIM);
        cb::cairo_rectangle(cr, 0.0, 0.0, w, h);
        if let Some(r) = hole {
            // Even-odd: the outer rectangle minus the hole.
            cb::cairo_set_fill_rule(cr, 1);
            cb::cairo_rectangle(cr, r.x, r.y, r.w, r.h);
        }
        cb::cairo_fill(cr);
        cb::cairo_restore(cr);

        match self.kind {
            Kind::Region => self.paint_region(cr, w, h),
            Kind::Window => self.paint_windows(cr, w, h),
            Kind::Screen => {
                cb::stroke_rounded_rect(cr, 2.0, 2.0, w - 4.0, h - 4.0, 0.0, (1.0, 1.0, 1.0, 0.8), 3.0);
                hint(cr, w, h * 0.42, "Click or press Enter to capture this screen");
            }
        }
        self.paint_bar(cr, w, h);
        if let Some(e) = &self.error {
            hint(cr, w, 40.0, e);
        }
    }

    unsafe fn paint_region(&mut self, cr: *mut Cairo, w: f64, h: f64) {
        let Some(r) = self.sel else {
            hint(cr, w, h * 0.42, "Drag to select an area · Enter or a click on Screen takes everything · Esc cancels");
            return;
        };
        cb::stroke_rounded_rect(cr, r.x - 0.5, r.y - 0.5, r.w + 1.0, r.h + 1.0, 0.0, WHITE, 1.5);
        if self.drag == Drag::None || matches!(self.drag, Drag::Move(..) | Drag::Resize(..)) {
            for (hx, hy) in handles(r) {
                cb::fill_rounded_rect(cr, hx - HANDLE / 2.0, hy - HANDLE / 2.0, HANDLE, HANDLE, 2.0, WHITE);
                cb::stroke_rounded_rect(cr, hx - HANDLE / 2.0, hy - HANDLE / 2.0, HANDLE, HANDLE, 2.0, (0.0, 0.0, 0.0, 0.5), 1.0);
            }
        }
        // Size in buffer pixels.
        let (sx, sy) = self.scale();
        let label = format!("{} × {}", (r.w * sx).round() as i64, (r.h * sy).round() as i64);
        let font = "Inter, Sans Semi-Bold 12px";
        let (tw, th) = cb::measure_pango_text(cr, &label, font, 400, Some(1));
        let (lw, lh) = (tw as f64 + 16.0, th as f64 + 8.0);
        let lx = r.x.min(w - lw - 4.0).max(4.0);
        let ly = if r.y > lh + 10.0 { r.y - lh - 6.0 } else { (r.y + r.h + 6.0).min(h - lh - 4.0) };
        cb::fill_rounded_rect(cr, lx, ly, lw, lh, lh / 2.0, BAR_BG);
        cb::draw_pango_text(cr, lx + 8.0, ly + 4.0, &label, font, WHITE, None);
    }

    unsafe fn paint_windows(&mut self, cr: *mut Cairo, w: f64, h: f64) {
        let list = self.filtered_windows();
        let cw = 560.0_f64.min(w - 40.0);
        let row = 52.0;
        let rows = list.len().clamp(1, 9) as f64;
        let ch = 70.0 + rows * row + 16.0;
        let c = Rect { x: ((w - cw) / 2.0).round(), y: ((h - ch) / 2.0 - 30.0).round().max(20.0), w: cw, h: ch };
        cb::fill_rounded_rect(cr, c.x, c.y, c.w, c.h, 18.0, BAR_BG);
        let q = if self.query.is_empty() { "Type to search windows".to_string() } else { self.query.clone() };
        let qcol = if self.query.is_empty() { (1.0, 1.0, 1.0, 0.45) } else { WHITE };
        cb::fill_rounded_rect(cr, c.x + 14.0, c.y + 14.0, c.w - 28.0, 40.0, 12.0, (1.0, 1.0, 1.0, 0.08));
        cb::draw_pango_text(cr, c.x + 28.0, c.y + 24.0, &q, "Inter, Sans 14px", qcol, Some((c.w - 56.0) as i32));
        if list.is_empty() {
            let msg = if self.cap.can_capture_windows() { "No windows match" } else { "This compositor cannot capture single windows" };
            cb::draw_pango_text(cr, c.x + 28.0, c.y + 80.0, msg, "Inter, Sans 14px", (1.0, 1.0, 1.0, 0.6), None);
            return;
        }
        let start = self.win_sel.saturating_sub(8);
        for (k, &i) in list.iter().enumerate().skip(start).take(9) {
            let win = &self.windows[i];
            let r = Rect { x: c.x + 10.0, y: c.y + 66.0 + (k - start) as f64 * row, w: c.w - 20.0, h: row - 4.0 };
            if k == self.win_sel || self.hover == Some(Ui::Window(k)) {
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 10.0, (1.0, 1.0, 1.0, if k == self.win_sel { 0.16 } else { 0.08 }));
            }
            let name = win.app_id.rsplit('.').next().unwrap_or(&win.app_id).to_string();
            cb::draw_app_icon(cr, &win.app_id, r.x + 10.0, r.y + 8.0, 32, &name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default(), (0.5, 0.5, 0.9, 1.0));
            let title = if win.title.is_empty() { name.clone() } else { win.title.clone() };
            cb::draw_pango_text(cr, r.x + 54.0, r.y + 6.0, &title, "Inter, Sans Semi-Bold 14px", WHITE, Some((r.w - 70.0) as i32));
            cb::draw_pango_text(cr, r.x + 54.0, r.y + 26.0, &win.app_id, "Inter, Sans 11px", (1.0, 1.0, 1.0, 0.55), Some((r.w - 70.0) as i32));
            self.hits.push((r, Ui::Window(k)));
        }
    }

    /// Width of a pill group: (label, optional icon, target, selected).
    unsafe fn group_width(cr: *mut Cairo, items: &[(String, Option<&str>, Ui, bool)]) -> f64 {
        let font = "Inter, Sans Semi-Bold 13px";
        let pad = 6.0;
        items.iter().map(|(l, icon, _, _)| cb::measure_pango_text(cr, l, font, 400, Some(1)).0 as f64 + 32.0 + if icon.is_some() { 22.0 } else { 0.0 }).sum::<f64>() + pad * (items.len() as f64 + 1.0)
    }

    /// Draw a pill group at (x, y); returns its width.
    unsafe fn draw_group(&mut self, cr: *mut Cairo, x: f64, y: f64, bh: f64, items: &[(String, Option<&str>, Ui, bool)]) -> f64 {
        let font = "Inter, Sans Semi-Bold 13px";
        let pad = 6.0;
        let total = Self::group_width(cr, items);
        cb::fill_rounded_rect(cr, x, y, total, bh, bh / 2.0, BAR_BG);
        let mut cx = x + pad;
        for (label, icon, ui, on) in items {
            let (tw, th) = cb::measure_pango_text(cr, label, font, 400, Some(1));
            let iw = if icon.is_some() { 22.0 } else { 0.0 };
            let r = Rect { x: cx, y: y + pad, w: tw as f64 + 32.0 + iw, h: bh - 2.0 * pad };
            let hov = self.hover.as_ref() == Some(ui);
            if *on || hov {
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, (1.0, 1.0, 1.0, if *on { 0.22 } else { 0.10 }));
            }
            let col = if *on || icon.is_none() { WHITE } else { (1.0, 1.0, 1.0, 0.6) };
            if let Some(path) = icon {
                crate::svg::draw_path(cr, path, r.x + 14.0, r.y + (r.h - 16.0) / 2.0, 16.0, col, Some(2.0));
            }
            cb::draw_pango_text(cr, r.x + 16.0 + iw, r.y + (r.h - th as f64) / 2.0, label, font, col, None);
            self.hits.push((r, ui.clone()));
            cx += r.w + pad;
        }
        total
    }

    unsafe fn paint_bar(&mut self, cr: *mut Cairo, w: f64, h: f64) {
        let bh = 44.0;
        let gap = 10.0;
        // Top: Screenshot / Record.
        let modes = vec![("Screenshot".to_string(), Some(ICON_CAMERA), Ui::Mode(false), !self.record), ("Record".to_string(), Some(ICON_VIDEO), Ui::Mode(true), self.record)];
        let mw = Self::group_width(cr, &modes);
        self.draw_group(cr, ((w - mw) / 2.0).round(), 24.0, bh, &modes);

        // Bottom: what to capture, recording options, then the round action button.
        let kinds: Vec<(String, Option<&str>, Ui, bool)> =
            [(Kind::Region, "Region"), (Kind::Window, "Window"), (Kind::Screen, "Screen")].iter().map(|(k, l)| (l.to_string(), None, Ui::Kind(*k), self.kind == *k)).collect();
        let opts: Vec<(String, Option<&str>, Ui, bool)> = if self.record {
            vec![
                (format!("{} fps", self.fps), None, Ui::Fps, false),
                ("Desktop audio".to_string(), Some(ICON_SPEAKER), Ui::AudioDesktop, self.audio_desktop),
                ("Mic".to_string(), Some(ICON_MIC), Ui::AudioMic, self.audio_mic),
            ]
        } else {
            Vec::new()
        };
        let kw = Self::group_width(cr, &kinds);
        let ow = if opts.is_empty() { 0.0 } else { Self::group_width(cr, &opts) + gap };
        let total = kw + ow + gap + bh;
        let by = h - bh - 28.0;
        let mut x = ((w - total) / 2.0).round();
        x += self.draw_group(cr, x, by, bh, &kinds) + gap;
        if !opts.is_empty() {
            x += self.draw_group(cr, x, by, bh, &opts) + gap;
        }
        let can = match self.kind {
            Kind::Region => self.sel.is_some_and(|r| !self.record || (r.w >= 16.0 && r.h >= 16.0)),
            Kind::Screen => true,
            Kind::Window => !self.filtered_windows().is_empty(),
        };
        let r = Rect { x, y: by, w: bh, h: bh };
        let (cx, cy) = (r.x + bh / 2.0, r.y + bh / 2.0);
        let hov = self.hover == Some(Ui::Capture);
        let accent = if self.record { (0.90, 0.25, 0.30) } else { (0.35, 0.55, 1.0) };
        let bg = if can { (accent.0, accent.1, accent.2, if hov { 1.0 } else { 0.88 }) } else { BAR_BG };
        cb::fill_circle(cr, cx, cy, bh / 2.0, bg);
        cb::stroke_circle(cr, cx, cy, bh / 2.0 - 1.0, (1.0, 1.0, 1.0, if can { 0.35 } else { 0.15 }), 2.0);
        let fg = if can { WHITE } else { (1.0, 1.0, 1.0, 0.4) };
        if self.record {
            cb::fill_circle(cr, cx, cy, 7.0, fg);
        } else {
            let i = 20.0;
            crate::svg::draw_path(cr, ICON_CAMERA, cx - i / 2.0, cy - i / 2.0, i, fg, Some(2.0));
        }
        if can {
            self.hits.push((r, Ui::Capture));
        }
    }

    fn hit(&self, x: f64, y: f64) -> Option<Ui> {
        self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, u)| u.clone())
    }

    fn press(&mut self, x: f64, y: f64) {
        self.error = None;
        if let Some(u) = self.hit(x, y) {
            match u {
                Ui::Kind(k) => self.set_kind(k),
                Ui::Mode(rec) => {
                    self.record = rec;
                    self.dirty = true;
                }
                Ui::Fps => {
                    self.fps = if self.fps == 60 { 30 } else { 60 };
                    self.dirty = true;
                }
                Ui::AudioDesktop => {
                    self.audio_desktop = !self.audio_desktop;
                    self.dirty = true;
                }
                Ui::AudioMic => {
                    self.audio_mic = !self.audio_mic;
                    self.dirty = true;
                }
                Ui::Capture => self.confirm(),
                Ui::Window(k) => {
                    self.win_sel = k;
                    self.confirm();
                }
            }
            return;
        }
        match self.kind {
            Kind::Screen => self.confirm(),
            Kind::Window => {}
            Kind::Region => {
                let double = self.last_click.is_some_and(|t| t.elapsed().as_millis() < 350);
                self.last_click = Some(std::time::Instant::now());
                if let Some(r) = self.sel {
                    if double && r.contains(x, y) {
                        self.confirm();
                        return;
                    }
                    if let Some(edges) = handle_at(r, x, y) {
                        self.drag = Drag::Resize(edges.0, edges.1, edges.2, edges.3);
                        return;
                    }
                    if r.contains(x, y) {
                        self.drag = Drag::Move(x - r.x, y - r.y);
                        return;
                    }
                }
                self.drag = Drag::New(x, y);
                self.sel = Some(Rect { x, y, w: 0.0, h: 0.0 });
            }
        }
        self.dirty = true;
    }

    fn motion(&mut self, x: f64, y: f64) {
        self.ptr = (x, y);
        let (w, h) = (self.size.0 as f64, self.size.1 as f64);
        match self.drag {
            Drag::New(x0, y0) => {
                self.sel = Some(Rect { x: x0.min(x).max(0.0), y: y0.min(y).max(0.0), w: (x - x0).abs().min(w), h: (y - y0).abs().min(h) });
                self.dirty = true;
            }
            Drag::Move(ox, oy) => {
                if let Some(r) = self.sel.as_mut() {
                    r.x = (x - ox).clamp(0.0, (w - r.w).max(0.0));
                    r.y = (y - oy).clamp(0.0, (h - r.h).max(0.0));
                    self.dirty = true;
                }
            }
            Drag::Resize(l, t, rr, b) => {
                if let Some(r) = self.sel.as_mut() {
                    let (mut x0, mut y0, mut x1, mut y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
                    let (cx, cy) = (x.clamp(0.0, w), y.clamp(0.0, h));
                    if l {
                        x0 = cx;
                    }
                    if rr {
                        x1 = cx;
                    }
                    if t {
                        y0 = cy;
                    }
                    if b {
                        y1 = cy;
                    }
                    *r = Rect { x: x0.min(x1), y: y0.min(y1), w: (x1 - x0).abs(), h: (y1 - y0).abs() };
                    self.dirty = true;
                }
            }
            Drag::None => {
                let hov = self.hit(x, y);
                if hov != self.hover {
                    self.hover = hov;
                    self.set_cursor_shape();
                    self.dirty = true;
                }
            }
        }
    }

    fn release(&mut self) {
        if let Drag::New(..) = self.drag {
            if self.sel.is_some_and(|r| r.w < 3.0 || r.h < 3.0) {
                self.sel = None;
            }
        }
        self.drag = Drag::None;
        self.dirty = true;
    }

    fn key(&mut self, sym: u32, utf8: String) {
        self.error = None;
        match sym {
            0xff1b => {
                // Escape: leave the search, else drop the selection, else cancel.
                if self.kind == Kind::Window && !self.query.is_empty() {
                    self.query.clear();
                } else if self.kind == Kind::Region && self.sel.is_some() {
                    self.sel = None;
                } else {
                    self.outcome = Some(Outcome::Cancelled);
                }
            }
            0xff0d | 0xff8d => {
                if self.kind == Kind::Region && self.sel.is_none() {
                    self.kind = Kind::Screen;
                }
                self.confirm();
            }
            0xff09 => {
                let next = match self.kind {
                    Kind::Region => Kind::Window,
                    Kind::Window => Kind::Screen,
                    Kind::Screen => Kind::Region,
                };
                self.set_kind(next);
            }
            0xff52 if self.kind == Kind::Window => self.win_sel = self.win_sel.saturating_sub(1),
            0xff54 if self.kind == Kind::Window => self.win_sel = (self.win_sel + 1).min(self.filtered_windows().len().saturating_sub(1)),
            0xff08 if self.kind == Kind::Window => {
                self.query.pop();
                self.win_sel = 0;
            }
            _ if self.kind == Kind::Window => {
                let t: String = utf8.chars().filter(|c| !c.is_control()).collect();
                if !t.is_empty() {
                    self.query.push_str(&t);
                    self.win_sel = 0;
                }
            }
            0x72 => self.set_kind(Kind::Region),   // r
            0x77 => self.set_kind(Kind::Window),   // w
            0x73 => self.set_kind(Kind::Screen),   // s
            _ => {}
        }
        self.dirty = true;
    }
}

fn handles(r: Rect) -> [(f64, f64); 8] {
    let (x0, y0, x1, y1, mx, my) = (r.x, r.y, r.x + r.w, r.y + r.h, r.x + r.w / 2.0, r.y + r.h / 2.0);
    [(x0, y0), (mx, y0), (x1, y0), (x1, my), (x1, y1), (mx, y1), (x0, y1), (x0, my)]
}

/// Edges moved by the handle at (x, y): (left, top, right, bottom).
fn handle_at(r: Rect, x: f64, y: f64) -> Option<(bool, bool, bool, bool)> {
    let edges = [
        (true, true, false, false),
        (false, true, false, false),
        (false, true, true, false),
        (false, false, true, false),
        (false, false, true, true),
        (false, false, false, true),
        (true, false, false, true),
        (true, false, false, false),
    ];
    handles(r).iter().position(|(hx, hy)| (x - hx).abs() <= HANDLE && (y - hy).abs() <= HANDLE).map(|i| edges[i])
}

unsafe fn hint(cr: *mut Cairo, w: f64, y: f64, text: &str) {
    let font = "Inter, Sans Semi-Bold 14px";
    let (tw, th) = cb::measure_pango_text(cr, text, font, (w - 80.0) as i32, Some(2));
    let (bw, bh) = (tw as f64 + 32.0, th as f64 + 18.0);
    let x = ((w - bw) / 2.0).round();
    cb::fill_rounded_rect(cr, x, y, bw, bh, bh / 2.0, BAR_BG);
    cb::draw_pango_text_wrapped(cr, x + 16.0, y + 9.0, text, font, WHITE, (w - 80.0) as i32, Some(2));
}

// ── Finishing: save, clipboard, notification ───────────────────────────

/// Save the image as `Screenshot_<time>.png` (unless `save` is false, then to the runtime dir),
/// copy it to the clipboard and post a notification. Returns the file path.
pub fn finish(img: Image, cfg: &Config, save: bool, ) -> Result<std::path::PathBuf, String> {
    let dir = if save {
        prism_common::config::expand_home(&cfg.capture_screenshot_dir)
    } else {
        std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()))
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("Screenshot_{}.png", timestamp()));
    let mut img = img;
    unsafe {
        let surf = shot_surface(&mut img);
        let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let r = cb::cairo_surface_write_to_png(surf, c.as_ptr());
        cb::cairo_surface_destroy(surf);
        if r != 0 {
            return Err(format!("could not write {} (cairo status {r})", path.display()));
        }
    }
    if cfg.capture_copy_to_clipboard {
        if let Ok(bytes) = std::fs::read(&path) {
            prism_common::wlclip::copy(&["image/png"], bytes, true);
        }
    }
    notify(&path, save, img.width, img.height);
    Ok(path)
}

pub use prism_common::capture::timestamp;

/// "Screenshot saved" with Open / Show in folder / Delete; actions are handled on a thread that
/// waits for the user's choice.
fn notify(path: &std::path::Path, saved: bool, w: u32, h: u32) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        use std::collections::HashMap;
        use zbus::zvariant::Value;
        let Ok(conn) = zbus::blocking::Connection::session() else { return };
        let p = path.to_string_lossy().into_owned();
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("image-path", Value::from(p.as_str()));
        hints.insert("desktop-entry", Value::from("prism-capture"));
        // Prism's notification UI shows the picture as a large preview, not as the icon.
        hints.insert("x-prism-preview", Value::from(true));
        let actions: Vec<&str> = if saved { vec!["open", "Open", "folder", "Show in folder", "delete", "Delete"] } else { vec!["open", "Open"] };
        let body = if saved { format!("{w} × {h} · copied to the clipboard\n{}", path.display()) } else { format!("{w} × {h} · copied to the clipboard") };
        const D: &str = "org.freedesktop.Notifications";
        let Ok(proxy) = zbus::blocking::Proxy::new(&conn, D, "/org/freedesktop/Notifications", D) else { return };
        let Ok(signals) = proxy.receive_all_signals() else { return };
        let Ok(reply) = conn.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Screenshot", 0u32, "applets-screenshooter", "Screenshot saved", body.as_str(), actions, hints, -1i32)) else { return };
        let Ok(id) = reply.body().deserialize::<u32>() else { return };
        for m in signals {
            let member = m.header().member().map(|s| s.to_string()).unwrap_or_default();
            match member.as_str() {
                "ActionInvoked" => {
                    if let Ok((nid, action)) = m.body().deserialize::<(u32, String)>() {
                        if nid != id {
                            continue;
                        }
                        let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                        match action.as_str() {
                            "open" | "default" => {
                                let _ = prism_common::apps::app_command("xdg-open", std::slice::from_ref(&p)).spawn();
                            }
                            "folder" => {
                                let _ = prism_common::apps::app_command("xdg-open", &[dir]).spawn();
                            }
                            "delete" => {
                                let _ = std::fs::remove_file(&path);
                            }
                            _ => {}
                        }
                        return;
                    }
                }
                "NotificationClosed"
                    if m.body().deserialize::<(u32, u32)>().is_ok_and(|(nid, _)| nid == id) => {
                        return;
                    }
                _ => {}
            }
        }
    });
}

/// Instant capture of a whole output (`prism-ctl screenshot screen`): the first output, or
/// the one named `name`.
pub fn capture_screen_now(conn: &Connection, name: Option<&str>, cursor: bool) -> Result<Image, String> {
    let mut cap = Capturer::new(conn)?;
    let outs = cap.outputs();
    let o = name.and_then(|n| outs.iter().find(|o| o.name == n)).or(outs.first()).cloned().ok_or("no outputs")?;
    cap.capture_output(&o, cursor)
}

// ── Wayland events ───────────────────────────────────────────────────────

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                st.size = (width as i32, height as i32);
                st.configured = true;
                st.dirty = true;
                if st.frame_cb.is_none() {
                    st.draw(qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => st.outcome = Some(Outcome::Cancelled),
            _ => {}
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(st: &mut Self, _: &wl_surface::WlSurface, ev: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // Show the frozen frame of the output the overlay is on.
        if let wl_surface::Event::Enter { output } = ev {
            if let Some(i) = st.shots.iter().position(|s| s.info.output.id() == output.id()) {
                st.cur = i;
                st.dirty = true;
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                st.enter_serial = serial;
                st.ptr = (surface_x, surface_y);
                st.set_cursor_shape();
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => st.motion(surface_x, surface_y),
            wl_pointer::Event::Button { button, state: WEnum::Value(s), .. } => match (button, s) {
                (0x110, wl_pointer::ButtonState::Pressed) => {
                    let (x, y) = st.ptr;
                    st.press(x, y);
                }
                (0x110, wl_pointer::ButtonState::Released) => st.release(),
                (0x111, wl_pointer::ButtonState::Pressed) => {
                    if st.kind == Kind::Region && st.sel.is_some() {
                        st.sel = None;
                        st.dirty = true;
                    } else {
                        st.outcome = Some(Outcome::Cancelled);
                    }
                }
                _ => {}
            },
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } if st.kind == Kind::Window => {
                let n = st.filtered_windows().len();
                st.win_sel = if value > 0.0 { (st.win_sel + 1).min(n.saturating_sub(1)) } else { st.win_sel.saturating_sub(1) };
                st.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(st: &mut Self, _: &wl_keyboard::WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                let km = unsafe { xkb::Keymap::new_from_fd(&st.xkb_ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) };
                st.xkb = km.ok().flatten().map(|k| xkb::State::new(&k));
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(x) = st.xkb.as_mut() {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                let (sym, text) = st.xkb.as_ref().map(|x| (x.key_get_one_sym(xkb::Keycode::new(key + 8)).raw(), x.key_get_utf8(xkb::Keycode::new(key + 8)))).unwrap_or((0, String::new()));
                st.key(sym, text);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(st: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        st.frame_cb = None;
        if st.dirty && st.outcome.is_none() {
            st.draw(qh);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(st: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            for b in st.bufs.iter_mut() {
                if &b.buffer == buffer {
                    b.busy = false;
                }
            }
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(wl_shm_pool::WlShmPool, WpCursorShapeDeviceV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_and_kinds() {
        let r = Rect { x: 10.0, y: 20.0, w: 100.0, h: 50.0 };
        assert_eq!(handle_at(r, 10.0, 20.0), Some((true, true, false, false)));
        assert_eq!(handle_at(r, 110.0, 70.0), Some((false, false, true, true)));
        assert_eq!(handle_at(r, 60.0, 45.0), None);
        assert_eq!(Kind::parse("output"), Kind::Screen);
        assert_eq!(Kind::parse("window").name(), "window");
        assert_eq!(timestamp().len(), 19);
    }
}
