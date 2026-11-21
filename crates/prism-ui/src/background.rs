//! Built-in wallpaper daemon (replaces awww/swww): one Background-layer surface per output,
//! namespace `prism-background`, showing the wallpaper cropped to cover the output.
//!
//! Images are decoded at the output's pixel size on a worker thread (no I/O on the render path)
//! and swapped in with a spring-driven transition (fade, wipe at a random angle, or a circle
//! growing from a random point). Between transitions each output keeps only the buffer on
//! screen; the next transition starts from a copy of it.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use std::os::fd::AsFd;

use prism_common::video::VideoStream;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::cairo_backend::*;
use crate::desktop::ShmBuf;
use crate::ui::SpringValue;
use crate::waker::Waker;
use crate::Globals;

/// The compositor globals the background needs (shared with the rest of prism-ui).
struct G {
    compositor: Option<wayland_client::protocol::wl_compositor::WlCompositor>,
    shm: Option<wayland_client::protocol::wl_shm::WlShm>,
    layer_shell: Option<wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    viewporter: Option<wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter>,
}

/// In-process requests (the wallpaper picker runs on its own thread).
static SHOW: std::sync::Mutex<Option<(Sender<PathBuf>, Waker)>> = std::sync::Mutex::new(None);

/// Show `path` on the built-in background (no-op when it is not running).
pub fn request_show(path: &Path) {
    if let Some((tx, w)) = SHOW.lock().unwrap().as_ref() {
        let _ = tx.send(path.to_path_buf());
        w.wake();
    }
}

/// Transition spring: (response s, damping ratio); critically damped, about a second to settle.
const SPRING: (f64, f64) = (0.9, 1.0);
const NAMESPACE: &str = "prism-background";

pub struct Background {
    queue: EventQueue<St>,
    st: St,
}

struct Job {
    out: u32,
    seq: u64,
    path: PathBuf,
    w: i32,
    h: i32,
}

struct Decoded {
    out: u32,
    seq: u64,
    w: i32,
    h: i32,
    data: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    None,
    Fade,
    Wipe(f64),
    Grow(f64, f64),
}

/// A video on one output: its decoder and one wl_buffer per frame of the shared memory.
struct OutVideo {
    stream: VideoStream,
    buffers: Vec<wl_buffer::WlBuffer>,
    /// A frame has been put on screen (until then the output shows black).
    showing: bool,
}

impl Drop for OutVideo {
    fn drop(&mut self) {
        for b in self.buffers.drain(..) {
            b.destroy();
        }
    }
}

struct Out {
    /// Registry name of the wl_output.
    name: u32,
    output: wl_output::WlOutput,
    mode: (i32, i32),
    scale: i32,
    surface: Option<wl_surface::WlSurface>,
    layer: Option<ZwlrLayerSurfaceV1>,
    viewport: Option<WpViewport>,
    /// Logical size from the layer configure.
    size: (i32, i32),
    bufs: Vec<ShmBuf>,
    /// Index into `bufs` of the buffer on screen.
    shown: Option<usize>,
    /// Incoming image (cover-scaled to the buffer size) and the outgoing picture.
    new: Option<*mut CairoSurface>,
    old: Option<*mut CairoSurface>,
    progress: SpringValue,
    kind: Kind,
    /// Playing a video instead of showing a picture.
    video: Option<OutVideo>,
    /// Decode request number; results for older ones are dropped.
    seq: u64,
    /// The path decoded (or being decoded) for this output, with its pixel size.
    want: Option<(PathBuf, i32, i32)>,
    frame: Option<wl_callback::WlCallback>,
}

impl Out {
    /// Buffer pixel size: the output mode (rotated to the logical aspect) when the viewporter can
    /// scale it, else logical size times the integer scale.
    fn pixel_size(&self, viewporter: bool) -> (i32, i32) {
        let (lw, lh) = self.size;
        if viewporter && self.mode.0 > 0 {
            let (mw, mh) = self.mode;
            let (mw, mh) = if (lw > lh) == (mw > mh) || lw == lh { (mw, mh) } else { (mh, mw) };
            return (mw, mh);
        }
        (lw * self.scale.max(1), lh * self.scale.max(1))
    }

    fn free_images(&mut self) {
        for s in [self.new.take(), self.old.take()].into_iter().flatten() {
            unsafe { cairo_surface_destroy(s) };
        }
    }

    fn destroy(&mut self) {
        self.video = None;
        self.free_images();
        if let Some(f) = self.frame.take() {
            drop(f);
        }
        self.bufs.clear();
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        if let Some(l) = self.layer.take() {
            l.destroy();
        }
        if let Some(s) = self.surface.take() {
            s.destroy();
        }
        self.shown = None;
        self.size = (0, 0);
    }
}

struct St {
    g: G,
    show_rx: Receiver<PathBuf>,
    outs: Vec<Out>,
    jobs: Sender<Job>,
    done: Receiver<Decoded>,
    /// Wallpaper shown (None: daemon off or nothing set yet).
    path: Option<PathBuf>,
    enabled: bool,
    /// The video to play instead of `path`.
    video: Option<PathBuf>,
    /// Video at 60 frames a second with lanczos scaling (`wallpaper_video_smooth`).
    smooth: bool,
    /// Video held on its current frame (`wallpaper_video_pause`); the decoder rests once its
    /// frames are all waiting.
    paused: bool,
    transition: String,
    time_scale: Option<f64>,
    seed: u64,
}

impl Background {
    pub fn new(conn: &Connection, g: &Globals, waker: Waker) -> Option<Background> {
        let (globals, queue) = registry_queue_init::<St>(conn).ok()?;
        let qh = queue.handle();
        let (jobs, job_rx) = channel::<Job>();
        let (done_tx, done) = channel::<Decoded>();
        let (show_tx, show_rx) = channel::<PathBuf>();
        *SHOW.lock().unwrap() = Some((show_tx, waker.clone()));
        let mut st = St {
            g: G { compositor: g.compositor.clone(), shm: g.shm.clone(), layer_shell: g.layer_shell.clone(), viewporter: g.viewporter.clone() },
            show_rx,
            outs: Vec::new(),
            jobs,
            done,
            path: None,
            enabled: false,
            video: None,
            smooth: false,
            paused: false,
            transition: "random".into(),
            time_scale: Some(1.0),
            seed: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1,
        };
        std::thread::Builder::new()
            .name("prism-background".into())
            .spawn(move || decoder(job_rx, done_tx, waker))
            .ok()?;
        for gl in globals.contents().clone_list() {
            if gl.interface == "wl_output" {
                st.add_output(&globals.registry().clone(), gl.name, gl.version, &qh);
            }
        }
        let mut b = Background { queue, st };
        b.apply_config(&prism_common::Config::load());
        Some(b)
    }

    /// Follow the config: show the current wallpaper when the built-in daemon is selected, remove
    /// the surfaces otherwise.
    pub fn apply_config(&mut self, cfg: &prism_common::Config) {
        let enabled = matches!(cfg.wallpaper_daemon.as_str(), "prism" | "prism-video");
        let video = cfg.video_wallpaper();
        self.st.transition = cfg.wallpaper_transition.clone();
        self.st.time_scale = crate::ui::Style::new(cfg, &prism_common::Theme::load()).time_scale();
        if enabled == self.st.enabled && video == self.st.video {
            // Same video in another quality: only the decoders start over.
            if cfg.wallpaper_video_smooth != self.st.smooth {
                self.st.smooth = cfg.wallpaper_video_smooth;
                let qh = self.queue.handle();
                for i in 0..self.st.outs.len() {
                    if self.st.outs[i].video.take().is_some() {
                        self.st.start_video(i, &qh);
                    }
                }
            }
            return;
        }
        self.st.smooth = cfg.wallpaper_video_smooth;
        // Switching between a picture and a video starts over with new surfaces.
        for o in self.st.outs.iter_mut() {
            o.destroy();
            o.want = None;
        }
        self.st.enabled = enabled;
        self.st.video = video;
        let qh = self.queue.handle();
        if enabled {
            self.st.path = prism_common::wallpaper::displayed_wallpaper();
            for i in 0..self.st.outs.len() {
                self.st.create_surface(i, &qh);
            }
        } else {
            for o in self.st.outs.iter_mut() {
                o.destroy();
            }
        }
    }

    /// What the event loop has to watch for the videos.
    pub fn video_fds(&self) -> Vec<i32> {
        self.st.outs.iter().filter_map(|o| o.video.as_ref()).map(|v| v.stream.fd()).collect()
    }

    /// Hold the video on its current frame (windows cover it) or let it play on.
    pub fn set_paused(&mut self, paused: bool) {
        self.st.paused = paused;
    }

    /// Show the frames that are due; returns ms until the next one.
    pub fn tick(&mut self) -> Option<i32> {
        let qh = self.queue.handle();
        self.st.play_videos(&qh)
    }

    /// Show `path` with a transition (when the built-in daemon is selected).
    pub fn show(&mut self, path: &Path) {
        self.st.path = Some(path.to_path_buf());
        if !self.st.enabled {
            return;
        }
        for i in 0..self.st.outs.len() {
            self.st.request(i);
        }
    }

    pub fn dispatch(&mut self) -> bool {
        let qh = self.queue.handle();
        while let Ok(p) = self.st.show_rx.try_recv() {
            self.show(&p);
        }
        for d in self.st.done.try_iter().collect::<Vec<_>>() {
            self.st.decoded(d, &qh);
        }
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }
}

impl St {
    fn rand(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 11) as f64 / (1u64 << 53) as f64
    }

    fn add_output(&mut self, reg: &wl_registry::WlRegistry, name: u32, version: u32, qh: &QueueHandle<St>) {
        let output: wl_output::WlOutput = reg.bind(name, version.min(4), qh, name);
        self.outs.push(Out {
            name,
            output,
            mode: (0, 0),
            scale: 1,
            surface: None,
            layer: None,
            viewport: None,
            size: (0, 0),
            bufs: Vec::new(),
            shown: None,
            new: None,
            old: None,
            progress: SpringValue::new(1.0),
            kind: Kind::None,
            seq: 0,
            want: None,
            video: None,
            frame: None,
        });
        if self.enabled {
            let i = self.outs.len() - 1;
            self.create_surface(i, qh);
        }
    }

    fn create_surface(&mut self, i: usize, qh: &QueueHandle<St>) {
        let (Some(comp), Some(shell)) = (self.g.compositor.clone(), self.g.layer_shell.clone()) else { return };
        let o = &mut self.outs[i];
        if o.surface.is_some() {
            return;
        }
        let surface = comp.create_surface(qh, ());
        // No input: clicks on the bare desktop go nowhere, as with awww.
        let region = comp.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let layer = shell.get_layer_surface(&surface, Some(&o.output), Layer::Background, NAMESPACE.into(), qh, o.name);
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        o.viewport = self.g.viewporter.as_ref().map(|v| v.get_viewport(&surface, qh, ()));
        surface.commit();
        o.surface = Some(surface);
        o.layer = Some(layer);
    }

    /// Start the video on output `i`, once its size is known. A video that cannot be played
    /// leaves the picture on screen.
    fn start_video(&mut self, i: usize, qh: &QueueHandle<St>) {
        let (Some(file), Some(shm)) = (self.video.clone(), self.g.shm.clone()) else { return };
        let vp = self.g.viewporter.is_some();
        let o = &mut self.outs[i];
        if o.video.is_some() || o.size.0 <= 0 || o.surface.is_none() {
            return;
        }
        let (w, h) = o.pixel_size(vp);
        let Some(stream) = VideoStream::open(&file, w, h, self.smooth) else {
            eprintln!("[prism-ui] cannot start the video wallpaper");
            self.video = None;
            return self.request(i);
        };
        let pool = shm.create_pool(stream.memory().as_fd(), stream.pool_size(), qh, ());
        let buffers = (0..prism_common::video::SLOTS as i32)
            .map(|slot| pool.create_buffer(slot * stream.frame_size(), w, h, w * 4, wl_shm::Format::Argb8888, qh, o.name))
            .collect();
        pool.destroy();
        o.video = Some(OutVideo { stream, buffers, showing: false });
    }

    /// Put the frames that are due on screen; returns ms until the next one is due.
    fn play_videos(&mut self, qh: &QueueHandle<St>) -> Option<i32> {
        let now = std::time::Instant::now();
        let mut next: Option<i32> = None;
        let mut failed = false;
        for o in self.outs.iter_mut() {
            let Some(v) = o.video.as_mut() else { continue };
            if !v.stream.pump() {
                failed = true;
                continue;
            }
            // The compositor stops asking for frames while the wallpaper is fully covered, and
            // a pause shows none: the decoder then runs out of free frames and rests. Playing
            // again carries on from there (`take_due` does not race to catch up).
            if o.frame.is_none() && !(self.paused && v.showing) {
                if let (Some(slot), Some(surface)) = (v.stream.take_due(now), o.surface.as_ref()) {
                    surface.attach(Some(&v.buffers[slot as usize]), 0, 0);
                    surface.damage_buffer(0, 0, v.stream.width, v.stream.height);
                    if let Some(view) = o.viewport.as_ref() {
                        view.set_destination(o.size.0, o.size.1);
                    }
                    o.frame = Some(surface.frame(qh, o.name));
                    surface.commit();
                    v.showing = true;
                }
                if let Some(wait) = v.stream.due_in(now) {
                    let ms = wait.as_millis() as i32 + 1;
                    next = Some(next.map_or(ms, |n| n.min(ms)));
                }
            }
        }
        if failed {
            eprintln!("[prism-ui] the video wallpaper stopped; showing the picture instead");
            self.video = None;
            for i in 0..self.outs.len() {
                self.outs[i].video = None;
                self.outs[i].want = None;
                self.request(i);
            }
        }
        next
    }

    /// Ask the worker for the current wallpaper at output `i`'s pixel size.
    fn request(&mut self, i: usize) {
        if self.video.is_some() {
            return;
        }
        let vp = self.g.viewporter.is_some();
        let o = &mut self.outs[i];
        let Some(path) = self.path.clone() else { return };
        if o.size.0 <= 0 || o.surface.is_none() {
            return;
        }
        let (w, h) = o.pixel_size(vp);
        let want = (path.clone(), w, h);
        if o.want.as_ref() == Some(&want) {
            return;
        }
        o.want = Some(want);
        o.seq += 1;
        let _ = self.jobs.send(Job { out: o.name, seq: o.seq, path, w, h });
    }

    fn decoded(&mut self, d: Decoded, qh: &QueueHandle<St>) {
        let Some(i) = self.outs.iter().position(|o| o.name == d.out) else { return };
        if self.outs[i].seq != d.seq || self.outs[i].surface.is_none() {
            return;
        }
        let Some(new) = (unsafe { surface_from_argb(d.w, d.h, &d.data) }) else { return };
        let kind = match self.transition.as_str() {
            "none" => Kind::None,
            "fade" => Kind::Fade,
            "wipe" => Kind::Wipe(self.rand() * std::f64::consts::TAU),
            "grow" => Kind::Grow(self.rand(), self.rand()),
            _ => match (self.rand() * 3.0) as u32 {
                0 => Kind::Fade,
                1 => Kind::Wipe(self.rand() * std::f64::consts::TAU),
                _ => Kind::Grow(self.rand(), self.rand()),
            },
        };
        let ts = self.time_scale;
        let o = &mut self.outs[i];
        // The outgoing picture: what is on screen now (mid-transition too).
        let old = o.shown.and_then(|s| o.bufs.get(s)).and_then(|b| unsafe { copy_buffer(b) });
        o.free_images();
        o.new = Some(new);
        o.old = old;
        if o.old.is_some() && kind != Kind::None && ts.is_some() {
            o.kind = kind;
            o.progress.snap(0.0);
            o.progress.set_scaled(1.0, SPRING, ts);
        } else {
            o.kind = Kind::None;
            o.progress.snap(1.0);
        }
        self.draw(i, qh);
    }

    fn draw(&mut self, i: usize, qh: &QueueHandle<St>) {
        let vp = self.g.viewporter.is_some();
        let Some(shm) = self.g.shm.clone() else { return };
        let o = &mut self.outs[i];
        let (Some(surface), Some(new)) = (o.surface.clone(), o.new) else { return };
        let (w, h) = o.pixel_size(vp);
        if w <= 0 || h <= 0 {
            return;
        }
        o.bufs.retain(|b| b.w == w && b.h == h || b.busy);
        let idx = match o.bufs.iter().position(|b| !b.busy && b.w == w && b.h == h) {
            Some(k) => k,
            None => match ShmBuf::new(&shm, qh, w, h, o.name) {
                Some(b) => {
                    o.bufs.push(b);
                    o.bufs.len() - 1
                }
                None => return,
            },
        };
        let p = o.progress.value().clamp(0.0, 1.0);
        let settled = o.progress.settled(0.001);
        let p = if settled { 1.0 } else { p };
        unsafe {
            let target = o.bufs[idx].cairo();
            let cr = cairo_create(target);
            cairo_set_operator(cr, 1); // SOURCE
            match o.old.filter(|_| p < 1.0) {
                Some(old) => {
                    cairo_set_source_surface(cr, old, 0.0, 0.0);
                    cairo_paint(cr);
                }
                None => {
                    cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 1.0);
                    cairo_paint(cr);
                }
            }
            cairo_set_operator(cr, 2); // OVER
            let (iw, ih) = (cairo_image_surface_get_width(new), cairo_image_surface_get_height(new));
            let (ox, oy) = (((w - iw) / 2) as f64, ((h - ih) / 2) as f64);
            let (fw, fh) = (w as f64, h as f64);
            match o.kind {
                _ if p >= 1.0 => {
                    cairo_set_source_surface(cr, new, ox, oy);
                    cairo_paint(cr);
                }
                Kind::None | Kind::Fade => {
                    cairo_set_source_surface(cr, new, ox, oy);
                    cairo_paint_with_alpha(cr, p);
                }
                Kind::Wipe(angle) => {
                    let r = (fw * fw + fh * fh).sqrt() / 2.0;
                    cairo_save(cr);
                    cairo_translate(cr, fw / 2.0, fh / 2.0);
                    cairo_rotate(cr, angle);
                    cairo_rectangle(cr, -r, -r, 2.0 * r * p, 2.0 * r);
                    cairo_restore(cr);
                    cairo_clip(cr);
                    cairo_set_source_surface(cr, new, ox, oy);
                    cairo_paint(cr);
                }
                Kind::Grow(fx, fy) => {
                    let (cx, cy) = (fx * fw, fy * fh);
                    let far = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)].iter().map(|(x, y)| ((x - cx) * (x - cx) + (y - cy) * (y - cy)).sqrt()).fold(0.0, f64::max);
                    cairo_new_path(cr);
                    cairo_arc(cr, cx, cy, far * p, 0.0, std::f64::consts::TAU);
                    cairo_clip(cr);
                    cairo_set_source_surface(cr, new, ox, oy);
                    cairo_paint(cr);
                }
            }
            cairo_destroy(cr);
            cairo_surface_destroy(target);
        }
        let b = &mut o.bufs[idx];
        b.busy = true;
        surface.attach(Some(&b.buffer), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        match &o.viewport {
            Some(v) => v.set_destination(o.size.0, o.size.1),
            None => surface.set_buffer_scale(o.scale.max(1)),
        }
        o.shown = Some(idx);
        if p >= 1.0 {
            // Done: keep only the buffer on screen.
            o.free_images();
            o.kind = Kind::None;
            let keep = o.bufs.swap_remove(idx);
            o.bufs.retain(|b| b.busy);
            o.bufs.push(keep);
            o.shown = Some(o.bufs.len() - 1);
            o.frame = None;
            // Decode buffers and transition images are large: give the memory back.
            unsafe { libc::malloc_trim(0) };
        } else {
            o.frame = Some(surface.frame(qh, o.name));
        }
        surface.commit();
    }
}

/// Copy a shown buffer into a new image surface (the outgoing picture of a transition).
unsafe fn copy_buffer(b: &ShmBuf) -> Option<*mut CairoSurface> {
    let s = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, b.w, b.h);
    cairo_surface_flush(s);
    let dst = cairo_image_surface_get_data(s);
    if dst.is_null() {
        cairo_surface_destroy(s);
        return None;
    }
    let stride = cairo_image_surface_get_stride(s) as usize;
    let row = b.w as usize * 4;
    for y in 0..b.h as usize {
        std::ptr::copy_nonoverlapping(b.ptr.add(y * row), dst.add(y * stride), row);
    }
    cairo_surface_mark_dirty(s);
    Some(s)
}

unsafe fn surface_from_argb(w: i32, h: i32, data: &[u8]) -> Option<*mut CairoSurface> {
    let s = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, w, h);
    cairo_surface_flush(s);
    let dst = cairo_image_surface_get_data(s);
    if dst.is_null() {
        cairo_surface_destroy(s);
        return None;
    }
    let stride = cairo_image_surface_get_stride(s) as usize;
    let row = w as usize * 4;
    for y in 0..h as usize {
        std::ptr::copy_nonoverlapping(data.as_ptr().add(y * row), dst.add(y * stride), row);
    }
    cairo_surface_mark_dirty(s);
    Some(s)
}

/// Worker: decode the newest job per output, scaled to cover w x h (cropped later when drawn).
fn decoder(rx: Receiver<Job>, tx: Sender<Decoded>, waker: Waker) {
    while let Ok(first) = rx.recv() {
        let mut jobs = vec![first];
        jobs.extend(rx.try_iter());
        // Only the latest request per output matters.
        let mut latest: Vec<Job> = Vec::new();
        for j in jobs {
            latest.retain(|l| l.out != j.out);
            latest.push(j);
        }
        for j in latest {
            match decode_cover(&j.path, j.w, j.h) {
                Some((w, h, data)) => {
                    let _ = tx.send(Decoded { out: j.out, seq: j.seq, w, h, data });
                    waker.wake();
                }
                None => eprintln!("[prism-ui/background] cannot load {}", j.path.display()),
            }
        }
    }
}

fn decode_cover(path: &Path, w: i32, h: i32) -> Option<(i32, i32, Vec<u8>)> {
    let (iw, ih) = prism_common::pixbuf::image_size(path)?;
    let s = (w as f64 / iw as f64).max(h as f64 / ih as f64);
    let (sw, sh) = (((iw as f64 * s).ceil() as i32).max(w), ((ih as f64 * s).ceil() as i32).max(h));
    let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let pb = gdk_pixbuf_new_from_file_at_scale(c.as_ptr(), sw, sh, 0, std::ptr::null_mut());
        if pb.is_null() {
            return None;
        }
        let (pw, ph) = (gdk_pixbuf_get_width(pb), gdk_pixbuf_get_height(pb));
        let stride = gdk_pixbuf_get_rowstride(pb) as usize;
        let n = gdk_pixbuf_get_n_channels(pb) as usize;
        let src = gdk_pixbuf_get_pixels(pb);
        let mut data = Vec::with_capacity((pw * ph * 4) as usize);
        for y in 0..ph as usize {
            let row = src.add(y * stride);
            for x in 0..pw as usize {
                let p = row.add(x * n);
                let a = if n == 4 { *p.add(3) as u32 } else { 255 };
                let (r, g, b) = (*p as u32 * a / 255, *p.add(1) as u32 * a / 255, *p.add(2) as u32 * a / 255);
                data.extend_from_slice(&((a << 24) | (r << 16) | (g << 8) | b).to_ne_bytes());
            }
        }
        g_object_unref(pb as *mut std::ffi::c_void);
        Some((pw, ph, data))
    }
}

// ── Wayland events ───────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
    fn event(st: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_registry::Event::Global { name, interface, version } if interface == "wl_output" => st.add_output(reg, name, version, qh),
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(i) = st.outs.iter().position(|o| o.name == name) {
                    let mut o = st.outs.remove(i);
                    o.destroy();
                    if o.output.version() >= 3 {
                        o.output.release();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for St {
    fn event(st: &mut Self, _: &wl_output::WlOutput, ev: wl_output::Event, name: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let Some(o) = st.outs.iter_mut().find(|o| o.name == *name) else { return };
        match ev {
            wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } if f.contains(wl_output::Mode::Current) => o.mode = (width, height),
            wl_output::Event::Scale { factor } => o.scale = factor,
            wl_output::Event::Done => {
                let i = st.outs.iter().position(|o| o.name == *name).unwrap();
                st.request(i);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for St {
    fn event(st: &mut Self, layer: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, name: &u32, _: &Connection, qh: &QueueHandle<Self>) {
        let Some(i) = st.outs.iter().position(|o| o.name == *name) else { return };
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                let o = &mut st.outs[i];
                let changed = o.size != (width as i32, height as i32);
                o.size = (width as i32, height as i32);
                if changed {
                    st.outs[i].video = None;
                    st.start_video(i, qh);
                    st.request(i);
                }
                // Something on screen at once (the old picture or black) until the decode lands.
                // Mango configures every layer surface again whenever another one changes (a
                // card opening): a playing video must not be covered with black each time.
                let playing = st.outs[i].video.as_ref().is_some_and(|v| v.showing);
                if st.outs[i].new.is_some() {
                    st.draw(i, qh);
                } else if st.outs[i].shown.is_none() && !playing {
                    st.blank(i, qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => st.outs[i].destroy(),
            _ => {}
        }
    }
}

impl St {
    /// A 1x1 black buffer stretched over the output while the first decode runs.
    fn blank(&mut self, i: usize, qh: &QueueHandle<St>) {
        let Some(shm) = self.g.shm.clone() else { return };
        let o = &mut self.outs[i];
        let (Some(surface), Some(view)) = (o.surface.clone(), o.viewport.as_ref()) else { return };
        let Some(mut b) = ShmBuf::new(&shm, qh, 1, 1, o.name) else { return };
        unsafe { *(b.ptr as *mut u32) = 0xff00_0000 };
        surface.attach(Some(&b.buffer), 0, 0);
        surface.damage_buffer(0, 0, 1, 1);
        view.set_destination(o.size.0, o.size.1);
        surface.commit();
        b.busy = true;
        o.bufs.push(b);
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for St {
    fn event(st: &mut Self, cb: &wl_callback::WlCallback, ev: wl_callback::Event, name: &u32, _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = ev {
            let Some(i) = st.outs.iter().position(|o| o.name == *name) else { return };
            if st.outs[i].frame.as_ref() == Some(cb) {
                st.outs[i].frame = None;
                if st.outs[i].video.is_none() {
                    st.draw(i, qh);
                }
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, u32> for St {
    fn event(st: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, name: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            if let Some(o) = st.outs.iter_mut().find(|o| o.name == *name) {
                if let Some(v) = o.video.as_mut() {
                    if let Some(slot) = v.buffers.iter().position(|b| b == buffer) {
                        v.stream.release(slot as u32);
                        return;
                    }
                }
                for b in o.bufs.iter_mut().filter(|b| &b.buffer == buffer) {
                    b.busy = false;
                }
                // Between transitions keep only the buffer on screen.
                if o.new.is_none() {
                    let shown = o.shown.and_then(|i| o.bufs.get(i)).map(|b| b.buffer.clone());
                    o.bufs.retain(|b| b.busy || Some(&b.buffer) == shown.as_ref());
                    o.shown = shown.and_then(|s| o.bufs.iter().position(|b| b.buffer == s));
                }
            }
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for St {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(wl_surface::WlSurface, wl_region::WlRegion, wl_shm_pool::WlShmPool, WpViewport);
