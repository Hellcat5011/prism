//! Recording indicator: a small pill at the top centre while prism-rec runs, with a pulsing
//! red dot, the elapsed time, an audio glyph when sound is recorded, and a Stop button.
//! prismd shows and hides it (`UiCommand::Recording`).

use prism_common::capture::RecordStatus;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_pointer, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};

use crate::cairo_backend as cb;
use crate::desktop::layout::Rect;
use crate::desktop::ShmBuf;
use crate::Globals;

const W: i32 = 196;
const H: i32 = 40;
/// Compact mode (the indicator would be in the recording): a dot in the top-right corner
/// that expands to the full pill on hover.
const DOT: i32 = 24;

pub struct Indicator {
    queue: EventQueue<State>,
    st: State,
}

struct State {
    status: RecordStatus,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    shm: wayland_client::protocol::wl_shm::WlShm,
    bufs: Vec<ShmBuf>,
    configured: bool,
    frame_cb: Option<wl_callback::WlCallback>,
    ptr: (f64, f64),
    hover_stop: bool,
    stop_rect: Rect,
    stopping: bool,
    compact: bool,
    expanded: bool,
    size: (i32, i32),
}

/// Would the pill (top centre, `W` wide, about 60 px tall) be inside the recorded area?
fn in_picture(target: &Option<prism_common::capture::RecordTarget>) -> bool {
    use prism_common::capture::RecordTarget as T;
    match target {
        Some(T::Window { .. }) => false,
        Some(T::Region { x, y, w, .. }) => {
            // The screen width is not known here; outputs are usually 1920-3840 px wide, so test
            // the centre band generously.
            *y < 64 && x + w > 700 && *x < 3200
        }
        _ => true,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Indicator {
    pub fn open(conn: &Connection, g: &Globals, status: RecordStatus) -> Option<Indicator> {
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let surface = g.compositor.as_ref()?.create_surface(&qh, ());
        let compact = in_picture(&status.target);
        // The dot must not get a blurred square behind it (blur covers the whole surface).
        let ns = if compact { "prism-osd-solid" } else { "prism-osd" };
        let layer = g.layer_shell.as_ref()?.get_layer_surface(&surface, None, Layer::Overlay, ns.into(), &qh, ());
        if compact {
            layer.set_anchor(Anchor::Top | Anchor::Right);
            layer.set_margin(8, 8, 0, 0);
            layer.set_size(DOT as u32, DOT as u32);
        } else {
            layer.set_anchor(Anchor::Top);
            layer.set_margin(12, 0, 0, 0);
            layer.set_size(W as u32, H as u32);
        }
        layer.set_exclusive_zone(-1);
        surface.commit();
        let pointer = g.seat.as_ref().map(|s| s.get_pointer(&qh, ()));
        let cursor_dev = match (&pointer, &g.cursor_shape_manager) {
            (Some(p), Some(m)) => Some(m.get_pointer(p, &qh, ())),
            _ => None,
        };
        Some(Indicator {
            queue,
            st: State {
                status,
                surface,
                layer,
                pointer,
                cursor_dev,
                shm: g.shm.clone()?,
                bufs: Vec::new(),
                configured: false,
                frame_cb: None,
                ptr: (0.0, 0.0),
                hover_stop: false,
                stop_rect: Rect::default(),
                stopping: false,
                compact,
                expanded: false,
                size: if compact { (DOT, DOT) } else { (W, H) },
            },
        })
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    /// Redraw (the clock and the pulse); returns the ms until the next tick.
    pub fn tick(&mut self) -> i32 {
        let qh = self.queue.handle();
        if self.st.configured && self.st.frame_cb.is_none() {
            self.st.draw(&qh);
        }
        100
    }

    pub fn teardown(mut self) {
        let s = &mut self.st;
        if let Some(d) = s.cursor_dev.take() {
            d.destroy();
        }
        if let Some(p) = s.pointer.take() {
            p.release();
        }
        s.bufs.clear();
        s.layer.destroy();
        s.surface.destroy();
        let _ = self.queue.dispatch_pending(&mut self.st);
    }
}

impl State {
    /// Compact mode: grow to the pill under the pointer, shrink back when it leaves.
    fn set_expanded(&mut self, on: bool) {
        if !self.compact || self.expanded == on {
            return;
        }
        self.expanded = on;
        let (w, h) = if on { (W, H) } else { (DOT, DOT) };
        self.layer.set_size(w as u32, h as u32);
        self.surface.commit();
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (sw, sh) = self.size;
        self.bufs.retain(|b| b.w == sw && b.h == sh);
        if sw != W && sh == DOT {
            // The dot.
            let idx = match self.bufs.iter().position(|b| !b.busy) {
                Some(i) => i,
                None if self.bufs.len() < 2 => match ShmBuf::new(&self.shm, qh, DOT, DOT, ()) {
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
                cb::cairo_set_operator(cr, 0);
                cb::cairo_paint(cr);
                cb::cairo_set_operator(cr, 2);
                let t = (now_ms() % 1600) as f64 / 1600.0;
                let a = 0.55 + 0.45 * (t * std::f64::consts::TAU).cos().abs();
                let c = DOT as f64 / 2.0;
                // Dark disc and white ring so it reads on any wallpaper.
                cb::fill_circle(cr, c, c, c - 1.0, (0.08, 0.08, 0.10, 0.85));
                cb::stroke_circle(cr, c, c, c - 1.5, (1.0, 1.0, 1.0, 0.35), 1.0);
                cb::fill_circle(cr, c, c, 5.5, (0.95, 0.25, 0.30, a));
                cb::cairo_destroy(cr);
                cb::cairo_surface_destroy(surf);
            }
            self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
            self.surface.damage_buffer(0, 0, DOT, DOT);
            self.frame_cb = Some(self.surface.frame(qh, ()));
            self.surface.commit();
            return;
        }
        let idx = match self.bufs.iter().position(|b| !b.busy) {
            Some(i) => i,
            None if self.bufs.len() < 2 => match ShmBuf::new(&self.shm, qh, W, H, ()) {
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
            cb::cairo_set_operator(cr, 0);
            cb::cairo_paint(cr);
            cb::cairo_set_operator(cr, 2);
            let (w, h) = (W as f64, H as f64);
            cb::fill_rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0, (0.08, 0.08, 0.10, 0.92));
            cb::stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, h / 2.0, (1.0, 1.0, 1.0, 0.12), 1.0);
            // Pulsing dot.
            let t = (now_ms() % 1600) as f64 / 1600.0;
            let a = 0.55 + 0.45 * (t * std::f64::consts::TAU).cos().abs();
            cb::fill_circle(cr, 22.0, h / 2.0, 6.5, (0.95, 0.25, 0.30, a));
            let secs = ((now_ms() - self.status.started_ms).max(0) / 1000) as u64;
            let clock = if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{:02}:{:02}", secs / 60, secs % 60) };
            let label = if self.stopping { "Saving…".to_string() } else { format!("REC  {clock}") };
            cb::draw_pango_text(cr, 38.0, 11.0, &label, "monospace Bold 13px", (1.0, 1.0, 1.0, 1.0), None);
            if self.status.audio {
                crate::svg::draw_path(cr, "M11 5 6 9H2v6h4l5 4V5zM15.54 8.46a5 5 0 0 1 0 7.07", w - 74.0, 12.0, 16.0, (1.0, 1.0, 1.0, 0.7), Some(2.0));
            }
            // Stop button.
            self.stop_rect = Rect { x: w - 46.0, y: 5.0, w: 30.0 + 11.0, h: 30.0 };
            let (cx, cy) = (w - 25.0, h / 2.0);
            cb::fill_circle(cr, cx, cy, 15.0, (1.0, 1.0, 1.0, if self.hover_stop { 0.22 } else { 0.10 }));
            cb::fill_rounded_rect(cr, cx - 5.5, cy - 5.5, 11.0, 11.0, 2.0, (0.95, 0.25, 0.30, 1.0));
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, W, H);
        self.frame_cb = Some(self.surface.frame(qh, ()));
        self.surface.commit();
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = ev {
            ls.ack_configure(serial);
            st.configured = true;
            if width > 0 && height > 0 {
                st.size = (width as i32, height as i32);
            }
            if st.frame_cb.is_none() {
                st.draw(qh);
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } if surface == st.surface => {
                st.ptr = (surface_x, surface_y);
                if let Some(d) = &st.cursor_dev {
                    d.set_shape(serial, CursorShape::Pointer);
                }
                st.set_expanded(true);
            }
            wl_pointer::Event::Leave { .. } => {
                st.hover_stop = false;
                st.set_expanded(false);
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.ptr = (surface_x, surface_y);
                st.hover_stop = st.stop_rect.contains(surface_x, surface_y);
            }
            wl_pointer::Event::Button { button: 0x110, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } if st.hover_stop && !st.stopping => {
                st.stopping = true;
                std::thread::spawn(|| {
                    let _ = prism_common::ipc::request(&prism_common::IpcCommand::Capture(prism_common::ipc::CaptureAction::Stop));
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(st: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        st.frame_cb = None;
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
ignore_events!(wl_surface::WlSurface, wl_shm_pool::WlShmPool, WpCursorShapeDeviceV1);
