// ─────────────────────────────────────────────────────────────────────────
// desktop/panel.rs — the surfaces of the bar and the dock
//
// Both are Top-layer surfaces of the desktop component, so they share its
// tray, media and pointer. The dock slides by its layer margin. While it can
// hide, a clear strip along the screen edge brings it back on hover.
// ─────────────────────────────────────────────────────────────────────────

use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_buffer, wl_callback, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape as CursorShape;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::cairo_backend::*;

use super::dbus::{DbusCmd, MenuEntry};
use super::layout::Rect;
use super::widgets::LocalTime;
use super::{bar, dock, sysmon, windows, Desk, PointerOn, ShmBuf};

/// A frame callback that has not arrived by now belongs to a surface that went off screen.
const FRAME_TIMEOUT: Duration = Duration::from_millis(300);
/// Thickness of the strip that brings a hidden dock back, and its shortest length.
const STRIP: f64 = 4.0;
const STRIP_MIN: f64 = 320.0;
/// Wheel movement per workspace.
const WHEEL_STEP: f64 = 15.0;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

const MENU_NEW_WINDOW: i32 = 1;
const MENU_PIN: i32 = 2;
const MENU_CLOSE: i32 = 3;
/// Key prefix of the dock's context menus (the tray's use their item key).
const DOCK_MENU: &str = "dock:";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelId {
    Bar,
    Dock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelHit {
    Workspace(usize),
    Launcher,
    Notifications,
    Power,
    Tray(usize),
    MediaPrev,
    MediaPlay,
    MediaNext,
    Updates,
    DockItem(usize),
}

/// How a panel sits on the screen.
#[derive(Clone, Debug, PartialEq)]
struct Spec {
    namespace: String,
    anchor: Anchor,
    size: (u32, u32),
    /// Top, right, bottom, left.
    margin: (i32, i32, i32, i32),
    exclusive: i32,
    /// On the screen, for placing menus and tooltips.
    rect: Rect,
}

pub struct Panel {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    spec: Spec,
    size: (i32, i32),
    configured: bool,
    bufs: Vec<ShmBuf>,
    frame_cb: Option<wl_callback::WlCallback>,
    frame_since: Instant,
    dirty: bool,
    animating: bool,
    hits: Vec<(PanelHit, Rect)>,
}

impl Panel {
    pub fn owns(&self, surface: &wl_surface::WlSurface) -> bool {
        &self.surface == surface
    }

    fn destroy(self) {
        self.layer.destroy();
        self.surface.destroy();
    }
}

/// The clear strip along the screen edge.
pub struct Strip {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    buffer: Option<wl_buffer::WlBuffer>,
    position: String,
    length: u32,
}

impl Strip {
    pub fn owns(&self, surface: &wl_surface::WlSurface) -> bool {
        &self.surface == surface
    }

    fn destroy(self) {
        if let Some(v) = self.viewport {
            v.destroy();
        }
        self.layer.destroy();
        self.surface.destroy();
        if let Some(b) = self.buffer {
            b.destroy();
        }
    }
}

#[derive(Clone, Copy)]
pub struct StripTag;

/// Panels' share of the desktop's state.
#[derive(Default)]
pub struct Panels {
    pub bar: Option<Panel>,
    pub dock: Option<Panel>,
    pub strip: Option<Strip>,
    pub bar_state: bar::BarState,
    pub dock_state: dock::DockState,
    pub hover: Option<PanelHit>,
    clients_seen: u64,
    catalog_seen: u64,
    stats_seen: u64,
}

impl Desk {
    fn panel(&self, id: PanelId) -> Option<&Panel> {
        match id {
            PanelId::Bar => self.panels.bar.as_ref(),
            PanelId::Dock => self.panels.dock.as_ref(),
        }
    }

    fn panel_mut(&mut self, id: PanelId) -> Option<&mut Panel> {
        match id {
            PanelId::Bar => self.panels.bar.as_mut(),
            PanelId::Dock => self.panels.dock.as_mut(),
        }
    }

    // ── Where the panels go ───────────────────────────────────────────

    fn bar_spec(&self) -> Option<Spec> {
        let c = &self.config.bar;
        if !c.enabled {
            return None;
        }
        let t = c.thickness.clamp(24, 96);
        let gap = if c.style == "full" { 0 } else { c.gap as i32 };
        let (sw, sh) = self.screen;
        let (g, tf) = (gap as f64, t as f64);
        let (anchor, size, margin, rect) = match c.position.as_str() {
            "bottom" => (Anchor::Bottom | Anchor::Left | Anchor::Right, (0, t), (0, gap, gap, gap), Rect { x: g, y: sh - g - tf, w: sw - 2.0 * g, h: tf }),
            "left" => (Anchor::Left | Anchor::Top | Anchor::Bottom, (t, 0), (gap, 0, gap, gap), Rect { x: g, y: g, w: tf, h: sh - 2.0 * g }),
            "right" => (Anchor::Right | Anchor::Top | Anchor::Bottom, (t, 0), (gap, gap, gap, 0), Rect { x: sw - g - tf, y: g, w: tf, h: sh - 2.0 * g }),
            _ => (Anchor::Top | Anchor::Left | Anchor::Right, (0, t), (gap, gap, 0, gap), Rect { x: g, y: g, w: sw - 2.0 * g, h: tf }),
        };
        // Islands leave gaps between their pills; Mango would blur the whole surface.
        let namespace = if c.style == "islands" { "prism-bar-solid".to_string() } else { self.style.namespace("prism-bar") };
        Some(Spec { namespace, anchor, size, margin, exclusive: if c.reserve_space { t as i32 } else { 0 }, rect })
    }

    fn dock_spec(&self) -> Option<Spec> {
        let c = &self.config.dock;
        let items = self.panels.dock_state.items.len();
        if !c.enabled || items == 0 {
            return None;
        }
        let (sw, sh) = self.screen;
        let thick = dock::thickness(c);
        let gap = c.gap as i32;
        let g = gap as f64;
        let vertical = dock::is_vertical(c);
        let len = dock::length(c, items, (if vertical { sh } else { sw }) - 2.0 * g);
        let (anchor, size, margin, rect) = match c.position.as_str() {
            "left" => (Anchor::Left, (thick as u32, len as u32), (0, 0, 0, gap), Rect { x: g, y: (sh - len) / 2.0, w: thick, h: len }),
            "right" => (Anchor::Right, (thick as u32, len as u32), (0, gap, 0, 0), Rect { x: sw - g - thick, y: (sh - len) / 2.0, w: thick, h: len }),
            _ => (Anchor::Bottom, (len as u32, thick as u32), (0, 0, gap, 0), Rect { x: (sw - len) / 2.0, y: sh - g - thick, w: len, h: thick }),
        };
        let reserve = c.autohide == "off" && !c.windows_behind;
        Some(Spec { namespace: self.style.namespace("prism-dock"), anchor, size, margin, exclusive: if reserve { thick as i32 } else { 0 }, rect })
    }

    /// Create, move or remove the panels to match the config.
    pub(super) fn sync_panels(&mut self, qh: &QueueHandle<Self>) {
        let (bar_on, dock_on) = (self.config.bar.enabled, self.config.dock.enabled);
        if bar_on || dock_on {
            windows::watch_clients(self.waker.clone());
        }
        if dock_on {
            dock::start(self.waker.clone());
        }
        let wants_stats = bar_on && self.config.bar.section_of("sysmon").is_some();
        if wants_stats {
            sysmon::start(self.waker.clone());
        }
        sysmon::set_wanted(wants_stats);
        let wants_updates = bar_on && self.config.bar.section_of("updates").is_some();
        let c = &self.config;
        super::updates::configure(wants_updates || c.update_notifications || c.updates_tray, c.update_notifications, c.bar.updates_interval_hours, &self.waker);
        self.panels.dock_state.refresh(&self.windows.toplevels);
        let (bar, dock) = (self.bar_spec(), self.dock_spec());
        self.sync_panel(PanelId::Bar, bar, qh);
        self.sync_panel(PanelId::Dock, dock, qh);
        self.sync_strip(qh);
    }

    fn sync_panel(&mut self, id: PanelId, spec: Option<Spec>, qh: &QueueHandle<Self>) {
        let slot = match id {
            PanelId::Bar => &mut self.panels.bar,
            PanelId::Dock => &mut self.panels.dock,
        };
        let Some(spec) = spec else {
            if let Some(p) = slot.take() {
                p.destroy();
            }
            return;
        };
        // A panel cannot change its namespace or edge: those get a new surface.
        if slot.as_ref().is_some_and(|p| p.spec.namespace != spec.namespace || p.spec.anchor != spec.anchor) {
            if let Some(p) = slot.take() {
                p.destroy();
            }
        }
        if let Some(p) = slot.as_mut() {
            if p.spec != spec {
                p.layer.set_size(spec.size.0, spec.size.1);
                p.layer.set_exclusive_zone(spec.exclusive);
                p.layer.set_margin(spec.margin.0, spec.margin.1, spec.margin.2, spec.margin.3);
                p.spec = spec;
                p.surface.commit();
            }
        } else {
            let (Some(comp), Some(ls)) = (self.compositor.clone(), self.layer_shell.clone()) else { return };
            let surface = comp.create_surface(qh, ());
            let layer = ls.get_layer_surface(&surface, self.output.as_ref(), Layer::Top, spec.namespace.clone(), qh, id);
            layer.set_anchor(spec.anchor);
            layer.set_size(spec.size.0, spec.size.1);
            layer.set_margin(spec.margin.0, spec.margin.1, spec.margin.2, spec.margin.3);
            layer.set_exclusive_zone(spec.exclusive);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            *slot = Some(Panel { surface, layer, spec, size: (0, 0), configured: false, bufs: Vec::new(), frame_cb: None, frame_since: Instant::now(), dirty: true, animating: false, hits: Vec::new() });
            if id == PanelId::Dock {
                self.settle_dock();
            }
        }
        self.mark_panel(id, qh);
    }

    /// A new dock starts where it belongs, without sliding there.
    fn settle_dock(&mut self) {
        let on_screen = self.windows.on_screen(&self.output_name);
        let d = &mut self.panels.dock_state;
        d.update_visibility(&self.config.dock, on_screen, false, &self.style);
        let hidden = match self.config.dock.autohide.as_str() {
            "always" => !d.hovered,
            "smart" => !d.hovered && on_screen,
            _ => false,
        };
        d.offset.snap(if hidden { 1.0 } else { 0.0 });
    }

    fn sync_strip(&mut self, qh: &QueueHandle<Self>) {
        let c = &self.config.dock;
        let want = self.panels.dock.as_ref().filter(|_| c.autohide != "off").map(|p| {
            let length = if dock::is_vertical(c) { p.spec.size.1 } else { p.spec.size.0 };
            (c.position.clone(), (length as f64).max(STRIP_MIN) as u32)
        });
        if let Some(s) = &self.panels.strip {
            if want.as_ref().is_none_or(|(pos, len)| *pos != s.position || *len != s.length) {
                if let Some(s) = self.panels.strip.take() {
                    s.destroy();
                }
            }
        }
        let Some((position, length)) = want else { return };
        if self.panels.strip.is_some() {
            return;
        }
        let (Some(comp), Some(ls)) = (self.compositor.clone(), self.layer_shell.clone()) else { return };
        let surface = comp.create_surface(qh, ());
        // Shares the scrim's namespace: never blurred or animated by the compositor.
        let layer = ls.get_layer_surface(&surface, self.output.as_ref(), Layer::Top, "prism-scrim".to_string(), qh, StripTag);
        let thin = STRIP as u32;
        match position.as_str() {
            "left" => {
                layer.set_anchor(Anchor::Left);
                layer.set_size(thin, length);
            }
            "right" => {
                layer.set_anchor(Anchor::Right);
                layer.set_size(thin, length);
            }
            _ => {
                layer.set_anchor(Anchor::Bottom);
                layer.set_size(length, thin);
            }
        }
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();
        self.panels.strip = Some(Strip { surface, layer, viewport: None, buffer: None, position, length });
    }

    // ── Drawing ───────────────────────────────────────────────────────

    pub(super) fn mark_panel(&mut self, id: PanelId, qh: &QueueHandle<Self>) {
        let Some(p) = self.panel_mut(id) else { return };
        p.dirty = true;
        if p.configured && p.frame_cb.is_none() {
            self.draw_panel(id, qh);
        }
    }

    fn draw_panel(&mut self, id: PanelId, qh: &QueueHandle<Self>) {
        let Some(shm) = self.shm.clone() else { return };
        let Some(p) = self.panel_mut(id) else { return };
        let (bw, bh) = p.size;
        if bw <= 0 || bh <= 0 {
            return;
        }
        p.bufs.retain(|b| b.w == bw && b.h == bh);
        let idx = match p.bufs.iter().position(|b| !b.busy) {
            Some(k) => k,
            None if p.bufs.len() < 2 => {
                let Some(b) = ShmBuf::new(&shm, qh, bw, bh, id) else { return };
                p.bufs.push(b);
                p.bufs.len() - 1
            }
            // Both are with the compositor: drawn again when one comes back.
            None => {
                p.dirty = true;
                return;
            }
        };
        let surf = p.bufs[idx].cairo();
        let cr = unsafe { cairo_create(surf) };
        unsafe {
            cairo_set_operator(cr, CAIRO_OPERATOR_CLEAR);
            cairo_paint(cr);
            cairo_set_operator(cr, 2);
        }
        let _ = crate::ui::take_anim_pending();
        let (w, h) = (bw as f64, bh as f64);
        let hits = match id {
            PanelId::Bar => self.paint_bar(cr, w, h),
            PanelId::Dock => {
                let radius = self.style.radius.max(12.0);
                let hover = self.panels.hover;
                dock::draw(cr, w, h, &self.config.dock, &mut self.panels.dock_state, hover, radius, &self.look, &self.style)
            }
        };
        if id == PanelId::Dock {
            self.apply_dock_slide();
        }
        let animating = crate::ui::take_anim_pending();
        unsafe {
            cairo_destroy(cr);
            cairo_surface_destroy(surf);
        }
        let Some(p) = self.panel_mut(id) else { return };
        p.hits = hits;
        p.dirty = false;
        p.animating = animating;
        let buf = &mut p.bufs[idx];
        buf.busy = true;
        p.surface.attach(Some(&buf.buffer), 0, 0);
        p.surface.damage_buffer(0, 0, bw, bh);
        if animating && p.frame_cb.is_none() {
            p.frame_cb = Some(p.surface.frame(qh, id));
            p.frame_since = Instant::now();
        }
        p.surface.commit();
    }

    fn paint_bar(&mut self, cr: *mut Cairo, w: f64, h: f64) -> Vec<(PanelHit, Rect)> {
        let workspaces = self.windows.workspace_views(self.output.as_ref(), &self.output_name);
        let now = LocalTime::now();
        self.panels.bar_state.clock = bar::clock_text(&self.config.bar, &now);
        let view = bar::BarView {
            cfg: &self.config.bar,
            look: &self.look,
            style: &self.style,
            workspaces: &workspaces,
            tray: &self.tray,
            tray_icons: &self.tray_icons,
            media: self.media.as_ref(),
            stats: sysmon::snapshot(),
            updates: super::updates::snapshot(),
            now: &now,
            hover: self.panels.hover,
        };
        bar::draw(cr, w, h, &view, &mut self.panels.bar_state)
    }

    /// Move the dock along its slide.
    fn apply_dock_slide(&mut self) {
        let c = &self.config.dock;
        let Some(p) = &self.panels.dock else { return };
        let hidden = self.panels.dock_state.offset.animated(0.002);
        let travel = dock::thickness(c) + c.gap as f64 + 2.0;
        let edge = c.gap as i32 - (travel * hidden).round() as i32;
        match c.position.as_str() {
            "left" => p.layer.set_margin(0, 0, 0, edge),
            "right" => p.layer.set_margin(0, edge, 0, 0),
            _ => p.layer.set_margin(0, 0, edge, 0),
        }
    }

    // ── Time ──────────────────────────────────────────────────────────

    /// Follow windows, apps, numbers and the clock; returns ms until the next look is due.
    pub(super) fn panels_tick(&mut self, qh: &QueueHandle<Self>) -> Option<i32> {
        let clients = windows::clients_generation();
        let catalog = dock::catalog_generation();
        if std::mem::take(&mut self.windows.changed) || clients != self.panels.clients_seen || catalog != self.panels.catalog_seen {
            self.panels.clients_seen = clients;
            self.panels.catalog_seen = catalog;
            self.sync_panels(qh);
            self.mark_panel(PanelId::Bar, qh);
        }
        let mut next: Option<i32> = None;
        if self.panels.bar.is_some() {
            let stats = sysmon::generation() + (super::updates::generation() << 32);
            let now = LocalTime::now();
            if stats != self.panels.stats_seen || bar::clock_text(&self.config.bar, &now) != self.panels.bar_state.clock {
                self.panels.stats_seen = stats;
                self.mark_panel(PanelId::Bar, qh);
            }
            if bar::clock_ticks(&self.config.bar) {
                next = Some(1000 - now.millis as i32 + 15);
            }
        }
        if self.panels.dock.is_some() {
            let before = self.panels.dock_state.offset.target();
            let on_screen = self.windows.on_screen(&self.output_name);
            let menu_open = self.menu.as_ref().is_some_and(|m| m.key.starts_with(DOCK_MENU));
            let wait = self.panels.dock_state.update_visibility(&self.config.dock, on_screen, menu_open, &self.style);
            if self.panels.dock_state.offset.target() != before {
                self.mark_panel(PanelId::Dock, qh);
            }
            if let Some(w) = wait {
                let ms = w.as_millis() as i32 + 5;
                next = Some(next.map_or(ms, |n| n.min(ms)));
            }
        }
        // A surface that slid away never gets its frame callback.
        for id in [PanelId::Bar, PanelId::Dock] {
            let stuck = self.panel(id).is_some_and(|p| p.frame_cb.is_some() && p.frame_since.elapsed() >= FRAME_TIMEOUT);
            if stuck {
                if let Some(p) = self.panel_mut(id) {
                    p.frame_cb = None;
                }
                self.mark_panel(id, qh);
            }
            if self.panel(id).is_some_and(|p| p.frame_cb.is_some()) {
                let ms = FRAME_TIMEOUT.as_millis() as i32;
                next = Some(next.map_or(ms, |n| n.min(ms)));
            }
        }
        next
    }

    // ── Input ─────────────────────────────────────────────────────────

    /// The pointer came onto a panel or the strip, or left them.
    pub(super) fn panel_pointer(&mut self) {
        let hovered = matches!(self.ptr_on, PointerOn::Panel(PanelId::Dock) | PointerOn::Strip);
        if self.panels.dock_state.hovered != hovered {
            self.panels.dock_state.hovered = hovered;
        }
    }

    pub(super) fn panel_motion(&mut self, id: PanelId, qh: &QueueHandle<Self>) {
        let (x, y) = self.ptr_pos;
        let hit = self.panel(id).and_then(|p| p.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h));
        if hit == self.panels.hover {
            return;
        }
        self.panels.hover = hit;
        self.set_cursor(qh, if hit.is_some() { CursorShape::Pointer } else { CursorShape::Default });
        self.mark_panel(id, qh);
        match hit {
            Some(h) => self.panel_tooltip(id, h, qh),
            None => self.hide_tip(),
        }
    }

    pub(super) fn panel_left(&mut self, id: PanelId, qh: &QueueHandle<Self>) {
        self.panels.hover = None;
        self.mark_panel(id, qh);
    }

    /// `hit`'s rectangle on the screen.
    fn panel_hit_rect(&self, id: PanelId, hit: PanelHit) -> Option<Rect> {
        let p = self.panel(id)?;
        let r = p.hits.iter().find(|(h, _)| *h == hit)?.1;
        Some(Rect { x: p.spec.rect.x + r.x, y: p.spec.rect.y + r.y, w: r.w, h: r.h })
    }

    /// Menus and tooltips open away from the edge a vertical panel is on.
    fn panel_side(&self, id: PanelId) -> Option<bool> {
        let position = match id {
            PanelId::Bar => &self.config.bar.position,
            PanelId::Dock => &self.config.dock.position,
        };
        match position.as_str() {
            "left" => Some(true),
            "right" => Some(false),
            _ => None,
        }
    }

    fn panel_tooltip(&mut self, id: PanelId, hit: PanelHit, qh: &QueueHandle<Self>) {
        let Some(rect) = self.panel_hit_rect(id, hit) else { return };
        let side = self.panel_side(id);
        match hit {
            PanelHit::Tray(k) => self.show_tray_tip(k, rect, side, qh),
            PanelHit::DockItem(i) => {
                let Some(item) = self.panels.dock_state.items.get(i) else { return };
                let (key, name) = (format!("{DOCK_MENU}{}", item.key), item.name.clone());
                self.open_tip(key, (name, String::new()), rect, side, qh);
            }
            _ => self.hide_tip(),
        }
    }

    pub(super) fn panel_click(&mut self, id: PanelId, button: u32, qh: &QueueHandle<Self>) {
        let Some(hit) = self.panels.hover else { return };
        let rect = self.panel_hit_rect(id, hit);
        match hit {
            PanelHit::Workspace(i) if button == BTN_LEFT => self.activate_workspace(i),
            PanelHit::Launcher if button == BTN_LEFT => run_ctl(&["launcher", "toggle"]),
            PanelHit::Notifications if button == BTN_LEFT => run_ctl(&["notif", "toggle"]),
            PanelHit::Power if button == BTN_LEFT => run_ctl(&["power", "toggle"]),
            PanelHit::Updates if button == BTN_LEFT => super::updates::run_update(&self.config.bar.updates_command),
            PanelHit::Updates => super::updates::recheck(),
            PanelHit::MediaPrev if button == BTN_LEFT => self.media_cmd("Previous"),
            PanelHit::MediaPlay if button == BTN_LEFT => self.media_cmd("PlayPause"),
            PanelHit::MediaNext if button == BTN_LEFT => self.media_cmd("Next"),
            PanelHit::Tray(k) => {
                if let Some(r) = rect {
                    self.menu_beside = self.panel_side(id);
                    self.tray_click_at(k, button, r);
                }
            }
            PanelHit::DockItem(i) => self.dock_click(i, button, rect, qh),
            _ => {}
        }
    }

    fn media_cmd(&self, method: &'static str) {
        let _ = self.dbus.cmds.send(DbusCmd::Media(method));
    }

    fn activate_workspace(&self, index: usize) {
        if let (Some(w), Some(m)) = (self.windows.workspaces.get(index), &self.windows.workspace_manager) {
            w.handle.activate();
            m.commit();
        }
    }

    /// Wheel over the workspaces: the next or previous one.
    pub(super) fn panel_scroll(&mut self, value: f64) {
        if !matches!(self.panels.hover, Some(PanelHit::Workspace(_))) {
            return;
        }
        let st = &mut self.panels.bar_state;
        st.wheel += value;
        if st.wheel.abs() < WHEEL_STEP {
            return;
        }
        let forward = st.wheel > 0.0;
        st.wheel = 0.0;
        let list = self.windows.workspace_views(self.output.as_ref(), &self.output_name);
        let Some(at) = list.iter().position(|w| w.active) else { return };
        let to = if forward { at + 1 } else { at.wrapping_sub(1) };
        if let Some(w) = list.get(to) {
            self.activate_workspace(w.index);
        }
    }

    // ── Dock ──────────────────────────────────────────────────────────

    fn dock_click(&mut self, i: usize, button: u32, rect: Option<Rect>, qh: &QueueHandle<Self>) {
        let Some(item) = self.panels.dock_state.items.get(i) else { return };
        match button {
            BTN_LEFT if item.windows.is_empty() => launch(item.app.as_ref()),
            BTN_LEFT => {
                let Some(seat) = self.seat.clone() else { return };
                let focused = self.windows.toplevels.iter().find(|t| t.focused && item.windows.contains(&t.handle)).map(|t| t.handle.clone());
                match focused {
                    // The only window, already in front: put it away.
                    Some(w) if item.windows.len() == 1 => w.set_minimized(),
                    // Several: go through them.
                    Some(w) => {
                        let at = item.windows.iter().position(|h| *h == w).unwrap_or(0);
                        let next = &item.windows[(at + 1) % item.windows.len()];
                        next.unset_minimized();
                        next.activate(&seat);
                    }
                    None => {
                        let w = &item.windows[0];
                        w.unset_minimized();
                        w.activate(&seat);
                    }
                }
            }
            BTN_MIDDLE => launch(item.app.as_ref()),
            BTN_RIGHT => {
                let entry = |id, label: &str| MenuEntry { id, label: label.to_string(), enabled: true, separator: false, toggle: None, depth: 0, is_submenu: false };
                let mut entries = Vec::new();
                if item.app.is_some() {
                    entries.push(entry(MENU_NEW_WINDOW, if item.windows.is_empty() { "Open" } else { "New Window" }));
                    entries.push(entry(MENU_PIN, if item.pinned { "Remove from Dock" } else { "Pin to Dock" }));
                }
                if !item.windows.is_empty() {
                    entries.push(entry(MENU_CLOSE, if item.windows.len() > 1 { "Close All Windows" } else { "Close" }));
                }
                let key = format!("{DOCK_MENU}{}", item.key);
                if let (Some(r), false) = (rect, entries.is_empty()) {
                    self.menu_beside = self.panel_side(PanelId::Dock);
                    self.open_menu(key, entries, r, qh);
                }
            }
            _ => {}
        }
    }

    /// A row of a dock menu was chosen; false when `key` is not a dock menu.
    pub(super) fn dock_menu_pick(&mut self, key: &str, id: i32, qh: &QueueHandle<Self>) -> bool {
        let Some(app_key) = key.strip_prefix(DOCK_MENU) else { return false };
        let d = &mut self.panels.dock_state;
        let Some(item) = d.items.iter().find(|it| it.key == app_key) else { return true };
        match id {
            MENU_NEW_WINDOW => launch(item.app.as_ref()),
            MENU_PIN if item.pinned => d.unpin(app_key),
            MENU_PIN => d.pin(app_key),
            MENU_CLOSE => item.windows.iter().for_each(|w| w.close()),
            _ => {}
        }
        self.sync_panels(qh);
        true
    }
}

fn launch(app: Option<&prism_common::apps::DesktopApp>) {
    match app.map(|a| a.launch()) {
        Some(Ok(child)) => crate::reap(child),
        Some(Err(e)) => eprintln!("[prism-dock] cannot launch: {e}"),
        None => {}
    }
}

fn run_ctl(args: &[&str]) {
    if let Ok(c) = std::process::Command::new("prism-ctl").args(args).spawn() {
        crate::reap(c);
    }
}

// ── Wayland dispatch ──────────────────────────────────────────────────────

impl Dispatch<ZwlrLayerSurfaceV1, PanelId> for Desk {
    fn event(d: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, id: &PanelId, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                let Some(p) = d.panel_mut(*id) else { return };
                p.size = (width.max(1) as i32, height.max(1) as i32);
                p.configured = true;
                p.frame_cb = None;
                d.mark_panel(*id, qh);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                let slot = match id {
                    PanelId::Bar => d.panels.bar.take(),
                    PanelId::Dock => d.panels.dock.take(),
                };
                if let Some(p) = slot {
                    p.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, PanelId> for Desk {
    fn event(d: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, id: &PanelId, _: &Connection, qh: &QueueHandle<Self>) {
        let Some(p) = d.panel_mut(*id) else { return };
        p.frame_cb = None;
        if p.dirty || p.animating {
            d.draw_panel(*id, qh);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, PanelId> for Desk {
    fn event(d: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, id: &PanelId, _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            let Some(p) = d.panel_mut(*id) else { return };
            for b in p.bufs.iter_mut().filter(|b| &b.buffer == buffer) {
                b.busy = false;
            }
            if p.dirty && p.frame_cb.is_none() && p.configured {
                d.draw_panel(*id, qh);
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, StripTag> for Desk {
    fn event(d: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &StripTag, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                let (Some(sp), Some(vp)) = (d.single_pixel.clone(), d.viewporter.clone()) else { return };
                let Some(s) = d.panels.strip.as_mut() else { return };
                let viewport = s.viewport.get_or_insert_with(|| vp.get_viewport(&s.surface, qh, ()));
                viewport.set_destination(width.max(1) as i32, height.max(1) as i32);
                if s.buffer.is_none() {
                    let buffer = sp.create_u32_rgba_buffer(0, 0, 0, 0, qh, StripTag);
                    s.surface.attach(Some(&buffer), 0, 0);
                    s.surface.damage_buffer(0, 0, 1, 1);
                    s.buffer = Some(buffer);
                }
                s.surface.commit();
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if let Some(s) = d.panels.strip.take() {
                    s.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, StripTag> for Desk {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &StripTag, _: &Connection, _: &QueueHandle<Self>) {}
}
