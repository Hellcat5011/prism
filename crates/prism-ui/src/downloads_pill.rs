//! Downloads pill: a small pill at the top centre while files download into the Downloads
//! folder (prismd's downloads watcher sends `UiCommand::Downloads`): what is downloading, how
//! much is on disk and the speed. A click opens the Downloads folder. Below the recording pill
//! when both show.

use prism_common::ipc::DownloadProgress;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_pointer, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};

use crate::cairo_backend as cb;
use crate::desktop::ShmBuf;
use crate::Globals;

const W: i32 = 320;
const H: i32 = 40;
/// Lucide "download".
const ICON: &str = "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3";

pub struct Pill {
    queue: EventQueue<State>,
    st: State,
}

struct State {
    list: Vec<DownloadProgress>,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    shm: wayland_client::protocol::wl_shm::WlShm,
    bufs: Vec<ShmBuf>,
    configured: bool,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,
    hover: bool,
}

fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

/// (title, detail) for the pill.
fn labels(list: &[DownloadProgress]) -> (String, String) {
    let bytes: u64 = list.iter().map(|d| d.bytes).sum();
    let rate: u64 = list.iter().map(|d| d.rate).sum();
    let title = match list {
        [one] => one.name.clone(),
        _ => format!("{} downloads", list.len()),
    };
    let detail = if rate > 0 { format!("{}  ·  {}/s", human(bytes), human(rate)) } else { human(bytes) };
    (title, detail)
}

impl Pill {
    /// `below_rec`: the recording pill is showing; sit under it.
    pub fn open(conn: &Connection, g: &Globals, list: Vec<DownloadProgress>, below_rec: bool) -> Option<Pill> {
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let surface = g.compositor.as_ref()?.create_surface(&qh, ());
        let layer = g.layer_shell.as_ref()?.get_layer_surface(&surface, None, Layer::Overlay, "prism-osd".into(), &qh, ());
        layer.set_anchor(Anchor::Top);
        layer.set_margin(if below_rec { 12 + 40 + 8 } else { 12 }, 0, 0, 0);
        layer.set_size(W as u32, H as u32);
        layer.set_exclusive_zone(-1);
        surface.commit();
        let pointer = g.seat.as_ref().map(|s| s.get_pointer(&qh, ()));
        let cursor_dev = match (&pointer, &g.cursor_shape_manager) {
            (Some(p), Some(m)) => Some(m.get_pointer(p, &qh, ())),
            _ => None,
        };
        Some(Pill {
            queue,
            st: State { list, surface, layer, pointer, cursor_dev, shm: g.shm.clone()?, bufs: Vec::new(), configured: false, frame_cb: None, dirty: true, hover: false },
        })
    }

    pub fn set(&mut self, list: Vec<DownloadProgress>) {
        if list != self.st.list {
            self.st.list = list;
            self.st.dirty = true;
        }
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    pub fn tick(&mut self) {
        let qh = self.queue.handle();
        if self.st.dirty && self.st.configured && self.st.frame_cb.is_none() {
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
    fn draw(&mut self, qh: &QueueHandle<Self>) {
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
        let (title, detail) = labels(&self.list);
        unsafe {
            let surf = self.bufs[idx].cairo();
            let cr = cb::cairo_create(surf);
            cb::cairo_set_operator(cr, 0);
            cb::cairo_paint(cr);
            cb::cairo_set_operator(cr, 2);
            let (w, h) = (W as f64, H as f64);
            cb::fill_rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0, (0.08, 0.08, 0.10, if self.hover { 0.97 } else { 0.92 }));
            cb::stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, h / 2.0, (1.0, 1.0, 1.0, if self.hover { 0.24 } else { 0.12 }), 1.0);
            crate::svg::draw_path(cr, ICON, 14.0, 11.0, 18.0, (0.55, 0.80, 1.0, 1.0), Some(2.0));
            let df = "Inter, Sans 12px";
            let (dw, dh) = cb::measure_pango_text(cr, &detail, df, 400, Some(1));
            cb::draw_pango_text(cr, w - 16.0 - dw as f64, ((h - dh as f64) / 2.0).round(), &detail, df, (1.0, 1.0, 1.0, 0.7), None);
            let tf = "Inter, Sans Semi-Bold 13px";
            let (_, th) = cb::measure_pango_text(cr, "Ag", tf, 400, Some(1));
            let tw = (w - 16.0 - dw as f64 - 12.0 - 42.0).max(20.0);
            cb::draw_pango_text(cr, 42.0, ((h - th as f64) / 2.0).round(), &title, tf, (1.0, 1.0, 1.0, 1.0), Some(tw as i32));
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, W, H);
        self.frame_cb = Some(self.surface.frame(qh, ()));
        self.surface.commit();
        self.dirty = false;
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = ev {
            ls.ack_configure(serial);
            st.configured = true;
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
                    d.set_shape(serial, CursorShape::Pointer);
                }
                st.hover = true;
                st.dirty = true;
            }
            wl_pointer::Event::Leave { surface, .. } if surface == st.surface => {
                st.hover = false;
                st.dirty = true;
            }
            wl_pointer::Event::Button { button: 0x110, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } if st.hover => {
                let dir = std::process::Command::new("xdg-user-dir").arg("DOWNLOAD").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|d| !d.is_empty());
                let dir = dir.unwrap_or_else(|| format!("{}/Downloads", std::env::var("HOME").unwrap_or_default()));
                if let Ok(c) = prism_common::apps::app_command("xdg-open", &[dir]).spawn() {
                    crate::reap(c);
                }
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
