//! Hot corners: a tiny clear surface in each screen corner that has an action
//! (`Config::hot_corners`). Resting the pointer there for the configured delay runs it once;
//! it runs again only after the pointer has left the corner. Corners live on the desktop's
//! output, on the Top layer (a fullscreen window covers them, so games are left alone).

use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_buffer, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1};

use super::Desk;

/// Size of a corner's target (the pointer stops at the screen edge, so a few px is plenty).
const SIZE: u32 = 2;

struct Corner {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    buffer: Option<wl_buffer::WlBuffer>,
    action: String,
}

impl Corner {
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

#[derive(Default)]
pub struct Corners {
    /// Top left, top right, bottom left, bottom right.
    slots: [Option<Corner>; 4],
    /// The corner the pointer is in, since when, and whether its action already ran.
    inside: Option<(usize, Instant, bool)>,
}

#[derive(Clone, Copy)]
pub struct CornerTag(usize);

impl Corners {
    pub fn which(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.slots.iter().position(|c| c.as_ref().is_some_and(|c| &c.surface == surface))
    }
}

/// Runs a corner action.
fn run(action: &str) {
    use prism_common::alt_tab::Compositor;
    let ctl: &[&str] = match action {
        "launcher" => &["launcher", "toggle"],
        "search" => &["search", "toggle"],
        "notifications" => &["notif", "toggle"],
        "lock" => &["lock", "lock"],
        "screenshot" => &["screenshot", "toggle"],
        "power" => &["power", "toggle"],
        "clipboard" => &["clipboard", "toggle"],
        "settings" => &["settings", "toggle"],
        "overview" => {
            let cmd: &[&str] = match Compositor::current() {
                Some(Compositor::Mango) => &["mmsg", "dispatch", "toggleoverview"],
                Some(Compositor::Niri) => &["niri", "msg", "action", "toggle-overview"],
                Some(Compositor::Hyprland) => &["hyprctl", "dispatch", "hyprexpo:expo", "toggle"],
                None => return,
            };
            if let Ok(c) = std::process::Command::new(cmd[0]).args(&cmd[1..]).spawn() {
                crate::reap(c);
            }
            return;
        }
        _ => return,
    };
    if let Ok(c) = std::process::Command::new("prism-ctl").args(ctl).spawn() {
        crate::reap(c);
    }
}

impl Desk {
    /// Create or drop corner surfaces to match the config.
    pub(super) fn sync_corners(&mut self, qh: &QueueHandle<Self>) {
        let actions = self.config.hot_corners.actions().map(|a| a.to_string());
        for (i, action) in actions.into_iter().enumerate() {
            let want = action != "none" && !action.is_empty();
            if let Some(c) = self.corners.slots[i].as_mut() {
                if want {
                    c.action = action;
                    continue;
                }
                if let Some(c) = self.corners.slots[i].take() {
                    c.destroy();
                }
                continue;
            }
            if !want {
                continue;
            }
            let (Some(comp), Some(ls)) = (self.compositor.clone(), self.layer_shell.clone()) else { return };
            let surface = comp.create_surface(qh, ());
            // Shares the scrim's namespace: never blurred or animated by the compositor.
            let layer = ls.get_layer_surface(&surface, self.output.as_ref(), Layer::Top, "prism-scrim".to_string(), qh, CornerTag(i));
            let v = if i < 2 { Anchor::Top } else { Anchor::Bottom };
            let h = if i % 2 == 0 { Anchor::Left } else { Anchor::Right };
            layer.set_anchor(v | h);
            layer.set_size(SIZE, SIZE);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            self.corners.slots[i] = Some(Corner { surface, layer, viewport: None, buffer: None, action });
        }
    }

    /// The pointer came into corner `i` (Some) or left it (None).
    pub(super) fn corner_pointer(&mut self, i: Option<usize>) {
        self.corners.inside = i.map(|i| (i, Instant::now(), false));
    }

    /// Run the action of the corner the pointer rests in once its delay is over; returns ms
    /// until then.
    pub(super) fn corners_tick(&mut self) -> Option<i32> {
        let (i, since, done) = self.corners.inside?;
        if done {
            return None;
        }
        let delay = Duration::from_millis(self.config.hot_corners.delay_ms as u64);
        let left = delay.saturating_sub(since.elapsed());
        if !left.is_zero() {
            return Some((left.as_millis() as i32).max(1));
        }
        self.corners.inside = Some((i, since, true));
        if let Some(c) = &self.corners.slots[i] {
            run(&c.action);
        }
        None
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, CornerTag> for Desk {
    fn event(d: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, tag: &CornerTag, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                let (Some(sp), Some(vp)) = (d.single_pixel.clone(), d.viewporter.clone()) else { return };
                let Some(c) = d.corners.slots[tag.0].as_mut() else { return };
                let viewport = c.viewport.get_or_insert_with(|| vp.get_viewport(&c.surface, qh, ()));
                viewport.set_destination(width.max(1) as i32, height.max(1) as i32);
                if c.buffer.is_none() {
                    let buffer = sp.create_u32_rgba_buffer(0, 0, 0, 0, qh, *tag);
                    c.surface.attach(Some(&buffer), 0, 0);
                    c.surface.damage_buffer(0, 0, 1, 1);
                    c.buffer = Some(buffer);
                }
                c.surface.commit();
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if let Some(c) = d.corners.slots[tag.0].take() {
                    c.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, CornerTag> for Desk {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &CornerTag, _: &Connection, _: &QueueHandle<Self>) {}
}
