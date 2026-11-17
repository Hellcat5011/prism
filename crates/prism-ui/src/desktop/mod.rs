// ─────────────────────────────────────────────────────────────────────────
// desktop/ — the desktop widgets layer (`prism-ui desktop`)
//
// Rewrite of the QML Desktop* windows. Like QML, every widget is its own
// layer-shell surface (Bottom layer) sized to the widget, so a change in one
// widget only redraws and uploads that widget's pixels:
//
//   clock        redrawn once a minute
//   analog clock once a second, plus the 169 ms eased second-dot tick
//   calendar     on interaction and at midnight
//   media        art/text cached; only the wave + progress strip is redrawn
//                per frame while playing (frame callbacks = display refresh)
//   tray         when items change (StatusNotifierItem over D-Bus)
//   shortcuts    on hover / press
//
// Data comes from the D-Bus worker (dbus.rs); config and colours are reloaded
// when ~/.config/prism/config.json or the matugen palette change (inotify),
// so edit mode, settings and theme changes apply live without polling.
// ─────────────────────────────────────────────────────────────────────────

pub mod dbus;
pub mod edit;
pub mod layout;
pub mod text;
pub mod widgets;

use std::os::fd::{AsFd, FromRawFd};
use std::time::Instant;

use prism_common::{Config, Theme};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use crate::cairo_backend::*;
use crate::ui::{out_cubic, Style};
use crate::waker::Waker;
use dbus::{DbusCmd, DbusEvent, MediaState, MenuEntry, TrayItem};
use layout::{shortcuts_vertical, tray_rect, widget_rect, Rect, WidgetId};
use widgets::*;

/// Theme.animSlow: duration of the analog clock's second-dot tick.
const ANIM_SLOW_MS: f64 = 169.0;

// ── Shared shm buffer ─────────────────────────────────────────────────────

pub struct ShmBuf {
    pub buffer: wl_buffer::WlBuffer,
    pub ptr: *mut u8,
    pub size: usize,
    pub w: i32,
    pub h: i32,
    pub busy: bool,
}

impl ShmBuf {
    pub fn new<D, U>(shm: &wl_shm::WlShm, qh: &QueueHandle<D>, w: i32, h: i32, udata: U) -> Option<Self>
    where
        D: Dispatch<wl_shm_pool::WlShmPool, ()> + Dispatch<wl_buffer::WlBuffer, U> + 'static,
        U: Send + Sync + 'static,
    {
        let stride = w * 4;
        let size = (stride * h) as usize;
        unsafe {
            let fd = libc::memfd_create(c"prism-desktop".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let file = std::fs::File::from_raw_fd(fd);
            if libc::ftruncate(fd, size as libc::off_t) < 0 {
                return None;
            }
            let ptr = libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            if ptr == libc::MAP_FAILED {
                return None;
            }
            let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
            let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, udata);
            pool.destroy();
            Some(ShmBuf { buffer, ptr: ptr as *mut u8, size, w, h, busy: false })
        }
    }

    /// Cairo image surface over the buffer memory (caller destroys it).
    pub fn cairo(&self) -> *mut CairoSurface {
        unsafe { cairo_image_surface_create_for_data(self.ptr, CAIRO_FORMAT_ARGB32, self.w, self.h, self.w * 4) }
    }
}

impl Drop for ShmBuf {
    fn drop(&mut self) {
        self.buffer.destroy();
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
        }
    }
}

// ── Per-widget surface ────────────────────────────────────────────────────

struct WSurf {
    id: WidgetId,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    /// Requested screen rectangle.
    rect: Rect,
    /// Configured size.
    size: (i32, i32),
    configured: bool,
    bufs: Vec<ShmBuf>,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,
    animating: bool,
    hits: Hits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufTag(WidgetId, usize);

#[derive(Clone, Copy)]
struct MenuTag;
#[derive(Clone, Copy)]
struct CatcherTag;

struct Menu {
    key: String,
    entries: Vec<MenuEntry>,
    rect: Rect,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    catcher: wl_surface::WlSurface,
    catcher_layer: ZwlrLayerSurfaceV1,
    catcher_vp: Option<WpViewport>,
    catcher_buf: Option<wl_buffer::WlBuffer>,
    buf: Option<ShmBuf>,
    hover: Option<usize>,
    rows: Vec<(usize, Rect)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PointerOn {
    None,
    Widget(WidgetId),
    Menu,
    Catcher,
}

struct Desk {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_mgr: Option<WpCursorShapeManagerV1>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    single_pixel: Option<WpSinglePixelBufferManagerV1>,
    viewporter: Option<WpViewporter>,
    output_mode: (i32, i32),
    output_scale: i32,

    config: Config,
    theme: Theme,
    style: Style,
    look: Look,
    screen: (f64, f64),
    widgets: Vec<WSurf>,

    // pointer
    ptr_on: PointerOn,
    ptr_pos: (f64, f64),
    ptr_serial: u32,
    pressed: bool,
    hover_hit: Option<Hit>,

    // data
    dbus: dbus::DbusHandle,
    media: Option<MediaState>,
    art: Option<*mut CairoSurface>,
    art_path: Option<std::path::PathBuf>,
    palette: MediaPalette,
    wave_time: f64,
    wave_h: f64,
    wave_from: f64,
    wave_anim: Option<Instant>,
    media_base: Option<(*mut CairoSurface, i32, i32)>,
    media_base_dirty: bool,
    media_canvas: Rect,
    last_media_frame: Option<Instant>,
    tray: Vec<TrayItem>,
    tray_icons: Vec<TrayIcon>,
    cal: CalState,
    second_from: f64,
    second_to: f64,
    second_anim: Option<Instant>,
    last_second: Option<u32>,
    menu: Option<Menu>,
    pending_menu_anchor: Option<(String, Rect)>,
    /// Depth effect: wallpaper foreground mask (white, alpha = foreground) published by
    /// prismd, and when it appeared (fade-in).
    depth_mask: Option<String>,
    depth_fade: Option<Instant>,
}

/// QML DepthService maskFade duration.
const DEPTH_FADE_MS: f64 = 200.0;

/// The desktop widgets layer as a component of the prism-ui event loop.
pub struct Desktop {
    queue: EventQueue<Desk>,
    state: Desk,
    inotify: Option<Inotify>,
    last_day: u32,
    last_minute: u32,
    icon_gen: u64,
}

impl Desktop {
    pub fn new(conn: &Connection, waker: Waker) -> Option<Desktop> {
        let mut queue: EventQueue<Desk> = conn.new_event_queue();
        let qh = queue.handle();
        let dbus = dbus::spawn(waker);

        let config = Config::load();
        let theme = Theme::load();
        let style = Style::new(&config, &theme);
        set_global_font(&style.font_family);
        let look = Look::new(&style, &theme);
        let now = LocalTime::now();
        let palette = MediaPalette::fallback(&look);

        let mut d = Desk {
            compositor: None,
            shm: None,
            layer_shell: None,
            seat: None,
            pointer: None,
            cursor_mgr: None,
            cursor_dev: None,
            single_pixel: None,
            viewporter: None,
            output_mode: (0, 0),
            output_scale: 1,
            config,
            theme,
            style,
            look,
            screen: (1920.0, 1080.0),
            widgets: Vec::new(),
            ptr_on: PointerOn::None,
            ptr_pos: (0.0, 0.0),
            ptr_serial: 0,
            pressed: false,
            hover_hit: None,
            dbus,
            media: None,
            art: None,
            art_path: None,
            palette,
            wave_time: 0.0,
            wave_h: 0.0,
            wave_from: 0.0,
            wave_anim: None,
            media_base: None,
            media_base_dirty: true,
            media_canvas: Rect::default(),
            last_media_frame: None,
            tray: Vec::new(),
            tray_icons: Vec::new(),
            cal: CalState { year: now.year, month: now.month0, view: CalView::Days },
            second_from: 0.0,
            second_to: 0.0,
            second_anim: None,
            last_second: None,
            menu: None,
            pending_menu_anchor: None,
            depth_mask: None,
            depth_fade: None,
        };

        let _registry = conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut d).ok()?;
        queue.roundtrip(&mut d).ok()?;
        if d.compositor.is_none() || d.layer_shell.is_none() || d.shm.is_none() {
            eprintln!("[prism-desktop] compositor lacks wl_compositor / wl_shm / layer-shell");
            return None;
        }
        d.update_screen();
        d.sync_time(true, &qh);
        d.load_depth_mask();
        d.reconcile(&qh);
        Some(Desktop {
            queue,
            state: d,
            inotify: Inotify::new(),
            last_day: now.mday,
            last_minute: now.minute,
            icon_gen: crate::icons::generation(),
        })
    }

    pub fn inotify_fd(&self) -> Option<i32> {
        self.inotify.as_ref().map(|i| i.fd)
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.state).is_ok()
    }

    /// Apply background data and clock changes; returns the next deadline in ms.
    pub fn tick(&mut self) -> i32 {
        let qh = self.queue.handle();
        let d = &mut self.state;
        let events: Vec<DbusEvent> = d.dbus.events.try_iter().collect();
        for ev in events {
            d.on_dbus(ev, &qh);
        }
        if self.inotify.as_ref().map(|i| i.changed()).unwrap_or(false) {
            d.reload(&qh);
        }
        // gsettings icon theme changed: re-resolve tray icons.
        let gen = crate::icons::generation();
        if gen != self.icon_gen {
            self.icon_gen = gen;
            let _ = d.dbus.cmds.send(DbusCmd::RefreshTray);
        }
        let now = LocalTime::now();
        if now.minute != self.last_minute {
            self.last_minute = now.minute;
            d.mark(WidgetId::Clock, &qh);
        }
        if now.mday != self.last_day {
            self.last_day = now.mday;
            d.mark(WidgetId::Calendar, &qh);
            d.mark(WidgetId::Clock, &qh);
        }
        d.sync_time(false, &qh);
        // Wake at the next wall-clock second (analog clock) or minute.
        let ms_to_second = 1000 - now.millis as i32 + 15;
        if d.has(WidgetId::AnalogClock) {
            ms_to_second
        } else {
            ((59 - now.second as i32) * 1000 + ms_to_second).max(50)
        }
    }
}

impl Desk {
    fn update_screen(&mut self) {
        let (w, h) = self.output_mode;
        if w > 0 && h > 0 {
            let s = self.output_scale.max(1) as f64;
            self.screen = (w as f64 / s, h as f64 / s);
            crate::ui::set_screen_height(self.screen.1);
        }
    }

    fn has(&self, id: WidgetId) -> bool {
        self.widgets.iter().any(|w| w.id == id)
    }

    fn rect_for(&self, id: WidgetId) -> Rect {
        let (sw, sh) = self.screen;
        widget_rect(id, id.cfg(&self.config.desktop_layout), sw, sh, self.tray.len())
    }

    /// Create / destroy / move widget surfaces to match the config.
    fn reconcile(&mut self, qh: &QueueHandle<Self>) {
        for id in WidgetId::ALL {
            let visible = id.cfg(&self.config.desktop_layout).visible;
            let pos = self.widgets.iter().position(|w| w.id == id);
            match (visible, pos) {
                (false, Some(i)) => {
                    let w = self.widgets.remove(i);
                    w.layer.destroy();
                    w.surface.destroy();
                }
                (true, None) => {
                    let rect = self.rect_for(id);
                    let (Some(comp), Some(ls)) = (&self.compositor, &self.layer_shell) else { continue };
                    let surface = comp.create_surface(qh, ());
                    let layer = ls.get_layer_surface(&surface, None, Layer::Bottom, "prism-desktop".to_string(), qh, id);
                    layer.set_anchor(Anchor::Top | Anchor::Left);
                    layer.set_exclusive_zone(-1);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.set_size(rect.w as u32, rect.h as u32);
                    layer.set_margin(rect.y as i32, 0, 0, rect.x as i32);
                    surface.commit();
                    self.widgets.push(WSurf {
                        id,
                        surface,
                        layer,
                        rect,
                        size: (0, 0),
                        configured: false,
                        bufs: Vec::new(),
                        frame_cb: None,
                        dirty: true,
                        animating: false,
                        hits: Vec::new(),
                    });
                }
                (true, Some(i)) => {
                    let rect = self.rect_for(id);
                    let w = &mut self.widgets[i];
                    if w.rect != rect {
                        w.rect = rect;
                        w.layer.set_size(rect.w as u32, rect.h as u32);
                        w.layer.set_margin(rect.y as i32, 0, 0, rect.x as i32);
                        w.surface.commit();
                    }
                    w.dirty = true;
                }
                (false, None) => {}
            }
        }
        self.media_base_dirty = true;
        let ids: Vec<WidgetId> = self.widgets.iter().map(|w| w.id).collect();
        for id in ids {
            self.mark(id, qh);
        }
    }

    fn reload(&mut self, qh: &QueueHandle<Self>) {
        self.config = Config::load();
        self.theme = Theme::load();
        self.style = Style::new(&self.config, &self.theme);
        set_global_font(&self.style.font_family);
        self.look = Look::new(&self.style, &self.theme);
        if self.art.is_none() {
            self.palette = MediaPalette::fallback(&self.look);
        }
        self.load_depth_mask();
        self.reconcile(qh);
    }

    fn load_depth_mask(&mut self) {
        let mask = if self.config.depth_enabled {
            prism_common::depth::current_mask().map(|p| p.display().to_string())
        } else {
            None
        };
        if mask != self.depth_mask {
            eprintln!("[prism-desktop] depth mask: {}", mask.as_deref().unwrap_or("none"));
            self.depth_fade = mask.as_ref().map(|_| Instant::now());
            self.depth_mask = mask;
        }
    }

    /// Hide the parts of a widget that the wallpaper's foreground covers (QML DepthMask with
    /// maskInverted): the screen-sized mask, cropped to the widget, is cut out with DEST_OUT.
    /// Returns true while the mask is still fading in.
    fn apply_depth_mask(&mut self, cr: *mut Cairo, rect: Rect) -> bool {
        let Some(path) = &self.depth_mask else { return false };
        let (sw, sh) = self.screen;
        let Some(img) = cached_image_cover(path, sw.round() as i32, sh.round() as i32) else { return false };
        let (iw, ih) = image_size(img);
        let k = match self.depth_fade {
            Some(t) => {
                let dur = self.style.duration_ms(DEPTH_FADE_MS);
                let k = if dur <= 0.0 { 1.0 } else { (t.elapsed().as_secs_f64() * 1000.0 / dur).min(1.0) };
                if k >= 1.0 {
                    self.depth_fade = None;
                }
                k
            }
            None => 1.0,
        };
        // InOutQuad, as the QML Behavior.
        let a = if k < 0.5 { 2.0 * k * k } else { 1.0 - (-2.0 * k + 2.0).powi(2) / 2.0 };
        unsafe {
            cairo_save(cr);
            cairo_set_operator(cr, CAIRO_OPERATOR_DEST_OUT);
            cairo_set_source_surface(cr, img, (sw - iw) / 2.0 - rect.x, (sh - ih) / 2.0 - rect.y);
            cairo_paint_with_alpha(cr, a);
            cairo_restore(cr);
        }
        self.depth_fade.is_some()
    }

    /// Request a redraw of one widget (coalesced to its next frame callback).
    fn mark(&mut self, id: WidgetId, qh: &QueueHandle<Self>) {
        if id == WidgetId::Mpris {
            self.media_base_dirty = true;
        }
        if let Some(i) = self.widgets.iter().position(|w| w.id == id) {
            self.widgets[i].dirty = true;
            if self.widgets[i].frame_cb.is_none() && self.widgets[i].configured {
                self.draw(i, qh);
            }
        }
    }

    // ── Time ──────────────────────────────────────────────────────────

    fn sync_time(&mut self, init: bool, qh: &QueueHandle<Self>) {
        let now = LocalTime::now();
        if self.last_second == Some(now.second) && !init {
            return;
        }
        let s = now.second;
        let target = s as f64 * 6.0;
        match self.last_second {
            Some(last) if !init => {
                let diff = (s + 60 - last) % 60;
                // Keep the angle increasing so the dot never spins backwards past 12.
                let to = self.second_to + diff as f64 * 6.0;
                if diff > 2 || !self.style.animations {
                    self.second_from = to;
                    self.second_to = to;
                    self.second_anim = None;
                } else if diff > 0 {
                    self.second_from = self.second_angle();
                    self.second_to = to;
                    self.second_anim = Some(Instant::now());
                }
            }
            _ => {
                self.second_from = target;
                self.second_to = target;
                self.second_anim = None;
            }
        }
        self.last_second = Some(s);
        if let Some(i) = self.widget_at(WidgetId::AnalogClock) {
            self.widgets[i].animating = self.second_anim.is_some();
        }
        self.mark(WidgetId::AnalogClock, qh);
    }

    fn second_angle(&self) -> f64 {
        match self.second_anim {
            Some(t) => {
                let p = (t.elapsed().as_secs_f64() * 1000.0 / self.style.duration_ms(ANIM_SLOW_MS).max(1.0)).min(1.0);
                self.second_from + (self.second_to - self.second_from) * out_cubic(p)
            }
            None => self.second_to,
        }
    }

    // ── D-Bus events ──────────────────────────────────────────────────

    fn on_dbus(&mut self, ev: DbusEvent, qh: &QueueHandle<Self>) {
        match ev {
            DbusEvent::Media(m) => {
                let was_playing = self.media.as_ref().map(|m| m.playing).unwrap_or(false);
                let now_playing = m.as_ref().map(|m| m.playing).unwrap_or(false);
                if was_playing != now_playing {
                    self.wave_from = self.wave_h;
                    self.wave_anim = Some(Instant::now());
                }
                let new_art = m.as_ref().and_then(|m| m.art_path.clone());
                if new_art != self.art_path {
                    self.art_path = new_art.clone();
                    self.art = new_art.as_ref().and_then(|p| cached_image(&p.to_string_lossy(), 512, 512, true));
                    self.palette = new_art
                        .as_ref()
                        .and_then(|p| sample_palette(p))
                        .unwrap_or_else(|| MediaPalette::fallback(&self.look));
                }
                self.media = m;
                self.mark(WidgetId::Mpris, qh);
            }
            DbusEvent::Tray(items) => {
                for ic in self.tray_icons.drain(..) {
                    if let Some(s) = ic.surface {
                        unsafe { cairo_surface_destroy(s) };
                    }
                }
                self.tray_icons = items.iter().map(|it| TrayIcon { surface: it.pixmap.as_ref().and_then(pixmap_surface) }).collect();
                let count_changed = self.tray.len() != items.len();
                self.tray = items;
                if count_changed {
                    self.reconcile(qh);
                } else {
                    self.mark(WidgetId::Tray, qh);
                }
            }
            DbusEvent::Menu { key, entries } => {
                if let Some((k, anchor)) = self.pending_menu_anchor.take() {
                    if k == key && !entries.is_empty() {
                        self.open_menu(key, entries, anchor, qh);
                    }
                }
            }
        }
    }

    // ── Drawing ───────────────────────────────────────────────────────

    fn draw(&mut self, i: usize, qh: &QueueHandle<Self>) {
        let (bw, bh) = self.widgets[i].size;
        if bw <= 0 || bh <= 0 {
            return;
        }
        let id = self.widgets[i].id;
        // Pick a free buffer of the right size (at most 2 per widget).
        self.widgets[i].bufs.retain(|b| b.w == bw && b.h == bh);
        let idx = match self.widgets[i].bufs.iter().position(|b| !b.busy) {
            Some(k) => k,
            None if self.widgets[i].bufs.len() < 2 => {
                let k = self.widgets[i].bufs.len();
                let Some(shm) = &self.shm else { return };
                let Some(b) = ShmBuf::new(shm, qh, bw, bh, BufTag(id, k)) else { return };
                self.widgets[i].bufs.push(b);
                k
            }
            None => {
                self.widgets[i].dirty = true;
                return; // both buffers held by the compositor; retry on release / frame
            }
        };

        let (w, h) = (bw as f64, bh as f64);
        let surf = self.widgets[i].bufs[idx].cairo();
        let cr = unsafe { cairo_create(surf) };
        unsafe {
            cairo_set_operator(cr, CAIRO_OPERATOR_CLEAR);
            cairo_paint(cr);
            cairo_set_operator(cr, 2);
        }
        let p = id.cfg(&self.config.desktop_layout).clone();
        let mut animating = false;
        let hits = match id {
            WidgetId::Clock => {
                draw_clock(cr, w, h, &p, &self.look, &LocalTime::now());
                Vec::new()
            }
            WidgetId::AnalogClock => {
                let now = LocalTime::now();
                let hands = ClockHands {
                    hour: ((now.hour % 12) as f64 + now.minute as f64 / 60.0) * 30.0,
                    minute: (now.minute as f64 + now.second as f64 / 60.0) * 6.0,
                    second: self.second_angle(),
                };
                if let Some(t) = self.second_anim {
                    if t.elapsed().as_secs_f64() * 1000.0 >= self.style.duration_ms(ANIM_SLOW_MS) {
                        self.second_anim = None;
                    } else {
                        animating = true;
                    }
                }
                draw_analog_clock(cr, w, h, &p, &self.look, &hands);
                Vec::new()
            }
            WidgetId::Calendar => draw_calendar(cr, w, h, &p, &self.look, &self.cal, &LocalTime::now()),
            WidgetId::Mpris => {
                let (hits, anim) = self.draw_media(cr, w, h, &p);
                animating = anim;
                hits
            }
            WidgetId::Tray => {
                let (_, vertical, _) = tray_rect(&p, self.tray.len(), self.screen.0, self.screen.1);
                let hovered = match (self.ptr_on, self.hover_hit) {
                    (PointerOn::Widget(WidgetId::Tray), Some(Hit::Tray(k))) => Some(k),
                    _ => None,
                };
                draw_tray(cr, w, h, &p, &self.look, &self.tray, &self.tray_icons, vertical, hovered, false)
            }
            WidgetId::Shortcuts => {
                let r = Rect { x: 0.0, y: 0.0, w, h };
                let hovered = match (self.ptr_on, self.hover_hit) {
                    (PointerOn::Widget(WidgetId::Shortcuts), Some(Hit::Shortcut(k))) => Some(k),
                    _ => None,
                };
                draw_shortcuts(cr, w, h, &p, &self.look, shortcuts_vertical(&p, &r), hovered, self.pressed, false)
            }
        };
        // Only widgets sent to the background pass behind the wallpaper scenery (QML DepthMask).
        if p.send_to_background {
            let rect = self.widgets[i].rect;
            animating |= self.apply_depth_mask(cr, rect);
        }
        unsafe {
            cairo_destroy(cr);
            cairo_surface_destroy(surf);
        }

        let ws = &mut self.widgets[i];
        ws.hits = hits;
        ws.dirty = false;
        ws.animating = animating;
        let buf = &mut ws.bufs[idx];
        buf.busy = true;
        ws.surface.attach(Some(&buf.buffer), 0, 0);
        ws.surface.damage_buffer(0, 0, bw, bh);
        if ws.frame_cb.is_none() {
            ws.frame_cb = Some(ws.surface.frame(qh, id));
        }
        ws.surface.commit();
    }

    /// Media widget: art/text/controls come from a cached base layer redrawn only when the
    /// track or layout changes; the wave/progress strip is drawn over it every frame.
    fn draw_media(&mut self, cr: *mut Cairo, w: f64, h: f64, p: &prism_common::config::WidgetPosition) -> (Hits, bool) {
        // Advance animation clocks by real time.
        let now = Instant::now();
        let dt = self.last_media_frame.map(|t| now.duration_since(t).as_secs_f64().min(0.1)).unwrap_or(0.0);
        self.last_media_frame = Some(now);
        let playing = self.media.as_ref().map(|m| m.playing).unwrap_or(false);
        let target = if playing { 1.0 } else { 0.0 };
        if let Some(t0) = self.wave_anim {
            let dur = self.style.duration_ms(if playing { 700.0 } else { 500.0 });
            let k = if dur <= 0.0 { 1.0 } else { (t0.elapsed().as_secs_f64() * 1000.0 / dur).min(1.0) };
            self.wave_h = self.wave_from + (target - self.wave_from) * out_cubic(k);
            if k >= 1.0 {
                self.wave_anim = None;
            }
        } else {
            self.wave_h = target;
        }
        if playing || self.wave_h > 0.001 {
            self.wave_time += dt;
        }
        let position = self
            .media
            .as_ref()
            .map(|m| {
                let p = m.position_s + if m.playing { m.sampled_at.elapsed().as_secs_f64() } else { 0.0 };
                if m.length_s > 0.0 { p.min(m.length_s) } else { p }
            })
            .unwrap_or(0.0);

        let (bw, bh) = (w as i32, h as i32);
        let stale = !matches!(self.media_base, Some((_, cw, ch)) if cw == bw && ch == bh);
        if stale {
            if let Some((s, _, _)) = self.media_base.take() {
                unsafe { cairo_surface_destroy(s) };
            }
            self.media_base = Some((unsafe { cairo_image_surface_create(CAIRO_FORMAT_ARGB32, bw, bh) }, bw, bh));
            self.media_base_dirty = true;
        }
        let (base, _, _) = self.media_base.unwrap();
        let view = MediaView {
            media: self.media.as_ref(),
            art: self.art,
            palette: self.palette,
            wave_time: self.wave_time,
            wave_height: self.wave_h,
            position,
        };
        let mut hits = Hits::new();
        if self.media_base_dirty {
            let bcr = unsafe { cairo_create(base) };
            unsafe {
                cairo_set_operator(bcr, CAIRO_OPERATOR_CLEAR);
                cairo_paint(bcr);
                cairo_set_operator(bcr, 2);
            }
            let (h2, canvas) = draw_mpris_base(bcr, w, h, p, &self.look, &view);
            hits = h2;
            self.media_canvas = canvas;
            unsafe { cairo_destroy(bcr) };
            self.media_base_dirty = false;
        } else if let Some(i) = self.widgets.iter().position(|x| x.id == WidgetId::Mpris) {
            hits = self.widgets[i].hits.clone();
        }
        unsafe {
            cairo_set_source_surface(cr, base, 0.0, 0.0);
            cairo_paint(cr);
        }
        let sf = (w / 400.0).min(h / 170.0).max(0.55);
        let c = self.media_canvas;
        draw_wave_progress(cr, c.x, c.y, c.w, c.h, sf, &self.look, &view);
        let animating = playing || self.wave_anim.is_some();
        if !animating {
            self.last_media_frame = None;
        }
        (hits, animating)
    }

    // ── Input ─────────────────────────────────────────────────────────

    fn widget_at(&self, id: WidgetId) -> Option<usize> {
        self.widgets.iter().position(|w| w.id == id)
    }

    fn hit_at(&self, id: WidgetId, x: f64, y: f64) -> Option<Hit> {
        let i = self.widget_at(id)?;
        self.widgets[i].hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)
    }

    fn set_cursor(&mut self, qh: &QueueHandle<Self>, shape: CursorShape) {
        if self.cursor_dev.is_none() {
            if let (Some(m), Some(p)) = (&self.cursor_mgr, &self.pointer) {
                self.cursor_dev = Some(m.get_pointer(p, qh, ()));
            }
        }
        if let Some(d) = &self.cursor_dev {
            d.set_shape(self.ptr_serial, shape);
        }
    }

    fn on_motion(&mut self, qh: &QueueHandle<Self>) {
        match self.ptr_on {
            PointerOn::Widget(id) => {
                let hit = self.hit_at(id, self.ptr_pos.0, self.ptr_pos.1);
                if hit != self.hover_hit {
                    self.hover_hit = hit;
                    self.set_cursor(qh, if hit.is_some() { CursorShape::Pointer } else { CursorShape::Default });
                    if matches!(id, WidgetId::Tray | WidgetId::Shortcuts) {
                        self.mark(id, qh);
                    }
                }
            }
            PointerOn::Menu => {
                let hover = self.menu.as_ref().and_then(|m| {
                    m.rows.iter().find(|(_, r)| r.contains(self.ptr_pos.0, self.ptr_pos.1)).map(|(k, _)| *k)
                });
                if let Some(m) = self.menu.as_mut() {
                    if m.hover != hover {
                        m.hover = hover;
                        self.draw_menu(qh);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_click(&mut self, button: u32, qh: &QueueHandle<Self>) {
        match self.ptr_on {
            PointerOn::Catcher => self.close_menu(),
            PointerOn::Menu => {
                let Some(m) = &self.menu else { return };
                let pick = m.rows.iter().find(|(_, r)| r.contains(self.ptr_pos.0, self.ptr_pos.1)).map(|(k, _)| *k);
                if let Some(k) = pick {
                    let e = &m.entries[k];
                    if e.enabled && !e.separator && !e.is_submenu {
                        let _ = self.dbus.cmds.send(DbusCmd::TrayMenuClick { key: m.key.clone(), id: e.id });
                        self.close_menu();
                    }
                }
            }
            PointerOn::Widget(id) => {
                let Some(hit) = self.hit_at(id, self.ptr_pos.0, self.ptr_pos.1) else { return };
                match hit {
                    Hit::CalPrev | Hit::CalNext => {
                        let delta: i32 = if hit == Hit::CalPrev { -1 } else { 1 };
                        let m = self.cal.month as i32 + delta;
                        self.cal.year += m.div_euclid(12);
                        self.cal.month = m.rem_euclid(12) as u32;
                        self.mark(WidgetId::Calendar, qh);
                    }
                    Hit::CalMonthTitle => {
                        self.cal.view = if self.cal.view == CalView::Months { CalView::Days } else { CalView::Months };
                        self.mark(WidgetId::Calendar, qh);
                    }
                    Hit::CalYearTitle => {
                        self.cal.view = if self.cal.view == CalView::Years { CalView::Days } else { CalView::Years };
                        self.mark(WidgetId::Calendar, qh);
                    }
                    Hit::CalPickMonth(mo) => {
                        self.cal.month = mo;
                        self.cal.view = CalView::Days;
                        self.mark(WidgetId::Calendar, qh);
                    }
                    Hit::CalPickYear(y) => {
                        self.cal.year = y;
                        self.cal.view = CalView::Days;
                        self.mark(WidgetId::Calendar, qh);
                    }
                    Hit::MediaPrev => {
                        let _ = self.dbus.cmds.send(DbusCmd::Media("Previous"));
                    }
                    Hit::MediaPlayPause => {
                        let _ = self.dbus.cmds.send(DbusCmd::Media("PlayPause"));
                    }
                    Hit::MediaNext => {
                        let _ = self.dbus.cmds.send(DbusCmd::Media("Next"));
                    }
                    Hit::Tray(k) => self.tray_click(k, button, qh),
                    Hit::Shortcut(k) => {
                        let args: &[&str] = match k {
                            0 => &["launcher", "toggle"],
                            1 => &["notif", "toggle"],
                            2 => &["settings", "toggle"],
                            _ => &["power", "toggle"],
                        };
                        if let Ok(c) = std::process::Command::new("prism-ctl").args(args).spawn() {
                            crate::reap(c);
                        }
                    }
                }
            }
            PointerOn::None => {}
        }
    }

    fn tray_click(&mut self, k: usize, button: u32, _qh: &QueueHandle<Self>) {
        let Some(item) = self.tray.get(k).cloned() else { return };
        let Some(i) = self.widget_at(WidgetId::Tray) else { return };
        let wrect = self.widgets[i].rect;
        let icon = self.widgets[i].hits.iter().find(|(h, _)| *h == Hit::Tray(k)).map(|(_, r)| *r).unwrap_or_default();
        let screen_icon = Rect { x: wrect.x + icon.x, y: wrect.y + icon.y, w: icon.w, h: icon.h };
        let (x, y) = ((screen_icon.x + screen_icon.w / 2.0) as i32, (screen_icon.y + screen_icon.h) as i32);
        const BTN_LEFT: u32 = 0x110;
        const BTN_RIGHT: u32 = 0x111;
        const BTN_MIDDLE: u32 = 0x112;
        let want_menu = button == BTN_RIGHT || (button == BTN_LEFT && item.item_is_menu);
        if want_menu && item.has_menu {
            self.pending_menu_anchor = Some((item.key.clone(), screen_icon));
            let _ = self.dbus.cmds.send(DbusCmd::TrayMenu { key: item.key });
        } else if button == BTN_MIDDLE || button == BTN_RIGHT {
            let _ = self.dbus.cmds.send(DbusCmd::TraySecondary { key: item.key, x, y });
        } else {
            let _ = self.dbus.cmds.send(DbusCmd::TrayActivate { key: item.key, x, y });
        }
    }

    // ── Tray context menu ─────────────────────────────────────────────

    fn open_menu(&mut self, key: String, entries: Vec<MenuEntry>, anchor: Rect, qh: &QueueHandle<Self>) {
        self.close_menu();
        let (Some(comp), Some(ls)) = (self.compositor.clone(), self.layer_shell.clone()) else { return };
        // Size: widest label, fixed row heights (QML popup menu metrics).
        let probe = unsafe { cairo_image_surface_create(CAIRO_FORMAT_ARGB32, 1, 1) };
        let pcr = unsafe { cairo_create(probe) };
        let st = text::TextStyle { family: &self.look.family, px: 13.0, bold: false, color: self.look.fg };
        let mut width: f64 = 180.0;
        for e in &entries {
            let (tw, _) = text::measure(pcr, &e.label, &st);
            width = width.max(tw + 56.0 + e.depth as f64 * 14.0);
        }
        unsafe {
            cairo_destroy(pcr);
            cairo_surface_destroy(probe);
        }
        let width = width.min(420.0).round();
        let height: f64 = 12.0 + entries.iter().map(|e| if e.separator { 9.0 } else { 30.0 }).sum::<f64>();
        let (sw, sh) = self.screen;
        let mut x = (anchor.x + anchor.w / 2.0 - width / 2.0).clamp(4.0, sw - width - 4.0);
        let mut y = anchor.y + anchor.h + 6.0;
        if y + height > sh - 4.0 {
            y = anchor.y - height - 6.0;
        }
        x = x.round();
        y = y.max(4.0).round();
        let rect = Rect { x, y, w: width, h: height };

        // Transparent full-screen catcher closes the menu on an outside click.
        let catcher = comp.create_surface(qh, ());
        let catcher_layer = ls.get_layer_surface(&catcher, None, Layer::Top, "prism-scrim".to_string(), qh, CatcherTag);
        catcher_layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        catcher_layer.set_exclusive_zone(-1);
        catcher.commit();

        let surface = comp.create_surface(qh, ());
        let layer = ls.get_layer_surface(&surface, None, Layer::Overlay, self.style.namespace("prism-modal"), qh, MenuTag);
        layer.set_anchor(Anchor::Top | Anchor::Left);
        layer.set_exclusive_zone(-1);
        layer.set_size(width as u32, height as u32);
        layer.set_margin(y as i32, 0, 0, x as i32);
        surface.commit();
        self.menu = Some(Menu {
            key,
            entries,
            rect,
            surface,
            layer,
            catcher,
            catcher_layer,
            catcher_vp: None,
            catcher_buf: None,
            buf: None,
            hover: None,
            rows: Vec::new(),
        });
    }

    fn close_menu(&mut self) {
        if let Some(m) = self.menu.take() {
            m.layer.destroy();
            m.surface.destroy();
            m.catcher_layer.destroy();
            m.catcher.destroy();
            if let Some(b) = m.catcher_buf {
                b.destroy();
            }
            if let Some(v) = m.catcher_vp {
                v.destroy();
            }
        }
    }

    fn draw_menu(&mut self, qh: &QueueHandle<Self>) {
        let Some(shm) = self.shm.clone() else { return };
        let look = self.look.clone();
        let radius = self.style.radius.min(14.0);
        let Some(m) = self.menu.as_mut() else { return };
        let (w, h) = (m.rect.w as i32, m.rect.h as i32);
        if m.buf.as_ref().map(|b| b.busy || b.w != w || b.h != h).unwrap_or(true) {
            m.buf = ShmBuf::new(&shm, qh, w, h, BufTag(WidgetId::Tray, 99));
        }
        let Some(buf) = m.buf.as_mut() else { return };
        let surf = buf.cairo();
        let cr = unsafe { cairo_create(surf) };
        unsafe {
            cairo_set_operator(cr, CAIRO_OPERATOR_CLEAR);
            cairo_paint(cr);
            cairo_set_operator(cr, 2);
        }
        fill_rounded_rect(cr, 0.0, 0.0, w as f64, h as f64, radius, look.card);
        stroke_rounded_rect(cr, 0.5, 0.5, w as f64 - 1.0, h as f64 - 1.0, radius, (look.fg.0, look.fg.1, look.fg.2, 0.12), 1.0);
        let mut y = 6.0;
        m.rows.clear();
        for (k, e) in m.entries.iter().enumerate() {
            if e.separator {
                unsafe {
                    cairo_set_source_rgba(cr, look.fg.0, look.fg.1, look.fg.2, 0.15);
                    cairo_rectangle(cr, 12.0, y + 4.0, w as f64 - 24.0, 1.0);
                    cairo_fill(cr);
                }
                y += 9.0;
                continue;
            }
            let r = Rect { x: 6.0, y, w: w as f64 - 12.0, h: 30.0 };
            if m.hover == Some(k) && e.enabled && !e.is_submenu {
                fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 8.0, (look.primary.0, look.primary.1, look.primary.2, 0.22));
            }
            let alpha = if e.enabled { 1.0 } else { 0.45 };
            let color = (look.fg.0, look.fg.1, look.fg.2, alpha);
            let indent = 14.0 * e.depth as f64;
            if let Some(checked) = e.toggle {
                let glyph = if checked { "\u{f012c}" } else { "" };
                text::draw_in_box(cr, glyph, &text::TextStyle { family: text::ICON_FONT, px: 14.0, bold: false, color }, r.x + 8.0 + indent, r.y, 18.0, r.h, text::Align::Start);
            }
            let st = text::TextStyle { family: &look.family, px: 13.0, bold: e.is_submenu, color };
            text::draw_in_box(cr, &e.label, &st, r.x + 32.0 + indent, r.y, r.w - 40.0 - indent, r.h, text::Align::Start);
            m.rows.push((k, r));
            y += 30.0;
        }
        unsafe {
            cairo_destroy(cr);
            cairo_surface_destroy(surf);
        }
        buf.busy = true;
        m.surface.attach(Some(&buf.buffer), 0, 0);
        m.surface.damage_buffer(0, 0, w, h);
        m.surface.commit();
    }
}

/// Decode a 32x32 sample of the album art and derive the wave palette from it.
fn sample_palette(path: &std::path::Path) -> Option<MediaPalette> {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let pb = gdk_pixbuf_new_from_file_at_scale(c.as_ptr(), 32, 32, 0, std::ptr::null_mut());
        if pb.is_null() {
            return None;
        }
        let (w, h) = (gdk_pixbuf_get_width(pb), gdk_pixbuf_get_height(pb));
        let stride = gdk_pixbuf_get_rowstride(pb) as usize;
        let n = gdk_pixbuf_get_n_channels(pb) as usize;
        let data = gdk_pixbuf_get_pixels(pb);
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = data.add(y * stride + x * n);
                let a = if n == 4 { *p.add(3) } else { 255 };
                px.push((*p, *p.add(1), *p.add(2), a));
            }
        }
        g_object_unref(pb as *mut std::ffi::c_void);
        palette_from_pixels(&px)
    }
}

/// Copy an SNI IconPixmap into an owned cairo image surface.
fn pixmap_surface(pm: &(i32, i32, Vec<u8>)) -> Option<*mut CairoSurface> {
    let (w, h, data) = pm;
    unsafe {
        let s = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, *w, *h);
        cairo_surface_flush(s);
        let dst = cairo_image_surface_get_data(s);
        let stride = cairo_image_surface_get_stride(s) as usize;
        if dst.is_null() {
            cairo_surface_destroy(s);
            return None;
        }
        for row in 0..*h as usize {
            std::ptr::copy_nonoverlapping(data.as_ptr().add(row * *w as usize * 4), dst.add(row * stride), *w as usize * 4);
        }
        cairo_surface_mark_dirty(s);
        Some(s)
    }
}

// ── inotify: config & palette changes ─────────────────────────────────────

struct Inotify {
    fd: i32,
    files: Vec<std::ffi::OsString>,
}

impl Inotify {
    fn new() -> Option<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let mut files = Vec::new();
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        for path in [Config::config_path(), Theme::source_path(), prism_common::depth::mask_pointer_path()] {
            if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
                files.push(name.to_os_string());
                if !dirs.iter().any(|d| d == dir) {
                    dirs.push(dir.to_path_buf());
                }
            }
        }
        for dir in dirs {
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(c) = std::ffi::CString::new(dir.to_string_lossy().as_bytes()) {
                unsafe {
                    libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE);
                }
            }
        }
        Some(Inotify { fd, files })
    }

    /// Drain pending events; true if one of the watched files changed.
    fn changed(&self) -> bool {
        let mut buf = [0u8; 4096];
        let mut hit = false;
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            let mut off = 0usize;
            while off + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                let ev = unsafe { &*(buf.as_ptr().add(off) as *const libc::inotify_event) };
                let name_start = off + std::mem::size_of::<libc::inotify_event>();
                let name_bytes = &buf[name_start..name_start + ev.len as usize];
                let name = name_bytes.split(|b| *b == 0).next().unwrap_or(&[]);
                if self.files.iter().any(|f| f.as_encoded_bytes() == name) {
                    hit = true;
                }
                off = name_start + ev.len as usize;
            }
        }
        hit
    }
}

// ── Wayland dispatch ──────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for Desk {
    fn event(d: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_compositor" => d.compositor = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_shm" => d.shm = Some(reg.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => d.layer_shell = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = reg.bind(name, version.min(5), qh, ());
                    d.pointer = Some(seat.get_pointer(qh, ()));
                    d.seat = Some(seat);
                }
                "wl_output" if d.output_mode.0 == 0 => {
                    let _o: wl_output::WlOutput = reg.bind(name, version.min(3), qh, ());
                }
                "wp_cursor_shape_manager_v1" => d.cursor_mgr = Some(reg.bind(name, 1, qh, ())),
                "wp_single_pixel_buffer_manager_v1" => d.single_pixel = Some(reg.bind(name, 1, qh, ())),
                "wp_viewporter" => d.viewporter = Some(reg.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Desk {
    fn event(d: &mut Self, _: &wl_output::WlOutput, ev: wl_output::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } => {
                if f.contains(wl_output::Mode::Current) && d.output_mode.0 == 0 {
                    d.output_mode = (width, height);
                    d.update_screen();
                }
            }
            wl_output::Event::Scale { factor } => {
                d.output_scale = factor;
                d.update_screen();
            }
            wl_output::Event::Done
                if !d.widgets.is_empty() => {
                    d.reconcile(qh);
                }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, WidgetId> for Desk {
    fn event(d: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, id: &WidgetId, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                if let Some(i) = d.widget_at(*id) {
                    let w = &mut d.widgets[i];
                    w.size = (width.max(1) as i32, height.max(1) as i32);
                    w.configured = true;
                    w.dirty = true;
                    if *id == WidgetId::Mpris {
                        d.media_base_dirty = true;
                    }
                    if d.widgets[i].frame_cb.is_none() {
                        d.draw(i, qh);
                    }
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if let Some(i) = d.widget_at(*id) {
                    let w = d.widgets.remove(i);
                    w.layer.destroy();
                    w.surface.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, MenuTag> for Desk {
    fn event(d: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &MenuTag, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                ls.ack_configure(serial);
                d.draw_menu(qh);
            }
            zwlr_layer_surface_v1::Event::Closed => d.close_menu(),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, CatcherTag> for Desk {
    fn event(d: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &CatcherTag, _: &Connection, qh: &QueueHandle<Self>) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = ev {
            ls.ack_configure(serial);
            let (Some(sp), Some(vp)) = (d.single_pixel.clone(), d.viewporter.clone()) else { return };
            if let Some(m) = d.menu.as_mut() {
                if m.catcher_vp.is_none() {
                    m.catcher_vp = Some(vp.get_viewport(&m.catcher, qh, ()));
                }
                if let Some(v) = &m.catcher_vp {
                    v.set_destination(width.max(1) as i32, height.max(1) as i32);
                }
                let buf = sp.create_u32_rgba_buffer(0, 0, 0, 0, qh, BufTag(WidgetId::Tray, 98));
                m.catcher.attach(Some(&buf), 0, 0);
                m.catcher.damage_buffer(0, 0, 1, 1);
                m.catcher.commit();
                if let Some(old) = m.catcher_buf.replace(buf) {
                    old.destroy();
                }
            }
        }
    }
}

impl Dispatch<wl_callback::WlCallback, WidgetId> for Desk {
    fn event(d: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, id: &WidgetId, _: &Connection, qh: &QueueHandle<Self>) {
        if let Some(i) = d.widget_at(*id) {
            d.widgets[i].frame_cb = None;
            if d.widgets[i].dirty || d.widgets[i].animating {
                d.draw(i, qh);
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, BufTag> for Desk {
    fn event(d: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, tag: &BufTag, _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            if tag.1 == 99 {
                if let Some(b) = d.menu.as_mut().and_then(|m| m.buf.as_mut()) {
                    if &b.buffer == buffer {
                        b.busy = false;
                    }
                }
                return;
            }
            if let Some(i) = d.widget_at(tag.0) {
                for b in d.widgets[i].bufs.iter_mut() {
                    if &b.buffer == buffer {
                        b.busy = false;
                    }
                }
                if d.widgets[i].dirty && d.widgets[i].frame_cb.is_none() {
                    d.draw(i, qh);
                }
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Desk {
    fn event(d: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                d.ptr_serial = serial;
                d.ptr_pos = (surface_x, surface_y);
                d.ptr_on = if let Some(w) = d.widgets.iter().find(|w| w.surface == surface) {
                    PointerOn::Widget(w.id)
                } else if d.menu.as_ref().map(|m| m.surface == surface).unwrap_or(false) {
                    PointerOn::Menu
                } else if d.menu.as_ref().map(|m| m.catcher == surface).unwrap_or(false) {
                    PointerOn::Catcher
                } else {
                    PointerOn::None
                };
                d.hover_hit = None;
                d.on_motion(qh);
            }
            wl_pointer::Event::Leave { .. } => {
                let prev = d.ptr_on;
                d.ptr_on = PointerOn::None;
                d.hover_hit = None;
                d.pressed = false;
                if let PointerOn::Widget(id) = prev {
                    if matches!(id, WidgetId::Tray | WidgetId::Shortcuts) {
                        d.mark(id, qh);
                    }
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                d.ptr_pos = (surface_x, surface_y);
                d.on_motion(qh);
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                d.pressed = pressed;
                if let PointerOn::Widget(WidgetId::Shortcuts) = d.ptr_on {
                    d.mark(WidgetId::Shortcuts, qh);
                }
                if !pressed {
                    d.on_click(button, qh);
                }
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Desk {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
    wl_seat::WlSeat,
    ZwlrLayerShellV1,
    WpCursorShapeManagerV1,
    WpCursorShapeDeviceV1,
    WpSinglePixelBufferManagerV1,
    WpViewporter,
    WpViewport
);
