use std::env;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::process::{exit, Command};

mod apps_watch;
mod background;
mod cal_events;
mod caffeinate;
mod displays;
mod capture;
mod cairo_backend;
mod desktop;
mod device_battery;
mod icons;
mod clipboard;
mod emoji;
mod keybinds;
mod launcher;
mod launcher_themes;
mod nc;
mod osd;
mod phone;
mod phone_client;
mod keyring_prompter;
mod notes;
mod picker;
mod privacy;
mod weather;
mod pw_watch;
mod meters;
mod quick;
mod xdp;
mod rec_indicator;
mod downloads_pill;
mod nm_agent;
mod polkit_agent;
mod power;
mod prompt;
mod portal;
mod settings;
mod shadow;
mod spotlight;
mod toasts;
mod trim;
mod download_confirm;
mod downloads_card;
mod compedit;
mod guide;
mod status;
mod status_tray;
mod window_watch;
mod wintrack;
mod svg;
mod switcher;
mod timer_client;
mod tray;
mod ui;
mod uikit;
mod view;
mod wallpaper;
mod waker;

use ui::{Motion, Pose, Style, Transition};

use waker::Waker;
use wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1;
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

/// Wait for a fire-and-forget child process on a helper thread so it never becomes a zombie.
fn reap(child: std::process::Child) {
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
}

/// User data tag for the full-screen dim ("scrim") layer surface, so its configure events are
/// handled separately from the card surface.
struct ScrimTag;
/// User data tag for the blurred surface under the power menu's scrim.
struct BackdropTag;

/// Clear full-screen surface the compositor blurs what is behind (see `prism-scrim-blur`).
struct Backdrop {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    buffer: Option<wl_buffer::WlBuffer>,
}


use prism_common::apps::{load_all_desktop_apps, DesktopApp};
use prism_common::clipboard::ClipboardStore;
use prism_common::emoji::{load_emojis, EmojiItem};
use prism_common::{Config, Theme};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::alpha_modifier::v1::client::{wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1, wp_alpha_modifier_v1::WpAlphaModifierV1};

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
enum UiMode {
    Osd { kind: String, value: f32 },
    Launcher,
    Spotlight,
    Wallpaper,
    Clipboard,
    Emoji,
    Settings,
    Desktop,
    DesktopEdit,
    NotificationCenter,
    Power,
    Keybinds,
    Notes,
    Phone,
    /// Password / secret prompt for the polkit and NetworkManager agents (prompt.rs).
    Prompt,
    /// Trim a recording (trim.rs).
    Trim,
    /// Confirm a browser download (download_confirm.rs).
    DownloadConfirm,
    /// Prism's downloads (downloads_card.rs).
    Downloads,
    /// Shortcuts and Window Rules editor (compedit.rs).
    CompEdit,
    /// First-time setup guide (guide.rs).
    Guide,
}

struct UiState {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    outputs: Vec<wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    mode: UiMode,
    configured: bool,
    width: u32,
    height: u32,
    exit_requested: bool,
    apps: Vec<DesktopApp>,
    pointer_x: i32,
    /// The scroll deltas being delivered come from a touchpad (wl_pointer.axis_source).
    scroll_finger: bool,
    pointer_y: i32,
    pointer_down: bool,
    card_bounds: (i32, i32, i32, i32),
    theme: Theme,
    config: Config,
    frame_callback: Option<wl_callback::WlCallback>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    pointer_serial: u32,
    needs_render: bool,
    buffer_pool: Vec<BufferSlot>,

    // ── Stage-0 performance infrastructure ──────────────────────────────
    single_pixel_mgr: Option<WpSinglePixelBufferManagerV1>,
    viewporter: Option<WpViewporter>,
    /// Full-screen dim behind modal cards: 1x1 single-pixel buffer scaled by wp_viewporter,
    /// committed once, never redrawn, on an un-blurred namespace.
    scrim_surface: Option<wl_surface::WlSurface>,
    backdrop: Option<Backdrop>,
    scrim_layer: Option<ZwlrLayerSurfaceV1>,
    scrim_viewport: Option<WpViewport>,
    scrim_buffer: Option<wl_buffer::WlBuffer>,
    /// Compositor-side opacity of the card surface (wp_alpha_modifier_v1). Fading the whole
    /// surface fades the compositor's blur behind it too; fading only the pixels left the blur
    /// rectangle behind until the surface was destroyed.
    alpha_surface: Option<WpAlphaModifierSurfaceV1>,
    /// Drop shadow under the card (subsurface of the scrim, see shadow.rs).
    shadow: Option<shadow::Shadow>,
    /// Card pose to apply to the shadow after this frame.
    shadow_pending: Option<Pose>,
    alpha_multiplier: u32,
    /// The card's layer surface is scaled about its centre during the scale transitions, its
    /// buffer stretched by this viewport.
    card_viewport: Option<WpViewport>,
    /// Layer surface geometry last sent (x, y, w, h).
    geom: (i32, i32, i32, i32),
    /// A buffer of the current card size is attached (geometry-only frames may reuse it).
    buffer_committed: bool,
    scrim_configured: bool,
    /// True when the main surface is sized to the card instead of the whole screen.
    card_surface: bool,
    /// Current main-surface rectangle in screen coordinates (x, y, w, h).
    surf_rect: (i32, i32, i32, i32),
    /// Screen-space origin of the surface the pointer is currently over.
    /// True while the pointer is over the scrim (at the screen origin), false over the card.
    pointer_on_scrim: bool,
    /// The pointer moved during the card's transition; hover catches up when it ends.
    hover_pending: bool,
    /// True while the last drawn frame was mid-animation (another frame is wanted).
    animating: bool,
    last_frame_at: Option<std::time::Instant>,
    /// Seconds since the previous presented frame (clamped), for frame-rate independent easing.
    frame_dt: f64,
    osd_rx: Option<std::sync::mpsc::Receiver<(String, f32)>>,
    /// Volume/brightness OSD state (osd.rs) when this overlay is the OSD.
    osd: Option<osd::OsdView>,
    /// Bottom margin last given to the OSD layer surface (the OSD slides by its margin).
    osd_margin: i32,
    /// Keyboard layout state from the compositor's keymap (wl_keyboard.keymap). Keys are
    /// translated through it instead of assuming raw US-layout evdev codes.
    xkb_context: xkbcommon::xkb::Context,
    xkb_state: Option<xkbcommon::xkb::State>,
    /// Key repeat from wl_keyboard.repeat_info (keys per second, delay in ms) and the held key.
    repeat_rate: i32,
    repeat_delay: i32,
    repeat: Option<(u32, std::time::Instant)>,

    // ── Global UI ───────────────────────────────────────────────────────
    /// Look & motion shared by every card (see ui.rs), rebuilt when config/theme change.
    style: Style,
    /// Enter/exit transition of the card surface (composited from `card_cache`).
    transition: Option<Transition>,
    /// How this card enters and leaves (the exit reuses it after the enter has finished).
    motion: Motion,
    /// Exit transition running; the process exits when it finishes.
    closing: bool,
    /// Card content must be re-rendered (a redraw was requested). Transition-only frames
    /// re-composite the cached card without re-rendering text and images.
    content_dirty: bool,
    /// The last content render was mid in-card animation (spotlight height, NC tiles...).
    content_animating: bool,
    /// Blink phase of the text caret in the last rendered content (None: no caret shown).
    caret: Option<u64>,
    card_cache: Option<(*mut cairo_backend::CairoSurface, i32, i32)>,
    /// Current scrim level (0..1 of dim_opacity); -1 = nothing attached yet.
    scrim_shown: f64,
    scrim_pending: Option<f64>,
    /// Settings window state (settings.rs) when this overlay is the Settings card.
    settings: Option<settings::SettingsView>,
    /// Wallpaper selector state (wallpaper.rs) when this overlay is the wallpaper picker.
    wallpaper: Option<wallpaper::WallpaperView>,
    /// Notification center (nc/) when this overlay is the control centre.
    nc: Option<nc::NcView>,
    /// App launcher (launcher.rs) / spotlight search (spotlight.rs).
    /// Card views behind the CardView trait (view.rs): launcher, spotlight, power, clipboard,
    /// emoji, keybinds.
    view: Option<Box<dyn view::CardView>>,
}

pub struct BufferSlot {
    pub buffer: wl_buffer::WlBuffer,
    pub ptr: *mut u8,
    pub size: usize,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub busy: bool,
}

impl BufferSlot {
    pub fn destroy(self) {
        self.buffer.destroy();
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
        }
    }
}

impl Drop for UiState {
    fn drop(&mut self) {
        for slot in self.buffer_pool.drain(..) {
            slot.destroy();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for UiState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version: _,
        } = event
        {
            if interface == "wl_compositor" {
                state.compositor = Some(registry.bind::<wl_compositor::WlCompositor, _, _>(name, 4, qh, ()));
            } else if interface == "wl_shm" {
                state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ()));
            } else if interface == "wl_output" {
                let output = registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, ());
                state.outputs.push(output);
            } else if interface == "wl_seat" {
                let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, 5, qh, ());
                state.keyboard = Some(seat.get_keyboard(qh, ()));
                state.pointer = Some(seat.get_pointer(qh, ()));
                state.seat = Some(seat);
            } else if interface == "zwlr_layer_shell_v1" {
                eprintln!("[prism-ui] Bound zwlr_layer_shell_v1");
                state.layer_shell = Some(registry.bind::<ZwlrLayerShellV1, _, _>(name, 1, qh, ()));
            } else if interface == "wp_single_pixel_buffer_manager_v1" {
                state.single_pixel_mgr = Some(registry.bind::<WpSinglePixelBufferManagerV1, _, _>(name, 1, qh, ()));
            } else if interface == "wp_viewporter" {
                state.viewporter = Some(registry.bind::<WpViewporter, _, _>(name, 1, qh, ()));
            } else if interface == "wp_cursor_shape_manager_v1" {
                eprintln!("[prism-ui] Bound wp_cursor_shape_manager_v1");
                state.cursor_shape_manager = Some(registry.bind::<WpCursorShapeManagerV1, _, _>(name, 1, qh, ()));
            }
        }
    }
}

impl Dispatch<WpSinglePixelBufferManagerV1, ()> for UiState {
    fn event(_: &mut Self, _: &WpSinglePixelBufferManagerV1, _: <WpSinglePixelBufferManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpAlphaModifierSurfaceV1, ()> for UiState {
    fn event(_: &mut Self, _: &WpAlphaModifierSurfaceV1, _: <WpAlphaModifierSurfaceV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_subsurface::WlSubsurface, ()> for UiState {
    fn event(_: &mut Self, _: &wl_subsurface::WlSubsurface, _: <wl_subsurface::WlSubsurface as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WpViewporter, ()> for UiState {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewport, ()> for UiState {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrLayerSurfaceV1, BackdropTag> for UiState {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &BackdropTag,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer_surface.ack_configure(serial);
                let (Some(mgr), Some(vp)) = (state.single_pixel_mgr.clone(), state.viewporter.clone()) else { return };
                let Some(b) = state.backdrop.as_mut() else { return };
                let viewport = b.viewport.get_or_insert_with(|| vp.get_viewport(&b.surface, qh, ()));
                viewport.set_destination(width.max(1) as i32, height.max(1) as i32);
                if b.buffer.is_none() {
                    // Barely above zero: Mango does not blur behind a fully clear buffer.
                    let buffer = mgr.create_u32_rgba_buffer(0, 0, 0, u32::MAX / 255, qh, ());
                    b.surface.attach(Some(&buffer), 0, 0);
                    b.surface.damage_buffer(0, 0, 1, 1);
                    b.buffer = Some(buffer);
                }
                b.surface.commit();
            }
            zwlr_layer_surface_v1::Event::Closed => state.exit_requested = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ScrimTag> for UiState {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &ScrimTag,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer_surface.ack_configure(serial);
                // The scrim spans the output, so its size is the logical screen size that all
                // card layout math is expressed in.
                if width > 0 && height > 0 {
                    state.width = width;
                    state.height = height;
                    ui::set_screen_height(height as f64);
                    state.style.scale = ui::scale_for(state.config.ui_scale, height as f64);
                }
                state.scrim_configured = true;
                state.commit_scrim(qh);
            }
            zwlr_layer_surface_v1::Event::Closed => state.exit_requested = true,
            _ => {}
        }
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for UiState {
    fn event(_: &mut Self, _: &WpCursorShapeManagerV1, _: <WpCursorShapeManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for UiState {
    fn event(_: &mut Self, _: &WpCursorShapeDeviceV1, _: <WpCursorShapeDeviceV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrLayerShellV1, ()> for UiState {
    fn event(_: &mut Self, _: &ZwlrLayerShellV1, _: <ZwlrLayerShellV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for UiState {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer_surface.ack_configure(serial);
                state.configured = true;
                if state.card_surface {
                    // Card-sized surface: width/height stay the logical screen size. The surface
                    // rectangle is set by draw_frame together with its buffer; a configure can
                    // be for an older size, so it is not adopted here.
                    // Nothing changed for the content either: a new buffer size re-renders it
                    // via the cache size check in draw_frame.
                    let _ = (width, height);
                } else {
                    state.width = if width > 0 { width } else { state.width };
                    state.height = if height > 0 { height } else { state.height };
                    state.surf_rect = (0, 0, state.width as i32, state.height as i32);
                    state.content_dirty = true;
                }
                // A frame already in flight draws on its callback; never draw twice per refresh
                // (the OSD reconfigures every frame while it slides).
                if state.frame_callback.is_none() {
                    state.draw_frame(qh);
                } else {
                    state.needs_render = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                eprintln!("[prism-ui] Layer surface closed by compositor.");
                state.exit_requested = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for UiState {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: <wl_compositor::WlCompositor as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_region::WlRegion, ()> for UiState {
    fn event(_: &mut Self, _: &wl_region::WlRegion, _: <wl_region::WlRegion as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_surface::WlSurface, ()> for UiState {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: <wl_surface::WlSurface as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for UiState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: <wl_shm::WlShm as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for UiState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: <wl_shm_pool::WlShmPool as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for UiState {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: <wl_buffer::WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            for slot in &mut state.buffer_pool {
                if slot.buffer == *buffer {
                    slot.busy = false;
                    break;
                }
            }
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for UiState {
    fn event(
        state: &mut Self,
        _callback: &wl_callback::WlCallback,
        _: <wl_callback::WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        state.frame_callback = None;
        // The transition clock starts once the first frame is on screen.
        if let Some(tr) = state.transition.as_mut() {
            if !tr.started() {
                tr.start_clock();
                if tr.entering {
                    ui::begin_entrance(&state.style);
                }
                state.animating = true;
            }
        }
        if state.configured && !state.exit_requested && (state.needs_render || state.animating) {
            state.draw_frame(qh);
        }
        // The card has arrived: catch up with where the pointer went meanwhile.
        if state.hover_pending && state.transition.is_none() && !state.exit_requested {
            state.hover_pending = false;
            let (px, py) = state.design_point(state.pointer_x as f64, state.pointer_y as f64);
            state.pointer_moved(px, py, qh);
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for UiState {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: <wl_output::WlOutput as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for UiState {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: <wl_seat::WlSeat as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_pointer::WlPointer, ()> for UiState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                state.pointer_serial = serial;
                // Pointer coordinates are kept in screen space. The scrim covers the screen at
                // (0, 0); the card surface sits at surf_rect's origin, read on every event
                // because the card can move or resize after the pointer entered it.
                state.pointer_on_scrim = state.scrim_surface.as_ref() == Some(&surface);
                let (ox, oy) = state.pointer_origin();
                state.pointer_x = surface_x as i32 + ox;
                state.pointer_y = surface_y as i32 + oy;
                state.set_cursor_shape(qh, CursorShape::Default);
            }
            wl_pointer::Event::Leave { .. } => {
                if state.nc.as_mut().map(|v| v.leave()).unwrap_or(false) {
                    state.render_and_commit(qh);
                }
                if state.wallpaper.as_mut().map(|v| v.leave()).unwrap_or(false) {
                    state.render_and_commit(qh);
                }
                if let Some(eff) = state.view.as_mut().map(|v| v.focus_lost()) {
                    state.apply_search_effect(eff, qh);
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                let (ox, oy) = state.pointer_origin();
                state.pointer_x = surface_x as i32 + ox;
                state.pointer_y = surface_y as i32 + oy;
                // Hover waits until the card has arrived: redrawing its content for pointer
                // events would replace the cheap scaled frames of the transition.
                if state.transition.is_some() {
                    state.hover_pending = true;
                    return;
                }
                let (px, py) = state.design_point(surface_x + ox as f64, surface_y + oy as f64);
                state.pointer_moved(px, py, qh);
            }
            // Hidden Settings (polkit dialog up): the scrim must not close it on a click.
            wl_pointer::Event::Button { .. } if state.settings_hidden() => {}
            wl_pointer::Event::Button { button, state: btn_state, .. } => {
                ui::caret_reset();
                match btn_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) if state.wallpaper.is_some() => {
                        state.pointer_down = true;
                        if button == 0x110 {
                            let (px, py) = state.design_point(state.pointer_x as f64, state.pointer_y as f64);
                            let style = state.style.clone();
                            let eff = state.wallpaper.as_mut().unwrap().click(px, py, &style);
                            state.apply_wallpaper_effect(eff, qh);
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Pressed) if state.settings.is_some() => {
                        state.pointer_down = true;
                        if button == 0x110 {
                            let (px, py) = state.design_point(state.pointer_x as f64, state.pointer_y as f64);
                            let (cx, cy, cw, ch) = state.card_bounds;
                            let (sx, sy) = (state.pointer_x as f64, state.pointer_y as f64);
                            let inside = sx >= cx as f64 && sx < (cx + cw) as f64 && sy >= cy as f64 && sy < (cy + ch) as f64;
                            let sv = state.settings.as_mut().unwrap();
                            let eff = if inside { sv.press(px, py) } else { sv.request_close() };
                            state.apply_settings_effect(eff, qh);
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Pressed) if state.view.is_some() => {
                        state.pointer_down = true;
                        if button == 0x110 {
                            let (px, py) = state.design_point(state.pointer_x as f64, state.pointer_y as f64);
                            let (cx, cy, cw, ch) = state.card_bounds;
                            let (sx, sy) = (state.pointer_x as f64, state.pointer_y as f64);
                            let inside = sx >= cx as f64 && sx < (cx + cw) as f64 && sy >= cy as f64 && sy < (cy + ch) as f64;
                            let (shift, _) = state.modifiers();
                            let eff = if inside { state.view.as_mut().unwrap().click(px, py, shift) } else { launcher::Effect::Close };
                            state.apply_search_effect(eff, qh);
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Pressed) if state.nc.is_some() => {
                        state.pointer_down = true;
                        if button == 0x110 {
                            let (px, py) = state.design_point(state.pointer_x as f64, state.pointer_y as f64);
                            let (cx, cy, cw, ch) = state.card_bounds;
                            let (sx, sy) = (state.pointer_x as f64, state.pointer_y as f64);
                            let inside = sx >= cx as f64 && sx < (cx + cw) as f64 && sy >= cy as f64 && sy < (cy + ch) as f64;
                            if inside {
                                let eff = state.nc.as_mut().unwrap().press(px, py);
                                state.apply_nc_effect(eff, qh);
                            } else {
                                state.exit_requested = true;
                            }
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        state.pointer_down = true;
                        if button == 0x110 { // BTN_LEFT
                            let (cx, cy, cw, ch) = state.card_bounds;
                            let is_modal = matches!(
                                state.mode,
                                UiMode::Launcher | UiMode::Spotlight | UiMode::Clipboard | UiMode::Emoji |
                                UiMode::Settings | UiMode::NotificationCenter | UiMode::Power | UiMode::Keybinds | UiMode::Notes | UiMode::Phone | UiMode::Prompt | UiMode::Trim | UiMode::DownloadConfirm | UiMode::Downloads | UiMode::CompEdit | UiMode::Guide
                            );

                            if is_modal && cw > 0 && ch > 0
                                && (state.pointer_x < cx || state.pointer_x >= cx + cw ||
                                   state.pointer_y < cy || state.pointer_y >= cy + ch) {
                                    eprintln!("[prism-ui] Click outside modal detected ({}, {}), dismissing.", state.pointer_x, state.pointer_y);
                                    state.exit_requested = true;
                                }
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        state.pointer_down = false;
                        if state.settings.as_mut().map(|sv| sv.release()).unwrap_or(false) {
                            state.render_and_commit(qh);
                        }
                        if let Some(v) = state.nc.as_mut() {
                            let eff = v.release();
                            state.apply_nc_effect(eff, qh);
                        }
                        if let Some(eff) = state.view.as_mut().map(|v| v.release()) {
                            state.apply_search_effect(eff, qh);
                        }
                    }
                    _ => {}
                }
            }
            wl_pointer::Event::AxisSource { axis_source } => {
                // Sent before the deltas of each frame: touchpad scrolling follows the fingers.
                let finger = matches!(axis_source, WEnum::Value(wl_pointer::AxisSource::Finger | wl_pointer::AxisSource::Continuous));
                ui::set_scroll_finger(finger);
                state.scroll_finger = finger;
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => {
                // Finger deltas are surface px (1:1 with the content); wheel steps are stretched.
                let delta = if state.scroll_finger { value / state.style.scale } else { value * 2.5 };
                let (dx, dy) = state.design_point(state.pointer_x as f64, state.pointer_y as f64);
                if let Some(sv) = state.settings.as_mut() {
                    if sv.scroll(dx, dy, delta) {
                        state.render_and_commit(qh);
                    }
                    return;
                }
                let searched = state.view.as_mut().map(|v| v.scroll(delta));
                if let Some(redraw) = searched {
                    if redraw {
                        state.render_and_commit(qh);
                    }
                    return;
                }
                if let Some(v) = state.nc.as_mut() {
                    if v.scroll(dx, dy, delta) {
                        state.render_and_commit(qh);
                    }
                    return;
                }
                if state.wallpaper.is_some() {
                    let style = state.style.clone();
                    let eff = state.wallpaper.as_mut().unwrap().scroll(value, &style);
                    state.apply_wallpaper_effect(eff, qh);
                    return;
                }
                {}
            }
            _ => {}
        }
    }
}

impl UiState {
    fn set_cursor_shape(&mut self, qh: &QueueHandle<Self>, shape: CursorShape) {
        if self.cursor_shape_device.is_none() {
            if let (Some(mgr), Some(ptr)) = (&self.cursor_shape_manager, &self.pointer) {
                self.cursor_shape_device = Some(mgr.get_pointer(ptr, qh, ()));
            }
        }
        if let Some(dev) = &self.cursor_shape_device {
            dev.set_shape(self.pointer_serial, shape);
        }
    }

}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for UiState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if let WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) = format {
                    let keymap = unsafe {
                        xkbcommon::xkb::Keymap::new_from_fd(
                            &state.xkb_context,
                            fd,
                            size as usize,
                            xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
                            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
                        )
                    };
                    state.xkb_state = keymap.ok().flatten().map(|km| xkbcommon::xkb::State::new(&km));
                }
                return;
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(xkb) = state.xkb_state.as_mut() {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                return;
            }
            _ => {}
        }
        match event {
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.repeat_rate = rate;
                state.repeat_delay = delay;
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                if let Some(v) = state.view.as_mut() {
                    v.key_down(key);
                }
                state.handle_key(key, qh);
                // Hold-to-repeat (clients repeat keys themselves on Wayland).
                let repeats = state.xkb_state.as_ref().map(|x| x.get_keymap().key_repeats(xkbcommon::xkb::Keycode::new(key + 8))).unwrap_or(false);
                state.repeat = (repeats && state.repeat_rate > 0)
                    .then(|| (key, std::time::Instant::now() + std::time::Duration::from_millis(state.repeat_delay.max(0) as u64)));
            }
            wl_keyboard::Event::Key { key, .. } => {
                if state.repeat.map(|(k, _)| k == key).unwrap_or(false) {
                    state.repeat = None;
                }
                if let Some(eff) = state.view.as_mut().map(|v| v.key_release(key)) {
                    state.apply_search_effect(eff, qh);
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                state.repeat = None;
                if let Some(eff) = state.view.as_mut().map(|v| v.focus_lost()) {
                    state.apply_search_effect(eff, qh);
                }
            }
            _ => {}
        }
    }
}

impl UiState {
    /// One key press (also called again for key repeat).
    fn handle_key(&mut self, key: u32, qh: &QueueHandle<Self>) {
        let state = self;
        // Translate through the active keymap. `key` becomes the equivalent US-layout
        // evdev code that the handlers below match on; `typed` is the character the
        // layout actually produces (respecting Shift, AltGr, non-US layouts, wtype).
        let raw_key = key;
        let (key, typed) = translate_key(state.xkb_state.as_ref(), key);
        ui::caret_reset();
        if state.wallpaper.is_some() {
            let style = state.style.clone();
            let eff = state.wallpaper.as_mut().unwrap().key(key, &style);
            state.apply_wallpaper_effect(eff, qh);
            return;
        }
        if state.settings.is_some() || state.view.is_some() {
            let special = if typed.is_none() { key } else { 0 };
            let (shift, ctrl) = state.xkb_state.as_ref().map_or((false, false), |x| {
                use xkbcommon::xkb;
                (x.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE), x.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE))
            });
            // Ctrl+letter has no text; pass the letter itself for shortcuts.
            let typed = match (ctrl, typed, state.xkb_state.as_ref()) {
                (true, None, Some(x)) => {
                    let sym = x.key_get_one_sym(xkbcommon::xkb::Keycode::new(raw_key + 8));
                    char::from_u32(xkbcommon::xkb::keysym_to_utf32(sym)).filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase())
                }
                (_, t, _) => t,
            };
            let special = if typed.is_some() { 0 } else { special };
            let alt = state.xkb_state.as_ref().is_some_and(|x| x.mod_name_is_active(xkbcommon::xkb::MOD_NAME_ALT, xkbcommon::xkb::STATE_MODS_EFFECTIVE));
            if let Some(v) = state.view.as_mut() {
                let eff = match typed.filter(|_| alt && !ctrl).and_then(|c| v.alt_key(c)) {
                    Some(eff) => eff,
                    None => v.key(special, typed, shift, ctrl),
                };
                state.apply_search_effect(eff, qh);
            } else {
                let sv = state.settings.as_mut().unwrap();
                let eff = sv.key(special, typed, shift, ctrl);
                state.apply_settings_effect(eff, qh);
            }
            return;
        }
        if let Some(v) = state.nc.as_mut() {
            let special = if typed.is_none() { key } else { 0 };
            let ctrl = state.xkb_state.as_ref().is_some_and(|x| x.mod_name_is_active(xkbcommon::xkb::MOD_NAME_CTRL, xkbcommon::xkb::STATE_MODS_EFFECTIVE));
            let eff = v.key(special, typed, ctrl);
            state.apply_nc_effect(eff, qh);
            return;
        }
        // Overlays without their own key handling close on Esc.
        if key == 1 {
            state.exit_requested = true;
        }
    }
}

/// Map a key press to (US-layout evdev code, produced character) using the compositor keymap.
/// Without a keymap (should not happen) falls back to treating `key` as a US-layout code.
pub(crate) fn translate_key(xkb: Option<&xkbcommon::xkb::State>, key: u32) -> (u32, Option<char>) {
    let Some(xkb) = xkb else {
        return (key, evdev_to_char(key));
    };
    let kc = xkbcommon::xkb::Keycode::new(key + 8);
    let special = match xkb.key_get_one_sym(kc).raw() {
        0xff1b => Some(1),           // Escape
        0xff0d | 0xff8d => Some(28), // Return, KP_Enter
        0xff08 => Some(14),          // BackSpace
        0xff09 | 0xfe20 => Some(15), // Tab, ISO_Left_Tab
        0xff51 => Some(105),         // Left
        0xff52 => Some(103),         // Up
        0xff53 => Some(106),         // Right
        0xff54 => Some(108),         // Down
        0xff55 => Some(104),         // Page_Up
        0xff56 => Some(109),         // Page_Down
        0xff50 => Some(102),         // Home
        0xff57 => Some(107),         // End
        0xffff => Some(111),         // Delete
        _ => None,
    };
    if let Some(code) = special {
        return (code, None);
    }
    let text = xkb.key_get_utf8(kc);
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => {
            // Keep hotkeys (e.g. power menu letters) working by mapping the character back to
            // its US-layout code; unknown characters get a code no handler matches.
            let code = char_to_evdev(c.to_ascii_lowercase()).unwrap_or(0x1_0000);
            (code, Some(c))
        }
        _ => (0x1_0000, None),
    }
}

fn char_to_evdev(c: char) -> Option<u32> {
    (1..=60).find(|&code| evdev_to_char(code) == Some(c))
}

fn evdev_to_char(code: u32) -> Option<char> {
    match code {
        2 => Some('1'), 3 => Some('2'), 4 => Some('3'), 5 => Some('4'), 6 => Some('5'),
        7 => Some('6'), 8 => Some('7'), 9 => Some('8'), 10 => Some('9'), 11 => Some('0'),
        12 => Some('-'), 13 => Some('='),
        16 => Some('q'), 17 => Some('w'), 18 => Some('e'), 19 => Some('r'), 20 => Some('t'),
        21 => Some('y'), 22 => Some('u'), 23 => Some('i'), 24 => Some('o'), 25 => Some('p'),
        30 => Some('a'), 31 => Some('s'), 32 => Some('d'), 33 => Some('f'), 34 => Some('g'),
        35 => Some('h'), 36 => Some('j'), 37 => Some('k'), 38 => Some('l'),
        44 => Some('z'), 45 => Some('x'), 46 => Some('c'), 47 => Some('v'), 48 => Some('b'),
        49 => Some('n'), 50 => Some('m'),
        51 => Some(','), 52 => Some('.'), 53 => Some('/'),
        57 => Some(' '),
        _ => None,
    }
}

fn create_buffer_slot(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<UiState>,
    width: i32,
    height: i32,
    stride: i32,
    size: usize,
) -> Option<BufferSlot> {
    unsafe {
        let fd = libc::memfd_create(c"prism-ui-shm".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 {
            return None;
        }
        if libc::ftruncate(fd, size as libc::off_t) < 0 {
            libc::close(fd);
            return None;
        }
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if ptr == libc::MAP_FAILED {
            libc::close(fd);
            return None;
        }

        let file = File::from_raw_fd(fd);
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();

        Some(BufferSlot {
            buffer,
            ptr: ptr as *mut u8,
            size,
            width,
            height,
            stride,
            busy: false,
        })
    }
}

impl UiState {
    /// Request a redraw. Draws immediately when no frame is in flight; otherwise the request is
    /// coalesced into the next `wl_surface.frame` callback. Input bursts (1000 Hz mice, key
    /// repeat) therefore never cause more than one render per display refresh.
    /// The pointer moved to (px, py) in design pixels: update hover in the open view.
    fn pointer_moved(&mut self, px: f64, py: f64, qh: &QueueHandle<Self>) {
        if let Some(sv) = self.settings.as_mut() {
            if sv.motion(px, py, self.pointer_down) {
                self.render_and_commit(qh);
            }
            return;
        }
        if let Some(v) = self.view.as_mut() {
            let (redraw, over) = (v.motion(px, py), v.clickable());
            self.set_cursor_shape(qh, if over { CursorShape::Pointer } else { CursorShape::Default });
            if redraw {
                self.render_and_commit(qh);
            }
            return;
        }
        if let Some(v) = self.nc.as_mut() {
            let redraw = v.motion(px, py);
            let shape = if v.over_clickable() { CursorShape::Pointer } else { CursorShape::Default };
            self.set_cursor_shape(qh, shape);
            if redraw {
                self.render_and_commit(qh);
            }
            return;
        }
        if let Some(v) = self.wallpaper.as_mut() {
            let (redraw, over) = v.motion(px, py);
            self.set_cursor_shape(qh, if over { CursorShape::Pointer } else { CursorShape::Default });
            if redraw {
                self.render_and_commit(qh);
            }
            return;
        }
        self.set_cursor_shape(qh, CursorShape::Default);
    }

    fn render_and_commit(&mut self, qh: &QueueHandle<Self>) {
        self.needs_render = true;
        self.content_dirty = true;
        if self.configured && self.frame_callback.is_none() {
            self.draw_frame(qh);
        }
    }

    fn apply_wallpaper_effect(&mut self, eff: wallpaper::Effect, qh: &QueueHandle<Self>) {
        match eff {
            wallpaper::Effect::None => {}
            wallpaper::Effect::Redraw => self.render_and_commit(qh),
            wallpaper::Effect::Close => self.exit_requested = true,
        }
    }

    /// Lower the Settings card (or another card that opened a system dialog) and scrim below
    /// normal windows (and give up the keyboard) while the dialog is open, or bring them back.
    fn apply_settings_layer(&mut self, qh: &QueueHandle<Self>) {
        let request = self.settings.as_mut().and_then(|s| s.take_layer_request()).or_else(|| self.view.as_mut().and_then(|v| v.take_layer_request()));
        let Some(lower) = request else { return };
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::KeyboardInteractivity;
        if let Some(ls) = &self.layer_surface {
            ls.set_layer(if lower { Layer::Bottom } else { Layer::Overlay });
            ls.set_keyboard_interactivity(if lower { KeyboardInteractivity::None } else { KeyboardInteractivity::Exclusive });
        }
        if let Some(scrim) = &self.scrim_layer {
            scrim.set_layer(if lower { Layer::Bottom } else { Layer::Top });
        }
        if let Some(s) = &self.scrim_surface {
            s.commit();
        }
        self.render_and_commit(qh);
    }

    fn apply_settings_effect(&mut self, eff: settings::Effect, qh: &QueueHandle<Self>) {
        self.apply_settings_layer(qh);
        match eff {
            settings::Effect::None => {}
            settings::Effect::Redraw => self.render_and_commit(qh),
            settings::Effect::Close => {
                if self.settings.as_mut().map(|s| s.take_edit_mode()).unwrap_or(false) {
                    let _ = Command::new("prism-ctl").args(["edit-mode", "open"]).spawn().map(reap);
                }
                self.exit_requested = true;
            }
        }
    }

    /// (shift, ctrl) held now.
    fn modifiers(&self) -> (bool, bool) {
        use xkbcommon::xkb;
        self.xkb_state.as_ref().map_or((false, false), |x| {
            (x.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE), x.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE))
        })
    }

    fn apply_search_effect(&mut self, eff: launcher::Effect, qh: &QueueHandle<Self>) {
        match eff {
            launcher::Effect::None => {}
            launcher::Effect::Redraw => self.render_and_commit(qh),
            launcher::Effect::Close => self.exit_requested = true,
        }
    }

    fn apply_nc_effect(&mut self, eff: nc::Effect, qh: &QueueHandle<Self>) {
        match eff {
            nc::Effect::None => {}
            nc::Effect::Redraw => self.render_and_commit(qh),
            nc::Effect::Close => self.exit_requested = true,
            nc::Effect::OpenSettings => {
                let _ = Command::new("prism-ctl").args(["settings", "open"]).spawn().map(reap);
            }
        }
    }

    /// Attach the dim to the scrim: a 1x1 single-pixel buffer scaled to the output by
    /// wp_viewporter. Runs on configure; the level follows the card transition.
    fn commit_scrim(&mut self, qh: &QueueHandle<Self>) {
        let (Some(surface), Some(vp)) = (&self.scrim_surface, &self.viewporter) else { return };
        if self.scrim_viewport.is_none() {
            self.scrim_viewport = Some(vp.get_viewport(surface, qh, ()));
        }
        if let Some(viewport) = &self.scrim_viewport {
            viewport.set_destination(self.width.max(1) as i32, self.height.max(1) as i32);
        }
        // Start hidden when a transition will fade it in.
        let level = if self.transition.is_some() { self.scrim_shown.max(0.0) } else { 1.0 };
        self.scrim_shown = -1.0;
        let _ = self.set_scrim_level(level, qh);
    }

    /// Set the dim to `level` (0..1) of the configured dim opacity by attaching a new 1x1
    /// buffer. The compositor just composites a solid rectangle; nothing is redrawn.
    /// True when the scrim was committed.
    fn set_scrim_level(&mut self, level: f64, qh: &QueueHandle<Self>) -> bool {
        let level = level.clamp(0.0, 1.0);
        if (level - self.scrim_shown).abs() < 0.004 {
            return false;
        }
        let Some(surface) = self.scrim_surface.clone() else { return false };
        if self.scrim_viewport.is_none() {
            return false; // not configured yet
        }
        self.scrim_shown = level;
        let dim = if self.style.dim_enabled { self.style.dim_opacity } else { 0.0 };
        let alpha = dim * level;
        let buffer = if let Some(mgr) = &self.single_pixel_mgr {
            Some(mgr.create_u32_rgba_buffer(0, 0, 0, (alpha * u32::MAX as f64) as u32, qh, ()))
        } else if let Some(shm) = &self.shm {
            create_buffer_slot(shm, qh, 1, 1, 4, 4).map(|slot| {
                unsafe { *(slot.ptr as *mut u32) = ((alpha * 255.0) as u32) << 24 };
                let b = slot.buffer.clone();
                std::mem::forget(slot);
                b
            })
        } else {
            None
        };
        if let Some(buf) = buffer {
            surface.attach(Some(&buf), 0, 0);
            surface.damage_buffer(0, 0, 1, 1);
            surface.commit();
            // The previous 1x1 buffer is no longer attached; free it.
            if let Some(old) = self.scrim_buffer.replace(buf) {
                old.destroy();
            }
            return true;
        }
        false
    }

    /// Move, scale and fade the drop shadow with the card's `pose`. True when the scrim must
    /// commit to apply it.
    fn update_shadow(&mut self, pose: &Pose, qh: &QueueHandle<Self>) -> bool {
        let Some(shm) = self.shm.clone() else { return false };
        let s = self.style.scale;
        // The card at rest, in screen px. The NC surface (and so design_card) is wider than the
        // card while it slides; the shadow must follow the card itself, as drawn in draw_frame.
        let rest = match self.mode {
            UiMode::NotificationCenter => {
                let c = nc::card_rect(&self.config.notification_center_position, self.width as f64 / s, self.height as f64 / s);
                ((c.x * s).round() as i32, (c.y * s).round() as i32, (c.w * s).round() as i32, (c.h * s).round() as i32)
            }
            _ => self.desired_card_rect(),
        };
        let (x, y, w, h) = (rest.0 as f64, rest.1 as f64, rest.2 as f64, rest.3 as f64);
        // Where the card is drawn now: the surface itself scales about its centre (collapse),
        // or the pixels inside the surface move and scale about its centre.
        let visible = if self.collapse() {
            let g = self.geometry_for(pose);
            (g.0 as f64, g.1 as f64, g.2 as f64, g.3 as f64)
        } else {
            let (sx, sy, sw, sh) = self.surf_rect;
            let (cx, cy) = (sx as f64 + sw as f64 / 2.0, sy as f64 + sh as f64 / 2.0);
            let k = pose.scale;
            (cx + (x + pose.tx * s - cx) * k, cy + (y + pose.ty * s - cy) * k, w * k, h * k)
        };
        let floating = (matches!(self.mode, UiMode::Launcher) && launcher_themes::floating(&self.config.launcher_theme))
            || (matches!(self.mode, UiMode::Power) && power::floating(&self.config.power_theme));
        let enabled = self.style.shadows && !self.settings_hidden() && !floating;
        let (radius, alpha) = (self.style.radius, pose.alpha);
        match self.shadow.as_mut() {
            Some(sh) => sh.update(&shm, qh, rest, radius, s, visible, alpha, enabled),
            None => false,
        }
    }

    /// Screen-space rectangle the card surface should occupy right now. Mirrors the card
    /// geometry in `draw_frame`; the surface is exactly the card so compositor blur (which on
    /// MangoWM always covers the whole layer surface) stays behind the card only.
    fn desired_card_rect(&self) -> (i32, i32, i32, i32) {
        let s = self.style.scale;
        let (x, y, w, h) = self.design_card_rect();
        let px = |v: i32| (v as f64 * s).round() as i32;
        match &self.mode {
            // Full-width strip: x/width are screen-relative already.
            UiMode::Wallpaper => (0, px(y), self.width as i32, px(h)),
            _ => (px(x), px(y), px(w), px(h)),
        }
    }

    /// `desired_card_rect` in design pixels (1440p layout; see ui::DESIGN_HEIGHT).
    fn design_card_rect(&self) -> (i32, i32, i32, i32) {
        let sw = self.width as f64 / self.style.scale;
        let sh = self.height as f64 / self.style.scale;
        let centered = |w: f64, h: f64| {
            let w = w.min(sw - 40.0).max(1.0);
            let h = h.min(sh - 40.0).max(1.0);
            (((sw - w) / 2.0).round() as i32, ((sh - h) / 2.0).round() as i32, w as i32, h as i32)
        };
        match &self.mode {
            UiMode::Launcher => {
                let (w, h) = launcher_themes::card_size(&self.config.launcher_theme);
                centered(w, h)
            }
            UiMode::Spotlight => {
                let w = spotlight::CARD_W.min(sw - 40.0);
                let h = self.view.as_ref().and_then(|v| v.height()).unwrap_or(60.0);
                (((sw - w) / 2.0).round() as i32, spotlight::TOP as i32, w as i32, h as i32)
            }
            UiMode::Clipboard => centered(clipboard::CARD_W, clipboard::CARD_H),
            UiMode::Phone => centered(phone::CARD_W, phone::CARD_H),
            UiMode::Prompt => centered(prompt::CARD_W, self.view.as_ref().and_then(|v| v.height()).unwrap_or(300.0)),
            UiMode::Power => {
                let (w, h) = power::card_size(&self.config, sw, sh);
                centered(w, h)
            }
            UiMode::Keybinds => {
                let (w, h) = keybinds::card_size(sw, sh);
                (((sw - w) / 2.0).round() as i32, ((sh - h) / 2.0).round() as i32, w as i32, h as i32)
            }
            UiMode::Emoji => centered(emoji::CARD_W, emoji::CARD_H),
            UiMode::Notes => centered(notes::CARD_W, notes::CARD_H),
            UiMode::Trim => centered(trim::CARD_W, trim::CARD_H),
            UiMode::DownloadConfirm => centered(download_confirm::CARD_W, download_confirm::CARD_H),
            UiMode::Downloads => centered(downloads_card::CARD_W, downloads_card::CARD_H),
            UiMode::CompEdit => centered(compedit::CARD_W.min(sw - 40.0), compedit::CARD_H.min(sh - 40.0)),
            UiMode::Guide => centered(guide::CARD_W.min(sw - 40.0), guide::CARD_H.min(sh - 40.0)),
            // Hidden for a polkit dialog: a 1x1 transparent surface (a full-size transparent
            // card would still show the compositor blur).
            UiMode::Settings if self.settings_hidden() => (0, 0, 1, 1),
            UiMode::Settings => centered(settings::CARD_W, settings::CARD_H),
            UiMode::NotificationCenter => {
                let c = nc::card_rect(&self.config.notification_center_position, sw, sh);
                let (x, y, w, h) = (c.x as i32, c.y as i32, c.w as i32, c.h as i32);
                let sliding = self.transition.is_some() && matches!(self.motion, Motion::SlideLeft { .. } | Motion::SlideRight { .. });
                match (sliding, self.config.notification_center_position.as_str()) {
                    // While sliding (the spring overshoots ~1-2% of 500 px) the surface spans from
                    // the screen edge past the overshoot, then shrinks to the card.
                    (true, "right") => (x - 80, y, sw as i32 - (x - 80), h),
                    (true, _) => (0, y, x + w + 80, h),
                    _ => (x, y, w, h),
                }
            }
            UiMode::Wallpaper => {
                // Full-width strip (QML panelWidth = screen width), vertically centred.
                let h = wallpaper::PANEL_H.min(sh);
                (0, ((sh - h) / 2.0).round() as i32, sw as i32, h as i32)
            }
            _ => (0, 0, sw as i32, sh as i32),
        }
    }

    /// Card rectangle in design pixels, for the views' draw calls.
    fn design_card(&self) -> desktop::layout::Rect {
        let (x, y, w, h) = self.design_card_rect();
        desktop::layout::Rect { x: x as f64, y: y as f64, w: w as f64, h: h as f64 }
    }

    /// Cards that grow from / collapse into their centre by scaling the surface itself.
    fn collapse(&self) -> bool {
        self.card_viewport.is_some() && matches!(self.motion, Motion::Scale)
    }

    /// Layer surface geometry for `pose`: the card rectangle, scaled toward its centre while
    /// a collapse transition runs.
    fn geometry_for(&self, pose: &Pose) -> (i32, i32, i32, i32) {
        let (x, y, w, h) = self.surf_rect;
        if !self.collapse() || (pose.scale - 1.0).abs() < 1e-4 {
            return (x, y, w, h);
        }
        let s = pose.scale.max(0.01);
        let (ox, oy) = (x as f64 + w as f64 / 2.0, y as f64 + h as f64 / 2.0);
        let nx = ox + (x as f64 - ox) * s;
        let ny = oy + (y as f64 - oy) * s;
        ((nx.round()) as i32, (ny.round()) as i32, ((w as f64 * s).round() as i32).max(1), ((h as f64 * s).round() as i32).max(1))
    }

    /// Send the layer surface geometry (applied with the next commit).
    fn apply_geometry(&mut self, g: (i32, i32, i32, i32)) {
        if g == self.geom {
            return;
        }
        if let Some(ls) = &self.layer_surface {
            ls.set_size(g.2.max(1) as u32, g.3.max(1) as u32);
            ls.set_margin(g.1, 0, 0, g.0);
        }
        if let Some(vp) = &self.card_viewport {
            vp.set_destination(g.2.max(1), g.3.max(1));
        }
        self.geom = g;
    }

    /// Per-frame transition state besides the pixels: surface opacity, scrim, end of exit.
    fn finish_pose(&mut self, pose: &Pose) {
        if let Some(a) = &self.alpha_surface {
            // The compositor fades the whole surface.
            let m = (pose.alpha.clamp(0.0, 1.0) * u32::MAX as f64).round() as u32;
            if m != self.alpha_multiplier {
                a.set_multiplier(m);
                self.alpha_multiplier = m;
            }
        }
        if pose.done && !self.closing {
            self.transition = None;
        }
        // The shadow follows the card; applied after drawing with the scrim (its parent).
        self.shadow_pending = Some(*pose);
        // Fade the scrim with the card; applied after drawing (see draw_frame).
        self.scrim_pending = Some(if self.settings_hidden() { 0.0 } else { pose.shown.clamp(0.0, 1.0) });
        if self.closing && pose.done {
            self.exit_requested = true;
        }
    }

    /// Settings is hidden while a polkit dialog it started is open.
    fn settings_hidden(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.hidden())
    }

    /// Screen position of the surface the pointer is over.
    fn pointer_origin(&self) -> (i32, i32) {
        if self.pointer_on_scrim {
            (0, 0)
        } else if self.collapse() {
            // A card that is still scaling is not at its final place yet.
            (self.geom.0, self.geom.1)
        } else {
            (self.surf_rect.0, self.surf_rect.1)
        }
    }

    /// Screen position to design pixels (what views use for hit tests).
    fn design_point(&self, x: f64, y: f64) -> (f64, f64) {
        (x / self.style.scale, y / self.style.scale)
    }


    fn draw_frame(&mut self, qh: &QueueHandle<Self>) {
        let perf_t0 = std::time::Instant::now();
        self.draw_frame_inner(qh);
        // The shadow is a synchronized subsurface of the scrim: its new state shows with the
        // scrim's next commit.
        let shadow_moved = self.shadow_pending.take().is_some_and(|pose| self.update_shadow(&pose, qh));
        let scrim_committed = match self.scrim_pending.take() {
            Some(level) => self.set_scrim_level(level, qh),
            None => false,
        };
        if shadow_moved && !scrim_committed {
            if let Some(s) = &self.scrim_surface {
                s.commit();
            }
        }
        if std::env::var_os("PRISM_PERF").is_some() {
            let content = PERF_CONTENT_MS.with(|c| c.replace(0.0));
            eprintln!("[perf] frame {:.2} ms (content {:.2} ms)", perf_t0.elapsed().as_secs_f64() * 1000.0, content);
        }
    }

    fn draw_frame_inner(&mut self, qh: &QueueHandle<Self>) {
        // Frame timing for frame-rate independent easing. After an idle period assume one
        // nominal frame instead of a huge jump.
        let frame_now = std::time::Instant::now();
        self.frame_dt = match self.last_frame_at {
            Some(t) if self.animating => frame_now.duration_since(t).as_secs_f64().clamp(0.0, 0.05),
            _ => 1.0 / 60.0,
        };
        self.last_frame_at = Some(frame_now);

        // Settings edits preview live: rebuild the global style from the draft config.
        if let (Some(sv), true) = (self.settings.as_ref(), self.content_dirty) {
            self.style = Style::new(&sv.config, &self.theme);
            cairo_backend::set_global_font(&self.style.font_family);
        }

        // The OSD slides by moving its layer surface (see osd.rs); applied with this frame's commit.
        if let (Some(v), Some(ls)) = (&self.osd, &self.layer_surface) {
            let m = (v.margin() as f64 * self.style.scale).round() as i32;
            if m != self.osd_margin {
                ls.set_margin(0, 0, m, 0);
                self.osd_margin = m;
            }
        }

        if self.card_surface {
            let want = self.desired_card_rect();
            if want != self.surf_rect {
                // Move/resize and attach a buffer of the new size in the same commit. Waiting
                // for the configure first let the old buffer jump with the new margin for a
                // frame (the shake at the end of the slide-in).
                self.surf_rect = want;
                self.content_dirty = true;
                self.buffer_committed = false;
            }
            let pose = self.transition.as_ref().map(|t| t.sample()).unwrap_or(Pose::REST);
            let geo = self.geometry_for(&pose);
            self.apply_geometry(geo);

            // Collapse/grow frames where only the geometry changes: no new buffer, the
            // compositor scales the attached one.
            let can_fade = self.alpha_surface.is_some() || pose.alpha >= 0.999;
            if self.collapse() && self.buffer_committed && !self.content_dirty && !self.content_animating && can_fade {
                if let Some(surface) = self.surface.clone() {
                    self.finish_pose(&pose);
                    if self.frame_callback.is_none() {
                        self.frame_callback = Some(surface.frame(qh, ()));
                    }
                    self.animating = !pose.done;
                    self.needs_render = false;
                    surface.commit();
                    return;
                }
            }
        }

        let (surface, shm) = match (self.surface.clone(), self.shm.clone()) {
            (Some(s), Some(shm)) => (s, shm),
            _ => return,
        };

        // `width`/`height` are the logical screen size used by all layout code. The buffer only
        // covers the surface rectangle (the card, for card surfaces).
        let width = self.width as i32;
        let height = self.height as i32;
        if width <= 0 || height <= 0 {
            return;
        }
        let (surf_x, surf_y, bw, bh) = if self.card_surface {
            self.surf_rect
        } else {
            (0, 0, width, height)
        };
        if bw <= 0 || bh <= 0 {
            return;
        }

        let stride = bw * 4;
        let size = (stride * bh) as usize;

        // Invalidate buffer pool if dimensions changed
        if self.buffer_pool.iter().any(|s| s.width != bw || s.height != bh) {
            for slot in self.buffer_pool.drain(..) {
                slot.destroy();
            }
        }

        // Find a free slot or allocate a new one (double/triple buffering)
        let slot_idx = if let Some(idx) = self.buffer_pool.iter().position(|s| !s.busy) {
            idx
        } else if self.buffer_pool.len() < 3 {
            if let Some(slot) = create_buffer_slot(&shm, qh, bw, bh, stride, size) {
                self.buffer_pool.push(slot);
                self.buffer_pool.len() - 1
            } else {
                self.needs_render = true;
                return;
            }
        } else {
            // Every buffer is still held by the compositor. Never draw into a buffer it may be
            // reading; retry on the next frame callback / release.
            self.needs_render = true;
            return;
        };

        let slot = &mut self.buffer_pool[slot_idx];
        slot.busy = true;
        let ptr = slot.ptr;
        let buffer = slot.buffer.clone();

        unsafe {
            libc::memset(ptr as *mut libc::c_void, 0, size);

            let cairo_surface = cairo_backend::cairo_image_surface_create_for_data(
                ptr,
                cairo_backend::CAIRO_FORMAT_ARGB32,
                bw,
                bh,
                stride,
            );
            // Card surfaces render their content into an offscreen cache and composite it with
            // the global enter/exit transition (ui.rs). Transition-only frames just re-composite
            // the cache, so text and images are rasterized once instead of at every new scale.
            let use_cache = self.card_surface;
            if use_cache {
                let stale = !matches!(self.card_cache, Some((_, w, h)) if w == bw && h == bh);
                if stale {
                    if let Some((old, _, _)) = self.card_cache.take() {
                        cairo_backend::cairo_surface_destroy(old);
                    }
                    let img = cairo_backend::cairo_image_surface_create(cairo_backend::CAIRO_FORMAT_ARGB32, bw, bh);
                    self.card_cache = Some((img, bw, bh));
                    self.content_dirty = true;
                }
            }
            let redraw_content = !use_cache || self.content_dirty || self.content_animating;
            let content_target = match (use_cache, self.card_cache) {
                (true, Some((img, _, _))) => img,
                _ => cairo_surface,
            };
            let cr = cairo_backend::cairo_create(content_target);
            if use_cache && redraw_content {
                cairo_backend::cairo_set_operator(cr, cairo_backend::CAIRO_OPERATOR_CLEAR);
                cairo_backend::cairo_paint(cr);
                cairo_backend::cairo_set_operator(cr, 2); // OVER
            }
            if surf_x != 0 || surf_y != 0 {
                // Draw in screen coordinates; the buffer is a window onto the card area.
                cairo_backend::cairo_translate(cr, -(surf_x as f64), -(surf_y as f64));
            }
            // Views draw in design pixels; scale them to the screen.
            let ui_scale = self.style.scale;
            if (ui_scale - 1.0).abs() > 1e-6 {
                cairo_backend::cairo_scale(cr, ui_scale, ui_scale);
            }
            let (dw, dh) = (width as f64 / ui_scale, height as f64 / ui_scale);


            // The buffer was cleared to transparent by memset above. The modal dim is a separate
            // scrim surface, so nothing full-screen is painted here.

            let mut is_animating = false;

            cairo_backend::cairo_save(cr);
            let content_t0 = std::time::Instant::now();
            ui::take_anim_pending(); // only what is drawn in this frame counts
            if redraw_content {
                ui::take_caret_drawn();
            }

            if redraw_content {
            match &self.mode {
                // The desktop layer and its edit mode live in desktop/ (separate code path).
                UiMode::Desktop | UiMode::DesktopEdit => {}
                UiMode::Wallpaper => {
                    let sw = dw;
                    let py = ((dh - wallpaper::PANEL_H) / 2.0).round();
                    self.card_bounds = self.desired_card_rect();
                    let (style, theme) = (self.style.clone(), self.theme.clone());
                    if let Some(v) = self.wallpaper.as_mut() {
                        v.draw(cr, 0.0, py, sw, &style, &theme);
                        if v.animating() {
                            is_animating = true;
                        }
                    }
                }
                UiMode::Settings => {
                    let card = self.design_card();
                    self.card_bounds = self.desired_card_rect();
                    let (style, theme) = (self.style.clone(), self.theme.clone());
                    if let Some(sv) = self.settings.as_mut() {
                        sv.draw(cr, card, &style, &theme);
                    }
                }
                UiMode::Osd { .. } => {
                    if let Some(v) = &self.osd {
                        v.draw(cr, &self.style, &self.theme);
                        is_animating |= v.animating();
                    }
                }
                UiMode::Launcher | UiMode::Spotlight | UiMode::Power | UiMode::Clipboard | UiMode::Emoji | UiMode::Keybinds | UiMode::Notes | UiMode::Phone | UiMode::Prompt | UiMode::Trim | UiMode::DownloadConfirm | UiMode::Downloads | UiMode::CompEdit | UiMode::Guide => {
                    let card = self.design_card();
                    self.card_bounds = self.desired_card_rect();
                    let (style, theme) = (self.style.clone(), self.theme.clone());
                    if let Some(v) = self.view.as_mut() {
                        v.draw(cr, card, &style, &theme);
                        is_animating |= v.animating();
                    }
                }
                UiMode::NotificationCenter => {
                    let card = nc::card_rect(&self.config.notification_center_position, dw, dh);
                    let s = ui_scale;
                    self.card_bounds = ((card.x * s) as i32, (card.y * s) as i32, (card.w * s) as i32, (card.h * s) as i32);
                    let (style, theme) = (self.style.clone(), self.theme.clone());
                    if let Some(v) = self.nc.as_mut() {
                        v.draw(cr, card, &style, &theme);
                        is_animating |= v.animating();
                    }
                }
            }

            } // if redraw_content
            if redraw_content {
                PERF_CONTENT_MS.with(|c| c.set(content_t0.elapsed().as_secs_f64() * 1000.0));
                // Rows fading in, scrolling, gliding highlights, fades: keep rendering.
                is_animating |= ui::take_anim_pending();
            }

            cairo_backend::cairo_restore(cr);
            cairo_backend::cairo_destroy(cr);

            if redraw_content {
                self.content_dirty = false;
                self.content_animating = use_cache && is_animating;
                self.caret = ui::take_caret_drawn().then(ui::caret_phase);
            }

            let mut pose = Pose::REST;
            if use_cache {
                if let Some(tr) = &self.transition {
                    pose = tr.sample();
                }
                if pose.done && !self.closing {
                    self.transition = None;
                }
                if let (Some((img, _, _)), false) = (self.card_cache, self.settings_hidden()) {
                    let out = cairo_backend::cairo_create(cairo_surface);
                    // A collapse scales the surface itself (viewport), not the pixels.
                    let collapse = self.collapse();
                    if !collapse {
                        cairo_backend::cairo_translate(out, pose.tx * ui_scale, pose.ty * ui_scale);
                    }
                    let scaled = !collapse && (pose.scale - 1.0).abs() > 1e-3;
                    if scaled {
                        let (cx, cy) = (bw as f64 / 2.0, bh as f64 / 2.0);
                        cairo_backend::cairo_translate(out, cx, cy);
                        cairo_backend::cairo_scale(out, pose.scale, pose.scale);
                        cairo_backend::cairo_translate(out, -cx, -cy);
                    }
                    cairo_backend::cairo_set_source_surface(out, img, 0.0, 0.0);
                    if scaled {
                        cairo_backend::cairo_pattern_set_filter(cairo_backend::cairo_get_source(out), cairo_backend::CAIRO_FILTER_BILINEAR);
                    }
                    if self.alpha_surface.is_some() {
                        // The compositor fades the surface (finish_pose sets the multiplier).
                        cairo_backend::cairo_paint(out);
                    } else if pose.alpha >= 0.999 {
                        cairo_backend::cairo_paint(out);
                    } else {
                        cairo_backend::cairo_paint_with_alpha(out, pose.alpha.max(0.0));
                    }
                    cairo_backend::cairo_destroy(out);
                }
                self.finish_pose(&pose);
            }
            cairo_backend::cairo_surface_destroy(cairo_surface);

            surface.attach(Some(&buffer), 0, 0);

            // Every commit asks for a frame callback: it paces redraws to the display refresh
            // (vblank) and wakes us to continue animations or apply coalesced redraw requests.
            if self.frame_callback.is_none() {
                self.frame_callback = Some(surface.frame(qh, ()));
            }
            self.animating = is_animating || (use_cache && !pose.done);
            self.needs_render = false;

            surface.damage_buffer(0, 0, bw, bh);
            surface.commit();
            self.buffer_committed = true;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Process model
//
// `prism-ui daemon` (started once by prismd) owns ONE Wayland connection and
// hosts every overlay, toast, OSD, the desktop widgets and edit mode. prismd
// sends `UiCommand`s over $XDG_RUNTIME_DIR/prism-ui.sock. Each overlay is an
// `Overlay` session: its own event queue + UiState, sharing the globals bound
// once in `Globals`. Expensive data (apps, emoji DB, decoded images, fonts)
// stays warm between openings, so opening a card costs one frame, not a
// process start.
//
// `prism-ui <mode>` still works for debugging: it runs the same loop with a
// single overlay and exits when it closes.
// ─────────────────────────────────────────────────────────────────────────

thread_local! {
    /// PRISM_PERF: time spent drawing card content in the current frame.
    static PERF_CONTENT_MS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// Globals bound once per process and shared by every overlay session.
pub struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    single_pixel_mgr: Option<WpSinglePixelBufferManagerV1>,
    viewporter: Option<WpViewporter>,
    idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    alpha_modifier: Option<WpAlphaModifierV1>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(g: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_compositor" => g.compositor = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_shm" => g.shm = Some(reg.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => g.layer_shell = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_seat" if g.seat.is_none() => g.seat = Some(reg.bind(name, version.min(5), qh, ())),
                "wp_cursor_shape_manager_v1" => g.cursor_shape_manager = Some(reg.bind(name, 1, qh, ())),
                "wp_single_pixel_buffer_manager_v1" => g.single_pixel_mgr = Some(reg.bind(name, 1, qh, ())),
                "wp_viewporter" => g.viewporter = Some(reg.bind(name, 1, qh, ())),
                "zwp_idle_inhibit_manager_v1" => g.idle_inhibit = Some(reg.bind(name, 1, qh, ())),
                "wp_alpha_modifier_v1" => g.alpha_modifier = Some(reg.bind(name, 1, qh, ())),
                "wl_subcompositor" => g.subcompositor = Some(reg.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

macro_rules! globals_ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Globals {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
globals_ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, ZwlrLayerShellV1, wl_seat::WlSeat, WpCursorShapeManagerV1, WpSinglePixelBufferManagerV1, WpViewporter, ZwpIdleInhibitManagerV1, WpAlphaModifierV1, wl_subcompositor::WlSubcompositor);

/// Data that is expensive to load, kept between overlay openings.
#[derive(Default)]
struct Resources {
    apps: Option<Vec<DesktopApp>>,
    emojis: Option<Vec<EmojiItem>>,
    /// Background loader of the app list and icons (daemon only).
    preload: Option<Preloader>,
}

impl Resources {
    fn apps(&mut self) -> Vec<DesktopApp> {
        // The daemon keeps the list current in the background (apps_watch.rs). Only the very
        // first opening, before any preload finished, scans inline.
        self.apps.get_or_insert_with(load_all_desktop_apps).clone()
    }

    fn emojis(&mut self) -> Vec<EmojiItem> {
        self.emojis.get_or_insert_with(load_emojis).clone()
    }

    /// Take finished preload results: decoded icons into the image cache, the app list here.
    fn take_preloaded(&mut self) {
        let Some(p) = &mut self.preload else { return };
        let mut apps = None;
        for item in p.rx.try_iter() {
            match item {
                Preloaded::Image(d) => cairo_backend::insert_decoded(d),
                Preloaded::Apps(a) => apps = Some(a),
            }
        }
        if let Some(a) = apps {
            p.running = false;
            self.apps = Some(a);
        }
        // Apps were installed or removed. A scan that is already running may have missed the
        // change: this waits for it to finish and scans again.
        let installed = apps_watch::generation();
        if installed != p.apps_gen && !p.running {
            p.apps_gen = installed;
            p.start();
        }
        // A new icon theme or wallpaper means other pictures: decode them again. The
        // wallpaper is checked at most every 2 s (one stat).
        let gen = icons::generation();
        if gen != p.icon_gen {
            p.icon_gen = gen;
            p.start();
        }
        if p.wallpaper_checked.elapsed().as_secs() >= 2 {
            p.wallpaper_checked = std::time::Instant::now();
            let mtime = cairo_backend::wallpaper_panel_path().and_then(|f| std::fs::metadata(f).ok()?.modified().ok());
            if mtime != p.wallpaper_mtime {
                p.wallpaper_mtime = mtime;
                p.start();
            }
        }
    }
}

enum Preloaded {
    Image(cairo_backend::Decoded),
    Apps(Vec<DesktopApp>),
}

/// Scans .desktop files and decodes the launcher and spotlight icons on a worker thread, so
/// the first opening of either does not stall the UI (it took ~60 ms inline).
struct Preloader {
    tx: std::sync::mpsc::Sender<Preloaded>,
    rx: std::sync::mpsc::Receiver<Preloaded>,
    waker: Waker,
    running: bool,
    /// apps_watch generation the last scan started from.
    apps_gen: u64,
    icon_gen: u64,
    wallpaper_mtime: Option<std::time::SystemTime>,
    wallpaper_checked: std::time::Instant,
}

/// Run the UI thread ahead of background work, so a compile or an indexer cannot delay frames and
/// clicks. Only this thread: everything it starts (workers, launched apps) returns to normal
/// priority. Needs the session's nice limit; without it the thread just keeps its priority.
fn raise_ui_priority() {
    const UI_NICE: libc::c_int = -10;
    unsafe {
        let param: libc::sched_param = std::mem::zeroed();
        if libc::sched_setscheduler(0, libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, &param) != 0 || libc::setpriority(libc::PRIO_PROCESS, 0, UI_NICE) != 0 {
            eprintln!("[prism-ui] cannot raise the UI thread's priority: {}", std::io::Error::last_os_error());
        }
    }
}

/// Icon sizes (design px) of the app rows in launcher.rs and spotlight.rs.
const APP_ICON_SIZES: [u32; 2] = [32, 28];

impl Preloader {
    fn new(waker: Waker) -> Preloader {
        let (tx, rx) = std::sync::mpsc::channel();
        let wallpaper_mtime = cairo_backend::wallpaper_panel_path().and_then(|f| std::fs::metadata(f).ok()?.modified().ok());
        Preloader { tx, rx, waker, running: false, apps_gen: apps_watch::generation(), icon_gen: icons::generation(), wallpaper_mtime, wallpaper_checked: std::time::Instant::now() }
    }

    fn start(&mut self) {
        if self.running {
            return;
        }
        self.running = true;
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        let spawned = std::thread::Builder::new().name("prism-preload".into()).spawn(move || {
            // Asks logind what the power menu can offer, ahead of the first open.
            let _ = prism_common::power::support();
            let apps = load_all_desktop_apps();
            let mut seen = std::collections::HashSet::new();
            for app in &apps {
                let name = app.icon.as_deref().unwrap_or("");
                for px in APP_ICON_SIZES {
                    let path = [name, "application-x-executable"].iter().filter(|n| !n.is_empty()).find_map(|n| icons::lookup(n, px));
                    let Some(path) = path else { continue };
                    if !seen.insert((path.clone(), px)) {
                        continue;
                    }
                    if let Some(d) = cairo_backend::decode_image(&path.to_string_lossy(), px as i32, px as i32, true) {
                        let _ = tx.send(Preloaded::Image(d));
                    }
                }
            }
            // The launcher's wallpaper panel, at the size it is drawn (design px).
            if let Some(path) = cairo_backend::wallpaper_panel_path() {
                let (w, h) = launcher_themes::wallpaper_size(&Config::load().launcher_theme);
                if let Some(d) = cairo_backend::decode_image_cover(&path, w.round() as i32, h.round() as i32) {
                    let _ = tx.send(Preloaded::Image(d));
                }
            }
            launcher_themes::prepare(&Config::load().launcher_theme, &apps);
            let _ = tx.send(Preloaded::Apps(apps));
            waker.wake();
        });
        if spawned.is_err() {
            self.running = false;
        }
    }
}


fn parse_mode(args: &[String]) -> UiMode {
    match args.first().map(|s| s.as_str()) {
        Some("osd") => {
            let kind = args.get(1).cloned().unwrap_or_else(|| "volume".to_string());
            let val_num: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50.0);
            UiMode::Osd { kind, value: val_num / 100.0 }
        }
        Some(name) => mode_from_name(name).unwrap_or(UiMode::Launcher),
        None => UiMode::Launcher,
    }
}

/// Start or stop the agents that prompt through prompt.rs.
fn apply_agents(cfg: &prism_common::Config) {
    polkit_agent::apply(cfg);
    nm_agent::apply(cfg);
    xdp::apply(cfg);
    privacy::apply(cfg);
    keyring_prompter::apply(cfg);
    status_tray::apply(cfg);
    tray::apply(cfg);
}

fn mode_from_name(name: &str) -> Option<UiMode> {
    Some(match name {
        "notif" | "notif-center" | "notifications" => UiMode::NotificationCenter,
        "search" | "spotlight" => UiMode::Spotlight,
        "wallpaper" => UiMode::Wallpaper,
        "clipboard" => UiMode::Clipboard,
        "emoji" => UiMode::Emoji,
        "settings" => UiMode::Settings,
        "power" => UiMode::Power,
        "keybinds" => UiMode::Keybinds,
        "notes" => UiMode::Notes,
        "phone" => UiMode::Phone,
        "trim" => UiMode::Trim,
        "download-confirm" => UiMode::DownloadConfirm,
        "downloads" => UiMode::Downloads,
        "shortcuts" | "window-rules" => UiMode::CompEdit,
        "setup" | "guide" => UiMode::Guide,
        "launcher" => UiMode::Launcher,
        "desktop" => UiMode::Desktop,
        "desktop-edit" | "edit-mode" => UiMode::DesktopEdit,
        _ => return None,
    })
}

fn mode_name(mode: &UiMode) -> &'static str {
    match mode {
        UiMode::Launcher => "launcher",
        UiMode::Spotlight => "search",
        UiMode::Wallpaper => "wallpaper",
        UiMode::Clipboard => "clipboard",
        UiMode::Emoji => "emoji",
        UiMode::Settings => "settings",
        UiMode::Power => "power",
        UiMode::Keybinds => "keybinds",
        UiMode::Notes => "notes",
        UiMode::Phone => "phone",
        UiMode::Prompt => "prompt",
        UiMode::Trim => "trim",
        UiMode::DownloadConfirm => "download-confirm",
        UiMode::Downloads => "downloads",
        UiMode::CompEdit => "shortcuts",
        UiMode::Guide => "setup",
        UiMode::NotificationCenter => "notif-center",
        UiMode::Osd { .. } => "osd",
        UiMode::Desktop => "desktop",
        UiMode::DesktopEdit => "desktop-edit",
    }
}

/// One open overlay: its own event queue and state on the shared connection.
struct Overlay {
    queue: EventQueue<UiState>,
    state: UiState,
    osd_tx: Option<std::sync::mpsc::Sender<(String, f32)>>,
}

impl Overlay {
    fn open(
        conn: &Connection,
        g: &Globals,
        mode: UiMode,
        res: &mut Resources,
        waker: &Waker,
    ) -> Result<Overlay, String> {
        let mut event_queue: EventQueue<UiState> = conn.new_event_queue();
        let qh = event_queue.handle();
        let cfg = Config::load();
        let theme = Theme::load();

        // Load only what this mode needs; apps and emojis stay cached in the daemon.
        let apps = if matches!(mode, UiMode::Launcher | UiMode::Spotlight) { res.apps() } else { Vec::new() };

        let (initial_w, initial_h) = match &mode {
            UiMode::Osd { .. } => {
                let s = ui::scale_for(cfg.ui_scale, ui::screen_height());
                ((osd::CARD_W as f64 * s).round() as u32, (osd::CARD_H as f64 * s).round() as u32)
            }
            _ => (0, 0),
        };

        let (osd_tx, osd_rx) = if matches!(mode, UiMode::Osd { .. }) {
            let (tx, rx) = std::sync::mpsc::channel::<(String, f32)>();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let style = Style::new(&cfg, &theme);
        cairo_backend::set_global_font(&style.font_family);
        let transition = match &mode {
            UiMode::NotificationCenter => Some(Transition::enter(
                match cfg.notification_center_position.as_str() {
                    "center" => Motion::Scale,
                    "right" => Motion::SlideRight { distance: nc::CARD_W + 100.0 },
                    _ => Motion::SlideLeft { distance: nc::CARD_W + 100.0 },
                },
                &style,
            )),
            UiMode::Launcher | UiMode::Spotlight | UiMode::Wallpaper | UiMode::Clipboard | UiMode::Emoji
            | UiMode::Settings | UiMode::Power | UiMode::Keybinds | UiMode::Notes | UiMode::Phone | UiMode::Prompt | UiMode::Trim | UiMode::DownloadConfirm | UiMode::Downloads | UiMode::CompEdit | UiMode::Guide => Some(Transition::enter(Motion::Scale, &style)),
            _ => None,
        };
        // Rows of the new card start hidden; the entrance restarts with its first shown frame.
        if transition.is_some() {
            ui::begin_entrance(&style);
        }



        let (Some(comp), Some(layer_shell), Some(seat)) = (g.compositor.clone(), g.layer_shell.clone(), g.seat.clone()) else {
            return Err("compositor lacks wl_compositor / layer-shell / seat".into());
        };

        let mut state = UiState {
            compositor: Some(comp.clone()),
            shm: g.shm.clone(),
            layer_shell: Some(layer_shell.clone()),
            outputs: Vec::new(),
            keyboard: Some(seat.get_keyboard(&qh, ())),
            pointer: Some(seat.get_pointer(&qh, ())),
            seat: Some(seat),
            surface: None,
            layer_surface: None,
            mode,
            configured: false,
            width: initial_w,
            height: initial_h,
            exit_requested: false,
            apps,
            pointer_x: 0,
            scroll_finger: false,
            pointer_y: 0,
            pointer_down: false,
            card_bounds: (0, 0, 0, 0),
            theme,
            config: cfg,
            frame_callback: None,
            cursor_shape_manager: g.cursor_shape_manager.clone(),
            cursor_shape_device: None,
            pointer_serial: 0,
            needs_render: false,
            buffer_pool: Vec::new(),
            single_pixel_mgr: g.single_pixel_mgr.clone(),
            viewporter: g.viewporter.clone(),
            shadow: None,
            shadow_pending: None,
            scrim_surface: None,
            backdrop: None,
            scrim_layer: None,
            scrim_viewport: None,
            scrim_buffer: None,
            alpha_surface: None,
            alpha_multiplier: u32::MAX,
            card_viewport: None,
            geom: (0, 0, 0, 0),
            buffer_committed: false,
            scrim_configured: false,
            card_surface: false,
            surf_rect: (0, 0, 0, 0),
            pointer_on_scrim: false,
            hover_pending: false,
            animating: false,
            last_frame_at: None,
            frame_dt: 1.0 / 60.0,
            osd_rx,
            osd: None,
            osd_margin: i32::MIN,
            xkb_context: xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            repeat_rate: 25,
            repeat_delay: 600,
            repeat: None,
            style: style.clone(),
            motion: transition.as_ref().map(|t| t.motion).unwrap_or(Motion::Scale),
            transition,
            closing: false,
            content_dirty: true,
            content_animating: false,
            caret: None,
            card_cache: None,
            scrim_shown: 0.0,
            scrim_pending: None,
            settings: None,
            wallpaper: None,
            nc: None,
            view: None,
        };
        if let UiMode::Osd { kind, value } = &state.mode {
            state.osd = Some(osd::OsdView::new(kind, *value * 100.0, &state.style));
        }
        if matches!(state.mode, UiMode::NotificationCenter) {
            state.nc = Some(nc::NcView::new(waker.clone()));
        }
        match state.mode {
            UiMode::Launcher => state.view = Some(launcher_themes::view(&state.config.launcher_theme, std::mem::take(&mut state.apps))),
            UiMode::Spotlight => {
                state.view = Some(Box::new(spotlight::SpotlightView::new(std::mem::take(&mut state.apps), state.config.clone(), waker.clone())))
            }
            UiMode::Power => state.view = Some(Box::new(power::PowerView::new(&state.config))),
            UiMode::Clipboard => state.view = Some(Box::new(clipboard::ClipboardView::new(ClipboardStore::load().items))),
            UiMode::Emoji => state.view = Some(Box::new(emoji::EmojiView::new(res.emojis()))),
            UiMode::Keybinds => state.view = Some(Box::new(keybinds::KeybindsView::new(waker.clone()))),
            UiMode::Notes => state.view = Some(Box::new(notes::NotesView::new())),
            UiMode::Phone => state.view = Some(Box::new(phone::PhoneView::new(waker.clone()))),
            UiMode::Trim => state.view = Some(Box::new(trim::TrimView::new(waker.clone()))),
            UiMode::DownloadConfirm => state.view = Some(Box::new(download_confirm::DownloadConfirmView::new(waker.clone()))),
            UiMode::Downloads => state.view = Some(Box::new(downloads_card::DownloadsView::new(waker.clone()))),
            UiMode::CompEdit => state.view = Some(Box::new(compedit::CompEditView::new())),
            UiMode::Guide => state.view = Some(Box::new(guide::GuideView::new(waker.clone()))),
            UiMode::Prompt => match prompt::next_view() {
                Some(v) => state.view = Some(Box::new(v)),
                None => return Err("no prompt queued".into()),
            },
            _ => {}
        }
        if matches!(state.mode, UiMode::Settings) {
            state.settings = Some(settings::SettingsView::new(&state.config, waker.clone()));
        }

        // Modal cards: a full-screen scrim (dim, click-outside) plus a card-sized surface.
        let uses_card_surface = matches!(
            state.mode,
            UiMode::Launcher | UiMode::Spotlight | UiMode::Wallpaper | UiMode::Clipboard | UiMode::Emoji
                | UiMode::Settings | UiMode::NotificationCenter | UiMode::Power | UiMode::Keybinds | UiMode::Notes | UiMode::Phone | UiMode::Prompt | UiMode::Trim | UiMode::DownloadConfirm | UiMode::Downloads | UiMode::CompEdit | UiMode::Guide
        );
        if uses_card_surface {
            // The power menu blurs the whole screen behind its dim. Mapped first so it stays below
            // the scrim, and separate from it: Mango's blur breaks on a surface with a subsurface
            // (the card shadow).
            if matches!(state.mode, UiMode::Power) && state.style.blur && state.single_pixel_mgr.is_some() && state.viewporter.is_some() {
                let surface = comp.create_surface(&qh, ());
                let layer = layer_shell.get_layer_surface(&surface, None, Layer::Top, "prism-scrim-blur".to_string(), &qh, BackdropTag);
                layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                layer.set_size(0, 0);
                surface.commit();
                state.backdrop = Some(Backdrop { surface, layer, viewport: None, buffer: None });
                while state.backdrop.as_ref().is_some_and(|b| b.buffer.is_none()) && !state.exit_requested {
                    event_queue.blocking_dispatch(&mut state).map_err(|e| e.to_string())?;
                }
            }
            // The scrim's configure reports the logical screen size the card is laid out in. It
            // is on the Top layer so it is always below the card (Overlay).
            let scrim = comp.create_surface(&qh, ());
            let scrim_layer = layer_shell.get_layer_surface(&scrim, None, Layer::Top, "prism-scrim".to_string(), &qh, ScrimTag);
            scrim_layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
            scrim_layer.set_exclusive_zone(-1);
            scrim_layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            scrim_layer.set_size(0, 0);
            scrim.commit();
            state.scrim_surface = Some(scrim);
            state.scrim_layer = Some(scrim_layer);
            while !state.scrim_configured && !state.exit_requested {
                event_queue.blocking_dispatch(&mut state).map_err(|e| e.to_string())?;
            }
            // Drop shadow between the dim and the card (not for the full-width wallpaper strip).
            if !matches!(state.mode, UiMode::Wallpaper) {
                if let Some(scrim) = &state.scrim_surface {
                    state.shadow = shadow::Shadow::new(&comp, g.subcompositor.as_ref(), g.viewporter.as_ref(), g.alpha_modifier.as_ref(), scrim, &qh);
                }
            }
        }

        if matches!(state.mode, UiMode::Wallpaper) {
            state.wallpaper = Some(wallpaper::WallpaperView::new(&state.config, state.width as f64 / state.style.scale, waker.clone()));
        }
        let surface = comp.create_surface(&qh, ());
        let (layer_type, namespace, anchor, keyboard_mode) = match state.mode {
            UiMode::Osd { .. } => (Layer::Overlay, "prism-osd", Anchor::Bottom, KeyboardInteractivity::None),
            // The carousel has no card background; it must not be blurred.
            UiMode::Wallpaper => (Layer::Overlay, "prism-wallpaper", Anchor::Top | Anchor::Left, KeyboardInteractivity::Exclusive),
            UiMode::Launcher if launcher_themes::floating(&state.config.launcher_theme) => (Layer::Overlay, "prism-wallpaper", Anchor::Top | Anchor::Left, KeyboardInteractivity::Exclusive),
            UiMode::Power if power::floating(&state.config.power_theme) => (Layer::Overlay, "prism-wallpaper", Anchor::Top | Anchor::Left, KeyboardInteractivity::Exclusive),
            _ => (Layer::Overlay, "prism-modal", Anchor::Top | Anchor::Left, KeyboardInteractivity::Exclusive),
        };
        // Blurred card namespaces get a "-solid" twin when blur is turned off (see ui::Style::namespace).
        let namespace = match namespace {
            "prism-modal" | "prism-osd" | "prism-notification" => state.style.namespace(namespace),
            other => other.to_string(),
        };
        let layer_surface = layer_shell.get_layer_surface(&surface, None, layer_type, namespace, &qh, ());
        layer_surface.set_anchor(anchor);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(keyboard_mode);
        if uses_card_surface {
            state.card_surface = true;
            state.layer_surface = Some(layer_surface.clone());
            let rect = state.desired_card_rect();
            state.surf_rect = rect;
            layer_surface.set_size(rect.2.max(1) as u32, rect.3.max(1) as u32);
            layer_surface.set_margin(rect.1, 0, 0, rect.0);
        } else {
            layer_surface.set_size(state.width, state.height);
            if let Some(v) = &state.osd {
                state.osd_margin = (v.margin() as f64 * state.style.scale).round() as i32;
                layer_surface.set_margin(0, 0, state.osd_margin, 0);
            }
        }
        if state.card_surface {
            state.alpha_surface = g.alpha_modifier.as_ref().map(|m| m.get_surface(&surface, &qh, ()));
            if matches!(state.motion, Motion::Scale) {
                state.card_viewport = g.viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
                // Map at the first, smallest geometry of the grow-in, not at full size.
                let pose = state.transition.as_ref().map(|t| t.sample()).unwrap_or(Pose::REST);
                let geo = state.geometry_for(&pose);
                state.apply_geometry(geo);
            }
        }
        surface.commit();
        state.surface = Some(surface);
        state.layer_surface = Some(layer_surface);
        Ok(Overlay { queue: event_queue, state, osd_tx })
    }

    fn name(&self) -> &'static str {
        mode_name(&self.state.mode)
    }

    fn is_modal(&self) -> bool {
        self.state.card_surface
    }

    fn closing(&self) -> bool {
        self.state.closing || self.state.exit_requested
    }

    fn close(&mut self) {
        self.state.exit_requested = true;
    }

    fn is_prompt(&self) -> bool {
        matches!(self.state.mode, UiMode::Prompt)
    }

    /// Timers, background data and the exit transition. Returns the next deadline (ms, -1 =
    /// none) or None once the overlay has finished and should be torn down.
    fn tick(&mut self) -> Option<i32> {
        let qh = self.queue.handle();
        let state = &mut self.state;
        let now = std::time::Instant::now();

        if state.exit_requested && state.card_surface && !state.closing && state.style.animations && state.configured {
            state.closing = true;
            state.exit_requested = false;
            let motion = state.motion;
            // An enter that is still running hands over its position and speed.
            let exit = Transition::exit(motion, &state.style, state.transition.as_ref());
            state.transition = Some(if state.collapse() { exit.with_min_scale(ui::COLLAPSE_SCALE) } else { exit });
            if let Some(tr) = state.transition.as_mut() {
                tr.start_clock();
            }
            state.animating = true;
            if state.frame_callback.is_none() {
                state.draw_frame(&qh);
            }
        }
        if state.exit_requested {
            return None;
        }

        if state.settings.as_mut().map(|s| s.poll()).unwrap_or(false) {
            state.apply_settings_layer(&qh);
            state.render_and_commit(&qh);
        }
        if state.nc.as_mut().map(|v| v.poll()).unwrap_or(false) {
            state.render_and_commit(&qh);
        }
        if state.nc.as_ref().is_some_and(|v| v.wants_close()) {
            state.exit_requested = true;
        }
        if let Some((redraw, close)) = state.wallpaper.as_mut().map(|w| w.poll()) {
            if close {
                state.exit_requested = true;
            } else if redraw {
                state.render_and_commit(&qh);
            }
        }
        if state.view.as_mut().map(|v| v.poll()).unwrap_or(false) {
            state.render_and_commit(&qh);
        }
        state.apply_settings_layer(&qh);
        if state.view.as_ref().is_some_and(|v| v.wants_close()) {
            state.exit_requested = true;
        }

        let mut timeout_ms: i32 = -1;
        if let UiMode::Osd { .. } = state.mode {
            let update = state.osd_rx.as_ref().and_then(|rx| rx.try_iter().last());
            let style = state.style.clone();
            let mut redraw = false;
            if let Some(v) = state.osd.as_mut() {
                if let Some((kind, value)) = update {
                    v.update(&kind, value * 100.0, &style);
                    redraw = true;
                }
                if let Some(wait) = v.tick(&style) {
                    timeout_ms = (wait.as_millis() as i32).max(1);
                } else if v.finished() {
                    state.exit_requested = true;
                } else if v.animating() {
                    redraw = true;
                }
            }
            if redraw && !state.exit_requested {
                state.render_and_commit(&qh);
            }
        }
        if let Some((key, at)) = state.repeat {
            if now >= at && state.repeat_rate > 0 {
                state.repeat = Some((key, now + std::time::Duration::from_micros(1_000_000 / state.repeat_rate as u64)));
                state.handle_key(key, &qh);
            }
            if let Some((_, at)) = state.repeat {
                let ms = (at.saturating_duration_since(std::time::Instant::now()).as_millis() as i32).max(1);
                timeout_ms = if timeout_ms < 0 { ms } else { timeout_ms.min(ms) };
            }
        }
        if let Some(d) = state.nc.as_ref().and_then(|v| v.wake_in()) {
            let ms = (d.as_millis() as i32).max(1);
            timeout_ms = if timeout_ms < 0 { ms } else { timeout_ms.min(ms) };
        }
        if let Some(d) = state.view.as_ref().and_then(|v| v.wake_in()) {
            let ms = (d.as_millis() as i32).max(1);
            timeout_ms = if timeout_ms < 0 { ms } else { timeout_ms.min(ms) };
        }
        // A text caret blinks: re-render when its phase changes.
        if let Some(drawn) = state.caret {
            if ui::caret_phase() != drawn {
                state.render_and_commit(&qh);
            } else if let Some(d) = ui::caret_wake_in() {
                let ms = (d.as_millis() as i32).max(1);
                timeout_ms = if timeout_ms < 0 { ms } else { timeout_ms.min(ms) };
            }
        }
        if state.exit_requested {
            return None;
        }
        // Compositors send no frame callbacks for surfaces that are off-screen (the OSD starts
        // below the screen edge). If a callback is overdue while frames are wanted, drop it and
        // drive the frame from here.
        if state.configured && (state.animating || state.needs_render) {
            const STALE: std::time::Duration = std::time::Duration::from_millis(40);
            let overdue = state.last_frame_at.map(|t| now.duration_since(t) >= STALE).unwrap_or(true);
            if state.frame_callback.is_some() && overdue {
                state.frame_callback = None;
                state.draw_frame(&qh);
            }
            if state.animating || state.needs_render {
                timeout_ms = if timeout_ms < 0 { 41 } else { timeout_ms.min(41) };
            }
        }
        Some(timeout_ms)
    }

    fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.state).is_ok()
    }

    /// Destroy every protocol object this overlay created (the process keeps running).
    fn teardown(mut self) {
        let s = &mut self.state;
        for slot in s.buffer_pool.drain(..) {
            slot.destroy();
        }
        if let Some((img, _, _)) = s.card_cache.take() {
            unsafe { cairo_backend::cairo_surface_destroy(img) };
        }
        if let Some(a) = s.alpha_surface.take() {
            a.destroy();
        }
        if let Some(v) = s.card_viewport.take() {
            v.destroy();
        }
        if let Some(l) = s.layer_surface.take() {
            l.destroy();
        }
        if let Some(sf) = s.surface.take() {
            sf.destroy();
        }
        if let Some(sh) = s.shadow.take() {
            sh.destroy();
        }
        if let Some(v) = s.scrim_viewport.take() {
            v.destroy();
        }
        if let Some(l) = s.scrim_layer.take() {
            l.destroy();
        }
        if let Some(sf) = s.scrim_surface.take() {
            sf.destroy();
        }
        if let Some(b) = s.scrim_buffer.take() {
            b.destroy();
        }
        if let Some(b) = s.backdrop.take() {
            if let Some(v) = b.viewport {
                v.destroy();
            }
            b.layer.destroy();
            b.surface.destroy();
            if let Some(buffer) = b.buffer {
                buffer.destroy();
            }
        }
        if let Some(d) = s.cursor_shape_device.take() {
            d.destroy();
        }
        if let Some(k) = s.keyboard.take() {
            k.release();
        }
        if let Some(p) = s.pointer.take() {
            p.release();
        }
        let _ = self.queue.dispatch_pending(&mut self.state);
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    // The file dialog helper: no secrets, and it must stay dumpable for xdg-desktop-portal.
    if args.first().map(String::as_str) == Some(portal::CHILD_ARG) {
        portal::child_main(&args[1..]);
    }
    // Polkit, keyring and Wi-Fi/VPN passwords pass through this process.
    prism_common::secret::harden_process();
    if env::var("WAYLAND_DISPLAY").is_err() && env::var("WAYLAND_SOCKET").is_err() {
        eprintln!("Error: WAYLAND_DISPLAY is not set. prism-ui requires an active Wayland compositor.");
        exit(1);
    }
    let daemon = args.first().map(|s| s == "daemon").unwrap_or(false);
    if daemon && !acquire_instance_lock("prism-ui-daemon.lock") {
        eprintln!("[prism-ui] daemon already running. Exiting.");
        exit(0);
    }
    if daemon {
        raise_ui_priority();
    }
    let mode = if daemon { None } else { Some(parse_mode(&args)) };
    if let Some(UiMode::Desktop) = mode {
        if !acquire_instance_lock("prism-desktop.lock") {
            eprintln!("[prism-ui] Desktop layer already running. Exiting.");
            exit(0);
        }
    }
    // `prism-ui notif <summary> [body] [app]`: show one popup (debugging).
    let toast = match (args.first().map(|s| s.as_str()), args.get(1)) {
        (Some("notif"), Some(summary)) => Some(prism_common::NotifOverlayMessage::Add {
            id: 1,
            summary: summary.clone(),
            body: args.get(2).cloned().unwrap_or_default(),
            app_name: args.get(3).cloned().unwrap_or_else(|| "System".to_string()),
            timeout_ms: 5000,
            icon: None,
            image: None,
            urgency: 1,
            actions: Vec::new(),
            desktop_entry: String::new(),
            preview: false,
        }),
        _ => None,
    };
    let mode = if toast.is_some() { None } else { mode };
    run(mode, toast, daemon);
}

/// The event loop for the daemon (`initial` = None) or a single standalone overlay.
fn run(initial: Option<UiMode>, toast: Option<prism_common::NotifOverlayMessage>, daemon: bool) {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to connect to Wayland compositor: {}", e);
            exit(1);
        }
    };
    let waker = Waker::new().expect("failed to create wake pipe");
    cairo_backend::start_image_decoder(waker.clone());
    // Icon theme index (gsettings), built in the background and kept in sync.
    let icon_waker = waker.clone();
    icons::start(move || icon_waker.wake());
    let mut gq: EventQueue<Globals> = conn.new_event_queue();
    let mut globals = Globals {
        compositor: None,
        shm: None,
        layer_shell: None,
        seat: None,
        cursor_shape_manager: None,
        single_pixel_mgr: None,
        viewporter: None,
        idle_inhibit: None,
        alpha_modifier: None,
        subcompositor: None,
    };
    let _registry = conn.display().get_registry(&gq.handle(), ());
    if gq.roundtrip(&mut globals).is_err() {
        exit(1);
    }

    let mut res = Resources::default();
    let mut overlays: Vec<Overlay> = Vec::new();
    let mut desktop: Option<desktop::Desktop> = None;
    let mut edit: Option<desktop::edit::EditSession> = None;

    // Commands from prismd (daemon only).
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<prism_common::UiCommand>();
    if daemon {
        let path = prism_common::get_ui_socket_path();
        let _ = std::fs::remove_file(&path);
        match std::os::unix::net::UnixListener::bind(&path) {
            Ok(listener) => {
                let w = waker.clone();
                std::thread::spawn(move || {
                    use std::io::{BufRead, BufReader};
                    for stream in listener.incoming().flatten() {
                        for line in BufReader::new(stream).lines().map_while(Result::ok) {
                            match serde_json::from_str::<prism_common::UiCommand>(&line) {
                                Ok(cmd) => {
                                    if cmd_tx.send(cmd).is_err() {
                                        return;
                                    }
                                    w.wake();
                                }
                                Err(e) => eprintln!("[prism-ui] bad command {:?}: {}", line, e),
                            }
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("[prism-ui] cannot bind {}: {}", path.display(), e);
                exit(1);
            }
        }
        desktop = desktop::Desktop::new(&conn, waker.clone());
    } else {
        match initial.clone() {
            Some(UiMode::Desktop) => desktop = desktop::Desktop::new(&conn, waker.clone()),
            Some(UiMode::DesktopEdit) => edit = desktop::edit::EditSession::open(&conn),
            Some(mode) => match Overlay::open(&conn, &globals, mode, &mut res, &waker) {
                Ok(o) => overlays.push(o),
                Err(e) => {
                    eprintln!("[prism-ui] {}", e);
                    exit(1);
                }
            },
            None => {}
        }
    }

    // Notification popups: one stack for the whole process (toasts.rs).
    let mut toasts = toasts::Toasts::new(&conn, &globals);
    let mut caffeinate = caffeinate::Caffeinate::new(&conn);
    let mut picker: Option<picker::Picker> = None;
    let mut shooter: Option<capture::Overlay> = None;
    // Portal requests waiting for the screenshot overlay / colour picker.
    let mut portal_shot: Option<std::sync::mpsc::Sender<Option<prism_capture::Image>>> = None;
    let mut portal_color: Option<std::sync::mpsc::Sender<Option<(u8, u8, u8)>>> = None;
    let mut indicator: Option<rec_indicator::Indicator> = None;
    let mut dl_pill: Option<downloads_pill::Pill> = None;
    // Focus's automatic silent mode as last sent to prismd.
    let mut auto_silent = false;
    // A fresh install (no config yet): the setup guide opens shortly after start.
    let mut guide_at = (daemon && !Config::load().setup_done).then(|| std::time::Instant::now() + std::time::Duration::from_secs(4));
    let mut guide_tries = 0u32;
    // Microphone / camera / screen-share pill (privacy.rs), checked when PipeWire or the
    // portal change.
    let mut privacy_pill: Option<privacy::Indicator> = None;
    // Window switcher (Alt+Tab) and the window list it shows, most recent first.
    let mut switcher: Option<switcher::Switcher> = None;
    let mut tracker = if daemon { wintrack::Tracker::new(&conn) } else { None };
    let mut privacy_seen = (u64::MAX, u64::MAX);
    // Built-in wallpaper daemon (Settings > Wallpaper > Wallpaper Daemon = Built-in).
    let mut background = if daemon { background::Background::new(&conn, &globals, waker.clone()) } else { None };
    let mut config_seq = desktop.as_ref().map(|d| d.config_seq()).unwrap_or(0);
    if daemon {
        prompt::init(waker.clone());
        xdp::init(waker.clone());
        apps_watch::start(waker.clone());
        apply_agents(&prism_common::Config::load());
        let mut p = Preloader::new(waker.clone());
        p.start();
        res.preload = Some(p);
    }
    if let (Some(t), Some(msg)) = (toasts.as_mut(), toast) {
        t.handle(msg);
    }
    loop {
        res.take_preloaded();
        // Agent prompts (polkit, network secrets): one card at a time, above everything.
        if prompt::pending() && !overlays.iter().any(|o| o.is_prompt()) {
            match Overlay::open(&conn, &globals, UiMode::Prompt, &mut res, &waker) {
                Ok(o) => overlays.push(o),
                Err(e) => eprintln!("[prism-ui] cannot open prompt: {e}"),
            }
        }
        // Portal (xdp.rs): screenshots and colour picks for apps.
        for req in xdp::take() {
            match req {
                xdp::UiReq::Screenshot { interactive: false, reply } => {
                    let cfg = Config::load();
                    let _ = reply.send(capture::capture_screen_now(&conn, None, cfg.capture_cursor).map_err(|e| eprintln!("[prism-ui] portal screenshot: {e}")).ok());
                }
                xdp::UiReq::Screenshot { interactive: true, reply } => {
                    if shooter.as_ref().is_some_and(|s| !s.finished()) {
                        let _ = reply.send(None);
                        continue;
                    }
                    for o in overlays.iter_mut().filter(|o| o.is_modal()) {
                        o.close();
                    }
                    let cfg = Config::load();
                    match capture::Overlay::open(&conn, &globals, capture::Kind::parse(&cfg.capture_last_mode), false, &cfg) {
                        Ok(o) => {
                            shooter = Some(o);
                            portal_shot = Some(reply);
                        }
                        Err(e) => {
                            eprintln!("[prism-ui] portal screenshot: {e}");
                            let _ = reply.send(None);
                        }
                    }
                }
                xdp::UiReq::PickColor { reply } => {
                    if picker.as_ref().is_some_and(|p| !p.finished()) {
                        let _ = reply.send(None);
                        continue;
                    }
                    for o in overlays.iter_mut().filter(|o| o.is_modal()) {
                        o.close();
                    }
                    picker = picker::Picker::open(&conn, &globals);
                    match picker.as_mut() {
                        Some(p) => {
                            p.set_quiet();
                            portal_color = Some(reply);
                        }
                        None => {
                            let _ = reply.send(None);
                        }
                    }
                }
            }
        }
        // 1. Commands
        for cmd in cmd_rx.try_iter() {
            if let prism_common::UiCommand::Caffeinate(on) = cmd {
                caffeinate.set(on, &globals);
                continue;
            }
            if let prism_common::UiCommand::Switcher(act) = &cmd {
                match (switcher.as_mut(), act.as_str()) {
                    (Some(s), "next") => s.step(true),
                    (Some(s), "prev") => s.step(false),
                    (Some(_), "show") => {}
                    (Some(s), _) => s.cancel(),
                    (None, "next" | "prev" | "show") if Config::load().window_switcher => {
                        if let Some(t) = tracker.as_mut() {
                            t.dispatch();
                            for o in overlays.iter_mut().filter(|o| o.is_modal()) {
                                o.close();
                            }
                            switcher = switcher::Switcher::open(&conn, &globals, t.mru(), act == "prev", act != "show", waker.clone());
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if let prism_common::UiCommand::Away(on) = cmd {
                // The desktop's next tick stops (or restarts) the cat's frames.
                ui::set_away(on);
                continue;
            }
            if let prism_common::UiCommand::GameMode(on) = cmd {
                // Cards opened from now on are built without animations and blur.
                ui::set_game_mode(on);
                continue;
            }
            if let prism_common::UiCommand::Wallpaper(path) = &cmd {
                if let Some(b) = background.as_mut() {
                    b.show(path);
                }
                continue;
            }
            if let prism_common::UiCommand::ColorPicker(what) = &cmd {
                let open = picker.as_ref().map(|p| !p.finished()).unwrap_or(false);
                match (what.as_str(), open) {
                    ("close", true) | ("toggle", true) => picker.as_mut().unwrap().close(),
                    ("open", false) | ("toggle", false) => {
                        // Modal cards would be frozen into the screenshot: close them first.
                        for o in overlays.iter_mut().filter(|o| o.is_modal()) {
                            o.close();
                        }
                        picker = picker::Picker::open(&conn, &globals);
                    }
                    _ => {}
                }
                continue;
            }
            if let prism_common::UiCommand::Downloads(list) = &cmd {
                // An empty list: the pill finishes (a tick if a file arrives) and goes by itself.
                match (dl_pill.as_mut(), list.is_empty()) {
                    (Some(p), _) => p.set(list.clone()),
                    (None, false) => dl_pill = downloads_pill::Pill::open(&conn, &globals, list.clone(), indicator.is_some()),
                    (None, true) => {}
                }
                continue;
            }
            if let prism_common::UiCommand::DownloadFailed { name, error } = &cmd {
                match dl_pill.as_mut() {
                    Some(p) => p.failed(name.clone(), error.clone()),
                    None => dl_pill = downloads_pill::Pill::open_failed(&conn, &globals, name.clone(), error.clone(), indicator.is_some()),
                }
                continue;
            }
            if let prism_common::UiCommand::DownloadDone(name) = &cmd {
                match dl_pill.as_mut() {
                    Some(p) => p.done(name.clone()),
                    None => dl_pill = downloads_pill::Pill::open_done(&conn, &globals, name.clone(), indicator.is_some()),
                }
                continue;
            }
            if let prism_common::UiCommand::Recording(status) = &cmd {
                if let Some(i) = indicator.take() {
                    i.teardown();
                }
                if let Some(st) = status.clone() {
                    indicator = rec_indicator::Indicator::open(&conn, &globals, st);
                }
                continue;
            }
            if let prism_common::UiCommand::Capture(action) = &cmd {
                use prism_common::ipc::CaptureAction as A;
                let cfg = Config::load();
                let open = shooter.as_ref().is_some_and(|s| !s.finished());
                let kind = match action {
                    A::Toggle if open => {
                        shooter.as_mut().unwrap().close();
                        None
                    }
                    A::Close => {
                        if let Some(s) = shooter.as_mut() {
                            s.close();
                        }
                        None
                    }
                    A::Toggle => Some((capture::Kind::parse(&cfg.capture_last_mode), false)),
                    A::Open(k) if k == "text" => Some((capture::Kind::Region, false)),
                    A::Open(k) => Some((capture::Kind::parse(k), false)),
                    A::Record => Some((capture::Kind::parse(&cfg.capture_last_mode), true)),
                    A::Screen { save, clipboard } => {
                        let mut cfg = cfg.clone();
                        cfg.capture_copy_to_clipboard &= *clipboard;
                        let r = capture::capture_screen_now(&conn, None, cfg.capture_cursor).and_then(|img| capture::finish(img, &cfg, *save));
                        if let Err(e) = r {
                            eprintln!("[prism-ui] screenshot: {e}");
                        }
                        None
                    }
                    _ => None,
                };
                if let (Some((k, record)), false) = (kind, open) {
                    for o in overlays.iter_mut().filter(|o| o.is_modal()) {
                        o.close();
                    }
                    let text_mode = matches!(action, A::Open(k) if k == "text");
                    match capture::Overlay::open(&conn, &globals, k, record, &cfg) {
                        Ok(mut o) => {
                            if text_mode {
                                o.set_text_mode();
                            }
                            shooter = Some(o);
                        }
                        Err(e) => eprintln!("[prism-ui] screenshot: {e}"),
                    }
                }
                continue;
            }
            handle_command(cmd, &conn, &globals, &mut overlays, &mut edit, &mut toasts, &mut res, &waker);
        }

        // Images decoded off-thread since the last turn (views that missed them redraw).
        cairo_backend::drain_decoded();

        // 2. Overlays: timers / transitions; tear down finished ones.
        let mut timeout: i32 = -1;
        let mut i = 0;
        while i < overlays.len() {
            match overlays[i].tick() {
                Some(t) => {
                    if t >= 0 && (timeout < 0 || t < timeout) {
                        timeout = t;
                    }
                    i += 1;
                }
                None => {
                    overlays.remove(i).teardown();
                }
            }
        }
        if let Some(d) = desktop.as_mut() {
            let t = d.tick();
            if t >= 0 && (timeout < 0 || t < timeout) {
                timeout = t;
            }
        }
        if let Some(at) = guide_at {
            let now = std::time::Instant::now();
            if now >= at {
                guide_at = None;
                if !Config::load().setup_done {
                    handle_command(prism_common::UiCommand::Open("setup".into()), &conn, &globals, &mut overlays, &mut edit, &mut toasts, &mut res, &waker);
                    // It could not open (no screen yet at login, say): try again, less often
                    // each time, until it is on screen.
                    if !overlays.iter().any(|o| o.name() == "setup" && !o.closing()) {
                        guide_tries += 1;
                        let wait = (3u64 << guide_tries.min(4)).min(30);
                        eprintln!("[prism-ui] the setup guide did not open; trying again in {wait} s");
                        guide_at = Some(now + std::time::Duration::from_secs(wait));
                    }
                }
            } else {
                let ms = (at - now).as_millis() as i32 + 1;
                if timeout < 0 || ms < timeout {
                    timeout = ms;
                }
            }
        }
        // Focus: silent while the screen is shared or a fullscreen window has focus.
        if let Some(d) = desktop.as_ref() {
            let c = d.config();
            let want = (c.focus_on_screen_share && !xdp::casts().is_empty()) || (c.focus_on_fullscreen && tracker.as_ref().is_some_and(|t| t.focused_fullscreen()));
            if want != auto_silent {
                auto_silent = want;
                std::thread::spawn(move || {
                    let _ = prism_common::ipc::request(&prism_common::IpcCommand::Notif(prism_common::ipc::NotifAction::AutoSilent(want)));
                });
            }
        }
        // A video wallpaper rests while windows cover it (Settings > Wallpaper > Pause Video).
        if let (Some(b), Some(d)) = (background.as_mut(), desktop.as_ref()) {
            b.set_paused(match d.config().wallpaper_video_pause.as_str() {
                "windows" => d.windows_on_screen(),
                "covered" => d.background_covered(),
                "fullscreen" => tracker.as_ref().is_some_and(|t| t.focused_fullscreen()),
                _ => false,
            });
        }
        if let Some(t) = background.as_mut().and_then(|b| b.tick()) {
            if timeout < 0 || t < timeout {
                timeout = t;
            }
        }
        if let Some(s) = shooter.as_mut() {
            let t = s.tick();
            if t >= 0 && (timeout < 0 || t < timeout) {
                timeout = t;
            }
        }
        if let Some(i) = indicator.as_mut() {
            let t = i.tick();
            if timeout < 0 || t < timeout {
                timeout = t;
            }
        }
        if daemon {
            let seen = (pw_watch::generation(), xdp::generation());
            if seen != privacy_seen {
                privacy_seen = seen;
                let usage = if privacy::enabled() { privacy::current() } else { privacy::Usage::default() };
                match (usage.is_empty(), privacy_pill.as_mut()) {
                    (true, Some(_)) => privacy_pill.take().unwrap().teardown(),
                    (false, Some(p)) => p.set_usage(usage),
                    (false, None) => privacy_pill = privacy::Indicator::open(&conn, &globals, usage),
                    (true, None) => {}
                }
            }
        }
        if let Some(p) = privacy_pill.as_mut() {
            p.tick();
        }
        if let Some(p) = dl_pill.as_mut() {
            p.tick();
        }
        if dl_pill.as_ref().is_some_and(|p| p.finished()) {
            if let Some(p) = dl_pill.take() {
                p.teardown();
            }
        }
        if let Some(s) = switcher.as_mut() {
            s.tick();
        }
        if switcher.as_ref().is_some_and(|s| s.finished()) {
            if let switcher::Outcome::Switch(w) = switcher.take().unwrap().teardown() {
                if let (Some(t), Some(seat)) = (tracker.as_ref(), globals.seat.as_ref()) {
                    t.activate(&w, seat);
                }
            }
            let _ = conn.flush();
        }
        if shooter.as_ref().is_some_and(|s| s.finished()) {
            let s = shooter.take().unwrap();
            let kind = s.kind();
            let outcome = s.teardown();
            let _ = conn.flush();
            let mut cfg = Config::load();
            if let Some(tx) = portal_shot.take() {
                // For an app: the image goes back to it, nothing is saved or recorded here.
                let _ = tx.send(match outcome {
                    capture::Outcome::Image(img) => Some(img),
                    _ => None,
                });
                continue;
            }
            if let capture::Outcome::Record(job) = &outcome {
                // Remember the recording options, then hand the job to prismd.
                if cfg.capture_fps != job.fps || cfg.capture_audio_desktop != job.audio_desktop || cfg.capture_audio_mic != job.audio_mic {
                    cfg.capture_fps = job.fps;
                    cfg.capture_audio_desktop = job.audio_desktop;
                    cfg.capture_audio_mic = job.audio_mic;
                    let _ = cfg.save();
                }
                let job = job.clone();
                std::thread::spawn(move || {
                    if let Err(e) = prism_common::ipc::request(&prism_common::IpcCommand::Capture(prism_common::ipc::CaptureAction::Start(job))) {
                        eprintln!("[prism-ui] recording: {e}");
                    }
                });
            }
            if let capture::Outcome::Text(img) = &outcome {
                capture::copy_text(img.clone(), cfg.ocr_language.clone());
            }
            if let capture::Outcome::Image(img) = outcome {
                if let Err(e) = capture::finish(img, &cfg, true) {
                    eprintln!("[prism-ui] screenshot: {e}");
                }
            }
            if cfg.capture_last_mode != kind.name() {
                cfg.capture_last_mode = kind.name().to_string();
                let _ = cfg.save();
            }
        }
        if let Some(p) = picker.as_mut() {
            if !p.tick() {
                if let Some(p) = picker.take() {
                    if let Some(tx) = portal_color.take() {
                        let _ = tx.send(p.picked());
                    }
                    p.teardown();
                }
            }
        }
        if let Some(t) = toasts.as_mut() {
            let ms = t.tick();
            if ms >= 0 && (timeout < 0 || ms < timeout) {
                timeout = ms;
            }
        }
        if edit.as_ref().map(|e| e.finished()).unwrap_or(false) {
            if let Some(e) = edit.take() {
                e.teardown();
            }
        }
        if !daemon && overlays.is_empty() && desktop.is_none() && edit.is_none() && toasts.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
            break;
        }

        // 3. Wait for Wayland events, worker wake-ups, desktop file changes or a deadline.
        let _ = conn.flush();
        let mut ok = gq.dispatch_pending(&mut globals).is_ok();
        for o in overlays.iter_mut() {
            ok &= o.dispatch();
        }
        if let Some(d) = desktop.as_mut() {
            ok &= d.dispatch();
        }
        if let Some(e) = edit.as_mut() {
            ok &= e.dispatch();
        }
        if let Some(t) = toasts.as_mut() {
            ok &= t.dispatch();
        }
        ok &= caffeinate.dispatch();
        if let Some(b) = background.as_mut() {
            ok &= b.dispatch();
            if let Some(seq) = desktop.as_ref().map(|d| d.config_seq()).filter(|s| *s != config_seq) {
                config_seq = seq;
                let cfg = prism_common::Config::load();
                b.apply_config(&cfg);
                apply_agents(&cfg);
                ok &= b.dispatch();
            }
        }
        if let Some(p) = picker.as_mut() {
            ok &= p.dispatch();
        }
        if let Some(s) = shooter.as_mut() {
            ok &= s.dispatch();
        }
        if let Some(i) = indicator.as_mut() {
            ok &= i.dispatch();
        }
        if let Some(p) = dl_pill.as_mut() {
            ok &= p.dispatch();
        }
        if let Some(p) = privacy_pill.as_mut() {
            ok &= p.dispatch();
        }
        if let Some(s) = switcher.as_mut() {
            ok &= s.dispatch();
        }
        if let Some(t) = tracker.as_mut() {
            ok &= t.dispatch();
        }
        if !ok {
            eprintln!("[prism-ui] Wayland connection error (protocol error: {:?})", conn.protocol_error());
            break;
        }
        let _ = conn.flush();
        if let Some(guard) = conn.prepare_read() {
            use std::os::unix::io::AsRawFd;
            let mut fds = vec![
                libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: waker.fd(), events: libc::POLLIN, revents: 0 },
            ];
            if let Some(fd) = desktop.as_ref().and_then(|d| d.inotify_fd()) {
                fds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
            }
            if let Some(b) = background.as_ref() {
                fds.extend(b.video_fds().into_iter().map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }));
            }
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if ret > 0 && (fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0 {
                match guard.read() {
                    Ok(_) => {}
                    // Spurious readiness (EAGAIN) or a signal (EINTR) is not a disconnect: the
                    // events were already drained. Treating it as fatal killed the whole UI.
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                    Err(e) => {
                        eprintln!("[prism-ui] Wayland connection lost: {} (protocol error: {:?})", e, conn.protocol_error());
                        break;
                    }
                }
            } else {
                drop(guard);
            }
            if ret > 0 && (fds[1].revents & libc::POLLIN) != 0 {
                waker.drain();
            }
        }
        let _ = gq.dispatch_pending(&mut globals);
        for o in overlays.iter_mut() {
            o.dispatch();
        }
        if let Some(d) = desktop.as_mut() {
            d.dispatch();
        }
        if let Some(e) = edit.as_mut() {
            e.dispatch();
        }
        if let Some(t) = toasts.as_mut() {
            t.dispatch();
        }
        caffeinate.dispatch();
        if let Some(b) = background.as_mut() {
            b.dispatch();
        }
        if let Some(p) = picker.as_mut() {
            p.dispatch();
        }
        if let Some(s) = shooter.as_mut() {
            s.dispatch();
        }
        if let Some(i) = indicator.as_mut() {
            i.dispatch();
        }
        if let Some(p) = dl_pill.as_mut() {
            p.dispatch();
        }
        if let Some(p) = privacy_pill.as_mut() {
            p.dispatch();
        }
        if let Some(s) = switcher.as_mut() {
            s.dispatch();
        }
        if let Some(t) = tracker.as_mut() {
            t.dispatch();
        }
    }
    for o in overlays.drain(..) {
        o.teardown();
    }
    if let Some(t) = toasts.take() {
        t.teardown();
    }
    let _ = conn.flush();
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    cmd: prism_common::UiCommand,
    conn: &Connection,
    g: &Globals,
    overlays: &mut Vec<Overlay>,
    edit: &mut Option<desktop::edit::EditSession>,
    toasts: &mut Option<toasts::Toasts>,
    res: &mut Resources,
    waker: &Waker,
) {
    use prism_common::UiCommand;
    let open_modal = |name: &str, overlays: &mut Vec<Overlay>, res: &mut Resources| {
        // Close any other open modal (it plays its exit transition), then open this one.
        for o in overlays.iter_mut().filter(|o| o.is_modal() && !o.closing()) {
            o.close();
        }
        if let Some(mode) = mode_from_name(name) {
            match Overlay::open(conn, g, mode, res, waker) {
                Ok(o) => overlays.push(o),
                Err(e) => eprintln!("[prism-ui] cannot open {}: {}", name, e),
            }
        }
    };
    let cmd = match cmd {
        UiCommand::Trim(path) => {
            trim::set_pending(path.into());
            UiCommand::Open("trim".into())
        }
        UiCommand::DownloadConfirm(ask) => {
            download_confirm::set_pending(ask);
            UiCommand::Open("download-confirm".into())
        }
        UiCommand::Search(text) => {
            spotlight::set_pending_query(text);
            waker.wake();
            UiCommand::Open("search".into())
        }
        UiCommand::SettingsTab(tab) => {
            settings::open_on_tab(tab);
            waker.wake();
            UiCommand::Open("settings".into())
        }
        UiCommand::Open(name) | UiCommand::Toggle(name) if name == "window-rules" || name == "shortcuts" => {
            compedit::open_on_rules(name == "window-rules");
            UiCommand::Open("shortcuts".into())
        }
        c => c,
    };
    match cmd {
        UiCommand::Toggle(name) => {
            let canonical = mode_from_name(&name).map(|m| mode_name(&m)).unwrap_or("");
            let open = overlays.iter_mut().find(|o| o.is_modal() && !o.closing() && o.name() == canonical);
            match open {
                Some(o) => o.close(),
                None => open_modal(&name, overlays, res),
            }
        }
        UiCommand::Open(name) => {
            let canonical = mode_from_name(&name).map(|m| mode_name(&m)).unwrap_or("");
            if !overlays.iter().any(|o| o.is_modal() && !o.closing() && o.name() == canonical) {
                open_modal(&name, overlays, res);
            }
        }
        UiCommand::Close => {
            for o in overlays.iter_mut().filter(|o| o.is_modal()) {
                o.close();
            }
        }
        UiCommand::Osd { kind, value } => {
            let v = value / 100.0;
            match overlays.iter().find(|o| o.name() == "osd" && !o.closing()).and_then(|o| o.osd_tx.clone()) {
                Some(tx) => {
                    let _ = tx.send((kind, v));
                    waker.wake();
                }
                None => match Overlay::open(conn, g, UiMode::Osd { kind, value: v }, res, waker) {
                    Ok(o) => overlays.push(o),
                    Err(e) => eprintln!("[prism-ui] cannot open OSD: {}", e),
                },
            }
        }
        UiCommand::Notify(msg) => {
            if let Some(t) = toasts.as_mut() {
                t.handle(msg);
            }
        }
        // Handled by the run loop (it owns the inhibitor and the picker).
        UiCommand::Caffeinate(_) | UiCommand::GameMode(_) | UiCommand::Away(_) | UiCommand::Switcher(_) | UiCommand::ColorPicker(_) | UiCommand::Capture(_) | UiCommand::Recording(_) | UiCommand::Downloads(_) | UiCommand::DownloadDone(_) | UiCommand::DownloadFailed { .. } | UiCommand::Trim(_) | UiCommand::Wallpaper(_) | UiCommand::Search(_) | UiCommand::SettingsTab(_) | UiCommand::DownloadConfirm(_) => {}
        UiCommand::EditMode(action) => {
            let open = edit.as_ref().map(|e| !e.finished()).unwrap_or(false);
            let want_open = match action.as_str() {
                "open" => true,
                "close" | "discard" => false,
                _ => !open,
            };
            if want_open && !open {
                *edit = desktop::edit::EditSession::open(conn);
            } else if !want_open && open {
                if let Some(e) = edit.as_mut() {
                    e.close();
                }
            }
        }
    }
}



/// Hold an exclusive flock on `$XDG_RUNTIME_DIR/<name>` for the life of the process.
/// Returns false if another process already holds it.
fn acquire_instance_lock(name: &str) -> bool {
    let path = prism_common::runtime::dir().join(name);
    let Ok(file) = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&path) else {
        return true; // cannot lock; do not block startup
    };
    use std::os::fd::AsRawFd;
    let ok = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if ok {
        std::mem::forget(file); // keep the fd (and the lock) open until exit
    }
    ok
}


