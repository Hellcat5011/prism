// ─────────────────────────────────────────────────────────────────────────
// desktop/cover.rs — do windows cover the desktop? (Pause Video: "covered")
//
// Needs window rectangles, which only the compositors' own IPC gives:
// Mango through the `mmsg watch all-clients` stream (windows.rs), Hyprland
// through `hyprctl -j clients` / `hyprctl -j monitors`, re-read whenever its
// event socket (.socket2.sock) reports a change, and niri through
// `niri msg -j event-stream` (window layouts, workspaces) with the output
// sizes from `niri msg -j outputs`. Elsewhere the desktop never counts as covered.
// ─────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::Duration;

use prism_common::alt_tab::Compositor;
use serde::Deserialize;

use crate::waker::Waker;

/// (x, y, width, height) in the compositor's logical pixels.
pub type Rect = (f64, f64, f64, f64);

/// One screen and the windows showing on it, in the same coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub rect: Rect,
    pub windows: Vec<Rect>,
}

/// Every screen is covered by its windows: the rectangles, each grown by 4% of the screen's
/// shorter side (the gaps between tiled windows and a bar along an edge do not count as
/// uncovered), leave no part of it open.
pub fn all_covered(screens: &[Screen]) -> bool {
    !screens.is_empty() && screens.iter().all(covered)
}

pub fn covered(s: &Screen) -> bool {
    let (sx, sy, sw, sh) = s.rect;
    if s.windows.is_empty() || sw <= 0.0 || sh <= 0.0 {
        return false;
    }
    let grow = sw.min(sh) * 0.04;
    let rects: Vec<(f64, f64, f64, f64)> = s.windows.iter().map(|r| (r.0 - grow, r.1 - grow, r.0 + r.2 + grow, r.1 + r.3 + grow)).collect();
    // Sample the screen every 8 pixels (cell centres): every sample must lie in some window.
    const STEP: f64 = 8.0;
    let (cols, rows) = ((sw / STEP).ceil() as usize, (sh / STEP).ceil() as usize);
    (0..rows).all(|j| {
        let y = sy + (j as f64 + 0.5) * STEP;
        (0..cols).all(|i| {
            let x = sx + (i as f64 + 0.5) * STEP;
            rects.iter().any(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3)
        })
    })
}

/// The screens as Hyprland or niri last described them (None: not known).
static SCREENS: Mutex<Option<Vec<Screen>>> = Mutex::new(None);
static START: Once = Once::new();

/// Windows cover every screen, by Hyprland's or niri's account (see the module comment).
pub fn desktop_covered(waker: &Waker) -> bool {
    let waker = waker.clone();
    START.call_once(move || match Compositor::current() {
        Some(Compositor::Hyprland) => {
            std::thread::Builder::new().name("prism-cover".into()).spawn(move || hyprland(waker)).ok();
        }
        Some(Compositor::Niri) => {
            std::thread::Builder::new().name("prism-cover".into()).spawn(move || niri(waker)).ok();
        }
        _ => {}
    });
    // Worked out once per change of the screens.
    static LAST: Mutex<Option<(Vec<Screen>, bool)>> = Mutex::new(None);
    let Some(screens) = SCREENS.lock().unwrap().clone() else { return false };
    let mut last = LAST.lock().unwrap();
    if let Some((s, v)) = last.as_ref() {
        if *s == screens {
            return *v;
        }
    }
    let v = all_covered(&screens);
    *last = Some((screens, v));
    v
}

fn publish(screens: Option<Vec<Screen>>, waker: &Waker) {
    let mut st = SCREENS.lock().unwrap();
    if *st != screens {
        *st = screens;
        drop(st);
        waker.wake();
    }
}

// ── Hyprland ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HyprWorkspaceRef {
    #[serde(default)]
    id: i64,
}

#[derive(Deserialize)]
struct HyprClient {
    #[serde(default = "yes")]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
    /// Logical layout position and size.
    at: (f64, f64),
    size: (f64, f64),
    workspace: HyprWorkspaceRef,
    /// The monitor's id (a number).
    #[serde(default)]
    monitor: i64,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct HyprMonitor {
    id: i64,
    /// Position in logical pixels; size in buffer pixels (divide by `scale`).
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    #[serde(default = "one")]
    scale: f64,
    /// wl_output transform: odd values turn the screen a quarter.
    #[serde(default)]
    transform: i64,
    #[serde(rename = "activeWorkspace")]
    active_workspace: HyprWorkspaceRef,
    #[serde(rename = "specialWorkspace")]
    special_workspace: Option<HyprWorkspaceRef>,
    #[serde(default)]
    disabled: bool,
}

fn one() -> f64 {
    1.0
}

/// HYPRLAND_INSTANCE_SIGNATURE: from the environment, else the newest instance directory (prismd
/// runs as a systemd user unit, which may not have it).
fn hypr_instance() -> Option<String> {
    if let Ok(s) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        return Some(s);
    }
    let dir = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").ok()?).join("hypr");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().join(".socket2.sock").exists())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
}

fn hyprctl<T: for<'de> Deserialize<'de>>(instance: &str, what: &str) -> Option<T> {
    let out = Command::new("hyprctl").args(["-j", what]).env("HYPRLAND_INSTANCE_SIGNATURE", instance).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Screens from `hyprctl -j monitors` and `hyprctl -j clients`.
fn hypr_screens(monitors: &[HyprMonitor], clients: &[HyprClient]) -> Vec<Screen> {
    monitors
        .iter()
        .filter(|m| !m.disabled && m.width > 0.0 && m.height > 0.0)
        .map(|m| {
            let s = if m.scale > 0.0 { m.scale } else { 1.0 };
            let (w, h) = if m.transform % 2 == 1 { (m.height / s, m.width / s) } else { (m.width / s, m.height / s) };
            // A special workspace opened over the monitor shows its windows on top.
            let shown: Vec<i64> = std::iter::once(m.active_workspace.id).chain(m.special_workspace.as_ref().map(|w| w.id).filter(|id| *id != 0)).collect();
            let windows = clients
                .iter()
                .filter(|c| c.mapped && !c.hidden && c.monitor == m.id && shown.contains(&c.workspace.id) && c.size.0 > 0.0 && c.size.1 > 0.0)
                .map(|c| (c.at.0, c.at.1, c.size.0, c.size.1))
                .collect();
            Screen { rect: (m.x, m.y, w, h), windows }
        })
        .collect()
}

fn hyprland(waker: Waker) {
    loop {
        let Some(instance) = hypr_instance() else {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        let read = |waker: &Waker| {
            let screens = hyprctl::<Vec<HyprMonitor>>(&instance, "monitors").zip(hyprctl::<Vec<HyprClient>>(&instance, "clients")).map(|(m, c)| hypr_screens(&m, &c));
            publish(screens, waker);
        };
        read(&waker);
        let path = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_default()).join("hypr").join(&instance).join(".socket2.sock");
        let Ok(sock) = std::os::unix::net::UnixStream::connect(&path) else {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        // Resizing a tiled window sends no event: read again now and then as well.
        let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
        let mut lines = BufReader::new(sock);
        let mut line = String::new();
        loop {
            line.clear();
            match lines.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let event = line.split(">>").next().unwrap_or("");
                    // Titles and focus change often and move nothing.
                    if !matches!(event, "windowtitle" | "windowtitlev2" | "activewindow" | "activewindowv2" | "activelayout" | "submap" | "urgent" | "bell") {
                        read(&waker);
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => read(&waker),
                Err(_) => break,
            }
        }
        publish(None, &waker);
        std::thread::sleep(Duration::from_secs(2));
    }
}

// ── niri ──────────────────────────────────────────────────────────────────

#[derive(Clone, Deserialize)]
struct NiriLayout {
    tile_size: (f64, f64),
    /// Tile position within the current view of its workspace (output-local logical pixels).
    #[serde(default)]
    tile_pos_in_workspace_view: Option<(f64, f64)>,
}

#[derive(Clone, Deserialize)]
struct NiriWindow {
    id: u64,
    #[serde(default)]
    workspace_id: Option<u64>,
    layout: NiriLayout,
}

#[derive(Clone, Deserialize)]
struct NiriWorkspace {
    id: u64,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    is_active: bool,
}

#[derive(Deserialize)]
struct NiriLogical {
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
struct NiriOutput {
    #[serde(default)]
    logical: Option<NiriLogical>,
}

#[derive(Deserialize)]
enum NiriEvent {
    WorkspacesChanged { workspaces: Vec<NiriWorkspace> },
    WorkspaceActivated { id: u64 },
    WindowsChanged { windows: Vec<NiriWindow> },
    WindowOpenedOrChanged { window: NiriWindow },
    WindowClosed { id: u64 },
    WindowLayoutsChanged { changes: Vec<(u64, NiriLayout)> },
    OverviewOpenedOrClosed { is_open: bool },
}

#[derive(Default)]
struct Niri {
    outputs: HashMap<String, (f64, f64)>,
    workspaces: Vec<NiriWorkspace>,
    windows: HashMap<u64, NiriWindow>,
    overview: bool,
}

impl Niri {
    fn apply(&mut self, ev: NiriEvent) {
        match ev {
            NiriEvent::WorkspacesChanged { workspaces } => self.workspaces = workspaces,
            NiriEvent::WorkspaceActivated { id, .. } => {
                let output = self.workspaces.iter().find(|w| w.id == id).and_then(|w| w.output.clone());
                for w in self.workspaces.iter_mut().filter(|w| w.output == output) {
                    w.is_active = w.id == id;
                }
            }
            NiriEvent::WindowsChanged { windows } => self.windows = windows.into_iter().map(|w| (w.id, w)).collect(),
            NiriEvent::WindowOpenedOrChanged { window } => {
                self.windows.insert(window.id, window);
            }
            NiriEvent::WindowClosed { id } => {
                self.windows.remove(&id);
            }
            NiriEvent::WindowLayoutsChanged { changes } => {
                for (id, layout) in changes {
                    if let Some(w) = self.windows.get_mut(&id) {
                        w.layout = layout;
                    }
                }
            }
            NiriEvent::OverviewOpenedOrClosed { is_open } => self.overview = is_open,
        }
    }

    /// Each output's active workspace and the tiles in view on it (output-local coordinates).
    fn screens(&self) -> Vec<Screen> {
        self.outputs
            .iter()
            .map(|(name, &(w, h))| {
                let active: Vec<u64> = self.workspaces.iter().filter(|ws| ws.is_active && ws.output.as_deref() == Some(name)).map(|ws| ws.id).collect();
                let windows = if self.overview {
                    Vec::new()
                } else {
                    self.windows
                        .values()
                        .filter(|win| win.workspace_id.is_some_and(|id| active.contains(&id)))
                        .filter_map(|win| win.layout.tile_pos_in_workspace_view.map(|p| (p.0, p.1, win.layout.tile_size.0, win.layout.tile_size.1)))
                        .collect()
                };
                Screen { rect: (0.0, 0.0, w, h), windows }
            })
            .collect()
    }
}

fn niri_outputs() -> HashMap<String, (f64, f64)> {
    let out = Command::new("niri").args(["msg", "-j", "outputs"]).stdin(Stdio::null()).stderr(Stdio::null()).output();
    let outputs: HashMap<String, NiriOutput> = out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default();
    outputs.into_iter().filter_map(|(name, o)| o.logical.map(|l| (name, (l.width, l.height)))).filter(|(_, (w, h))| *w > 0.0 && *h > 0.0).collect()
}

fn niri(waker: Waker) {
    loop {
        let mut cmd = Command::new("niri");
        cmd.args(["msg", "-j", "event-stream"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        // The stream must end with this process, also when it is killed.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        if let Ok(mut child) = cmd.spawn() {
            let mut st = Niri { outputs: niri_outputs(), ..Default::default() };
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    // Events this does not follow (keyboard layouts, casts, ...) do not parse.
                    let Ok(ev) = serde_json::from_str::<NiriEvent>(&line) else { continue };
                    // A workspace on an output not seen yet: a screen was plugged in.
                    if let NiriEvent::WorkspacesChanged { workspaces } = &ev {
                        if workspaces.iter().any(|w| w.output.as_ref().is_some_and(|o| !st.outputs.contains_key(o))) {
                            st.outputs = niri_outputs();
                        }
                    }
                    st.apply(ev);
                    publish(Some(st.screens()), &waker);
                }
            }
            let _ = child.wait();
        }
        publish(None, &waker);
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_with_gaps_cover_and_half_a_screen_does_not() {
        let screen = |windows| Screen { rect: (0.0, 0.0, 2560.0, 1440.0), windows };
        assert!(covered(&screen(vec![(10.0, 50.0, 1265.0, 1380.0), (1285.0, 50.0, 1265.0, 1380.0)])));
        assert!(!covered(&screen(vec![(641.0, 1.0, 1277.0, 1438.0)])));
        assert!(!covered(&screen(Vec::new())));
    }

    #[test]
    fn hyprland_clients_on_the_shown_workspace_count() {
        let monitors: Vec<HyprMonitor> = serde_json::from_str(
            r#"[{"id":0,"name":"DP-1","x":0,"y":0,"width":3840,"height":2160,"scale":1.5,"transform":0,
                 "activeWorkspace":{"id":1,"name":"1"},"specialWorkspace":{"id":0,"name":""},"disabled":false}]"#,
        )
        .unwrap();
        let clients: Vec<HyprClient> = serde_json::from_str(
            r#"[{"address":"0x1","mapped":true,"hidden":false,"at":[5,45],"size":[2550,1390],"workspace":{"id":1,"name":"1"},"monitor":0},
                {"address":"0x2","mapped":true,"hidden":false,"at":[0,0],"size":[100,100],"workspace":{"id":2,"name":"2"},"monitor":0}]"#,
        )
        .unwrap();
        let screens = hypr_screens(&monitors, &clients);
        assert_eq!(screens[0].rect, (0.0, 0.0, 2560.0, 1440.0));
        assert_eq!(screens[0].windows, vec![(5.0, 45.0, 2550.0, 1390.0)]);
        assert!(all_covered(&screens));
    }

    #[test]
    fn niri_follows_the_event_stream() {
        let mut st = Niri { outputs: [("DP-1".to_string(), (1920.0, 1080.0))].into(), ..Default::default() };
        for line in [
            r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"name":null,"output":"DP-1","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null},{"id":2,"idx":2,"name":null,"output":"DP-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null}]}}"#,
            r#"{"WindowsChanged":{"windows":[{"id":7,"title":"t","app_id":"a","pid":1,"workspace_id":1,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1],"tile_size":[1900.0,1060.0],"window_size":[1896,1056],"tile_pos_in_workspace_view":[10.0,10.0],"window_offset_in_tile":[2.0,2.0]},"focus_timestamp":null}]}}"#,
        ] {
            st.apply(serde_json::from_str(line).unwrap());
        }
        assert!(all_covered(&st.screens()));
        // Another workspace comes into view: nothing on it.
        st.apply(serde_json::from_str(r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#).unwrap());
        assert!(!all_covered(&st.screens()));
        st.apply(serde_json::from_str(r#"{"WorkspaceActivated":{"id":1,"focused":true}}"#).unwrap());
        // Scrolled half out of view.
        st.apply(serde_json::from_str(r#"{"WindowLayoutsChanged":{"changes":[[7,{"pos_in_scrolling_layout":[1,1],"tile_size":[1900.0,1060.0],"window_size":[1896,1056],"tile_pos_in_workspace_view":[960.0,10.0],"window_offset_in_tile":[2.0,2.0]}]]}}"#).unwrap());
        assert!(!all_covered(&st.screens()));
        // Unknown events are skipped by the caller; the overview shows the wallpaper.
        assert!(serde_json::from_str::<NiriEvent>(r#"{"KeyboardLayoutSwitched":{"idx":0}}"#).is_err());
    }
}
