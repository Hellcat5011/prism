//! Open windows in most-recently-used order, for the window switcher: wlr-foreign-toplevel on
//! its own event queue of the daemon's connection, dispatched by the run loop. Each window
//! remembers when it last had focus; the switcher lists them newest first and activates one.

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as toplevel, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};

#[derive(Clone, Debug)]
pub struct Window {
    pub handle: ZwlrForeignToplevelHandleV1,
    pub title: String,
    pub app_id: String,
    pub focused: bool,
    /// Higher = focused more recently.
    pub last_focus: u64,
    pending_focus: bool,
}

pub struct State {
    windows: Vec<Window>,
    clock: u64,
}

pub struct Tracker {
    queue: EventQueue<State>,
    st: State,
    _mgr: ZwlrForeignToplevelManagerV1,
}

impl Tracker {
    pub fn new(conn: &Connection) -> Option<Tracker> {
        let (globals, mut queue) = registry_queue_init::<State>(conn).ok()?;
        let qh = queue.handle();
        let mgr: ZwlrForeignToplevelManagerV1 = globals.bind(&qh, 1..=3, ()).ok()?;
        let mut st = State { windows: Vec::new(), clock: 0 };
        let _ = queue.roundtrip(&mut st);
        Some(Tracker { queue, st, _mgr: mgr })
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    /// Windows, most recently focused first (the focused one leads).
    pub fn mru(&self) -> Vec<Window> {
        let mut w = self.st.windows.clone();
        w.sort_by(|a, b| b.focused.cmp(&a.focused).then(b.last_focus.cmp(&a.last_focus)));
        w
    }

    pub fn activate(&self, w: &Window, seat: &wl_seat::WlSeat) {
        if self.st.windows.iter().any(|x| x.handle.id() == w.handle.id()) {
            w.handle.activate(seat);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(st: &mut Self, _: &ZwlrForeignToplevelManagerV1, ev: manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let manager::Event::Toplevel { toplevel } = ev {
            st.windows.push(Window { handle: toplevel, title: String::new(), app_id: String::new(), focused: false, last_focus: 0, pending_focus: false });
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(st: &mut Self, handle: &ZwlrForeignToplevelHandleV1, ev: toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(i) = st.windows.iter().position(|w| w.handle.id() == handle.id()) else { return };
        match ev {
            toplevel::Event::Title { title } => st.windows[i].title = title,
            toplevel::Event::AppId { app_id } => st.windows[i].app_id = app_id,
            toplevel::Event::State { state } => {
                let states: Vec<u32> = state.chunks_exact(4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])).collect();
                st.windows[i].pending_focus = states.contains(&(toplevel::State::Activated as u32));
            }
            toplevel::Event::Done => {
                let w = &mut st.windows[i];
                if w.pending_focus && !w.focused {
                    st.clock += 1;
                    w.last_focus = st.clock;
                }
                w.focused = w.pending_focus;
            }
            toplevel::Event::Closed => {
                st.windows.remove(i).handle.destroy();
            }
            _ => {}
        }
    }
}
