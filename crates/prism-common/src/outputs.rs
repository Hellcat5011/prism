//! Monitor configuration through wlr-output-management (replaces wlr-randr): list outputs with
//! their modes, and apply Prism's saved per-output settings (`Config::outputs`). Each call opens
//! its own short Wayland connection.

use serde::{Deserialize, Serialize};
use wayland_client::protocol::wl_registry;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1 as cfg_head, zwlr_output_configuration_v1 as configuration, zwlr_output_head_v1 as head,
    zwlr_output_manager_v1 as manager, zwlr_output_mode_v1 as mode,
};

/// Saved settings for one output (by connector name). Unset fields keep the current value.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct OutputSettings {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// "WIDTHxHEIGHT@MILLIHERTZ", e.g. "2560x1440@143999"; empty keeps the current mode.
    #[serde(default)]
    pub mode: String,
    /// 0 keeps the current scale.
    #[serde(default)]
    pub scale: f64,
    /// normal, 90, 180, 270, flipped, flipped-90, flipped-180, flipped-270; empty keeps it.
    #[serde(default)]
    pub transform: String,
    #[serde(default)]
    pub position: Option<(i32, i32)>,
    #[serde(default)]
    pub adaptive_sync: Option<bool>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    /// mHz.
    pub refresh: i32,
    pub preferred: bool,
}

impl Mode {
    pub fn key(&self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.refresh)
    }

    pub fn label(&self) -> String {
        format!("{} \u{00D7} {} @ {:.0} Hz", self.width, self.height, self.refresh as f64 / 1000.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub modes: Vec<Mode>,
    pub current: Option<usize>,
    pub position: (i32, i32),
    pub transform: String,
    pub scale: f64,
    /// None when the compositor does not report adaptive sync (protocol v4).
    pub adaptive_sync: Option<bool>,
}

impl OutputInfo {
    pub fn current_mode(&self) -> Option<&Mode> {
        self.current.and_then(|i| self.modes.get(i))
    }
}

pub const TRANSFORMS: [&str; 8] = ["normal", "90", "180", "270", "flipped", "flipped-90", "flipped-180", "flipped-270"];

fn transform_name(t: i32) -> String {
    TRANSFORMS.get(t as usize).copied().unwrap_or("normal").to_string()
}

fn transform_value(name: &str) -> Option<wayland_client::protocol::wl_output::Transform> {
    use wayland_client::protocol::wl_output::Transform as T;
    Some(match name {
        "normal" => T::Normal,
        "90" => T::_90,
        "180" => T::_180,
        "270" => T::_270,
        "flipped" => T::Flipped,
        "flipped-90" => T::Flipped90,
        "flipped-180" => T::Flipped180,
        "flipped-270" => T::Flipped270,
        _ => return None,
    })
}

struct Head {
    obj: head::ZwlrOutputHeadV1,
    info: OutputInfo,
    modes: Vec<mode::ZwlrOutputModeV1>,
    current: Option<mode::ZwlrOutputModeV1>,
}

#[derive(Default)]
struct St {
    manager: Option<manager::ZwlrOutputManagerV1>,
    heads: Vec<Head>,
    serial: Option<u32>,
    result: Option<Result<(), String>>,
}

impl St {
    fn head(&mut self, h: &head::ZwlrOutputHeadV1) -> Option<&mut Head> {
        self.heads.iter_mut().find(|x| &x.obj == h)
    }
}

fn connect() -> Result<(Connection, wayland_client::EventQueue<St>, St), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("Wayland: {e}"))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut st = St::default();
    queue.roundtrip(&mut st).map_err(|e| e.to_string())?;
    if st.manager.is_none() {
        return Err("the compositor does not support wlr-output-management".into());
    }
    // Heads, modes and the done event.
    for _ in 0..4 {
        if st.serial.is_some() {
            break;
        }
        queue.roundtrip(&mut st).map_err(|e| e.to_string())?;
    }
    for h in st.heads.iter_mut() {
        h.info.current = h.current.as_ref().and_then(|c| h.modes.iter().position(|m| m == c));
    }
    Ok((conn, queue, st))
}

/// The outputs as the compositor reports them now.
pub fn list() -> Result<Vec<OutputInfo>, String> {
    let (_conn, _queue, st) = connect()?;
    Ok(st.heads.into_iter().map(|h| h.info).collect())
}

fn parse_mode(s: &str) -> Option<(i32, i32, i32)> {
    let (wh, r) = s.split_once('@')?;
    let (w, h) = wh.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?, r.parse().ok()?))
}

/// Apply `settings` (outputs not listed keep their state). The configuration is tested first;
/// nothing changes when the compositor rejects it.
pub fn apply(settings: &[OutputSettings]) -> Result<(), String> {
    let (_conn, mut queue, mut st) = connect()?;
    let serial = st.serial.ok_or("no output state from the compositor")?;
    let manager = st.manager.clone().unwrap();
    let qh = queue.handle();
    let build = |st: &St, qh: &QueueHandle<St>| -> configuration::ZwlrOutputConfigurationV1 {
        let c = manager.create_configuration(serial, qh, ());
        for h in &st.heads {
            let want = settings.iter().find(|s| s.name == h.info.name);
            let enabled = want.map(|w| w.enabled).unwrap_or(h.info.enabled);
            if !enabled {
                c.disable_head(&h.obj);
                continue;
            }
            let ch = c.enable_head(&h.obj, qh, ());
            let mode_obj = want
                .and_then(|w| parse_mode(&w.mode))
                .and_then(|(w, hh, r)| h.info.modes.iter().position(|m| m.width == w && m.height == hh && m.refresh == r))
                .or(h.info.current)
                .and_then(|i| h.modes.get(i));
            if let Some(m) = mode_obj {
                ch.set_mode(m);
            }
            let (x, y) = want.and_then(|w| w.position).unwrap_or(h.info.position);
            ch.set_position(x, y);
            if let Some(t) = want.and_then(|w| transform_value(&w.transform)).or_else(|| transform_value(&h.info.transform)) {
                ch.set_transform(t);
            }
            let scale = want.map(|w| w.scale).filter(|s| *s > 0.0).unwrap_or(h.info.scale);
            if scale > 0.0 {
                ch.set_scale(scale);
            }
            if ch.version() >= 4 {
                if let Some(on) = want.and_then(|w| w.adaptive_sync) {
                    ch.set_adaptive_sync(if on { head::AdaptiveSyncState::Enabled } else { head::AdaptiveSyncState::Disabled });
                }
            }
        }
        c
    };
    for test in [true, false] {
        let c = build(&st, &qh);
        if test {
            c.test();
        } else {
            c.apply();
        }
        st.result = None;
        while st.result.is_none() {
            queue.blocking_dispatch(&mut st).map_err(|e| e.to_string())?;
        }
        c.destroy();
        if let Some(Err(e)) = st.result.take() {
            return Err(if test { format!("the compositor rejected the configuration ({e})") } else { e });
        }
    }
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for St {
    fn event(st: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            if interface == "zwlr_output_manager_v1" {
                st.manager = Some(reg.bind(name, version.min(4), qh, ()));
            }
        }
    }
}

impl Dispatch<manager::ZwlrOutputManagerV1, ()> for St {
    fn event(st: &mut Self, _: &manager::ZwlrOutputManagerV1, ev: manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            manager::Event::Head { head } => st.heads.push(Head { obj: head, info: OutputInfo { enabled: true, scale: 1.0, ..Default::default() }, modes: Vec::new(), current: None }),
            manager::Event::Done { serial } => st.serial = Some(serial),
            _ => {}
        }
    }
    event_created_child!(St, manager::ZwlrOutputManagerV1, [manager::EVT_HEAD_OPCODE => (head::ZwlrOutputHeadV1, ())]);
}

impl Dispatch<head::ZwlrOutputHeadV1, ()> for St {
    fn event(st: &mut Self, h: &head::ZwlrOutputHeadV1, ev: head::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(x) = st.head(h) else { return };
        match ev {
            head::Event::Name { name } => x.info.name = name,
            head::Event::Description { description } => x.info.description = description,
            head::Event::Mode { mode } => {
                x.modes.push(mode);
                x.info.modes.push(Mode::default());
            }
            head::Event::Enabled { enabled } => x.info.enabled = enabled != 0,
            head::Event::CurrentMode { mode } => x.current = Some(mode),
            head::Event::Position { x: px, y: py } => x.info.position = (px, py),
            head::Event::Transform { transform } => {
                x.info.transform = transform_name(match transform {
                    WEnum::Value(t) => t as i32,
                    WEnum::Unknown(v) => v as i32,
                })
            }
            head::Event::Scale { scale } => x.info.scale = scale,
            head::Event::AdaptiveSync { state } => x.info.adaptive_sync = Some(matches!(state, WEnum::Value(head::AdaptiveSyncState::Enabled))),
            _ => {}
        }
    }
    event_created_child!(St, head::ZwlrOutputHeadV1, [head::EVT_MODE_OPCODE => (mode::ZwlrOutputModeV1, ())]);
}

impl Dispatch<mode::ZwlrOutputModeV1, ()> for St {
    fn event(st: &mut Self, m: &mode::ZwlrOutputModeV1, ev: mode::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        for h in st.heads.iter_mut() {
            if let Some(i) = h.modes.iter().position(|x| x == m) {
                let info = &mut h.info.modes[i];
                match ev {
                    mode::Event::Size { width, height } => {
                        info.width = width;
                        info.height = height;
                    }
                    mode::Event::Refresh { refresh } => info.refresh = refresh,
                    mode::Event::Preferred => info.preferred = true,
                    _ => {}
                }
                return;
            }
        }
    }
}

impl Dispatch<configuration::ZwlrOutputConfigurationV1, ()> for St {
    fn event(st: &mut Self, _: &configuration::ZwlrOutputConfigurationV1, ev: configuration::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        st.result = Some(match ev {
            configuration::Event::Succeeded => Ok(()),
            configuration::Event::Failed => Err("failed".into()),
            configuration::Event::Cancelled => Err("cancelled (outputs changed meanwhile)".into()),
            _ => return,
        });
    }
}

impl Dispatch<cfg_head::ZwlrOutputConfigurationHeadV1, ()> for St {
    fn event(_: &mut Self, _: &cfg_head::ZwlrOutputConfigurationHeadV1, _: cfg_head::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
