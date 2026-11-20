//! Privacy indicator: a small pill in the top-right corner while an app uses the microphone or
//! a camera, or Prism's portal shares the screen. Each in use shows its glyph (mic orange,
//! camera green, screen red). Hovering widens it to name the apps; while the screen is shared
//! it has a Stop button. Config `privacy_indicator`.
//!
//! Microphone: running audio capture streams that do not record a sink's monitor. Camera: a
//! camera device node (PipeWire's v4l2 / libcamera) that is running; apps that open
//! /dev/video* directly, bypassing PipeWire, are not seen.

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
use crate::pw_watch::StreamKind;
use crate::Globals;

const H: i32 = 30;
const EXPANDED_W: i32 = 340;

const ICON_MIC: &str = "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3zM19 10v2a7 7 0 0 1-14 0v-2M12 19v3";
const ICON_CAMERA: &str = "M23 7l-7 5 7 5V7zM1 5h15v14H1z";
const ICON_SCREEN: &str = "M2 3h20v14H2zM8 21h8M12 17v4";
const ORANGE: (f64, f64, f64, f64) = (1.0, 0.62, 0.18, 1.0);
const GREEN: (f64, f64, f64, f64) = (0.30, 0.85, 0.45, 1.0);
const RED: (f64, f64, f64, f64) = (0.95, 0.30, 0.32, 1.0);

/// Who uses what right now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub mic: Vec<String>,
    pub camera: Vec<String>,
    pub screen: Vec<String>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.mic.is_empty() && self.camera.is_empty() && self.screen.is_empty()
    }

    fn glyphs(&self) -> Vec<(&'static str, (f64, f64, f64, f64))> {
        let mut g = Vec::new();
        if !self.mic.is_empty() {
            g.push((ICON_MIC, ORANGE));
        }
        if !self.camera.is_empty() {
            g.push((ICON_CAMERA, GREEN));
        }
        if !self.screen.is_empty() {
            g.push((ICON_SCREEN, RED));
        }
        g
    }

    fn text(&self) -> String {
        let mut parts = Vec::new();
        for (what, apps) in [("Mic", &self.mic), ("Camera", &self.camera), ("Screen", &self.screen)] {
            if !apps.is_empty() {
                parts.push(format!("{what}: {}", apps.join(", ")));
            }
        }
        parts.join("  \u{b7}  ")
    }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

/// The current usage from PipeWire and the portal.
pub fn current() -> Usage {
    let streams = crate::pw_watch::snapshot();
    let mut u = Usage::default();
    for s in streams.iter().filter(|s| s.active) {
        match s.kind {
            StreamKind::Record if !s.monitor => push_unique(&mut u.mic, s.app.clone()),
            StreamKind::Camera => {
                // Name the readers when PipeWire knows them.
                let readers: Vec<String> = streams.iter().filter(|r| r.kind == StreamKind::Video && r.active).map(|r| r.app.clone()).collect();
                if readers.is_empty() {
                    push_unique(&mut u.camera, s.app.clone());
                }
                for r in readers {
                    push_unique(&mut u.camera, r);
                }
            }
            _ => {}
        }
    }
    u.screen = crate::xdp::casts();
    u
}

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Follow PipeWire (for the microphone and camera) while the indicator is on.
pub fn apply(cfg: &prism_common::Config) {
    ENABLED.store(cfg.privacy_indicator, std::sync::atomic::Ordering::Relaxed);
    if cfg.privacy_indicator {
        if let Some(w) = crate::xdp::waker() {
            crate::pw_watch::start(w);
        }
    }
}

pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct Indicator {
    queue: EventQueue<State>,
    st: State,
}

struct State {
    usage: Usage,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    shm: wayland_client::protocol::wl_shm::WlShm,
    bufs: Vec<ShmBuf>,
    configured: bool,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,
    expanded: bool,
    hover_stop: bool,
    stop_rect: Rect,
    size: (i32, i32),
}

fn compact_w(u: &Usage) -> i32 {
    12 + u.glyphs().len() as i32 * 22 + 4
}

impl Indicator {
    pub fn open(conn: &Connection, g: &Globals, usage: Usage) -> Option<Indicator> {
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let surface = g.compositor.as_ref()?.create_surface(&qh, ());
        let layer = g.layer_shell.as_ref()?.get_layer_surface(&surface, None, Layer::Overlay, "prism-osd-solid".into(), &qh, ());
        layer.set_anchor(Anchor::Top | Anchor::Right);
        layer.set_margin(8, 8, 0, 0);
        let w = compact_w(&usage);
        layer.set_size(w as u32, H as u32);
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
                usage,
                surface,
                layer,
                pointer,
                cursor_dev,
                shm: g.shm.clone()?,
                bufs: Vec::new(),
                configured: false,
                frame_cb: None,
                dirty: true,
                expanded: false,
                hover_stop: false,
                stop_rect: Rect::default(),
                size: (w, H),
            },
        })
    }

    pub fn set_usage(&mut self, u: Usage) {
        if u != self.st.usage {
            self.st.usage = u;
            self.st.resize();
        }
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    pub fn tick(&mut self) {
        let qh = self.queue.handle();
        if self.st.configured && self.st.dirty && self.st.frame_cb.is_none() {
            self.st.draw(&qh);
        }
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
    fn resize(&mut self) {
        let w = if self.expanded { EXPANDED_W } else { compact_w(&self.usage) };
        self.layer.set_size(w as u32, H as u32);
        self.surface.commit();
        self.dirty = true;
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (sw, sh) = self.size;
        self.bufs.retain(|b| b.w == sw && b.h == sh);
        let idx = match self.bufs.iter().position(|b| !b.busy) {
            Some(i) => i,
            None if self.bufs.len() < 2 => match ShmBuf::new(&self.shm, qh, sw, sh, ()) {
                Some(b) => {
                    self.bufs.push(b);
                    self.bufs.len() - 1
                }
                None => return,
            },
            None => return,
        };
        self.bufs[idx].busy = true;
        self.dirty = false;
        unsafe {
            let surf = self.bufs[idx].cairo();
            let cr = cb::cairo_create(surf);
            cb::cairo_set_operator(cr, 0);
            cb::cairo_paint(cr);
            cb::cairo_set_operator(cr, 2);
            let (w, h) = (sw as f64, sh as f64);
            cb::fill_rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0, (0.08, 0.08, 0.10, 0.92));
            cb::stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, h / 2.0, (1.0, 1.0, 1.0, 0.12), 1.0);
            let mut x = 10.0;
            for (icon, color) in self.usage.glyphs() {
                crate::svg::draw_path(cr, icon, x, (h - 16.0) / 2.0, 16.0, color, Some(2.0));
                x += 22.0;
            }
            self.stop_rect = Rect::default();
            if self.expanded {
                let stop_w = if self.usage.screen.is_empty() { 0.0 } else { 52.0 };
                let tw = w - x - 8.0 - stop_w;
                crate::desktop::text::draw_in_box(
                    cr,
                    &self.usage.text(),
                    &crate::desktop::text::TextStyle { family: "Inter, Sans", px: 12.0, bold: false, color: (1.0, 1.0, 1.0, 0.92) },
                    x + 2.0,
                    0.0,
                    tw,
                    h,
                    crate::desktop::text::Align::Start,
                );
                if stop_w > 0.0 {
                    let r = Rect { x: w - stop_w - 4.0, y: 4.0, w: stop_w, h: h - 8.0 };
                    cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, (RED.0, RED.1, RED.2, if self.hover_stop { 1.0 } else { 0.8 }));
                    cb::draw_pango_text(cr, r.x + 10.0, r.y + 3.0, "Stop", "Inter, Sans Bold 11px", (1.0, 1.0, 1.0, 1.0), None);
                    self.stop_rect = r;
                }
            }
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, sw, sh);
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
            st.dirty = true;
            if st.frame_cb.is_none() {
                st.draw(qh);
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface, .. } if surface == st.surface => {
                if let Some(d) = &st.cursor_dev {
                    d.set_shape(serial, CursorShape::Default);
                }
                st.expanded = true;
                st.resize();
            }
            wl_pointer::Event::Leave { surface, .. } if surface == st.surface => {
                st.expanded = false;
                st.hover_stop = false;
                st.resize();
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                let on = st.stop_rect.contains(surface_x, surface_y);
                if on != st.hover_stop {
                    st.hover_stop = on;
                    st.dirty = true;
                }
            }
            wl_pointer::Event::Button { button: 0x110, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } if st.hover_stop => {
                crate::xdp::stop_casts();
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
