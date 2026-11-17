//! Puts data on the clipboard without wl-copy: an `ext_data_control_v1` source owned by the
//! resident prism-ui process, which answers paste requests until another client takes the
//! selection. Used for screenshots (`image/png`).

use std::io::Write;
use std::sync::Arc;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as device, ext_data_control_manager_v1 as manager, ext_data_control_offer_v1 as offer, ext_data_control_source_v1 as source,
};

pub struct ClipOwner {
    queue: EventQueue<State>,
    st: State,
}

struct State {
    manager: manager::ExtDataControlManagerV1,
    device: device::ExtDataControlDeviceV1,
    /// Current source and what it serves.
    current: Option<(source::ExtDataControlSourceV1, Vec<String>, Arc<Vec<u8>>)>,
    /// Offers the compositor announces (we never read them); destroyed when replaced.
    offers: Vec<offer::ExtDataControlOfferV1>,
}

impl ClipOwner {
    pub fn new(conn: &Connection, seat: &wl_seat::WlSeat) -> Option<ClipOwner> {
        let (globals, queue) = registry_queue_init::<State>(conn).ok()?;
        let qh = queue.handle();
        let manager: manager::ExtDataControlManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
        let device = manager.get_data_device(seat, &qh, ());
        Some(ClipOwner { queue, st: State { manager, device, current: None, offers: Vec::new() } })
    }

    /// Own the clipboard with `data` offered under each of `mimes`.
    pub fn set(&mut self, mimes: &[&str], data: Vec<u8>) {
        let qh = self.queue.handle();
        let src = self.st.manager.create_data_source(&qh, ());
        for m in mimes {
            src.offer(m.to_string());
        }
        self.st.device.set_selection(Some(&src));
        if let Some((old, _, _)) = self.st.current.replace((src, mimes.iter().map(|m| m.to_string()).collect(), Arc::new(data))) {
            old.destroy();
        }
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }
}

impl Dispatch<source::ExtDataControlSourceV1, ()> for State {
    fn event(st: &mut Self, src: &source::ExtDataControlSourceV1, ev: source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            source::Event::Send { mime_type, fd } => {
                if let Some((cur, mimes, data)) = &st.current {
                    if cur == src && mimes.contains(&mime_type) {
                        let data = data.clone();
                        // Off the UI thread: the reader may be slow.
                        std::thread::spawn(move || {
                            let mut f = std::fs::File::from(fd);
                            let _ = f.write_all(&data);
                        });
                    }
                }
            }
            source::Event::Cancelled => {
                if st.current.as_ref().is_some_and(|(c, _, _)| c == src) {
                    st.current = None;
                }
                src.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<device::ExtDataControlDeviceV1, ()> for State {
    fn event(st: &mut Self, _: &device::ExtDataControlDeviceV1, ev: device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            device::Event::DataOffer { id } => {
                for o in st.offers.drain(..) {
                    o.destroy();
                }
                st.offers.push(id);
            }
            device::Event::Finished => st.current = None,
            _ => {}
        }
    }
    event_created_child!(State, device::ExtDataControlDeviceV1, [device::EVT_DATA_OFFER_OPCODE => (offer::ExtDataControlOfferV1, ())]);
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<manager::ExtDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &manager::ExtDataControlManagerV1, _: manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<offer::ExtDataControlOfferV1, ()> for State {
    fn event(_: &mut Self, _: &offer::ExtDataControlOfferV1, _: offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
