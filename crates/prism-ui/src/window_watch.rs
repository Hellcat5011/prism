//! "Did that open a window?": after a notification action, the notification center stays open
//! unless the app shows a window in answer (a new window, or an existing one brought to the
//! front). A short-lived thread with its own Wayland connection follows wlr-foreign-toplevel
//! for a few seconds and reports the first such window.

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as toplevel, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};

struct Window {
    handle: ZwlrForeignToplevelHandleV1,
    focused: bool,
    pending_focus: bool,
}

struct State {
    windows: Vec<Window>,
    /// The initial window list is in (events after that are news).
    settled: bool,
    seen: bool,
}

/// Watch for `within`; the receiver gets one message if a window opens or comes to the front
/// (and `waker` wakes the run loop to read it).
pub fn watch(within: Duration, waker: Option<crate::waker::Waker>) -> Receiver<()> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("prism-window-watch".into())
        .spawn(move || {
            if run(within) {
                let _ = tx.send(());
                if let Some(w) = waker {
                    w.wake();
                }
            }
        })
        .ok();
    rx
}

fn run(within: Duration) -> bool {
    let Ok(conn) = Connection::connect_to_env() else { return false };
    let Ok((globals, mut queue)) = registry_queue_init::<State>(&conn) else { return false };
    let qh = queue.handle();
    let Ok(mgr) = globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ()) else { return false };
    let mut st = State { windows: Vec::new(), settled: false, seen: false };
    // The existing windows and their states arrive first.
    let _ = queue.roundtrip(&mut st);
    let _ = queue.roundtrip(&mut st);
    st.settled = true;
    let end = Instant::now() + within;
    while Instant::now() < end && !st.seen {
        let _ = queue.flush();
        if let Some(guard) = queue.prepare_read() {
            use std::os::fd::AsRawFd;
            let mut fds = [libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            let left = end.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32;
            if unsafe { libc::poll(fds.as_mut_ptr(), 1, left) } > 0 {
                let _ = guard.read();
            }
        }
        let _ = queue.dispatch_pending(&mut st);
    }
    let seen = st.seen;
    mgr.stop();
    for w in st.windows.drain(..) {
        w.handle.destroy();
    }
    let _ = queue.roundtrip(&mut st);
    seen
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(st: &mut Self, _: &ZwlrForeignToplevelManagerV1, ev: manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let manager::Event::Toplevel { toplevel } = ev {
            // A window that appears now was opened in answer to the action.
            if st.settled {
                st.seen = true;
            }
            st.windows.push(Window { handle: toplevel, focused: false, pending_focus: false });
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
            toplevel::Event::State { state } => {
                let states: Vec<u32> = state.chunks_exact(4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])).collect();
                st.windows[i].pending_focus = states.contains(&(toplevel::State::Activated as u32));
            }
            toplevel::Event::Done => {
                let w = &mut st.windows[i];
                // An existing window brought to the front.
                if st.settled && w.pending_focus && !w.focused {
                    st.seen = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Live: PRISM_WATCH_EXPECT=1 while a window opens, 0 when nothing happens.
    #[test]
    #[ignore]
    fn sees_new_windows() {
        let expect = std::env::var("PRISM_WATCH_EXPECT").unwrap_or_default() == "1";
        assert_eq!(run(Duration::from_secs(3)), expect);
    }
}
