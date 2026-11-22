//! Downloads pill: an overlay pill at the top centre while files download into the Downloads
//! folder (prismd's downloads watcher sends `UiCommand::Downloads`): what is downloading, how
//! much is on disk, the speed, and a progress bar (filling when the browser says how far along
//! it is, else sweeping). At rest it is small (the percentage and a thin bar) at 80% opacity;
//! under the pointer it grows to the title, the bar and the percentage, fully opaque. The layer
//! surface is resized with it (Mango blurs the whole surface). When a
//! download's file arrives (`UiCommand::DownloadDone`) it shows a tick and "Download completed",
//! then fades away; a download that ends with nothing arriving just fades. A click opens the
//! Downloads folder. Below the recording pill when both show.

use std::time::Instant;

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

/// Size at rest and under the pointer (or saying a download completed).
const SMALL: (f64, f64) = (150.0, 36.0);
const LARGE: (f64, f64) = (420.0, 64.0);
/// Opacity at rest and under the pointer.
const REST: f64 = 0.8;
/// "Download completed" stays this long, then fades out over FADE_S.
const DONE_S: f64 = 1.8;
const FADE_S: f64 = 0.45;
/// After the downloads end, how long to wait for word that a file arrived.
const FINISH_WAIT_S: f64 = 6.0;
/// Lucide "check".
const CHECK: &str = "M20 6 9 17l-5-5";
const CROSS: &str = "M7 7l10 10M17 7 7 17";
/// Lucide "download".
const ICON: &str = "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3";

pub struct Pill {
    queue: EventQueue<State>,
    st: State,
}

struct State {
    list: Vec<DownloadProgress>,
    /// The downloads as last shown while there were any (kept while finishing).
    last: Vec<DownloadProgress>,
    /// A finished download's name and when word came.
    done: Option<(String, Instant)>,
    /// When the list went empty without word of a file (cancelled, or the word is late).
    ended: Option<Instant>,
    opened: Instant,
    /// The bar's fill, following the overall progress on a spring.
    fill: crate::ui::SpringValue,
    /// Hover: 0 at rest, 1 under the pointer, eased toward `hover`.
    lit: f64,
    /// Size: 0 small, 1 large, eased toward hovering (or completed).
    grow: f64,
    lit_at: Instant,
    /// The surface size last asked for.
    size: (i32, i32),
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
    /// Pointer position on the surface.
    pos: (f64, f64),
    /// The Cancel button (a download of Prism's own, large layout) and which download it stops.
    cancel: Option<((f64, f64, f64, f64), u64)>,
    cancel_lit: f64,
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

/// Overall progress (0 to 1) when every download has one: weighted by size where the size can
/// be worked out.
fn overall(list: &[DownloadProgress]) -> Option<f64> {
    let ps: Vec<f64> = list.iter().map(|d| d.progress).collect::<Option<Vec<f64>>>()?;
    (!ps.is_empty()).then(|| ps.iter().sum::<f64>() / ps.len() as f64)
}

/// (title, detail) for the pill.
fn labels(list: &[DownloadProgress]) -> (String, String) {
    let bytes: u64 = list.iter().map(|d| d.bytes).sum();
    let rate: u64 = list.iter().map(|d| d.rate).sum();
    let title = match list {
        [one] => one.name.clone(),
        _ => format!("{} downloads", list.len()),
    };
    let mut parts = Vec::new();
    match overall(list) {
        // The total, worked out from what is on disk and how far along it is.
        Some(p) if p > 0.02 => {
            parts.push(format!("{:.0}%", p * 100.0));
            parts.push(format!("{} of {}", human(bytes), human((bytes as f64 / p) as u64)));
        }
        _ => parts.push(human(bytes)),
    }
    if rate > 0 {
        parts.push(format!("{}/s", human(rate)));
    }
    (title, parts.join("  \u{b7}  "))
}

/// Ease in and out (0 to 1).
fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Pill {
    /// `below_rec`: the recording pill is showing; sit under it.
    pub fn open(conn: &Connection, g: &Globals, list: Vec<DownloadProgress>, below_rec: bool) -> Option<Pill> {
        Pill::open_with(conn, g, list, None, below_rec)
    }

    /// A pill that only says a download finished (it ended before the pill ever showed).
    pub fn open_done(conn: &Connection, g: &Globals, name: String, below_rec: bool) -> Option<Pill> {
        Pill::open_with(conn, g, Vec::new(), Some((name, Instant::now())), below_rec)
    }

    fn open_with(conn: &Connection, g: &Globals, list: Vec<DownloadProgress>, done: Option<(String, Instant)>, below_rec: bool) -> Option<Pill> {
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let surface = g.compositor.as_ref()?.create_surface(&qh, ());
        let layer = g.layer_shell.as_ref()?.get_layer_surface(&surface, None, Layer::Overlay, "prism-osd".into(), &qh, ());
        layer.set_anchor(Anchor::Top);
        layer.set_margin(if below_rec { 12 + 40 + 8 } else { 12 }, 0, 0, 0);
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        let (w0, h0) = if done.is_some() { LARGE } else { SMALL };
        layer.set_size(w0 as u32, h0 as u32);
        layer.set_exclusive_zone(-1);
        surface.commit();
        let pointer = g.seat.as_ref().map(|s| s.get_pointer(&qh, ()));
        let cursor_dev = match (&pointer, &g.cursor_shape_manager) {
            (Some(p), Some(m)) => Some(m.get_pointer(p, &qh, ())),
            _ => None,
        };
        Some(Pill {
            queue,
            st: State {
                last: list.clone(),
                list,
                done,
                ended: None,
                opened: Instant::now(),
            fill: crate::ui::SpringValue::new(0.0),
                lit: 0.0,
                grow: if w0 == LARGE.0 { 1.0 } else { 0.0 },
                lit_at: Instant::now(),
                size: (w0 as i32, h0 as i32),
                surface,
                layer,
                pointer,
                cursor_dev,
                shm: g.shm.clone()?,
                bufs: Vec::new(),
                configured: false,
                frame_cb: None,
                dirty: true,
                hover: false,
                pos: (-1.0, -1.0),
                cancel: None,
                cancel_lit: 0.0,
            },
        })
    }

    /// The downloads now; empty: they ended (the pill finishes, then goes).
    pub fn set(&mut self, list: Vec<DownloadProgress>) {
        if list.is_empty() {
            if self.st.ended.is_none() {
                self.st.ended = Some(Instant::now());
            }
        } else {
            self.st.ended = None;
            self.st.last = list.clone();
        }
        if list != self.st.list {
            self.st.list = list;
        }
        self.st.dirty = true;
    }

    /// A download's file arrived.
    pub fn done(&mut self, name: String) {
        self.st.done = Some((name, Instant::now()));
        self.st.dirty = true;
    }

    /// Finished and faded out: tear it down.
    pub fn finished(&self) -> bool {
        self.st.phase().1 <= 0.0
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

/// What the pill shows.
#[derive(PartialEq)]
enum Phase {
    /// Downloads in progress (these).
    Active,
    /// Ended, waiting for word of a file.
    Finishing,
    /// A file arrived.
    Completed,
}

impl State {
    /// The downloads still going (one whose file arrived is done even before the list says so).
    fn going(&self) -> Vec<DownloadProgress> {
        let done = self.done.as_ref().map(|d| d.0.as_str());
        self.list.iter().filter(|d| Some(d.name.as_str()) != done).cloned().collect()
    }

    /// The phase and how visible the pill is overall (0: gone).
    fn phase(&self) -> (Phase, f64) {
        let appear = smooth(self.opened.elapsed().as_secs_f64() / 0.25);
        if !self.going().is_empty() {
            return (Phase::Active, appear);
        }
        if let Some((_, t)) = &self.done {
            let t = t.elapsed().as_secs_f64();
            return (Phase::Completed, appear * (1.0 - smooth((t - DONE_S) / FADE_S)));
        }
        let waited = self.ended.map_or(0.0, |t| t.elapsed().as_secs_f64());
        (Phase::Finishing, appear * (1.0 - smooth((waited - FINISH_WAIT_S) / FADE_S)))
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (phase, shown) = self.phase();
        // Hover eases the opacity up from 80% and the size from small to large.
        let dt = self.lit_at.elapsed().as_secs_f64();
        self.lit_at = Instant::now();
        let ease = |v: &mut f64, to: f64, secs: f64| {
            *v += (to - *v) * (dt / secs).min(1.0);
            if (*v - to).abs() < 0.01 {
                *v = to;
            }
        };
        let lit_to = if self.hover { 1.0 } else { 0.0 };
        let grow_to = if self.hover || phase == Phase::Completed { 1.0 } else { 0.0 };
        ease(&mut self.lit, lit_to, 0.15);
        ease(&mut self.grow, grow_to, 0.12);
        let g = smooth(self.grow);
        let (w, h) = (SMALL.0 + (LARGE.0 - SMALL.0) * g, SMALL.1 + (LARGE.1 - SMALL.1) * g);
        let (iw, ih) = (w.round() as i32, h.round() as i32);
        if (iw, ih) != self.size {
            self.size = (iw, ih);
            self.layer.set_size(iw as u32, ih as u32);
        }
        // A buffer of this size (others are dropped once the compositor lets go of them).
        self.bufs.retain(|b| b.busy || (b.w, b.h) == (iw, ih));
        let idx = match self.bufs.iter().position(|b| !b.busy && (b.w, b.h) == (iw, ih)) {
            Some(i) => i,
            None if self.bufs.len() < 3 => match ShmBuf::new(&self.shm, qh, iw, ih, ()) {
                Some(b) => {
                    self.bufs.push(b);
                    self.bufs.len() - 1
                }
                None => return,
            },
            None => return,
        };
        self.bufs[idx].busy = true;
        let alpha = (REST + (1.0 - REST) * self.lit) * shown;
        let cancel_was = self.cancel_lit;
        self.cancel = None;
        let going = self.going();
        let list = if going.is_empty() { self.last.clone() } else { going };
        let progress = match phase {
            Phase::Active => overall(&list),
            // Every download has ended: full until the word of the file comes.
            _ => Some(1.0),
        };
        if let Some(p) = progress {
            // A new, less advanced download starts from where it is, not sliding back.
            if p + 0.25 < self.fill.target() {
                self.fill.snap(p);
            } else if (p - self.fill.target()).abs() > 1e-4 {
                self.fill.set_scaled(p, (0.6, 1.0), Some(1.0));
            }
        }
        let fill = self.fill.value().clamp(0.0, 1.0);
        let filling = (fill - self.fill.target()).abs() > 1e-3;
        let blue = (0.55, 0.80, 1.0, 1.0);
        let green = (0.45, 0.85, 0.55, 1.0);
        let (w, h) = (iw as f64, ih as f64);
        // The large layout fades in over the small one as it grows.
        let big = smooth((g - 0.35) / 0.65);
        let small = 1.0 - smooth(g / 0.5);
        let white = |a: f64| (1.0, 1.0, 1.0, a);
        let with = |c: (f64, f64, f64, f64), a: f64| (c.0, c.1, c.2, c.3 * a);
        unsafe {
            let surf = self.bufs[idx].cairo();
            let cr = cb::cairo_create(surf);
            cb::cairo_set_operator(cr, 0);
            cb::cairo_paint(cr);
            cb::cairo_set_operator(cr, 2);
            cb::cairo_push_group(cr);
            cb::fill_rounded_rect(cr, 0.0, 0.0, w, h, h / 2.0, (0.08, 0.08, 0.10, 0.94));
            cb::stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, h / 2.0, (1.0, 1.0, 1.0, 0.10 + 0.12 * self.lit), 1.0);
            let pct = |p: Option<f64>| p.map_or_else(|| human(list.iter().map(|d| d.bytes).sum()), |p| format!("{:.0}%", p * 100.0));
            // The bar: filled to the progress, or a sweeping stretch when the total is unknown.
            let bar = |x: f64, y: f64, bw: f64, bh: f64, a: f64| {
                cb::fill_rounded_rect(cr, x, y, bw, bh, bh / 2.0, white(0.12 * a));
                match progress {
                    Some(_) => cb::fill_rounded_rect(cr, x, y, (bw * fill).max(bh), bh, bh / 2.0, with(blue, a)),
                    None => {
                        let t = self.opened.elapsed().as_secs_f64();
                        let seg = bw * 0.3;
                        let pos = (t / 1.4).fract() * (bw + seg) - seg;
                        let (x0, x1) = (pos.max(0.0), (pos + seg).min(bw));
                        if x1 > x0 {
                            cb::fill_rounded_rect(cr, x + x0, y, x1 - x0, bh, bh / 2.0, with(blue, a));
                        }
                    }
                }
            };
            let icon_x = 12.0 + 6.0 * g;
            if phase == Phase::Completed {
                let pop = smooth(self.done.as_ref().map_or(1.0, |d| d.1.elapsed().as_secs_f64() / 0.3)).max(0.3);
                let cx = icon_x + 14.0;
                cb::fill_circle(cr, cx, h / 2.0, 14.0, with(green, 0.25));
                crate::svg::draw_path(cr, CHECK, cx - 9.0 * pop, h / 2.0 - 9.0 * pop, 18.0 * pop, green, Some(2.4));
                let name = self.done.as_ref().map(|d| d.0.clone()).unwrap_or_default();
                let tx = cx + 26.0;
                cb::draw_pango_text(cr, tx, h / 2.0 - 19.0, "Download completed", "Inter, Sans Semi-Bold 13px", white(big), Some((w - tx - 18.0) as i32));
                cb::draw_pango_text(cr, tx, h / 2.0 + 1.0, &name, "Inter, Sans 12px", white(0.65 * big), Some((w - tx - 18.0) as i32));
            } else {
                let cx = icon_x + 12.0;
                let cy = 18.0 + 6.0 * g;
                cb::fill_circle(cr, cx, cy, 12.0 + 2.0 * g, with(blue, 0.14));
                crate::svg::draw_path(cr, ICON, cx - 7.0, cy - 7.0, 14.0, blue, Some(2.0));
                if small > 0.0 {
                    // Small: the percentage (or the size), a thin bar under it.
                    let label = if phase == Phase::Finishing { "\u{2026}".to_string() } else { pct(progress) };
                    cb::draw_pango_text(cr, cx + 18.0, 6.0, &label, "Inter, Sans Semi-Bold 12px", white(small), None);
                    bar(cx + 18.0, h - 9.0, w - (cx + 18.0) - 16.0, 2.5, small);
                }
                if big > 0.0 {
                    // Large: the title (and size and speed), the bar with the percentage beside it.
                    let (title, detail) = labels(&list);
                    let detail = if phase == Phase::Finishing { "Finishing\u{2026}".to_string() } else { detail };
                    let tx = cx + 22.0;
                    // One download of Prism's own: a Cancel button at the end of the title row.
                    let mut right = w - 20.0;
                    if let (true, [one]) = (phase == Phase::Active, list.as_slice()) {
                        if let Some(id) = one.id {
                            let r = (w - 16.0 - 22.0, 6.0, 22.0, 22.0);
                            let hot = r.0 <= self.pos.0 && self.pos.0 <= r.0 + r.2 && r.1 <= self.pos.1 && self.pos.1 <= r.1 + r.3;
                            ease(&mut self.cancel_lit, if hot { 1.0 } else { 0.0 }, 0.1);
                            cb::fill_circle(cr, r.0 + r.2 / 2.0, r.1 + r.3 / 2.0, r.2 / 2.0, white((0.06 + 0.14 * self.cancel_lit) * big));
                            crate::svg::draw_path(cr, CROSS, r.0 + 4.0, r.1 + 4.0, 14.0, white((0.6 + 0.4 * self.cancel_lit) * big), Some(2.2));
                            self.cancel = (big > 0.5).then_some((r, id));
                            right = r.0 - 8.0;
                        }
                    }
                    let (dw, _) = cb::measure_pango_text(cr, &detail, "Inter, Sans 11px", 400, Some(1));
                    cb::draw_pango_text(cr, right - dw as f64, 13.0, &detail, "Inter, Sans 11px", white(0.6 * big), None);
                    cb::draw_pango_text(cr, tx, 10.0, &title, "Inter, Sans Semi-Bold 13px", white(big), Some((right - tx - 12.0 - dw as f64) as i32));
                    let label = pct(progress);
                    let (pw, ph) = cb::measure_pango_text(cr, &label, "Inter, Sans Semi-Bold 12px", 400, Some(1));
                    let by = 42.0;
                    cb::draw_pango_text(cr, w - 20.0 - pw as f64, by + 2.0 - ph as f64 / 2.0, &label, "Inter, Sans Semi-Bold 12px", white(big), None);
                    bar(tx, by, w - tx - 20.0 - pw as f64 - 12.0, 4.0, big);
                }
            }
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, alpha.clamp(0.0, 1.0));
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        // Keep drawing while anything moves: the sweep, a fade, the hover easing, the tick.
        let sweeping = progress.is_none() || filling;
        let fading = shown < 1.0 || self.done.is_some() || self.ended.is_some();
        self.dirty = sweeping || fading || self.lit != lit_to || self.grow != grow_to || self.cancel_lit != cancel_was;
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, iw, ih);
        self.frame_cb = Some(self.surface.frame(qh, ()));
        self.surface.commit();
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
                st.pos = (-1.0, -1.0);
                st.dirty = true;
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } if st.hover => {
                st.pos = (surface_x, surface_y);
                st.dirty = true;
            }
            wl_pointer::Event::Button { button: 0x110, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } if st.hover && st.cancel.is_some_and(|(r, _)| r.0 <= st.pos.0 && st.pos.0 <= r.0 + r.2 && r.1 <= st.pos.1 && st.pos.1 <= r.1 + r.3) => {
                let id = st.cancel.map(|(_, id)| id).unwrap_or_default();
                if let Ok(c) = std::process::Command::new("prism-ctl").args(["download", "cancel", &id.to_string()]).spawn() {
                    crate::reap(c);
                }
            }
            wl_pointer::Event::Button { button: 0x110, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } if st.hover => {
                // Prism's own downloads: the Downloads card; the browsers': the Downloads folder.
                if st.list.iter().chain(st.last.iter()).any(|d| d.id.is_some()) {
                    if let Ok(c) = std::process::Command::new("prism-ctl").args(["download", "open"]).spawn() {
                        crate::reap(c);
                    }
                    return;
                }
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
