//! Screen capture core shared by prism-ui (screenshots) and prism-rec (recording).
//!
//! `Capturer` binds the capture globals on its own event queue of an existing Wayland
//! connection, lists outputs (with xdg-output logical geometry) and windows
//! (`ext_foreign_toplevel_list_v1`), and copies an output or a window into an `Image` at the
//! buffer's native size. It prefers `ext_image_copy_capture_v1` and falls back to
//! `zwlr_screencopy_v1` for outputs.

mod shm;

use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{ext_foreign_toplevel_handle_v1 as tl_handle, ext_foreign_toplevel_list_v1 as tl_list};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1 as tl_src_mgr, ext_image_capture_source_v1 as src, ext_output_image_capture_source_manager_v1 as out_src_mgr,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1 as icc_frame, ext_image_copy_capture_manager_v1 as icc_mgr, ext_image_copy_capture_session_v1 as icc_session,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1 as xo_mgr, zxdg_output_v1 as xo};
use wayland_protocols_wlr::screencopy::v1::client::{zwlr_screencopy_frame_v1 as sc_frame, zwlr_screencopy_manager_v1 as sc_mgr};

pub use shm::ShmBuffer;

/// Pixels in cairo's ARGB32 layout (premultiplied, native-endian 0xAARRGGBB = bytes B, G, R, A).
#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: Vec<u8>,
}

impl Image {
    /// The sub-rectangle (x, y, w, h) in pixels, clamped to the image.
    pub fn crop(&self, x: i32, y: i32, w: i32, h: i32) -> Image {
        let x0 = x.clamp(0, self.width as i32) as usize;
        let y0 = y.clamp(0, self.height as i32) as usize;
        let x1 = (x + w).clamp(0, self.width as i32) as usize;
        let y1 = (y + h).clamp(0, self.height as i32) as usize;
        let (cw, ch) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        let stride = cw * 4;
        let mut data = Vec::with_capacity(stride * ch);
        for row in y0..y0 + ch {
            let start = row * self.stride as usize + x0 * 4;
            data.extend_from_slice(&self.data[start..start + stride]);
        }
        Image { width: cw as u32, height: ch as u32, stride: stride as u32, data }
    }
}

#[derive(Clone, Debug)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    /// Logical position and size (compositor coordinates, after scaling).
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub output: wl_output::WlOutput,
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    /// Stable identifier from the compositor.
    pub id: String,
    pub title: String,
    pub app_id: String,
    pub handle: tl_handle::ExtForeignToplevelHandleV1,
}

#[derive(Default)]
struct OutputState {
    name: String,
    description: String,
    pos: (i32, i32),
    size: (i32, i32),
    xdg: Option<xo::ZxdgOutputV1>,
}

#[derive(Default)]
struct WindowState {
    id: String,
    title: String,
    app_id: String,
    closed: bool,
}

/// Progress of one capture.
#[derive(Default)]
struct Pending {
    size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    session_done: bool,
    /// Screencopy: (format, width, height, stride) offers.
    sc_offers: Vec<(wl_shm::Format, u32, u32, u32)>,
    sc_done: bool,
    ready: bool,
    failed: Option<String>,
    /// Buffer constraints changed (frame failed with buffer_constraints): reallocate.
    constraints: bool,
    /// The session ended for good (source gone).
    stopped: bool,
    /// Presentation time of the last ready frame (CLOCK_MONOTONIC, ns).
    pres_ns: u64,
}

pub struct State {
    outputs: Vec<(wl_output::WlOutput, OutputState)>,
    windows: Vec<(tl_handle::ExtForeignToplevelHandleV1, WindowState)>,
    pending: Pending,
}

/// A frame from a `Stream`: valid until the next `next_frame` call.
pub struct Frame<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: wl_shm::Format,
    /// CLOCK_MONOTONIC nanoseconds when the frame was presented.
    pub time_ns: u64,
}

/// A continuous capture of one source (recording).
struct Stream {
    session: icc_session::ExtImageCopyCaptureSessionV1,
    source: src::ExtImageCaptureSourceV1,
    cursor: bool,
    buf: Option<ShmBuffer>,
    format: wl_shm::Format,
}

pub struct Capturer {
    stream: Option<Stream>,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    shm: wl_shm::WlShm,
    icc: Option<icc_mgr::ExtImageCopyCaptureManagerV1>,
    out_src: Option<out_src_mgr::ExtOutputImageCaptureSourceManagerV1>,
    tl_src: Option<tl_src_mgr::ExtForeignToplevelImageCaptureSourceManagerV1>,
    screencopy: Option<sc_mgr::ZwlrScreencopyManagerV1>,
    _list: Option<tl_list::ExtForeignToplevelListV1>,
}

const TIMEOUT: Duration = Duration::from_secs(3);

impl Capturer {
    pub fn new(conn: &Connection) -> Result<Capturer, String> {
        let (globals, mut queue) = registry_queue_init::<State>(conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(|_| "no wl_shm")?;
        let icc: Option<icc_mgr::ExtImageCopyCaptureManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let out_src: Option<out_src_mgr::ExtOutputImageCaptureSourceManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let tl_src: Option<tl_src_mgr::ExtForeignToplevelImageCaptureSourceManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let screencopy: Option<sc_mgr::ZwlrScreencopyManagerV1> = globals.bind(&qh, 1..=3, ()).ok();
        let list: Option<tl_list::ExtForeignToplevelListV1> = globals.bind(&qh, 1..=1, ()).ok();
        let xdg: Option<xo_mgr::ZxdgOutputManagerV1> = globals.bind(&qh, 1..=3, ()).ok();
        if (icc.is_none() || out_src.is_none()) && screencopy.is_none() {
            return Err("the compositor offers no screen capture protocol".into());
        }
        let mut state = State { outputs: Vec::new(), windows: Vec::new(), pending: Pending::default() };
        for g in globals.contents().clone_list() {
            if g.interface == wl_output::WlOutput::interface().name {
                let o: wl_output::WlOutput = globals.registry().bind(g.name, g.version.min(4), &qh, ());
                let mut st = OutputState::default();
                if let Some(x) = &xdg {
                    st.xdg = Some(x.get_xdg_output(&o, &qh, o.clone()));
                }
                state.outputs.push((o, st));
            }
        }
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        Ok(Capturer { stream: None, queue, qh, state, shm, icc, out_src, tl_src, screencopy, _list: list })
    }

    /// Process pending events (window list changes).
    pub fn refresh(&mut self) {
        let _ = self.queue.roundtrip(&mut self.state);
    }

    pub fn outputs(&self) -> Vec<OutputInfo> {
        self.state
            .outputs
            .iter()
            .map(|(o, s)| OutputInfo { name: s.name.clone(), description: s.description.clone(), x: s.pos.0, y: s.pos.1, width: s.size.0, height: s.size.1, output: o.clone() })
            .collect()
    }

    pub fn windows(&self) -> Vec<WindowInfo> {
        self.state
            .windows
            .iter()
            .filter(|(_, w)| !w.closed)
            .map(|(h, w)| WindowInfo { id: w.id.clone(), title: w.title.clone(), app_id: w.app_id.clone(), handle: h.clone() })
            .collect()
    }

    pub fn can_capture_windows(&self) -> bool {
        self.icc.is_some() && self.tl_src.is_some()
    }

    pub fn capture_output(&mut self, output: &OutputInfo, cursor: bool) -> Result<Image, String> {
        if let (Some(_), Some(m)) = (&self.icc, &self.out_src) {
            let source = m.create_source(&output.output, &self.qh, ());
            let r = self.capture_source(&source, cursor);
            source.destroy();
            if r.is_ok() || self.screencopy.is_none() {
                return r;
            }
        }
        self.capture_screencopy(output, cursor)
    }

    pub fn capture_window(&mut self, window: &WindowInfo, cursor: bool) -> Result<Image, String> {
        let m = self.tl_src.clone().ok_or("the compositor cannot capture single windows")?;
        let source = m.create_source(&window.handle, &self.qh, ());
        let r = self.capture_source(&source, cursor);
        source.destroy();
        r
    }

    /// Start streaming an output (Some) or a window.
    pub fn start_stream(&mut self, output: Option<&OutputInfo>, window: Option<&WindowInfo>, cursor: bool) -> Result<(), String> {
        let icc = self.icc.clone().ok_or("recording needs ext_image_copy_capture_v1")?;
        let source = match (output, window) {
            (Some(o), _) => self.out_src.clone().ok_or("no output capture source")?.create_source(&o.output, &self.qh, ()),
            (None, Some(w)) => self.tl_src.clone().ok_or("the compositor cannot capture single windows")?.create_source(&w.handle, &self.qh, ()),
            _ => return Err("nothing to record".into()),
        };
        self.stop_stream();
        self.state.pending = Pending::default();
        let opts = if cursor { icc_mgr::Options::PaintCursors } else { icc_mgr::Options::empty() };
        let session = icc.create_session(&source, opts, &self.qh, ());
        self.stream = Some(Stream { session, source, cursor, buf: None, format: wl_shm::Format::Xrgb8888 });
        self.wait(|p| p.session_done || p.failed.is_some())?;
        if let Some(e) = self.state.pending.failed.take() {
            return Err(e);
        }
        self.alloc_stream_buffer()
    }

    fn alloc_stream_buffer(&mut self) -> Result<(), String> {
        let (w, h) = self.state.pending.size.ok_or("no buffer size")?;
        let format = pick_format(&self.state.pending.formats).ok_or("no supported pixel format")?;
        let buf = ShmBuffer::new(&self.shm, &self.qh, w, h, w * 4, format)?;
        let st = self.stream.as_mut().ok_or("no stream")?;
        if let Some(mut old) = st.buf.replace(buf) {
            old.destroy();
        }
        st.format = format;
        Ok(())
    }

    /// Capture the next frame of the stream. The compositor answers when the source has
    /// changed (or right away for the first frame). Err when the source is gone.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame<'_>>, String> {
        let Some(st) = self.stream.as_ref() else { return Err("no stream".into()) };
        let Some(buf) = st.buf.as_ref() else { return Err("no buffer".into()) };
        let p = &mut self.state.pending;
        p.ready = false;
        p.failed = None;
        p.constraints = false;
        let frame = st.session.create_frame(&self.qh, ());
        frame.attach_buffer(&buf.buffer);
        frame.damage_buffer(0, 0, buf.width as i32, buf.height as i32);
        frame.capture();
        let deadline = Instant::now() + timeout;
        let r = loop {
            let p = &self.state.pending;
            if p.ready || p.failed.is_some() || p.stopped {
                break Ok(());
            }
            if Instant::now() > deadline {
                break Err(());
            }
            // Wait for events with a timeout.
            let _ = self.queue.flush();
            if let Some(guard) = self.queue.prepare_read() {
                use std::os::fd::AsRawFd;
                let mut fds = [libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }];
                let left = deadline.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32;
                if unsafe { libc::poll(fds.as_mut_ptr(), 1, left) } > 0 {
                    let _ = guard.read();
                }
            }
            self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
        };
        frame.destroy();
        if r.is_err() {
            return Ok(None);
        }
        let p = &mut self.state.pending;
        if p.stopped {
            return Err("the recorded source went away".into());
        }
        if p.constraints {
            // Size or format changed (window resized): new buffer, try again next call.
            self.wait(|p| p.session_done)?;
            self.alloc_stream_buffer()?;
            return Ok(None);
        }
        if let Some(e) = p.failed.take() {
            return Err(e);
        }
        let time_ns = p.pres_ns;
        let st = self.stream.as_ref().unwrap();
        let buf = st.buf.as_ref().unwrap();
        Ok(Some(Frame { data: buf.bytes(), width: buf.width, height: buf.height, stride: buf.stride, format: st.format, time_ns }))
    }

    /// Buffer size of the running stream.
    pub fn stream_size(&self) -> Option<(u32, u32)> {
        self.stream.as_ref().and_then(|s| s.buf.as_ref()).map(|b| (b.width, b.height))
    }

    pub fn stop_stream(&mut self) {
        if let Some(mut st) = self.stream.take() {
            if let Some(mut b) = st.buf.take() {
                b.destroy();
            }
            st.session.destroy();
            st.source.destroy();
            let _ = st.cursor;
            let _ = self.queue.roundtrip(&mut self.state);
        }
    }

    fn wait(&mut self, done: impl Fn(&Pending) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
            if done(&self.state.pending) {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("capture timed out".into());
            }
            self.queue.flush().map_err(|e| e.to_string())?;
            // Poll with the time left: a compositor that stays silent must not hang the caller.
            let Some(guard) = self.queue.prepare_read() else { continue };
            use std::os::fd::AsRawFd;
            let mut fds = [libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            if unsafe { libc::poll(fds.as_mut_ptr(), 1, left.as_millis().min(i32::MAX as u128) as i32 + 1) } > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
    }

    fn capture_source(&mut self, source: &src::ExtImageCaptureSourceV1, cursor: bool) -> Result<Image, String> {
        let icc = self.icc.clone().ok_or("no ext_image_copy_capture")?;
        self.state.pending = Pending::default();
        let opts = if cursor { icc_mgr::Options::PaintCursors } else { icc_mgr::Options::empty() };
        let session = icc.create_session(source, opts, &self.qh, ());
        let res = (|| {
            self.wait(|p| p.session_done || p.failed.is_some())?;
            if let Some(e) = self.state.pending.failed.take() {
                return Err(e);
            }
            let (w, h) = self.state.pending.size.ok_or("no buffer size")?;
            let format = pick_format(&self.state.pending.formats).ok_or("no supported pixel format")?;
            let mut buf = ShmBuffer::new(&self.shm, &self.qh, w, h, w * 4, format)?;
            let frame = session.create_frame(&self.qh, ());
            frame.attach_buffer(&buf.buffer);
            frame.damage_buffer(0, 0, w as i32, h as i32);
            frame.capture();
            let r = self.wait(|p| p.ready || p.failed.is_some());
            frame.destroy();
            r?;
            if let Some(e) = self.state.pending.failed.take() {
                buf.destroy();
                return Err(e);
            }
            let img = buf.to_image(format);
            buf.destroy();
            Ok(img)
        })();
        session.destroy();
        let _ = self.queue.roundtrip(&mut self.state);
        res
    }

    fn capture_screencopy(&mut self, output: &OutputInfo, cursor: bool) -> Result<Image, String> {
        let sc = self.screencopy.clone().ok_or("no screencopy")?;
        self.state.pending = Pending::default();
        let frame = sc.capture_output(cursor as i32, &output.output, &self.qh, ());
        let res = (|| {
            // v3 sends buffer_done after its offers; v1/v2 send only `buffer`.
            let v3 = sc.version() >= 3;
            self.wait(|p| p.failed.is_some() || if v3 { p.sc_done } else { !p.sc_offers.is_empty() })?;
            if let Some(e) = self.state.pending.failed.take() {
                return Err(e);
            }
            let fmts: Vec<wl_shm::Format> = self.state.pending.sc_offers.iter().map(|o| o.0).collect();
            let format = pick_format(&fmts).ok_or("no supported pixel format")?;
            let (_, w, h, stride) = *self.state.pending.sc_offers.iter().find(|o| o.0 == format).unwrap();
            let mut buf = ShmBuffer::new(&self.shm, &self.qh, w, h, stride, format)?;
            frame.copy(&buf.buffer);
            let r = self.wait(|p| p.ready || p.failed.is_some());
            r?;
            if let Some(e) = self.state.pending.failed.take() {
                buf.destroy();
                return Err(e);
            }
            let img = buf.to_image(format);
            buf.destroy();
            Ok(img)
        })();
        frame.destroy();
        res
    }
}

/// Formats we can turn into cairo ARGB32, best first.
pub fn pick_format(offered: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    use wl_shm::Format::*;
    [Xrgb8888, Argb8888, Xbgr8888, Abgr8888, Xrgb2101010, Argb2101010, Xbgr2101010, Abgr2101010].into_iter().find(|f| offered.contains(f))
}

// ── Dispatch ────────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(st: &mut Self, o: &wl_output::WlOutput, ev: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some((_, s)) = st.outputs.iter_mut().find(|(x, _)| x == o) else { return };
        match ev {
            wl_output::Event::Name { name } => s.name = name,
            wl_output::Event::Description { description } => s.description = description,
            wl_output::Event::Geometry { x, y, .. } if s.xdg.is_none() => s.pos = (x, y),
            wl_output::Event::Mode { width, height, flags: WEnum::Value(f), .. } if s.xdg.is_none() && f.contains(wl_output::Mode::Current) => s.size = (width, height),
            _ => {}
        }
    }
}

impl Dispatch<xo::ZxdgOutputV1, wl_output::WlOutput> for State {
    fn event(st: &mut Self, _: &xo::ZxdgOutputV1, ev: xo::Event, o: &wl_output::WlOutput, _: &Connection, _: &QueueHandle<Self>) {
        let Some((_, s)) = st.outputs.iter_mut().find(|(x, _)| x == o) else { return };
        match ev {
            xo::Event::LogicalPosition { x, y } => s.pos = (x, y),
            xo::Event::LogicalSize { width, height } => s.size = (width, height),
            xo::Event::Name { name } if s.name.is_empty() => s.name = name,
            _ => {}
        }
    }
}

impl Dispatch<tl_list::ExtForeignToplevelListV1, ()> for State {
    fn event(st: &mut Self, _: &tl_list::ExtForeignToplevelListV1, ev: tl_list::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let tl_list::Event::Toplevel { toplevel } = ev {
            st.windows.push((toplevel, WindowState::default()));
        }
    }
    event_created_child!(State, tl_list::ExtForeignToplevelListV1, [tl_list::EVT_TOPLEVEL_OPCODE => (tl_handle::ExtForeignToplevelHandleV1, ())]);
}

impl Dispatch<tl_handle::ExtForeignToplevelHandleV1, ()> for State {
    fn event(st: &mut Self, h: &tl_handle::ExtForeignToplevelHandleV1, ev: tl_handle::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some((_, w)) = st.windows.iter_mut().find(|(x, _)| x == h) else { return };
        match ev {
            tl_handle::Event::Title { title } => w.title = title,
            tl_handle::Event::AppId { app_id } => w.app_id = app_id,
            tl_handle::Event::Identifier { identifier } => w.id = identifier,
            tl_handle::Event::Closed => {
                w.closed = true;
                h.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<icc_session::ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(st: &mut Self, _: &icc_session::ExtImageCopyCaptureSessionV1, ev: icc_session::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let p = &mut st.pending;
        match ev {
            icc_session::Event::BufferSize { width, height } => {
                // A new set of constraints starts.
                p.size = Some((width, height));
                p.formats.clear();
            }
            icc_session::Event::ShmFormat { format: WEnum::Value(f) } => p.formats.push(f),
            icc_session::Event::Done => p.session_done = true,
            icc_session::Event::Stopped => {
                p.stopped = true;
                p.failed = Some("the capture source went away".into());
            }
            _ => {}
        }
    }
}

impl Dispatch<icc_frame::ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(st: &mut Self, _: &icc_frame::ExtImageCopyCaptureFrameV1, ev: icc_frame::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            icc_frame::Event::Ready => st.pending.ready = true,
            icc_frame::Event::PresentationTime { tv_sec_hi, tv_sec_lo, tv_nsec } => {
                st.pending.pres_ns = ((((tv_sec_hi as u64) << 32) | tv_sec_lo as u64) * 1_000_000_000) + tv_nsec as u64;
            }
            icc_frame::Event::Failed { reason: WEnum::Value(icc_frame::FailureReason::BufferConstraints) } => {
                st.pending.constraints = true;
                st.pending.failed = Some("buffer constraints changed".into());
            }
            icc_frame::Event::Failed { reason } => st.pending.failed = Some(format!("capture failed ({reason:?})")),
            _ => {}
        }
    }
}

impl Dispatch<sc_frame::ZwlrScreencopyFrameV1, ()> for State {
    fn event(st: &mut Self, _: &sc_frame::ZwlrScreencopyFrameV1, ev: sc_frame::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let p = &mut st.pending;
        match ev {
            sc_frame::Event::Buffer { format: WEnum::Value(f), width, height, stride } => p.sc_offers.push((f, width, height, stride)),
            sc_frame::Event::BufferDone => p.sc_done = true,
            sc_frame::Event::Ready { .. } => p.ready = true,
            sc_frame::Event::Failed => p.failed = Some("screencopy failed".into()),
            _ => {}
        }
    }
}

macro_rules! noop {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
noop!(
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    icc_mgr::ExtImageCopyCaptureManagerV1,
    out_src_mgr::ExtOutputImageCaptureSourceManagerV1,
    tl_src_mgr::ExtForeignToplevelImageCaptureSourceManagerV1,
    src::ExtImageCaptureSourceV1,
    sc_mgr::ZwlrScreencopyManagerV1,
    xo_mgr::ZxdgOutputManagerV1
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_and_picks_formats() {
        let img = Image { width: 4, height: 3, stride: 16, data: (0..48).collect() };
        let c = img.crop(1, 1, 2, 5);
        assert_eq!((c.width, c.height, c.stride), (2, 2, 8));
        assert_eq!(&c.data[..8], &[20, 21, 22, 23, 24, 25, 26, 27]);
        assert_eq!(pick_format(&[wl_shm::Format::Abgr8888, wl_shm::Format::Xrgb8888]), Some(wl_shm::Format::Xrgb8888));
        assert_eq!(pick_format(&[wl_shm::Format::Rgb565]), None);
    }
}
