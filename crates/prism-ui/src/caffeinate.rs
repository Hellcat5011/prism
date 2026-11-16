//! Caffeinate: keep the session awake with an idle inhibitor (QML shell.qml IdleInhibitor).
//!
//! zwp_idle_inhibitor_v1 needs a mapped surface, so this holds a 1x1 fully transparent layer
//! surface with an empty input region on the background layer while it is on. Compositor idle
//! (screen blanking, ext-idle-notify used by prismd's auto-lock) is suppressed while it exists.

use wayland_client::protocol::{wl_buffer, wl_region, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::desktop::ShmBuf;
use crate::Globals;

pub struct Caffeinate {
    queue: EventQueue<State>,
    st: State,
}

#[derive(Default)]
struct State {
    surface: Option<wl_surface::WlSurface>,
    layer: Option<ZwlrLayerSurfaceV1>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
    buf: Option<ShmBuf>,
    shm: Option<wayland_client::protocol::wl_shm::WlShm>,
}

impl Caffeinate {
    pub fn new(conn: &Connection) -> Caffeinate {
        Caffeinate { queue: conn.new_event_queue(), st: State::default() }
    }

    pub fn active(&self) -> bool {
        self.st.inhibitor.is_some()
    }

    pub fn set(&mut self, on: bool, g: &Globals) {
        if on == self.active() {
            return;
        }
        if !on {
            self.release();
            return;
        }
        let qh = self.queue.handle();
        let (Some(comp), Some(shell), Some(mgr)) = (&g.compositor, &g.layer_shell, &g.idle_inhibit) else {
            eprintln!("[prism-ui] caffeinate: compositor lacks zwp_idle_inhibit_manager_v1");
            return;
        };
        let surface = comp.create_surface(&qh, ());
        let region = comp.create_region(&qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let layer = shell.get_layer_surface(&surface, None, Layer::Background, "prism-caffeinate".into(), &qh, ());
        layer.set_anchor(Anchor::Top | Anchor::Left);
        layer.set_size(1, 1);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();
        self.st.inhibitor = Some(mgr.create_inhibitor(&surface, &qh, ()));
        self.st.shm = g.shm.clone();
        self.st.surface = Some(surface);
        self.st.layer = Some(layer);
        eprintln!("[prism-ui] caffeinate on");
    }

    fn release(&mut self) {
        if let Some(i) = self.st.inhibitor.take() {
            i.destroy();
        }
        if let Some(l) = self.st.layer.take() {
            l.destroy();
        }
        if let Some(s) = self.st.surface.take() {
            s.destroy();
        }
        self.st.buf = None;
        eprintln!("[prism-ui] caffeinate off");
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = ev {
            ls.ack_configure(serial);
            // Map with one transparent pixel (shm memory starts zeroed).
            if st.buf.is_none() {
                st.buf = st.shm.as_ref().and_then(|shm| ShmBuf::new(shm, qh, 1, 1, ()));
            }
            if let (Some(s), Some(b)) = (&st.surface, &st.buf) {
                s.attach(Some(&b.buffer), 0, 0);
                s.damage_buffer(0, 0, 1, 1);
                s.commit();
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
ignore_events!(wl_surface::WlSurface, wl_region::WlRegion, wl_buffer::WlBuffer, wl_shm_pool::WlShmPool, ZwpIdleInhibitorV1);
