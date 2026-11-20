//! Notification popups (port of QML NotificationPopup.qml + NcPopupItem.qml).
//!
//! A stack of cards in a screen corner or edge centre (`notification_popup_position`). Each
//! popup is its own card-sized layer surface, so compositor blur (which covers a whole layer
//! surface on Mango) sits behind each card only, as the QML per-card blur regions did. A popup
//! moves by changing its layer margins: it slides in from its side (from the top/bottom edge
//! for centred positions) and collapses its slot when it leaves so the others glide over.
//! The oldest popup is nearest the screen edge.
//!
//! Lifecycle per popup: entering -> shown -> leaving -> removed. Each popup animates on its own
//! wall clock; the auto-dismiss timer pauses while hovered and restarts on in-place updates.
//! Timed-out popups leave one at a time, oldest first, each after the previous has finished.
//! At most `MAX_VISIBLE` popups are active; older ones are retired with their leave animation.

use std::time::{Duration, Instant};

use prism_common::{Config, NotifOverlayMessage, Theme};
use wayland_client::protocol::{wl_buffer, wl_callback, wl_pointer, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::ShmBuf;
use crate::ui::{in_cubic, out_back, out_cubic, Rgba, Style};
use crate::Globals;

const MAX_VISIBLE: usize = 4;
const CARD_W: f64 = 360.0;
/// Vertical gap below each card (QML fullHeight = card.height + 12).
const GAP: f64 = 12.0;
const TOP_MARGIN: f64 = 20.0;
const REST_X: f64 = 10.0;
const HIDDEN_X: f64 = -460.0;
const PAD: f64 = 12.0;
const ICON: f64 = 44.0;
const CLOSE: f64 = 22.0;
/// Popups whose frame callback is this late are redrawn anyway: compositors send no frame
/// callbacks to surfaces that are off-screen (a popup starts left of the screen edge).
const STALE_FRAME: Duration = Duration::from_millis(40);

const FONT_APP: &str = "Inter, Sans 11px";
const FONT_SUMMARY: &str = "Inter, Sans Bold 15px";
const FONT_BODY: &str = "Inter, Sans 13px";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Entering,
    Shown,
    Leaving,
}

struct Popup {
    /// Unique per popup (a notification id can come back after its popup left).
    uid: u32,
    nid: u32,
    app_name: String,
    summary: String,
    body: String,
    critical: bool,
    icon: Option<String>,
    image: Option<String>,
    /// Show `image` as a large preview below the text instead of as the icon.
    preview: bool,
    desktop_entry: String,
    /// (key, label), at most 3 shown.
    actions: Vec<(String, String)>,
    action_hover: Option<usize>,
    timeout: Duration,
    phase: Phase,
    phase_start: Instant,
    /// Values captured when the leave animation started (it may interrupt the entry).
    leave_from: (f64, f64, f64),
    dismiss_at: Option<Instant>,
    /// Timer expired; waiting for the popup above to finish leaving (one leaves at a time).
    due: bool,
    hovered: bool,
    close_hovered: bool,
    card_h: f64,

    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    configured: bool,
    bufs: Vec<ShmBuf>,
    frame_cb: Option<wl_callback::WlCallback>,
    last_draw: Option<Instant>,
    /// Content changed (hover, text); redraw even when not animating.
    dirty: bool,
    /// Layer margins (top, right, bottom, left).
    margin: (i32, i32, i32, i32),
    size: (i32, i32),
}

/// Where the stack sits: which screen edge it grows from and which side popups slide in from.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    bottom: bool,
    /// -1 left, 0 centre, 1 right.
    side: i8,
}

impl Placement {
    fn parse(s: &str) -> Self {
        let bottom = s.starts_with("bottom");
        let side = if s.ends_with("right") {
            1
        } else if s.ends_with("center") || s.ends_with("centre") {
            0
        } else {
            -1
        };
        Placement { bottom, side }
    }

    fn anchor(&self) -> Anchor {
        let v = if self.bottom { Anchor::Bottom } else { Anchor::Top };
        match self.side {
            -1 => v | Anchor::Left,
            1 => v | Anchor::Right,
            _ => v,
        }
    }

    /// Margins for a popup whose stack offset from the edge is `y` and whose slide position is
    /// `x` (HIDDEN_X..REST_X, overshooting past REST_X).
    fn margins(&self, y: f64, x: f64, card_h: f64) -> (i32, i32, i32, i32) {
        let (mut edge, mut side) = (y, x);
        if self.side == 0 {
            // Centred: slide in from the screen edge instead of from a side.
            let k = (x - HIDDEN_X) / (REST_X - HIDDEN_X);
            edge = y + (k - 1.0) * (card_h + y + 20.0);
            side = 0.0;
        }
        let (e, s) = (edge.round() as i32, side.round() as i32);
        let (top, bottom) = if self.bottom { (0, e) } else { (e, 0) };
        let (right, left) = match self.side {
            1 => (s, 0),
            -1 => (0, s),
            _ => (0, 0),
        };
        (top, right, bottom, left)
    }
}

/// Animated values at `now`: (x, opacity, reveal).
fn sample(p: &Popup, style: &Style, now: Instant) -> (f64, f64, f64) {
    let ms = now.duration_since(p.phase_start).as_secs_f64() * 1000.0;
    let prog = |base: f64, delay: f64| {
        let d = style.duration_ms(base);
        let delay = style.duration_ms(delay);
        if d <= 0.0 { if ms >= delay { 1.0 } else { 0.0 } } else { ((ms - delay) / d).clamp(0.0, 1.0) }
    };
    match p.phase {
        Phase::Shown => (REST_X, 1.0, 1.0),
        Phase::Entering => {
            let x = HIDDEN_X + (REST_X - HIDDEN_X) * out_back(prog(380.0, 0.0), 0.9);
            (x, prog(200.0, 0.0), out_cubic(prog(260.0, 0.0)))
        }
        Phase::Leaving => {
            let (fx, fo, fr) = p.leave_from;
            let x = fx + (HIDDEN_X - fx) * in_cubic(prog(300.0, 0.0));
            let o = fo * (1.0 - prog(260.0, 0.0));
            let r = fr * (1.0 - in_out_cubic(prog(260.0, 160.0)));
            (x, o, r)
        }
    }
}

fn phase_done(p: &Popup, style: &Style, now: Instant) -> bool {
    let ms = now.duration_since(p.phase_start).as_secs_f64() * 1000.0;
    match p.phase {
        Phase::Entering => ms >= style.duration_ms(380.0),
        Phase::Leaving => ms >= style.duration_ms(420.0),
        Phase::Shown => false,
    }
}

fn in_out_cubic(t: f64) -> f64 {
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Notification bodies may carry the spec's markup subset; show it as plain text.
fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Text column width: card minus padding, icon and spacing.
fn text_w() -> f64 {
    CARD_W - 2.0 * PAD - ICON - 12.0
}

const ACTION_H: f64 = 28.0;
const MAX_ACTIONS: usize = 3;

/// Largest preview picture height (design px); wider pictures fill the card width.
const PREVIEW_MAX_H: f64 = 190.0;

/// The image to show as a preview, if the notification asks for one and it exists.
fn preview_path(image: Option<&str>, preview: bool) -> Option<&str> {
    image.filter(|p| preview && std::path::Path::new(p).exists())
}

/// Height of the preview picture for `path` at width `w`: its own aspect ratio, capped (taller
/// pictures are cover-cropped), 0 without a picture.
pub fn preview_height(path: Option<&str>, w: f64) -> f64 {
    let Some((iw, ih)) = path.and_then(cb::image_dims) else { return 0.0 };
    (w * ih as f64 / iw.max(1) as f64).clamp(48.0, PREVIEW_MAX_H).round()
}

/// Card height for this content (QML: content.implicitHeight + 24; the action row adds 10 + 28,
/// a preview picture 10 + its height).
fn measure(summary: &str, body: &str, actions: usize, preview: Option<&str>) -> f64 {
    unsafe {
        let img = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, 1, 1);
        let cr = cb::cairo_create(img);
        let w = text_w() as i32;
        let (_, sh) = cb::measure_pango_text(cr, summary, FONT_SUMMARY, w, Some(1));
        let bh = if body.is_empty() { 0 } else { cb::measure_pango_text(cr, body, FONT_BODY, w, Some(3)).1 };
        cb::cairo_destroy(cr);
        cb::cairo_surface_destroy(img);
        let text_h = CLOSE + 2.0 + sh as f64 + if bh > 0 { 2.0 + bh as f64 } else { 0.0 };
        let row = if actions > 0 { 10.0 + ACTION_H } else { 0.0 };
        let ph = preview_height(preview, CARD_W - 2.0 * PAD);
        let pic = if ph > 0.0 { 10.0 + ph } else { 0.0 };
        (text_h.max(ICON) + pic + row + 2.0 * PAD).ceil()
    }
}

pub struct Toasts {
    queue: EventQueue<Stack>,
    st: Stack,
}

struct Stack {
    globals: GlobalsLite,
    pointer: Option<wl_pointer::WlPointer>,
    cursor: Option<WpCursorShapeDeviceV1>,
    style: Style,
    theme: Theme,
    place: Placement,
    popups: Vec<Popup>,
    next_uid: u32,
    ptr_on: Option<u32>,
    ptr_pos: (f64, f64),
    ptr_serial: u32,
}

/// The globals this module needs (cloned proxies from the process-wide `Globals`).
struct GlobalsLite {
    compositor: wayland_client::protocol::wl_compositor::WlCompositor,
    shm: wayland_client::protocol::wl_shm::WlShm,
    layer_shell: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1,
    cursor_mgr: Option<wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
}

impl Toasts {
    pub fn new(conn: &Connection, g: &Globals) -> Option<Toasts> {
        let queue: EventQueue<Stack> = conn.new_event_queue();
        let qh = queue.handle();
        let globals = GlobalsLite {
            compositor: g.compositor.clone()?,
            shm: g.shm.clone()?,
            layer_shell: g.layer_shell.clone()?,
            cursor_mgr: g.cursor_shape_manager.clone(),
        };
        let pointer = g.seat.as_ref().map(|s| s.get_pointer(&qh, ()));
        let cursor = match (&globals.cursor_mgr, &pointer) {
            (Some(m), Some(p)) => Some(m.get_pointer(p, &qh, ())),
            _ => None,
        };
        let config = Config::load();
        let theme = Theme::load();
        Some(Toasts {
            queue,
            st: Stack {
                globals,
                pointer,
                cursor,
                style: Style::new(&config, &theme),
                theme,
                place: Placement::parse(&config.notification_popup_position),
                popups: Vec::new(),
                next_uid: 1,
                ptr_on: None,
                ptr_pos: (0.0, 0.0),
                ptr_serial: 0,
            },
        })
    }

    pub fn is_empty(&self) -> bool {
        self.st.popups.is_empty()
    }

    /// A message from prismd: add or update a popup, or dismiss (id 0 = all).
    pub fn handle(&mut self, msg: NotifOverlayMessage) {
        let qh = self.queue.handle();
        let st = &mut self.st;
        match msg {
            NotifOverlayMessage::Add { id, app_name, summary, body, timeout_ms, icon, image, urgency, actions, desktop_entry, preview } => {
                if st.popups.is_empty() {
                    // Pick up theme / config changes made since the last popup.
                    let config = Config::load();
                    st.theme = Theme::load();
                    st.style = Style::new(&config, &st.theme);
                    st.place = Placement::parse(&config.notification_popup_position);
                }
                let extra = Extra { icon, image, critical: urgency >= 2, actions, desktop_entry, preview };
                st.add(id, app_name, summary, strip_markup(&body), timeout_ms, extra, &qh);
            }
            NotifOverlayMessage::Dismiss { id } => {
                let now = Instant::now();
                let style = st.style.clone();
                for p in st.popups.iter_mut().filter(|p| id == 0 || p.nid == id) {
                    start_leave(p, &style, now);
                }
            }
        }
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    /// Timers and animation frames. Returns the next deadline in ms (-1 = none).
    pub fn tick(&mut self) -> i32 {
        let qh = self.queue.handle();
        self.st.step(&qh)
    }

    /// Destroy every surface (process exit).
    pub fn teardown(mut self) {
        for p in self.st.popups.drain(..) {
            destroy(p);
        }
        if let Some(c) = self.st.cursor.take() {
            c.destroy();
        }
        if let Some(p) = self.st.pointer.take() {
            p.release();
        }
        let _ = self.queue.dispatch_pending(&mut self.st);
    }
}

fn start_leave(p: &mut Popup, style: &Style, now: Instant) {
    if p.phase == Phase::Leaving {
        return;
    }
    p.leave_from = sample(p, style, now);
    p.phase = Phase::Leaving;
    p.phase_start = now;
    p.dismiss_at = None;
}

fn arm_timer(p: &mut Popup, now: Instant) {
    p.due = false;
    p.dismiss_at = if p.phase == Phase::Shown && !p.critical && !p.hovered { Some(now + p.timeout) } else { None };
}

fn destroy(p: Popup) {
    p.layer.destroy();
    p.surface.destroy();
    drop(p.bufs);
}

/// The parts of a notification beyond its text.
struct Extra {
    icon: Option<String>,
    image: Option<String>,
    critical: bool,
    actions: Vec<(String, String)>,
    desktop_entry: String,
    /// Show `image` as a large preview, not as the icon.
    preview: bool,
}

impl Stack {
    #[allow(clippy::too_many_arguments)]
    fn add(&mut self, nid: u32, app_name: String, summary: String, body: String, timeout_ms: u64, mut extra: Extra, qh: &QueueHandle<Self>) {
        let now = Instant::now();
        let timeout = Duration::from_millis(timeout_ms.clamp(3000, 20000));
        extra.actions.truncate(MAX_ACTIONS);
        if let Some(p) = self.popups.iter_mut().find(|p| p.nid == nid && p.phase != Phase::Leaving) {
            p.app_name = app_name;
            p.card_h = measure(&summary, &body, extra.actions.len(), preview_path(extra.image.as_deref(), extra.preview));
            p.summary = summary;
            p.body = body;
            p.timeout = timeout;
            p.critical = extra.critical;
            p.icon = extra.icon;
            p.image = extra.image;
            p.preview = extra.preview;
            p.desktop_entry = extra.desktop_entry;
            p.actions = extra.actions;
            p.dirty = true;
            arm_timer(p, now);
            return;
        }
        let card_h = measure(&summary, &body, extra.actions.len(), preview_path(extra.image.as_deref(), extra.preview));
        let uid = self.next_uid;
        self.next_uid = self.next_uid.wrapping_add(1).max(1);
        let g = &self.globals;
        let surface = g.compositor.create_surface(qh, ());
        let layer = g.layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Overlay,
            self.style.namespace("prism-notification"),
            qh,
            uid,
        );
        layer.set_anchor(self.place.anchor());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let sc = self.style.scale;
        layer.set_size((CARD_W * sc).round() as u32, (card_h * sc).round() as u32);
        surface.commit();
        self.popups.push(Popup {
            uid,
            nid,
            app_name,
            summary,
            body,
            critical: extra.critical,
            icon: extra.icon,
            image: extra.image,
            preview: extra.preview,
            desktop_entry: extra.desktop_entry,
            actions: extra.actions,
            action_hover: None,
            timeout,
            phase: Phase::Entering,
            phase_start: now,
            leave_from: (HIDDEN_X, 0.0, 0.0),
            dismiss_at: None,
            due: false,
            hovered: false,
            close_hovered: false,
            card_h,
            surface,
            layer,
            configured: false,
            bufs: Vec::new(),
            frame_cb: None,
            last_draw: None,
            dirty: true,
            margin: (i32::MIN, 0, 0, 0),
            size: ((CARD_W * sc).round() as i32, (card_h * sc).round() as i32),
        });
        // Too many on screen: retire the oldest gracefully.
        let style = self.style.clone();
        let mut excess = self.popups.iter().filter(|p| p.phase != Phase::Leaving).count().saturating_sub(MAX_VISIBLE);
        for p in self.popups.iter_mut() {
            if excess == 0 {
                break;
            }
            if p.phase != Phase::Leaving {
                start_leave(p, &style, now);
                excess -= 1;
            }
        }
    }

    fn step(&mut self, qh: &QueueHandle<Self>) -> i32 {
        let now = Instant::now();
        let style = self.style.clone();

        // Phase changes and timers.
        for p in self.popups.iter_mut() {
            if phase_done(p, &style, now) && p.phase == Phase::Entering {
                p.phase = Phase::Shown;
                p.phase_start = now;
                arm_timer(p, now);
            }
            if p.dismiss_at.map(|t| now >= t).unwrap_or(false) {
                p.dismiss_at = None;
                p.due = true;
            }
        }
        // Timed-out popups leave one at a time, top first: the next starts once the one
        // above has slid out and its slot has collapsed (the stack moves up in between).
        if !self.popups.iter().any(|p| p.phase == Phase::Leaving) {
            if let Some(p) = self.popups.iter_mut().find(|p| p.due && p.phase == Phase::Shown) {
                start_leave(p, &style, now);
            }
        }
        let mut i = 0;
        while i < self.popups.len() {
            if self.popups[i].phase == Phase::Leaving && phase_done(&self.popups[i], &style, now) {
                let p = self.popups.remove(i);
                if self.ptr_on == Some(p.uid) {
                    self.ptr_on = None;
                }
                destroy(p);
            } else {
                i += 1;
            }
        }

        // Layout and frames.
        let mut y = TOP_MARGIN;
        let mut animating = false;
        for idx in 0..self.popups.len() {
            let (x, opacity, reveal) = sample(&self.popups[idx], &style, now);
            let moving = self.popups[idx].phase != Phase::Shown;
            animating |= moving;
            let p = &mut self.popups[idx];
            let sc = style.scale;
            let m = self.place.margins(y, x, p.card_h);
            let px = |v: i32| (v as f64 * sc).round() as i32;
            let margin = (px(m.0), px(m.1), px(m.2), px(m.3));
            y += (p.card_h + GAP) * reveal;
            if !p.configured {
                continue;
            }
            let size = ((CARD_W * sc).round() as i32, (p.card_h * sc).round() as i32);
            let geometry_changed = margin != p.margin || size != p.size;
            if size != p.size {
                p.layer.set_size(size.0 as u32, size.1 as u32);
                p.size = size;
            }
            if margin != p.margin {
                p.layer.set_margin(margin.0, margin.1, margin.2, margin.3);
                p.margin = margin;
            }
            let ready = p.frame_cb.is_none() || p.last_draw.map(|t| now.duration_since(t) >= STALE_FRAME).unwrap_or(true);
            if (p.dirty || moving || geometry_changed) && ready {
                draw(p, opacity, &style, &self.theme, &self.globals, qh);
                p.last_draw = Some(now);
                p.dirty = false;
            }
        }

        let next_timer = self
            .popups
            .iter()
            .filter_map(|p| p.dismiss_at)
            .min()
            .map(|t| t.saturating_duration_since(now).as_millis() as i32 + 1);
        let frame = if animating || self.popups.iter().any(|p| p.dirty) { Some(STALE_FRAME.as_millis() as i32 + 1) } else { None };
        match (next_timer, frame) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => -1,
        }
    }

    fn popup_mut(&mut self, uid: u32) -> Option<&mut Popup> {
        self.popups.iter_mut().find(|p| p.uid == uid)
    }

    fn set_cursor(&self, shape: CursorShape) {
        if let Some(c) = &self.cursor {
            c.set_shape(self.ptr_serial, shape);
        }
    }

    fn on_motion(&mut self) {
        let Some(uid) = self.ptr_on else { return };
        let (x, y) = (self.ptr_pos.0 / self.style.scale, self.ptr_pos.1 / self.style.scale);
        let Some(p) = self.popup_mut(uid) else { return };
        let over_close = in_close(x, y);
        let over_action = action_at(&p.actions, p.card_h, x, y);
        if over_action != p.action_hover {
            p.action_hover = over_action;
            p.dirty = true;
        }
        if !p.hovered {
            p.hovered = true;
            p.dismiss_at = None;
            p.due = false;
        }
        if over_close != p.close_hovered {
            p.close_hovered = over_close;
            p.dirty = true;
        }
        self.set_cursor(if over_close || over_action.is_some() { CursorShape::Pointer } else { CursorShape::Default });
    }

    fn on_leave(&mut self) {
        let Some(uid) = self.ptr_on.take() else { return };
        let now = Instant::now();
        if let Some(p) = self.popup_mut(uid) {
            p.hovered = false;
            if p.close_hovered || p.action_hover.is_some() {
                p.close_hovered = false;
                p.action_hover = None;
                p.dirty = true;
            }
            arm_timer(p, now);
        }
    }

    /// Click on an action runs it in the app; anywhere else (or the close button) hides the
    /// popup, which stays in the notification center.
    fn on_click(&mut self) {
        let Some(uid) = self.ptr_on else { return };
        let (x, y) = (self.ptr_pos.0 / self.style.scale, self.ptr_pos.1 / self.style.scale);
        let style = self.style.clone();
        if let Some(p) = self.popup_mut(uid) {
            if let Some(i) = action_at(&p.actions, p.card_h, x, y) {
                if p.actions[i].0 == crate::nc::INLINE_REPLY {
                    // Popups take no keyboard: type the reply in the notification center.
                    crate::nc::open_reply(p.nid);
                } else {
                    invoke_action(p.nid, p.actions[i].0.clone());
                }
            }
            start_leave(p, &style, Instant::now());
        }
    }
}

/// Rectangles of the action buttons (equal widths across the card, bottom row).
const FONT_ACTION: &str = "Inter, Sans Medium 12px";

/// Action buttons along the bottom of the card, each as wide as its label needs plus an equal
/// share of the spare room.
fn action_rects(actions: &[(String, String)], card_h: f64) -> Vec<(f64, f64, f64, f64)> {
    if actions.is_empty() {
        return Vec::new();
    }
    let gap = 8.0;
    let natural: Vec<f64> = unsafe {
        let img = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, 1, 1);
        let cr = cb::cairo_create(img);
        let w = actions.iter().map(|(_, l)| cb::measure_pango_text(cr, l, FONT_ACTION, 4000, Some(1)).0 as f64 + 24.0).collect();
        cb::cairo_destroy(cr);
        cb::cairo_surface_destroy(img);
        w
    };
    let widths = crate::ui::fit_widths(&natural, CARD_W - 2.0 * PAD, gap);
    let y = card_h - PAD - ACTION_H;
    let mut x = PAD;
    widths
        .into_iter()
        .map(|w| {
            let r = (x, y, w, ACTION_H);
            x += w + gap;
            r
        })
        .collect()
}

fn action_at(actions: &[(String, String)], card_h: f64, x: f64, y: f64) -> Option<usize> {
    action_rects(actions, card_h).iter().position(|r| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3)
}

/// Send the chosen action to the app (through prismd, which owns the D-Bus name).
/// Send an inline reply (the app gets NotificationReplied).
pub fn send_reply(id: u32, text: String) {
    std::thread::spawn(move || {
        let cmd = prism_common::IpcCommand::Notif(prism_common::ipc::NotifAction::Reply { id, text });
        if let Err(e) = prism_common::ipc::request(&cmd) {
            eprintln!("[prism-ui] notification reply: {e}");
        }
    });
}

pub fn invoke_action(id: u32, action: String) {
    std::thread::spawn(move || {
        let cmd = prism_common::IpcCommand::Notif(prism_common::ipc::NotifAction::Invoke { id, action });
        if let Err(e) = prism_common::ipc::request(&cmd) {
            eprintln!("[prism-ui] notification action: {e}");
        }
    });
}

/// QML NcNotifIcon: a picture (cover-cropped, rounded) when the notification has one,
/// otherwise a tinted circle with the themed icon (app icon, desktop entry, app name,
/// dialog-information), otherwise the app's initial.
#[allow(clippy::too_many_arguments)]
pub fn draw_notif_icon(
    cr: *mut Cairo,
    x: f64,
    y: f64,
    size: f64,
    app_name: &str,
    icon: Option<&str>,
    image: Option<&str>,
    desktop_entry: &str,
    fg: Rgba,
    alpha: f64,
) {
    if let Some(img) = image.filter(|p| std::path::Path::new(p).exists()) {
        unsafe { cb::cairo_push_group(cr) };
        let ok = cb::draw_image_cover(cr, img, x, y, size, size, 12.0);
        unsafe {
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, alpha);
        }
        if ok {
            return;
        }
    }
    cb::fill_circle(cr, x + size / 2.0, y + size / 2.0, size / 2.0, with_alpha(fg, 0.14 * alpha));
    let isz = (size * 0.58).round();
    let app_icon = app_name.to_lowercase().replace(' ', "-");
    let entry_icon = desktop_entry.rsplit('/').next().unwrap_or("").trim_end_matches(".desktop").to_lowercase();
    let names = [icon.unwrap_or(""), entry_icon.as_str(), app_icon.as_str(), "dialog-information"];
    let path = names.iter().filter(|n| !n.is_empty()).find_map(|n| crate::icons::lookup(n, isz as u32));
    let drawn = path
        .and_then(|p| cb::cached_image(&p.to_string_lossy(), isz as i32, isz as i32, true))
        .map(|img| unsafe {
            let (w, h) = cb::image_size(img);
            cb::cairo_set_source_surface(cr, img, x + (size - w) / 2.0, y + (size - h) / 2.0);
            cb::cairo_paint_with_alpha(cr, alpha);
        })
        .is_some();
    if !drawn {
        let letter = app_name.chars().next().unwrap_or('?').to_uppercase().to_string();
        let (lw, lh) = cb::measure_pango_text(cr, &letter, "Inter, Sans Bold 16px", 40, None);
        cb::draw_pango_text(cr, x + (size - lw as f64) / 2.0, y + (size - lh as f64) / 2.0, &letter, "Inter, Sans Bold 16px", with_alpha(fg, alpha), None);
    }
}

fn in_close(x: f64, y: f64) -> bool {
    let (cx, cy) = close_pos();
    x >= cx && x <= cx + CLOSE && y >= cy && y <= cy + CLOSE
}

fn close_pos() -> (f64, f64) {
    (CARD_W - PAD - CLOSE, PAD)
}

fn with_alpha(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, c.3 * a)
}

fn draw(p: &mut Popup, opacity: f64, style: &Style, theme: &Theme, g: &GlobalsLite, qh: &QueueHandle<Stack>) {
    let (w, h) = p.size;
    if p.bufs.iter().any(|b| b.w != w || b.h != h) {
        p.bufs.retain(|b| b.busy && b.w == w && b.h == h);
    }
    let idx = match p.bufs.iter().position(|b| !b.busy) {
        Some(i) => i,
        None if p.bufs.len() < 3 => match ShmBuf::new(&g.shm, qh, w, h, ()) {
            Some(b) => {
                p.bufs.push(b);
                p.bufs.len() - 1
            }
            None => return,
        },
        None => {
            p.dirty = true;
            return;
        }
    };
    p.bufs[idx].busy = true;
    let content = p_content(p);
    let buf = &p.bufs[idx];
    unsafe {
        libc::memset(buf.ptr as *mut libc::c_void, 0, buf.size);
        let surf = buf.cairo();
        let cr = cb::cairo_create(surf);
        cb::cairo_scale(cr, style.scale, style.scale);
        draw_card(cr, content, opacity, style, theme);
        cb::cairo_destroy(cr);
        cb::cairo_surface_destroy(surf);
    }
    p.surface.attach(Some(&buf.buffer), 0, 0);
    p.surface.damage_buffer(0, 0, w, h);
    p.frame_cb = Some(p.surface.frame(qh, p.uid));
    p.surface.commit();
}

struct Content<'a> {
    app_name: &'a str,
    summary: &'a str,
    body: &'a str,
    critical: bool,
    close_hovered: bool,
    h: f64,
    icon: Option<&'a str>,
    image: Option<&'a str>,
    preview: bool,
    desktop_entry: &'a str,
    actions: &'a [(String, String)],
    action_hover: Option<usize>,
}

fn p_content(p: &Popup) -> Content<'_> {
    Content {
        app_name: &p.app_name,
        summary: &p.summary,
        body: &p.body,
        critical: p.critical,
        close_hovered: p.close_hovered,
        h: p.card_h,
        icon: p.icon.as_deref(),
        image: p.image.as_deref(),
        preview: p.preview,
        desktop_entry: &p.desktop_entry,
        actions: &p.actions,
        action_hover: p.action_hover,
    }
}

fn draw_card(cr: *mut Cairo, c: Content, opacity: f64, style: &Style, theme: &Theme) {
    let a = opacity.clamp(0.0, 1.0);
    if a <= 0.001 {
        return;
    }
    let fg = style.fg;
    let r = style.radius.min(c.h / 2.0);
    cb::fill_rounded_rect(cr, 0.0, 0.0, CARD_W, c.h, r, with_alpha(style.card_bg, a));
    let (bw, bc) = if c.critical {
        (2.0, cb::hex_to_rgba(&theme.error, 1.0))
    } else {
        (1.0, cb::hex_to_rgba(&theme.outline_variant, 1.0))
    };
    cb::stroke_rounded_rect(cr, bw / 2.0, bw / 2.0, CARD_W - bw, c.h - bw, r, with_alpha(bc, a), bw);

    // A preview picture is shown large below the text, so the icon is the app's.
    let preview = preview_path(c.image, c.preview);
    let icon_image = if preview.is_some() { None } else { c.image };
    draw_notif_icon(cr, PAD, PAD, ICON, c.app_name, c.icon, icon_image, c.desktop_entry, fg, a);

    // Text column.
    let tx = PAD + ICON + 12.0;
    let tw = text_w();
    let (cx, cy) = close_pos();
    let (_, ah) = cb::measure_pango_text(cr, c.app_name, FONT_APP, tw as i32, Some(1));
    cb::draw_pango_text(cr, tx, PAD + (CLOSE - ah as f64) / 2.0, c.app_name, FONT_APP, with_alpha(fg, 0.65 * a), Some((cx - tx - 4.0) as i32));
    if c.close_hovered {
        cb::fill_circle(cr, cx + CLOSE / 2.0, cy + CLOSE / 2.0, CLOSE / 2.0, with_alpha(fg, a));
    }
    let xc = if c.close_hovered { cb::hex_to_rgba(&theme.inverse_primary, a) } else { with_alpha(fg, a) };
    draw_close_icon(cr, cx + CLOSE / 2.0, cy + CLOSE / 2.0, 12.0, xc);

    let sy = PAD + CLOSE + 2.0;
    let (_, sh) = cb::measure_pango_text(cr, c.summary, FONT_SUMMARY, tw as i32, Some(1));
    cb::draw_pango_text(cr, tx, sy, c.summary, FONT_SUMMARY, with_alpha(fg, a), Some(tw as i32));
    if !c.body.is_empty() {
        cb::draw_pango_text_wrapped(cr, tx, sy + sh as f64 + 2.0, c.body, FONT_BODY, with_alpha(fg, 0.9 * a), tw as i32, Some(3));
    }

    // Preview picture across the card, above the actions.
    if let Some(path) = preview {
        let pw = CARD_W - 2.0 * PAD;
        let ph = preview_height(Some(path), pw);
        let row = if c.actions.is_empty() { 0.0 } else { 10.0 + ACTION_H };
        let py = c.h - PAD - row - ph;
        unsafe { cb::cairo_push_group(cr) };
        cb::draw_image_cover(cr, path, PAD, py, pw, ph, 10.0);
        cb::stroke_rounded_rect(cr, PAD + 0.5, py + 0.5, pw - 1.0, ph - 1.0, 10.0, with_alpha(fg, 0.12), 1.0);
        unsafe {
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, a);
        }
    }

    // Action buttons (QML NcButton compact).
    let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
    for (i, (r, (_, label))) in action_rects(c.actions, c.h).into_iter().zip(c.actions.iter()).enumerate() {
        let hov = c.action_hover == Some(i);
        cb::fill_rounded_rect(cr, r.0, r.1, r.2, r.3, 12.0, with_alpha(fg, if hov { 0.85 } else { 0.2 } * a));
        let (lw, lh) = cb::measure_pango_text(cr, label, FONT_ACTION, (r.2 - 12.0) as i32, Some(1));
        let col = if hov { with_alpha(inv, a) } else { with_alpha(fg, a) };
        cb::draw_pango_text(cr, r.0 + ((r.2 - lw as f64) / 2.0).max(6.0), r.1 + (r.3 - lh as f64) / 2.0, label, FONT_ACTION, col, Some((r.2 - 12.0) as i32));
    }
}

/// The "close" glyph: an X inside a `size` box, stroke ~1.5.
fn draw_close_icon(cr: *mut Cairo, cx: f64, cy: f64, size: f64, color: Rgba) {
    let d = size * 0.32;
    unsafe {
        cb::cairo_save(cr);
        cb::cairo_new_path(cr);
        cb::cairo_set_line_width(cr, 1.6);
        cb::cairo_set_line_cap(cr, 1);
        cb::cairo_set_source_rgba(cr, color.0, color.1, color.2, color.3);
        cb::cairo_move_to(cr, cx - d, cy - d);
        cb::cairo_line_to(cr, cx + d, cy + d);
        cb::cairo_move_to(cr, cx + d, cy - d);
        cb::cairo_line_to(cr, cx - d, cy + d);
        cb::cairo_stroke(cr);
        cb::cairo_restore(cr);
    }
}

// ── Wayland events ────────────────────────────────────────────────────────

impl Dispatch<ZwlrLayerSurfaceV1, u32> for Stack {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, uid: &u32, _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                ls.ack_configure(serial);
                if let Some(p) = st.popup_mut(*uid) {
                    p.configured = true;
                    p.dirty = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                let style = st.style.clone();
                if let Some(p) = st.popup_mut(*uid) {
                    start_leave(p, &style, Instant::now());
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Stack {
    fn event(st: &mut Self, cb: &wl_callback::WlCallback, _: wl_callback::Event, uid: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let Some(p) = st.popup_mut(*uid) {
            if p.frame_cb.as_ref() == Some(cb) {
                p.frame_cb = None;
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Stack {
    fn event(st: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            for b in st.popups.iter_mut().flat_map(|p| p.bufs.iter_mut()) {
                if &b.buffer == buffer {
                    b.busy = false;
                }
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Stack {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                st.ptr_serial = serial;
                st.ptr_pos = (surface_x, surface_y);
                st.ptr_on = st.popups.iter().find(|p| p.surface == surface).map(|p| p.uid);
                st.on_motion();
            }
            wl_pointer::Event::Leave { .. } => st.on_leave(),
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.ptr_pos = (surface_x, surface_y);
                st.on_motion();
            }
            wl_pointer::Event::Button { state: WEnum::Value(wl_pointer::ButtonState::Released), .. } => st.on_click(),
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Stack {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(wl_surface::WlSurface, wl_shm_pool::WlShmPool, WpCursorShapeDeviceV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_stripped() {
        assert_eq!(strip_markup("<b>Hi</b> &amp; <i>bye</i> &lt;3"), "Hi & bye <3");
        assert_eq!(strip_markup("a > b"), "a > b");
    }

    #[test]
    fn easing_ends() {
        assert!((in_out_cubic(1.0) - 1.0).abs() < 1e-9);
        assert!(in_out_cubic(0.0).abs() < 1e-9);
        assert!((in_out_cubic(0.5) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn card_fits_icon_when_text_is_short() {
        assert!(measure("Hi", "", 0, None) >= ICON + 2.0 * PAD);
        assert!(measure("Hi", "line one\nline two\nline three", 0, None) > measure("Hi", "", 0, None));
        assert!(measure("Hi", "", 2, None) > measure("Hi", "", 0, None));
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    #[test]
    fn placements() {
        let tl = Placement::parse("top-left");
        assert_eq!(tl.margins(20.0, 10.0, 80.0), (20, 0, 0, 10));
        let br = Placement::parse("bottom-right");
        assert_eq!(br.margins(20.0, 10.0, 80.0), (0, 10, 20, 0));
        let tc = Placement::parse("top-center");
        assert_eq!(tc.margins(20.0, REST_X, 80.0), (20, 0, 0, 0));
        // Hidden: fully beyond the top edge.
        let (top, ..) = tc.margins(20.0, HIDDEN_X, 80.0);
        assert!(top <= -80);
    }
}
