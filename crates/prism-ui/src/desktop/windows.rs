// ─────────────────────────────────────────────────────────────────────────
// desktop/windows.rs — what the compositor says about windows and workspaces
//
// Windows come from wlr-foreign-toplevel (app id, focus, activate, minimize,
// close) and workspaces from ext-workspace. Neither says which windows are
// on screen right now or which workspaces hold windows, so on Mango a
// `mmsg watch all-clients` stream adds that; elsewhere every window that is
// not minimized counts as on screen.
// ─────────────────────────────────────────────────────────────────────────

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::Duration;

use serde::Deserialize;
use wayland_client::protocol::wl_output;
use wayland_client::{event_created_child, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self as ws_group, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self as ws, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self as ws_manager, ExtWorkspaceManagerV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as toplevel, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as toplevel_manager, ZwlrForeignToplevelManagerV1},
};

use crate::waker::Waker;

use super::Desk;

pub struct Toplevel {
    pub handle: ZwlrForeignToplevelHandleV1,
    pub app_id: String,
    pub focused: bool,
    pub minimized: bool,
}

pub struct Workspace {
    pub handle: ExtWorkspaceHandleV1,
    pub name: String,
    pub active: bool,
    pub urgent: bool,
}

struct Group {
    handle: ExtWorkspaceGroupHandleV1,
    outputs: Vec<wl_output::WlOutput>,
    workspaces: Vec<ExtWorkspaceHandleV1>,
}

/// One workspace as the bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceView {
    pub index: usize,
    pub name: String,
    pub active: bool,
    pub urgent: bool,
    pub occupied: bool,
}

#[derive(Default)]
pub struct Windows {
    pub workspace_manager: Option<ExtWorkspaceManagerV1>,
    pub toplevels: Vec<Toplevel>,
    pub workspaces: Vec<Workspace>,
    groups: Vec<Group>,
    /// Something changed since the panels last looked.
    pub changed: bool,
    /// A window changed in a way the panels show, and has not finished updating. Titles do not
    /// count: a busy terminal or browser tab changes its title many times a second.
    pending: bool,
}

impl Windows {
    /// The workspaces of `output` in order, with what Mango knows about their windows.
    pub fn workspace_views(&self, output: Option<&wl_output::WlOutput>, output_name: &str) -> Vec<WorkspaceView> {
        let group = output.and_then(|o| self.groups.iter().find(|g| g.outputs.contains(o)));
        let clients = clients();
        let mut views: Vec<WorkspaceView> = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, w)| group.is_none_or(|g| g.workspaces.contains(&w.handle)))
            .map(|(index, w)| WorkspaceView {
                index,
                name: w.name.clone(),
                active: w.active,
                urgent: w.urgent,
                occupied: clients.as_ref().is_some_and(|c| occupied(c, &w.name, output_name)),
            })
            .collect();
        views.sort_by_key(|v| (v.name.parse::<u32>().unwrap_or(u32::MAX), v.index));
        views
    }

    /// Whether windows are showing on `output_name` now.
    pub fn on_screen(&self, output_name: &str) -> bool {
        match clients() {
            Some(c) => c.iter().any(|c| c.is_visible && !c.is_minimized && (output_name.is_empty() || c.monitor == output_name)),
            None => self.toplevels.iter().any(|t| !t.minimized),
        }
    }
}

fn occupied(clients: &[Client], workspace: &str, output_name: &str) -> bool {
    let Ok(tag) = workspace.parse::<u32>() else { return false };
    clients.iter().any(|c| c.tags.contains(&tag) && (output_name.is_empty() || c.monitor == output_name))
}

// ── mmsg ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Client {
    #[serde(default)]
    pub monitor: String,
    #[serde(default)]
    pub tags: Vec<u32>,
    #[serde(default)]
    pub is_visible: bool,
    #[serde(default)]
    pub is_minimized: bool,
    /// Where the window is, in the layout's pixels.
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
}

#[derive(Deserialize)]
struct ClientList {
    clients: Vec<Client>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Monitor {
    name: String,
    #[serde(default)]
    active: bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Deserialize)]
struct MonitorList {
    monitors: Vec<Monitor>,
}

static CLIENTS: Mutex<(u64, Option<Vec<Client>>)> = Mutex::new((0, None));
/// Mango's monitors, read when the window stream starts and when a window names an unknown one.
static MONITORS: Mutex<Vec<Monitor>> = Mutex::new(Vec::new());

fn read_monitors() {
    let out = Command::new("mmsg").args(["get", "all-monitors"]).stdin(Stdio::null()).stderr(Stdio::null()).output();
    if let Some(list) = out.ok().and_then(|o| serde_json::from_slice::<MonitorList>(&o.stdout).ok()) {
        *MONITORS.lock().unwrap() = list.monitors.into_iter().filter(|m| m.active && m.width > 0 && m.height > 0).collect();
    }
}

/// Windows cover every screen, by Mango's account (see `cover::covered`). False where Mango's
/// window list is not available.
pub fn background_covered() -> bool {
    // Worked out once per change of the window list (or of the monitors).
    static LAST: Mutex<Option<(u64, usize, bool)>> = Mutex::new(None);
    let generation = clients_generation();
    let monitors = MONITORS.lock().unwrap().clone();
    let mut last = LAST.lock().unwrap();
    if let Some((g, n, v)) = *last {
        if g == generation && n == monitors.len() {
            return v;
        }
    }
    let v = clients().is_some_and(|c| super::cover::all_covered(&screens(&monitors, &c)));
    *last = Some((generation, monitors.len(), v));
    v
}

fn screens(monitors: &[Monitor], clients: &[Client]) -> Vec<super::cover::Screen> {
    monitors
        .iter()
        .map(|m| super::cover::Screen {
            rect: (m.x as f64, m.y as f64, m.width as f64, m.height as f64),
            windows: clients
                .iter()
                .filter(|c| c.is_visible && !c.is_minimized && c.monitor == m.name && c.width > 0 && c.height > 0)
                .map(|c| (c.x as f64, c.y as f64, c.width as f64, c.height as f64))
                .collect(),
        })
        .collect()
}

static WATCH: Once = Once::new();

fn clients() -> Option<Vec<Client>> {
    CLIENTS.lock().unwrap().1.clone()
}

pub fn clients_generation() -> u64 {
    CLIENTS.lock().unwrap().0
}

fn publish(list: Option<Vec<Client>>, waker: &Waker) {
    let unknown = list.as_ref().is_some_and(|l| {
        let known = MONITORS.lock().unwrap();
        l.iter().any(|c| !c.monitor.is_empty() && !known.iter().any(|m| m.name == c.monitor))
    });
    if unknown {
        read_monitors();
    }
    let mut st = CLIENTS.lock().unwrap();
    if st.1 != list {
        st.0 += 1;
        st.1 = list;
        drop(st);
        waker.wake();
    }
}

/// Follow Mango's window list for as long as this process runs.
pub fn watch_clients(waker: Waker) {
    WATCH.call_once(|| {
        std::thread::spawn(move || loop {
            read_monitors();
            let mut cmd = Command::new("mmsg");
            cmd.args(["watch", "all-clients"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            // The stream must end with this process, also when it is killed.
            unsafe {
                use std::os::unix::process::CommandExt;
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            if let Ok(mut child) = cmd.spawn() {
                if let Some(out) = child.stdout.take() {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if let Ok(list) = serde_json::from_str::<ClientList>(&line) {
                            publish(Some(list.clients), &waker);
                        }
                    }
                }
                let _ = child.wait();
            }
            publish(None, &waker);
            std::thread::sleep(Duration::from_secs(5));
        });
    });
}

// ── Protocol events ───────────────────────────────────────────────────────

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Desk {
    fn event(d: &mut Self, _: &ZwlrForeignToplevelManagerV1, ev: toplevel_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let toplevel_manager::Event::Toplevel { toplevel } = ev {
            d.windows.toplevels.push(Toplevel { handle: toplevel, app_id: String::new(), focused: false, minimized: false });
            d.windows.pending = true;
        }
    }

    event_created_child!(Desk, ZwlrForeignToplevelManagerV1, [
        toplevel_manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Desk {
    fn event(d: &mut Self, handle: &ZwlrForeignToplevelHandleV1, ev: toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let w = &mut d.windows;
        let Some(i) = w.toplevels.iter().position(|t| &t.handle == handle) else { return };
        match ev {
            toplevel::Event::AppId { app_id } => {
                w.pending |= w.toplevels[i].app_id != app_id;
                w.toplevels[i].app_id = app_id;
            }
            toplevel::Event::State { state } => {
                // An array of native-endian u32 state values.
                let states: Vec<u32> = state.chunks_exact(4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])).collect();
                let focused = states.contains(&(toplevel::State::Activated as u32));
                let minimized = states.contains(&(toplevel::State::Minimized as u32));
                w.pending |= w.toplevels[i].focused != focused || w.toplevels[i].minimized != minimized;
                w.toplevels[i].focused = focused;
                w.toplevels[i].minimized = minimized;
            }
            toplevel::Event::Done => w.changed |= std::mem::take(&mut w.pending),
            toplevel::Event::Closed => {
                w.toplevels.remove(i);
                handle.destroy();
                w.changed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Desk {
    fn event(d: &mut Self, _: &ExtWorkspaceManagerV1, ev: ws_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            ws_manager::Event::WorkspaceGroup { workspace_group } => {
                d.windows.groups.push(Group { handle: workspace_group, outputs: Vec::new(), workspaces: Vec::new() });
            }
            ws_manager::Event::Workspace { workspace } => {
                d.windows.workspaces.push(Workspace { handle: workspace, name: String::new(), active: false, urgent: false });
            }
            ws_manager::Event::Done => d.windows.changed = true,
            _ => {}
        }
    }

    event_created_child!(Desk, ExtWorkspaceManagerV1, [
        ws_manager::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ws_manager::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Desk {
    fn event(d: &mut Self, handle: &ExtWorkspaceGroupHandleV1, ev: ws_group::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let groups = &mut d.windows.groups;
        let Some(i) = groups.iter().position(|g| &g.handle == handle) else { return };
        match ev {
            ws_group::Event::OutputEnter { output } => groups[i].outputs.push(output),
            ws_group::Event::OutputLeave { output } => groups[i].outputs.retain(|o| *o != output),
            ws_group::Event::WorkspaceEnter { workspace } => groups[i].workspaces.push(workspace),
            ws_group::Event::WorkspaceLeave { workspace } => groups[i].workspaces.retain(|w| *w != workspace),
            ws_group::Event::Removed => {
                groups.remove(i);
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for Desk {
    fn event(d: &mut Self, handle: &ExtWorkspaceHandleV1, ev: ws::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let list = &mut d.windows.workspaces;
        let Some(i) = list.iter().position(|w| &w.handle == handle) else { return };
        match ev {
            ws::Event::Name { name } => list[i].name = name,
            ws::Event::State { state: WEnum::Value(state) } => {
                list[i].active = state.contains(ws::State::Active);
                list[i].urgent = state.contains(ws::State::Urgent);
            }
            ws::Event::Removed => {
                list.remove(i);
                for g in &mut d.windows.groups {
                    g.workspaces.retain(|w| w != handle);
                }
                handle.destroy();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(tags: &[u32], monitor: &str) -> Client {
        Client { monitor: monitor.into(), tags: tags.to_vec(), is_visible: true, is_minimized: false, x: 0, y: 0, width: 0, height: 0 }
    }

    #[test]
    fn a_workspace_is_occupied_by_windows_on_its_own_monitor() {
        let list = [client(&[2], "DP-1"), client(&[3], "DP-2")];
        assert!(occupied(&list, "2", "DP-1"));
        assert!(!occupied(&list, "3", "DP-1"));
        assert!(!occupied(&list, "1", "DP-1"));
        assert!(!occupied(&list, "web", "DP-1"));
    }

    #[test]
    fn mango_client_lists_parse() {
        let line = r#"{"clients":[{"id":1,"appid":"x","monitor":"DP-2","tags":[1],"is_visible":true,"is_minimized":false,"x":1}]}"#;
        let list: ClientList = serde_json::from_str(line).unwrap();
        let mut want = client(&[1], "DP-2");
        want.x = 1;
        assert_eq!(list.clients, vec![want]);
    }


    fn win(x: i32, y: i32, width: i32, height: i32) -> Client {
        Client { monitor: "DP-2".into(), tags: vec![1], is_visible: true, is_minimized: false, x, y, width, height }
    }

    #[test]
    fn tiled_windows_with_gaps_and_a_bar_cover_the_screen() {
        let m = Monitor { name: "DP-2".into(), active: true, x: 0, y: 0, width: 2560, height: 1440 };
        // Two tiles under a 40 px bar, 10 px gaps.
        let covered = |m: &Monitor, c: &[Client]| super::super::cover::all_covered(&screens(std::slice::from_ref(m), c));
        assert!(covered(&m, &[win(10, 50, 1265, 1380), win(1285, 50, 1265, 1380)]));
        // One half-screen window leaves the other half open.
        assert!(!covered(&m, &[win(641, 1, 1277, 1438)]));
        // A minimised window covers nothing.
        let mut hidden = win(0, 0, 2560, 1440);
        hidden.is_minimized = true;
        assert!(!covered(&m, &[hidden]));
        // Another monitor's window does not count.
        let mut elsewhere = win(0, 0, 2560, 1440);
        elsewhere.monitor = "HDMI-A-1".into();
        assert!(!covered(&m, &[elsewhere]));
    }
}
