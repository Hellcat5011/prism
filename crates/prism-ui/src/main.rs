use std::env;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

mod animation;
mod cairo_backend;
#[allow(dead_code)]
mod font;
#[allow(dead_code)]
mod render;
mod desktop;
mod icons;
mod portal;
mod settings;
mod status;
mod ui;
mod uikit;
mod wallpaper;
mod waker;

use ui::{Motion, Pose, Style, Transition};

use status::{StatusRequest, StatusScope, StatusSnapshot};
use waker::Waker;
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


use animation::SlideAnimation;
use prism_common::apps::{load_all_desktop_apps, search_apps, DesktopApp};
use prism_common::clipboard::{ClipboardItem, ClipboardStore};
use prism_common::emoji::{load_emojis, search_emojis, EmojiItem};
use prism_common::files::{search_files, FileResult};
use prism_common::keybinds::KeybindEntry;
use prism_common::anim::SpringAnimation;
use prism_common::{Config, Theme};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;

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
    Notification { summary: String, body: String, app_name: String },
    NotificationCenter,
    Power,
    Keybinds,
}

#[derive(Clone)]
enum SpotlightResult {
    App(DesktopApp),
    Folder(FileResult),
    File(FileResult),
}

struct SpotlightSection {
    title: &'static str,
    count: usize,
    items: Vec<SpotlightResult>,
}

fn get_emoji_recents_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache/prism/emoji_recents.json")
    } else {
        PathBuf::from("/tmp/prism_emoji_recents.json")
    }
}

fn load_emoji_recents() -> Vec<String> {
    let path = get_emoji_recents_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&content) {
            return list;
        }
    }
    Vec::new()
}

fn save_emoji_recents(recents: &[String]) {
    let path = get_emoji_recents_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(recents) {
        let _ = std::fs::write(&path, json);
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum ToastPhase {
    Entering,
    Shown,
    Leaving,
    Finished,
}

#[derive(Clone, Debug)]
struct PopupToast {
    id: u32,
    app_name: String,
    summary: String,
    body: String,
    created_at: std::time::Instant,
    timeout_ms: u64,
    slide_x: f64,
    opacity: f64,
    reveal: f64,
    card_height: f64,
    phase: ToastPhase,
    hovered: bool,
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
    search_query: String,
    selected_index: usize,
    selected_tab: usize,
    pointer_x: i32,
    pointer_y: i32,
    pointer_down: bool,
    card_bounds: (i32, i32, i32, i32),
    theme: Theme,
    config: Config,
    animation: Option<SlideAnimation>,
    frame_callback: Option<wl_callback::WlCallback>,
    clipboard_items: Vec<ClipboardItem>,
    emojis: Vec<EmojiItem>,
    scroll_offset: f64,
    launcher_scroll: f64,
    emoji_scroll: f64,
    keybinds_scroll: f64,
    keybinds: Vec<KeybindEntry>,
    emoji_recents: Vec<String>,
    spotlight_tab: usize,
    spotlight_height_anim: f64,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    pointer_serial: u32,
    spring_anim: Option<SpringAnimation>,
    spotlight_tx: Option<std::sync::mpsc::Sender<String>>,
    spotlight_rx: Option<std::sync::mpsc::Receiver<Vec<FileResult>>>,
    spotlight_files: Vec<FileResult>,
    caffeinate_active: bool,
    nc_tile_widths: [f64; 4],
    nc_expanded_group: Option<String>,
    nc_dnd_silent: bool,
    nc_notif_scroll: f64,
    nc_wifi_networks: Vec<prism_common::network::WifiNetwork>,
    nc_wifi_expanded_ssid: Option<String>,
    nc_wifi_password_input: String,
    _nc_wifi_scanning: bool,
    nc_network_scroll: f64,
    nc_bt_devices: Vec<prism_common::network::BluetoothDevice>,
    _nc_bt_scanning: bool,
    nc_bt_scroll: f64,
    nc_sink_menu_open: bool,
    nc_audio_sinks: Vec<prism_common::network::AudioSink>,
    notif_toasts: Vec<PopupToast>,
    notif_socket_rx: Option<std::sync::mpsc::Receiver<prism_common::NotifOverlayMessage>>,
    needs_render: bool,
    buffer_pool: Vec<BufferSlot>,

    // ── Stage-0 performance infrastructure ──────────────────────────────
    /// Cached system state from the background status worker (never queried on the render path).
    status: StatusSnapshot,
    status_tx: Option<std::sync::mpsc::Sender<StatusRequest>>,
    status_rx: Option<std::sync::mpsc::Receiver<StatusSnapshot>>,
    single_pixel_mgr: Option<WpSinglePixelBufferManagerV1>,
    viewporter: Option<WpViewporter>,
    /// Full-screen dim behind modal cards: 1x1 single-pixel buffer scaled by wp_viewporter,
    /// committed once, never redrawn, on an un-blurred namespace.
    scrim_surface: Option<wl_surface::WlSurface>,
    scrim_layer: Option<ZwlrLayerSurfaceV1>,
    scrim_viewport: Option<WpViewport>,
    scrim_buffer: Option<wl_buffer::WlBuffer>,
    scrim_configured: bool,
    /// True when the main surface is sized to the card instead of the whole screen.
    card_surface: bool,
    /// Current main-surface rectangle in screen coordinates (x, y, w, h).
    surf_rect: (i32, i32, i32, i32),
    /// Rectangle requested from the compositor and not yet configured.
    pending_rect: Option<(i32, i32, i32, i32)>,
    /// Screen-space origin of the surface the pointer is currently over.
    pointer_origin: (i32, i32),
    /// True while the last drawn frame was mid-animation (another frame is wanted).
    animating: bool,
    last_frame_at: Option<std::time::Instant>,
    /// Seconds since the previous presented frame (clamped), for frame-rate independent easing.
    frame_dt: f64,
    osd_rx: Option<std::sync::mpsc::Receiver<(String, f32)>>,
    osd_hide_at: Option<std::time::Instant>,
    /// Keyboard layout state from the compositor's keymap (wl_keyboard.keymap). Keys are
    /// translated through it instead of assuming raw US-layout evdev codes.
    xkb_context: xkbcommon::xkb::Context,
    xkb_state: Option<xkbcommon::xkb::State>,

    // ── Global UI ───────────────────────────────────────────────────────
    /// Look & motion shared by every card (see ui.rs), rebuilt when config/theme change.
    style: Style,
    /// Enter/exit transition of the card surface (composited from `card_cache`).
    transition: Option<Transition>,
    /// Exit transition running; the process exits when it finishes.
    closing: bool,
    /// Card content must be re-rendered (a redraw was requested). Transition-only frames
    /// re-composite the cached card without re-rendering text and images.
    content_dirty: bool,
    /// The last content render was mid in-card animation (spotlight height, NC tiles...).
    content_animating: bool,
    card_cache: Option<(*mut cairo_backend::CairoSurface, i32, i32)>,
    /// Current scrim level (0..1 of dim_opacity); -1 = nothing attached yet.
    scrim_shown: f64,
    scrim_pending: Option<f64>,
    /// Settings window state (settings.rs) when this overlay is the Settings card.
    settings: Option<settings::SettingsView>,
    /// Wallpaper selector state (wallpaper.rs) when this overlay is the wallpaper picker.
    wallpaper: Option<wallpaper::WallpaperView>,
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

pub const EMOJI_CATEGORIES: [(&str, &str, Option<&str>); 11] = [
    ("🕒", "Recent", Some("recent")),
    ("🌐", "All", None),
    ("😀", "Smileys", Some("Smileys & Emotion")),
    ("👋", "People", Some("People & Body")),
    ("🐶", "Nature", Some("Animals & Nature")),
    ("🍔", "Food", Some("Food & Drink")),
    ("✈️", "Travel", Some("Travel & Places")),
    ("⚽", "Activities", Some("Activities")),
    ("💡", "Objects", Some("Objects")),
    ("🔣", "Symbols", Some("Symbols")),
    ("🏁", "Flags", Some("Flags")),
];

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
impl Dispatch<WpViewporter, ()> for UiState {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewport, ()> for UiState {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
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
                    // Card-sized surface: width/height stay the logical screen size; only the
                    // surface rectangle changes.
                    if let Some(rect) = state.pending_rect.take() {
                        state.surf_rect = rect;
                    }
                    if width > 0 && height > 0 {
                        state.surf_rect.2 = width as i32;
                        state.surf_rect.3 = height as i32;
                    }
                } else {
                    state.width = if width > 0 { width } else { state.width };
                    state.height = if height > 0 { height } else { state.height };
                    state.surf_rect = (0, 0, state.width as i32, state.height as i32);
                }
                state.content_dirty = true;
                state.draw_frame(qh);
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
                state.animating = true;
            }
        }
        if state.configured && !state.exit_requested && (state.needs_render || state.animating) {
            state.draw_frame(qh);
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
                // (0, 0); the card surface sits at surf_rect's origin.
                state.pointer_origin = if state.scrim_surface.as_ref() == Some(&surface) {
                    (0, 0)
                } else {
                    (state.surf_rect.0, state.surf_rect.1)
                };
                state.pointer_x = surface_x as i32 + state.pointer_origin.0;
                state.pointer_y = surface_y as i32 + state.pointer_origin.1;
                state.set_cursor_shape(qh, CursorShape::Default);
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.pointer_x = surface_x as i32 + state.pointer_origin.0;
                state.pointer_y = surface_y as i32 + state.pointer_origin.1;
                if let Some(sv) = state.settings.as_mut() {
                    let (px, py) = (surface_x + state.pointer_origin.0 as f64, surface_y + state.pointer_origin.1 as f64);
                    if sv.motion(px, py, state.pointer_down) {
                        state.render_and_commit(qh);
                    }
                    return;
                }

                let px = state.pointer_x as f64;
                let py = state.pointer_y as f64;
                let (cx, cy, cw, ch) = state.card_bounds;

                // Dynamic cursor shape detection
                let is_over_text = match &state.mode {
                    UiMode::Launcher => {
                        let left_w = (cw as f64 * 0.60).round();
                        let right_x = cx as f64 + left_w;
                        let pad = 20.0;
                        let content_x = right_x + pad;
                        let content_w = (cw as f64 - left_w) - (pad * 2.0);
                        px >= content_x && px <= content_x + content_w && py >= cy as f64 + 20.0 && py <= cy as f64 + 68.0
                    }
                    UiMode::Spotlight => {
                        px >= cx as f64 + 20.0 && px <= (cx + cw) as f64 - 20.0 && py >= cy as f64 + 16.0 && py <= cy as f64 + 60.0
                    }
                    UiMode::Emoji => {
                        let right_x = cx as f64 + 170.0;
                        let right_w = (cw as f64 - 190.0).max(100.0);
                        px >= right_x && px <= right_x + right_w && py >= cy as f64 + 20.0 && py <= cy as f64 + 64.0
                    }
                    UiMode::Clipboard => {
                        let content_x = cx as f64 + 24.0;
                        let content_w = cw as f64 - 48.0;
                        px >= content_x && px <= content_x + content_w && py >= cy as f64 + 16.0 && py <= cy as f64 + 56.0
                    }
                    UiMode::Keybinds => {
                        let search_w = 340.0;
                        let search_x = cx as f64 + cw as f64 - search_w - 28.0;
                        let search_y = cy as f64 + 20.0;
                        px >= search_x && px <= search_x + search_w - 32.0 && py >= search_y && py <= search_y + 38.0
                    }
                    _ => false,
                };

                let is_over_clickable = match &state.mode {
                    UiMode::Launcher => {
                        let left_w = (cw as f64 * 0.60).round();
                        let content_x = cx as f64 + left_w + 20.0;
                        let content_w = (cw as f64 - left_w) - 40.0;
                        let list_start_y = cy as f64 + 100.0;
                        px >= content_x && px <= content_x + content_w && py >= list_start_y && py <= (cy + ch - 20) as f64
                    }
                    UiMode::Spotlight => {
                        let chips = py >= cy as f64 + 58.0 && py <= cy as f64 + 88.0 && px >= cx as f64 + 18.0 && px <= cx as f64 + 440.0;
                        let list_start_y = cy as f64 + 96.0;
                        let in_list = px >= cx as f64 && px <= (cx + cw) as f64 && py >= list_start_y && py <= (cy + ch) as f64;
                        chips || in_list
                    }
                    UiMode::Emoji => {
                        (px >= cx as f64 + 16.0 && px <= cx as f64 + 156.0 && py >= cy as f64 + 20.0) ||
                        (px >= cx as f64 + 170.0 && py >= cy as f64 + 78.0)
                    }
                    UiMode::Power => {
                        px >= cx as f64 && px <= (cx + cw) as f64 && py >= cy as f64 && py <= (cy + ch) as f64
                    }
                    UiMode::NotificationCenter => {
                        px >= cx as f64 + 10.0 && px <= (cx + cw) as f64 - 10.0 && py >= cy as f64 + 10.0 && py <= (cy + ch) as f64 - 10.0
                    }
                    UiMode::Clipboard => {
                        let clear_all = px >= cx as f64 + cw as f64 - 110.0 && px <= cx as f64 + cw as f64 - 20.0 && py >= cy as f64 + 16.0 && py <= cy as f64 + 56.0;
                        let list = py >= cy as f64 + 64.0 && py <= (cy + ch) as f64;
                        clear_all || list
                    }
                    UiMode::Keybinds => {
                        let search_w = 340.0;
                        let search_x = cx as f64 + cw as f64 - search_w - 28.0;
                        let search_y = cy as f64 + 20.0;
                        let clear_btn = px >= search_x + search_w - 32.0 && px <= search_x + search_w && py >= search_y && py <= search_y + 38.0;
                        let list_start_y = cy as f64 + 78.0;
                        let in_list = px >= cx as f64 + 20.0 && px <= (cx + cw) as f64 - 20.0 && py >= list_start_y && py <= (cy + ch - 20) as f64;
                        clear_btn || in_list
                    }
                    UiMode::Notification { .. } => {
                        let mut over_close = false;
                        let mut cur_y = 0.0;
                        for toast in state.notif_toasts.iter_mut() {
                            if toast.reveal > 0.01 {
                                let full_h = (toast.card_height + 12.0) * toast.reveal;
                                let card_x = toast.slide_x;
                                let card_y = cur_y;
                                let card_w = 360.0;
                                let card_h = toast.card_height;
                                let in_card = px >= card_x && px <= card_x + card_w && py >= card_y && py <= card_y + card_h;
                                toast.hovered = in_card;
                                if in_card {
                                    let close_x = card_x + card_w - 30.0;
                                    let close_y = card_y + 10.0;
                                    if px >= close_x && px <= close_x + 22.0 && py >= close_y && py <= close_y + 22.0 {
                                        over_close = true;
                                    }
                                }
                                cur_y += full_h;
                            }
                        }
                        over_close
                    }
                    _ => false,
                };

                if is_over_text {
                    state.set_cursor_shape(qh, CursorShape::Text);
                } else if is_over_clickable {
                    state.set_cursor_shape(qh, CursorShape::Pointer);
                } else {
                    state.set_cursor_shape(qh, CursorShape::Default);
                }

                match &state.mode {
                    UiMode::Launcher => {
                        let (cx, cy, cw, ch) = state.card_bounds;
                        let card_w = cw as f64;
                        let left_w = (card_w * 0.60).round();
                        let right_x = cx as f64 + left_w;
                        let pad = 20.0;
                        let content_x = right_x + pad;
                        let content_w = (card_w - left_w) - (pad * 2.0);
                        let search_h = 48.0;
                        let count_h = 22.0;
                        let list_start_y = cy as f64 + pad + search_h + 10.0 + count_h;
                        let item_h = 54.0;
                        let item_spacing = 4.0;
                        let viewport_h = (cy as f64 + ch as f64 - 20.0) - list_start_y;

                        if px >= content_x && px <= content_x + content_w && py >= list_start_y && py <= list_start_y + viewport_h {
                            let rel_y = py - list_start_y + state.launcher_scroll;
                            let index = (rel_y / (item_h + item_spacing)).floor() as usize;
                            let matches_len = search_apps(&state.apps, &state.search_query).len();
                            if index < matches_len && state.selected_index != index {
                                state.selected_index = index;
                                state.render_and_commit(qh);
                            }
                        }
                    }
                    UiMode::Spotlight => {
                        let (cx, cy, cw, _ch) = state.card_bounds;
                        let list_start_y = cy as f64 + 68.0;
                        let item_h = 44.0;
                        if px >= cx as f64 && px <= (cx + cw) as f64 && py >= list_start_y {
                            let rel_y = py - list_start_y;
                            let index = (rel_y / item_h).floor() as usize;
                            if index < 10 && state.selected_index != index {
                                state.selected_index = index;
                                state.render_and_commit(qh);
                            }
                        }
                    }
                    UiMode::Power => {
                        let (cx, cy, cw, ch) = state.card_bounds;
                        if px >= cx as f64 && px <= (cx + cw) as f64 && py >= cy as f64 && py <= (cy + ch) as f64 {
                            let pad = 32.0;
                            let spacing = 24.0;
                            let btn_w = (cw as f64 - pad * 2.0 - spacing * 3.0) / 4.0;
                            if px >= cx as f64 + pad && px <= (cx + cw) as f64 - pad {
                                let rel_x = px - (cx as f64 + pad);
                                let idx = (rel_x / (btn_w + spacing)).floor() as usize;
                                if idx < 4 && state.selected_index != idx {
                                    state.selected_index = idx;
                                    state.render_and_commit(qh);
                                }
                            }
                        }
                    }
                    UiMode::NotificationCenter if state.pointer_down => {
                        let (cx, cy, cw, ch) = state.card_bounds;
                        let inner_x = cx as f64 + 20.0;
                        let inner_w = cw as f64 - 40.0;
                        let footer_h = 100.0;
                        let footer_y = cy as f64 + ch as f64 - footer_h - 16.0;
                        let slider_x = inner_x + 140.0;
                        let slider_w = inner_w - 154.0;

                        let vol_y = footer_y + 8.0;
                        let vol_h = 32.0;
                        if py >= vol_y && py <= vol_y + vol_h {
                            let pct = (((px - slider_x) / slider_w).clamp(0.0, 1.0) * 100.0).round() as u32;
                            state.request_status(StatusRequest::SetVolume(pct));
                            state.status.volume = pct;
                            state.render_and_commit(qh);
                        }

                        let bright_y = footer_y + 50.0;
                        let bright_h = 32.0;
                        if py >= bright_y && py <= bright_y + bright_h {
                            let pct = (((px - slider_x) / slider_w).clamp(0.0, 1.0) * 100.0).round() as u32;
                            state.request_status(StatusRequest::SetBrightness(pct));
                            state.status.brightness = pct;
                            state.render_and_commit(qh);
                        }
                    }
                    _ => {}
                }
            }
            wl_pointer::Event::Button { button, state: btn_state, .. } => {
                match btn_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) if state.wallpaper.is_some() => {
                        state.pointer_down = true;
                        if button == 0x110 {
                            let (px, py) = (state.pointer_x as f64, state.pointer_y as f64);
                            let style = state.style.clone();
                            let eff = state.wallpaper.as_mut().unwrap().click(px, py, &style);
                            state.apply_wallpaper_effect(eff, qh);
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Pressed) if state.settings.is_some() => {
                        state.pointer_down = true;
                        if button == 0x110 {
                            let (px, py) = (state.pointer_x as f64, state.pointer_y as f64);
                            let (cx, cy, cw, ch) = state.card_bounds;
                            let inside = px >= cx as f64 && px < (cx + cw) as f64 && py >= cy as f64 && py < (cy + ch) as f64;
                            let sv = state.settings.as_mut().unwrap();
                            let eff = if inside { sv.press(px, py) } else { sv.request_close() };
                            state.apply_settings_effect(eff, qh);
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        state.pointer_down = true;
                        if button == 0x110 { // BTN_LEFT
                            let (cx, cy, cw, ch) = state.card_bounds;
                            let is_modal = matches!(
                                state.mode,
                                UiMode::Launcher | UiMode::Spotlight | UiMode::Clipboard | UiMode::Emoji |
                                UiMode::Settings | UiMode::NotificationCenter | UiMode::Power | UiMode::Keybinds
                            );

                            if is_modal && cw > 0 && ch > 0 {
                                if state.pointer_x < cx || state.pointer_x >= cx + cw ||
                                   state.pointer_y < cy || state.pointer_y >= cy + ch {
                                    eprintln!("[prism-ui] Click outside modal detected ({}, {}), dismissing.", state.pointer_x, state.pointer_y);
                                    state.exit_requested = true;
                                } else {
                                    state.handle_card_click(qh);
                                }
                            } else if matches!(state.mode, UiMode::Notification { .. }) {
                                state.handle_card_click(qh);
                            }
                        }
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        state.pointer_down = false;
                        if state.settings.as_mut().map(|sv| sv.release()).unwrap_or(false) {
                            state.render_and_commit(qh);
                        }
                    }
                    _ => {}
                }
            }
            wl_pointer::Event::Axis { value, .. } => {
                let delta = value * 2.5;
                if let Some(sv) = state.settings.as_mut() {
                    if sv.scroll(state.pointer_x as f64, state.pointer_y as f64, delta) {
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
                match &state.mode {
                    UiMode::Launcher => {
                        let item_h = 54.0;
                        let item_spacing = 4.0;
                        let matches_len = search_apps(&state.apps, &state.search_query).len();
                        let total_h = (matches_len as f64) * (item_h + item_spacing);
                        let (_cx, cy, _cw, ch) = state.card_bounds;
                        let list_start_y = cy as f64 + 100.0;
                        let viewport_h = (cy as f64 + ch as f64 - 20.0) - list_start_y;
                        let max_scroll = (total_h - viewport_h).max(0.0);
                        state.launcher_scroll = (state.launcher_scroll + delta).clamp(0.0, max_scroll);
                        state.render_and_commit(qh);
                    }
                    UiMode::Emoji => {
                        let matches_len = state.get_emoji_matches().len();
                        let total_rows = ((matches_len as f64) / 8.0).ceil();
                        let total_h = total_rows * 48.0;
                        let max_scroll = (total_h - 400.0).max(0.0);
                        state.emoji_scroll = (state.emoji_scroll + delta).clamp(0.0, max_scroll);
                        state.render_and_commit(qh);
                    }
                    UiMode::Clipboard => {
                        let total_h = (state.clipboard_items.len() as f64) * 62.0;
                        let max_scroll = (total_h - 480.0).max(0.0);
                        state.scroll_offset = (state.scroll_offset + delta).clamp(0.0, max_scroll);
                        state.render_and_commit(qh);
                    }
                    UiMode::Keybinds => {
                        let total_rows = ((state.get_filtered_keybinds().len() + 1) / 2) as f64;
                        let item_h = 42.0;
                        let spacing = 6.0;
                        let (_cx, cy, _cw, ch) = state.card_bounds;
                        let list_start_y = cy as f64 + 78.0;
                        let viewport_h = (cy as f64 + ch as f64 - 20.0) - list_start_y;
                        let max_scroll = (total_rows * (item_h + spacing) - viewport_h).max(0.0);
                        state.keybinds_scroll = (state.keybinds_scroll + delta).clamp(0.0, max_scroll);
                        state.render_and_commit(qh);
                    }
                    UiMode::NotificationCenter => {
                        match state.selected_tab {
                            0 => {
                                state.nc_notif_scroll = (state.nc_notif_scroll + delta).max(0.0);
                                state.render_and_commit(qh);
                            }
                            1 => {
                                state.nc_network_scroll = (state.nc_network_scroll + delta).max(0.0);
                                state.render_and_commit(qh);
                            }
                            2 => {
                                state.nc_bt_scroll = (state.nc_bt_scroll + delta).max(0.0);
                                state.render_and_commit(qh);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
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

    fn get_filtered_keybinds(&self) -> Vec<&KeybindEntry> {
        let q = self.search_query.trim().to_lowercase();
        if q.is_empty() {
            self.keybinds.iter().collect()
        } else {
            self.keybinds
                .iter()
                .filter(|b| {
                    b.key.to_lowercase().contains(&q)
                        || b.action.to_lowercase().contains(&q)
                        || b.category.to_lowercase().contains(&q)
                        || b.mods.to_lowercase().contains(&q)
                })
                .collect()
        }
    }

    fn get_emoji_matches(&self) -> Vec<EmojiItem> {
        if !self.search_query.trim().is_empty() {
            return search_emojis(&self.emojis, &self.search_query, None, 500);
        }

        if let Some(cat_tuple) = EMOJI_CATEGORIES.get(self.selected_tab) {
            if cat_tuple.2 == Some("recent") {
                let mut recents = Vec::new();
                for glyph in &self.emoji_recents {
                    if let Some(em) = self.emojis.iter().find(|e| &e.emoji == glyph) {
                        recents.push(em.clone());
                    }
                }
                return recents;
            } else if let Some(cat) = cat_tuple.2 {
                return search_emojis(&self.emojis, "", Some(cat), 500);
            }
        }
        search_emojis(&self.emojis, "", None, 500)
    }

    fn get_spotlight_sections(&self) -> Vec<SpotlightSection> {
        let mut sections = Vec::new();
        if self.search_query.trim().is_empty() {
            return sections;
        }

        let app_matches = search_apps(&self.apps, &self.search_query);
        let mut folders = Vec::new();
        let mut files = Vec::new();
        for file in &self.spotlight_files {
            if file.is_dir {
                folders.push(file.clone());
            } else {
                files.push(file.clone());
            }
        }

        let show_apps = self.spotlight_tab == 0 || self.spotlight_tab == 1;
        let show_folders = self.spotlight_tab == 0 || self.spotlight_tab == 2;
        let show_files = self.spotlight_tab == 0 || self.spotlight_tab == 3;

        let max_apps = if self.spotlight_tab == 1 { 8 } else { 3 };
        let max_folders = if self.spotlight_tab == 2 { 8 } else { 3 };
        let max_files = if self.spotlight_tab == 3 { 8 } else { 4 };

        if show_apps && !app_matches.is_empty() {
            let total_apps = app_matches.len();
            let items = app_matches.into_iter().take(max_apps).map(SpotlightResult::App).collect();
            sections.push(SpotlightSection {
                title: "APPLICATIONS",
                count: total_apps,
                items,
            });
        }

        if show_folders && !folders.is_empty() {
            let total_folders = folders.len();
            let items = folders.into_iter().take(max_folders).map(SpotlightResult::Folder).collect();
            sections.push(SpotlightSection {
                title: "FOLDERS",
                count: total_folders,
                items,
            });
        }

        if show_files && !files.is_empty() {
            let total_files = files.len();
            let items = files.into_iter().take(max_files).map(SpotlightResult::File).collect();
            sections.push(SpotlightSection {
                title: "FILES",
                count: total_files,
                items,
            });
        }

        sections
    }

    fn get_spotlight_results(&self) -> Vec<SpotlightResult> {
        let sections = self.get_spotlight_sections();
        let mut flat = Vec::new();
        for sec in sections {
            for it in sec.items {
                flat.push(it);
            }
        }
        flat
    }

    fn handle_card_click(&mut self, qh: &QueueHandle<Self>) {
        match &self.mode {
            UiMode::Launcher => {
                let (cx, cy, cw, ch) = self.card_bounds;
                let card_w = cw as f64;
                let left_w = (card_w * 0.60).round();
                let right_x = cx as f64 + left_w;
                let pad = 20.0;
                let search_h = 48.0;
                let count_h = 22.0;
                let list_start_y = cy as f64 + pad + search_h + 10.0 + count_h;
                let viewport_h = (cy as f64 + ch as f64 - 20.0) - list_start_y;
                let item_h = 54.0;
                let item_spacing = 4.0;

                let matches = search_apps(&self.apps, &self.search_query);
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;

                if px >= right_x && px <= (cx + cw) as f64 && py >= list_start_y && py <= list_start_y + viewport_h {
                    let rel_y = py - list_start_y + self.launcher_scroll;
                    let index = (rel_y / (item_h + item_spacing)).floor() as usize;
                    if let Some(app) = matches.get(index) {
                        eprintln!("[prism-ui] Clicked app '{}', launching...", app.name);
                        let _ = app.launch();
                        self.exit_requested = true;
                    }
                }
            }
            UiMode::Spotlight => {
                let (cx, cy, cw, _ch) = self.card_bounds;
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;
                let card_x = cx as f64;
                let card_y = cy as f64;

                // Category chips
                if !self.search_query.trim().is_empty() && py >= card_y + 58.0 && py <= card_y + 88.0 {
                    let mut chip_x = card_x + 18.0;
                    for i in 0..4 {
                        let chip_w = 95.0;
                        if px >= chip_x && px <= chip_x + chip_w {
                            self.spotlight_tab = i;
                            self.selected_index = 0;
                            self.render_and_commit(qh);
                            return;
                        }
                        chip_x += chip_w + 8.0;
                    }
                }

                // Section items
                let sections = self.get_spotlight_sections();
                let mut cur_y = card_y + 96.0;
                for sec in &sections {
                    cur_y += 26.0; // skip header
                    for item in &sec.items {
                        let item_h = 42.0;
                        if py >= cur_y && py <= cur_y + item_h && px >= card_x + 12.0 && px <= (card_x + cw as f64) - 12.0 {
                            match item {
                                SpotlightResult::App(app) => {
                                    eprintln!("[prism-ui] Spotlight launching app: {}", app.name);
                                    let _ = app.launch();
                                }
                                SpotlightResult::Folder(f) | SpotlightResult::File(f) => {
                                    eprintln!("[prism-ui] Spotlight opening: {:?}", f.path);
                                    let _ = Command::new("xdg-open").arg(&f.path).spawn().map(reap);
                                }
                            }
                            self.exit_requested = true;
                            return;
                        }
                        cur_y += item_h;
                    }
                }
            }
            UiMode::Clipboard => {
                let (cx, cy, cw, _ch) = self.card_bounds;
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;

                // Clear button
                let clear_x = cx as f64 + cw as f64 - 110.0;
                let clear_y = cy as f64 + 20.0;
                if px >= clear_x && px <= clear_x + 90.0 && py >= clear_y && py <= clear_y + 32.0 {
                    let mut store = ClipboardStore::load();
                    store.clear();
                    self.clipboard_items.clear();
                    self.render_and_commit(qh);
                    return;
                }

                // List items
                let list_start_y = cy as f64 + 115.0;
                let item_h = 58.0;
                if py >= list_start_y {
                    let rel_y = (py - list_start_y) + self.scroll_offset;
                    let idx = (rel_y / (item_h + 4.0)).floor() as usize;
                    let filtered: Vec<&ClipboardItem> = self.clipboard_items
                        .iter()
                        .filter(|it| self.search_query.is_empty() || it.content.to_lowercase().contains(&self.search_query.to_lowercase()))
                        .collect();

                    if let Some(item) = filtered.get(idx) {
                        eprintln!("[prism-ui] Copying clipboard item to system clipboard...");
                        prism_common::clipboard::touch_self_copy_lock();

                        if item.is_image {
                            if let Some(ref path) = item.image_path {
                                let local = path.replace("file://", "");
                                if let Ok(f) = std::fs::File::open(&local) {
                                    let _ = Command::new("wl-copy")
                                        .arg("-t")
                                        .arg("image/png")
                                        .stdin(f)
                                        .status();
                                }
                            }
                        } else {
                            if let Ok(mut child) = Command::new("wl-copy")
                                .stdin(std::process::Stdio::piped())
                                .spawn()
                            {
                                if let Some(mut stdin) = child.stdin.take() {
                                    use std::io::Write;
                                    let _ = stdin.write_all(item.content.as_bytes());
                                }
                                let _ = child.wait();
                            }
                        }

                        let _ = Command::new("notify-send")
                            .args(["-a", "Prism", "Clipboard", "Copied item to clipboard", "-i", "edit-copy"])
                            .spawn().map(reap);

                        self.exit_requested = true;
                    }
                }
            }
            UiMode::Emoji => {
                let (cx, cy, _cw, ch) = self.card_bounds;
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;
                let card_x = cx as f64;
                let card_y = cy as f64;
                let rail_w = 140.0;

                // Left category rail
                if px >= card_x + 10.0 && px <= card_x + rail_w + 14.0 && py >= card_y + 20.0 {
                    let tab_idx = ((py - (card_y + 20.0)) / 42.0).floor() as usize;
                    if tab_idx < EMOJI_CATEGORIES.len() {
                        self.selected_tab = tab_idx;
                        self.emoji_scroll = 0.0;
                        self.selected_index = 0;
                        self.render_and_commit(qh);
                        return;
                    }
                }

                // Grid click
                let right_x = card_x + rail_w + 30.0;
                let grid_y = card_y + 78.0;
                let cell_w = 54.0;
                let cell_h = 48.0;
                let viewport_h = (card_y + ch as f64 - 60.0) - grid_y;

                if px >= right_x && px <= right_x + 8.0 * cell_w && py >= grid_y && py <= grid_y + viewport_h {
                    let col = ((px - right_x) / cell_w).floor() as usize;
                    let row = ((py - grid_y + self.emoji_scroll) / cell_h).floor() as usize;
                    if col < 8 {
                        let idx = row * 8 + col;
                        let matches = self.get_emoji_matches();
                        if let Some(em) = matches.get(idx) {
                            eprintln!("[prism-ui] Selected emoji '{}' ({})", em.emoji, em.name);
                            // Prepend to recents & persist
                            let mut recents = self.emoji_recents.clone();
                            recents.retain(|g| g != &em.emoji);
                            recents.insert(0, em.emoji.clone());
                            recents.truncate(40);
                            self.emoji_recents = recents;
                            save_emoji_recents(&self.emoji_recents);

                            // Copy to clipboard
                            let _ = Command::new("wl-copy").arg(&em.emoji).spawn().map(reap);

                            // Auto-type directly if wtype is available
                            let _ = Command::new("wtype").arg(&em.emoji).spawn().map(reap);

                            self.exit_requested = true;
                        }
                    }
                }
            }
            UiMode::Keybinds => {
                let (cx, cy, cw, _ch) = self.card_bounds;
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;
                let card_x = cx as f64;
                let card_y = cy as f64;
                let card_w = cw as f64;

                let search_w = 340.0;
                let search_h = 38.0;
                let search_x = card_x + card_w - search_w - 28.0;
                let search_y = card_y + 20.0;

                // Clear button click
                if !self.search_query.is_empty() && px >= search_x + search_w - 36.0 && px <= search_x + search_w && py >= search_y && py <= search_y + search_h {
                    self.search_query.clear();
                    self.keybinds_scroll = 0.0;
                    self.render_and_commit(qh);
                }
            }
            UiMode::Power => {
                let (cx, cy, cw, ch) = self.card_bounds;
                let pad = 32.0;
                let spacing = 24.0;
                let btn_w = (cw as f64 - pad * 2.0 - spacing * 3.0) / 4.0;
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;

                if py >= cy as f64 + pad && py <= cy as f64 + ch as f64 - pad {
                    for i in 0..4 {
                        let bx = cx as f64 + pad + (i as f64) * (btn_w + spacing);
                        if px >= bx && px <= bx + btn_w {
                            let action = match i {
                                0 => prism_common::PowerAction::Lock,
                                1 => prism_common::PowerAction::Logout,
                                2 => prism_common::PowerAction::Reboot,
                                _ => prism_common::PowerAction::Shutdown,
                            };
                            let _ = prism_common::power::execute_power(action);
                            self.exit_requested = true;
                            return;
                        }
                    }
                }
            }
            UiMode::NotificationCenter => {
                let (cx, cy, cw, ch) = self.card_bounds;
                let inner_x = cx as f64 + 20.0;
                let inner_w = cw as f64 - 40.0;
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;

                // 1. Settings gear button
                let gear_x = inner_x + inner_w - 32.0;
                let gear_y = cy as f64 + 16.0;
                if px >= gear_x && px <= gear_x + 32.0 && py >= gear_y && py <= gear_y + 32.0 {
                    eprintln!("[prism-ui] Settings gear clicked, launching prism-ui settings...");
                    let _ = Command::new("prism-ctl").arg("settings").spawn().map(reap);
                    self.exit_requested = true;
                    return;
                }

                // 2. Quick Tiles row: 4 morphing tiles
                let tile_y = cy as f64 + 60.0;
                let tile_h = 44.0;
                let tile_gap = 8.0;

                let mut cur_tx = inner_x;
                for i in 0..4 {
                    let tw = self.nc_tile_widths[i];
                    if px >= cur_tx && px <= cur_tx + tw && py >= tile_y && py <= tile_y + tile_h {
                        match i {
                            0 => {
                                self.selected_tab = 0;
                                self.render_and_commit(qh);
                            }
                            1 => {
                                self.selected_tab = 1;
                                self.request_status(StatusRequest::RescanWifi);
                                self.render_and_commit(qh);
                            }
                            2 => {
                                self.selected_tab = 2;
                                self.request_status(StatusRequest::RescanBluetooth);
                                self.render_and_commit(qh);
                            }
                            3 => {
                                self.caffeinate_active = !self.caffeinate_active;
                                self.render_and_commit(qh);
                            }
                            _ => {}
                        }
                        return;
                    }
                    cur_tx += tw + tile_gap;
                }

                // 3. Audio Sink Menu Popover clicks (if open)
                let footer_h = 100.0;
                let footer_y = cy as f64 + ch as f64 - footer_h - 16.0;
                if self.nc_sink_menu_open {
                    let sink_row_h = 32.0;
                    let sink_menu_h = ((self.nc_audio_sinks.len() as f64) * sink_row_h + 16.0).min(180.0);
                    let sink_menu_y = footer_y - sink_menu_h - 8.0;
                    if px >= inner_x && px <= inner_x + inner_w && py >= sink_menu_y && py <= sink_menu_y + sink_menu_h {
                        let sink_idx = ((py - (sink_menu_y + 8.0)) / sink_row_h).floor() as usize;
                        if let Some(sink) = self.nc_audio_sinks.get(sink_idx) {
                            self.request_status(StatusRequest::SetDefaultSink(sink.id.clone()));
                            for (idx, s) in self.nc_audio_sinks.iter_mut().enumerate() {
                                s.is_default = idx == sink_idx;
                            }
                            self.nc_sink_menu_open = false;
                            self.render_and_commit(qh);
                            return;
                        }
                    }
                }

                // 4. Footer Volume & Brightness controls
                let slider_x = inner_x + 46.0;
                let slider_w = inner_w - 92.0;

                // Chevron button for audio sink menu
                let chev_x = inner_x + inner_w - 34.0;
                let chev_y = footer_y + 12.0;
                if px >= chev_x && px <= chev_x + 32.0 && py >= chev_y && py <= chev_y + 32.0 {
                    self.nc_sink_menu_open = !self.nc_sink_menu_open;
                    if self.nc_sink_menu_open {
                        self.request_status(StatusRequest::RefreshSinks);
                    }
                    self.render_and_commit(qh);
                    return;
                }

                // Volume slider
                let vol_y = footer_y + 12.0;
                let vol_h = 32.0;
                if py >= vol_y && py <= vol_y + vol_h && px >= slider_x - 10.0 && px <= slider_x + slider_w + 10.0 {
                    let pct = (((px - slider_x) / slider_w).clamp(0.0, 1.0) * 100.0).round() as u32;
                    self.request_status(StatusRequest::SetVolume(pct));
                    self.status.volume = pct;
                    self.render_and_commit(qh);
                    return;
                }

                // Brightness slider
                let bright_y = footer_y + 54.0;
                let bright_h = 32.0;
                if py >= bright_y && py <= bright_y + bright_h && px >= slider_x - 10.0 && px <= slider_x + slider_w + 10.0 {
                    let pct = (((px - slider_x) / slider_w).clamp(0.0, 1.0) * 100.0).round() as u32;
                    self.request_status(StatusRequest::SetBrightness(pct));
                    self.status.brightness = pct;
                    self.render_and_commit(qh);
                    return;
                }

                // 5. Tab Page Content Clicks
                let content_top_y = tile_y + tile_h + 16.0;
                let content_bot_y = footer_y - 10.0;

                match self.selected_tab {
                    0 => {
                        // Silent button (left of clear all)
                        let clear_btn_w = 64.0;
                        let clear_btn_x = inner_x + inner_w - clear_btn_w;
                        let silent_btn_w = 64.0;
                        let silent_btn_x = clear_btn_x - silent_btn_w - 8.0;

                        if px >= silent_btn_x && px <= silent_btn_x + silent_btn_w && py >= content_top_y - 4.0 && py <= content_top_y + 24.0 {
                            self.nc_dnd_silent = !self.nc_dnd_silent;
                            self.render_and_commit(qh);
                            return;
                        }

                        // Clear all button
                        if px >= clear_btn_x && px <= clear_btn_x + clear_btn_w && py >= content_top_y - 4.0 && py <= content_top_y + 24.0 {
                            // prismd owns the store; update the cached copy so the UI reflects it at once.
                            self.status.notifications.notifications.clear();
                            let _ = Command::new("prism-ctl").args(["notif", "clear"]).spawn().map(reap);
                            self.request_status(StatusRequest::RefreshAll);
                            self.render_and_commit(qh);
                            return;
                        }

                        // Notification List Clicks
                        let notifs = self.status.notifications.clone();
                        let list_start_y = content_top_y + 32.0;
                        if py >= list_start_y && py <= content_bot_y && px >= inner_x && px <= inner_x + inner_w {
                            let mut groups: Vec<(String, Vec<prism_common::notifications::Notification>)> = Vec::new();
                            for n in &notifs.notifications {
                                if let Some((_, list)) = groups.iter_mut().find(|(k, _)| k == &n.app_name) {
                                    list.push(n.clone());
                                } else {
                                    groups.push((n.app_name.clone(), vec![n.clone()]));
                                }
                            }

                            let mut y_cursor = list_start_y - self.nc_notif_scroll;
                            for (app_name, items) in groups {
                                let is_expanded = self.nc_expanded_group.as_deref() == Some(&app_name);
                                let header_h = 56.0;

                                // Check group header click
                                if py >= y_cursor && py <= y_cursor + header_h {
                                    if is_expanded {
                                        self.nc_expanded_group = None;
                                    } else {
                                        self.nc_expanded_group = Some(app_name);
                                    }
                                    self.render_and_commit(qh);
                                    return;
                                }

                                y_cursor += header_h;

                                if is_expanded {
                                    for item in items {
                                        let item_h = 48.0;
                                        // Check dismiss "✕" click
                                        let dismiss_x = inner_x + inner_w - 36.0;
                                        if px >= dismiss_x && px <= dismiss_x + 28.0 && py >= y_cursor && py <= y_cursor + item_h {
                                            self.status.notifications.notifications.retain(|n| n.id != item.id);
                                            let _ = Command::new("prism-ctl").args(["notif", "dismiss", &item.id.to_string()]).spawn().map(reap);
                                            self.render_and_commit(qh);
                                            return;
                                        }
                                        y_cursor += item_h + 4.0;
                                    }
                                }

                                y_cursor += 8.0;
                            }
                        }
                    }
                    1 => {
                        // Wi-Fi Power switch
                        let row1_y = content_top_y + 30.0;
                        let toggle_x = inner_x + inner_w - 54.0;
                        if px >= toggle_x - 10.0 && px <= toggle_x + 50.0 && py >= row1_y && py <= row1_y + 30.0 {
                            self.request_status(StatusRequest::ToggleWifi);
                            self.render_and_commit(qh);
                            return;
                        }

                        // Rescan button
                        let rescan_x = inner_x + inner_w - 60.0;
                        let net_hdr_y = content_top_y + 110.0;
                        if px >= rescan_x && px <= rescan_x + 60.0 && py >= net_hdr_y && py <= net_hdr_y + 24.0 {
                            let _ = Command::new("nmcli").args(["dev", "wifi", "rescan"]).spawn().map(reap);
                            self.request_status(StatusRequest::RescanWifi);
                            self.render_and_commit(qh);
                            return;
                        }

                        // AP List clicks
                        let list_start_y = net_hdr_y + 30.0;
                        if py >= list_start_y && py <= content_bot_y && px >= inner_x && px <= inner_x + inner_w {
                            let mut y_cursor = list_start_y - self.nc_network_scroll;
                            for net in &self.nc_wifi_networks {
                                let is_exp = self.nc_wifi_expanded_ssid.as_deref() == Some(&net.ssid);
                                let row_h = 42.0;

                                if is_exp {
                                    let pass_h = 44.0;
                                    // Check Connect button click in password row
                                    let conn_btn_x = inner_x + inner_w - 90.0;
                                    let conn_btn_y = y_cursor + row_h + 6.0;
                                    if px >= conn_btn_x && px <= conn_btn_x + 70.0 && py >= conn_btn_y && py <= conn_btn_y + 30.0 {
                                        let ssid = net.ssid.clone();
                                        let pass = self.nc_wifi_password_input.clone();
                                        self.nc_wifi_expanded_ssid = None;
                                        self.nc_wifi_password_input.clear();
                                        self.request_status(StatusRequest::ConnectWifi { ssid, password: Some(pass) });
                                        self.render_and_commit(qh);
                                        return;
                                    }

                                    if py >= y_cursor && py <= y_cursor + row_h {
                                        // Collapse password
                                        self.nc_wifi_expanded_ssid = None;
                                        self.render_and_commit(qh);
                                        return;
                                    }

                                    y_cursor += row_h + pass_h + 8.0;
                                } else {
                                    if py >= y_cursor && py <= y_cursor + row_h {
                                        let is_secured = !net.security.is_empty() && net.security != "--";
                                        if is_secured {
                                            self.nc_wifi_expanded_ssid = Some(net.ssid.clone());
                                            self.nc_wifi_password_input.clear();
                                        } else {
                                            self.request_status(StatusRequest::ConnectWifi { ssid: net.ssid.clone(), password: None });
                                        }
                                        self.render_and_commit(qh);
                                        return;
                                    }
                                    y_cursor += row_h + 4.0;
                                }
                            }
                        }
                    }
                    2 => {
                        // Bluetooth Power switch
                        let row1_y = content_top_y + 30.0;
                        let toggle_x = inner_x + inner_w - 54.0;
                        if px >= toggle_x - 10.0 && px <= toggle_x + 50.0 && py >= row1_y && py <= row1_y + 30.0 {
                            self.request_status(StatusRequest::ToggleBluetooth);
                            self.render_and_commit(qh);
                            return;
                        }

                        // Scan button
                        let scan_x = inner_x + inner_w - 50.0;
                        let bt_hdr_y = content_top_y + 80.0;
                        if px >= scan_x && px <= scan_x + 50.0 && py >= bt_hdr_y && py <= bt_hdr_y + 24.0 {
                            let _ = Command::new("bluetoothctl").args(["--timeout", "10", "scan", "on"]).spawn().map(reap);
                            self.request_status(StatusRequest::RescanBluetooth);
                            self.render_and_commit(qh);
                            return;
                        }

                        // Bluetooth device action buttons
                        let list_start_y = bt_hdr_y + 30.0;
                        if py >= list_start_y && py <= content_bot_y && px >= inner_x && px <= inner_x + inner_w {
                            let row_h = 44.0;
                            let spacing = 6.0;
                            let rel_y = py - list_start_y + self.nc_bt_scroll;
                            let dev_idx = (rel_y / (row_h + spacing)).floor() as usize;

                            if let Some(dev) = self.nc_bt_devices.get(dev_idx) {
                                let btn_x = inner_x + inner_w - 88.0;
                                if px >= btn_x && px <= btn_x + 78.0 {
                                    let req = if dev.connected {
                                        StatusRequest::DisconnectBluetooth(dev.mac.clone())
                                    } else {
                                        StatusRequest::ConnectBluetooth(dev.mac.clone())
                                    };
                                    self.request_status(req);
                                    self.render_and_commit(qh);
                                    return;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            UiMode::Notification { .. } => {
                let px = self.pointer_x as f64;
                let py = self.pointer_y as f64;
                let mut cur_y = 0.0;
                for toast in self.notif_toasts.iter_mut() {
                    if toast.reveal > 0.01 && toast.phase != ToastPhase::Leaving {
                        let full_h = (toast.card_height + 12.0) * toast.reveal;
                        let card_x = toast.slide_x;
                        let card_y = cur_y;
                        let card_w = 360.0;
                        let card_h = toast.card_height;
                        if px >= card_x && px <= card_x + card_w && py >= card_y && py <= card_y + card_h {
                            eprintln!("[prism-ui] Toast #{} clicked, starting leave animation", toast.id);
                            toast.phase = ToastPhase::Leaving;
                            self.render_and_commit(qh);
                            return;
                        }
                        cur_y += full_h;
                    }
                }
            }
            _ => {}
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
        if let wl_keyboard::Event::Key { key, state: key_state, .. } = event {
            if let WEnum::Value(wl_keyboard::KeyState::Pressed) = key_state {
                // Translate through the active keymap. `key` becomes the equivalent US-layout
                // evdev code that the handlers below match on; `typed` is the character the
                // layout actually produces (respecting Shift, AltGr, non-US layouts, wtype).
                let raw_key = key;
                let (key, typed) = translate_key(state.xkb_state.as_ref(), key);
                if state.wallpaper.is_some() {
                    let style = state.style.clone();
                    let eff = state.wallpaper.as_mut().unwrap().key(key, &style);
                    state.apply_wallpaper_effect(eff, qh);
                    return;
                }
                if state.settings.is_some() {
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
                    let sv = state.settings.as_mut().unwrap();
                    let eff = sv.key(special, typed, shift, ctrl);
                    state.apply_settings_effect(eff, qh);
                    return;
                }
                match key {
                    1 => {
                        // KEY_ESC: dismiss window
                        if state.mode == UiMode::NotificationCenter {
                            if state.nc_sink_menu_open {
                                state.nc_sink_menu_open = false;
                                state.render_and_commit(qh);
                                return;
                            } else if state.nc_wifi_expanded_ssid.is_some() {
                                state.nc_wifi_expanded_ssid = None;
                                state.nc_wifi_password_input.clear();
                                state.render_and_commit(qh);
                                return;
                            }
                        }
                        eprintln!("[prism-ui] ESC pressed, dismissing window.");
                        state.exit_requested = true;
                    }
                    // Power Menu Hotkeys
                    2 | 38 if state.mode == UiMode::Power => { // 1 or L
                        let _ = prism_common::power::execute_power(prism_common::PowerAction::Lock);
                        state.exit_requested = true;
                    }
                    3 | 18 if state.mode == UiMode::Power => { // 2 or E
                        let _ = prism_common::power::execute_power(prism_common::PowerAction::Logout);
                        state.exit_requested = true;
                    }
                    4 | 19 if state.mode == UiMode::Power => { // 3 or R
                        let _ = prism_common::power::execute_power(prism_common::PowerAction::Reboot);
                        state.exit_requested = true;
                    }
                    5 | 25 | 31 if state.mode == UiMode::Power => { // 4 or P or S
                        let _ = prism_common::power::execute_power(prism_common::PowerAction::Shutdown);
                        state.exit_requested = true;
                    }
                    15 => {
                        // KEY_TAB
                        match state.mode {
                            UiMode::Spotlight => {
                                state.spotlight_tab = (state.spotlight_tab + 1) % 4;
                                state.selected_index = 0;
                                state.render_and_commit(qh);
                            }
                            UiMode::Emoji => {
                                state.selected_tab = (state.selected_tab + 1) % EMOJI_CATEGORIES.len();
                                state.emoji_scroll = 0.0;
                                state.selected_index = 0;
                                state.render_and_commit(qh);
                            }
                            _ => {}
                        }
                    }
                    105 => {
                        // KEY_LEFT
                        if let UiMode::Power = state.mode {
                            state.selected_index = state.selected_index.saturating_sub(1);
                            state.render_and_commit(qh);
                        } else if let UiMode::Emoji = state.mode {
                            if state.selected_index > 0 {
                                state.selected_index -= 1;
                                let row_top = (state.selected_index / 8) as f64 * 48.0;
                                if row_top < state.emoji_scroll {
                                    state.emoji_scroll = row_top;
                                }
                                state.render_and_commit(qh);
                            }
                        }
                    }
                    106 => {
                        // KEY_RIGHT
                        if let UiMode::Power = state.mode {
                            state.selected_index = (state.selected_index + 1).min(3);
                            state.render_and_commit(qh);
                        } else if let UiMode::Emoji = state.mode {
                            let matches_len = state.get_emoji_matches().len();
                            if matches_len > 0 && state.selected_index + 1 < matches_len {
                                state.selected_index += 1;
                                let row_bottom = (state.selected_index / 8 + 1) as f64 * 48.0;
                                let viewport_h = 400.0;
                                if row_bottom > state.emoji_scroll + viewport_h {
                                    state.emoji_scroll = row_bottom - viewport_h;
                                }
                                state.render_and_commit(qh);
                            }
                        }
                    }
                    103 => {
                        // KEY_UP
                        match state.mode {
                            UiMode::Launcher => {
                                if state.selected_index > 0 {
                                    state.selected_index -= 1;
                                    let item_top = (state.selected_index as f64) * 58.0;
                                    if item_top < state.launcher_scroll {
                                        state.launcher_scroll = item_top;
                                    }
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Spotlight => {
                                state.selected_index = state.selected_index.saturating_sub(1);
                                state.render_and_commit(qh);
                            }
                            UiMode::Clipboard => {
                                if state.selected_index > 0 {
                                    state.selected_index -= 1;
                                    let item_top = (state.selected_index as f64) * 62.0;
                                    if item_top < state.scroll_offset {
                                        state.scroll_offset = item_top;
                                    }
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Emoji => {
                                let matches_len = state.get_emoji_matches().len();
                                if matches_len > 0 {
                                    state.selected_index = state.selected_index.saturating_sub(8);
                                    let row_top = (state.selected_index / 8) as f64 * 48.0;
                                    if row_top < state.emoji_scroll {
                                        state.emoji_scroll = row_top;
                                    }
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Keybinds => {
                                let total_rows = ((state.get_filtered_keybinds().len() + 1) / 2) as f64;
                                let item_h = 42.0;
                                let spacing = 6.0;
                                let (_cx, cy, _cw, ch) = state.card_bounds;
                                let list_start_y = cy as f64 + 78.0;
                                let viewport_h = (cy as f64 + ch as f64 - 20.0) - list_start_y;
                                let max_scroll = (total_rows * (item_h + spacing) - viewport_h).max(0.0);
                                state.keybinds_scroll = (state.keybinds_scroll - (item_h + spacing)).clamp(0.0, max_scroll);
                                state.render_and_commit(qh);
                            }
                            _ => {}
                        }
                    }
                    108 => {
                        // KEY_DOWN
                        match state.mode {
                            UiMode::Launcher => {
                                let matches_len = search_apps(&state.apps, &state.search_query).len();
                                if matches_len > 0 && state.selected_index + 1 < matches_len {
                                    state.selected_index += 1;
                                    let item_bottom = (state.selected_index as f64 + 1.0) * 58.0;
                                    let viewport_h = 450.0;
                                    if item_bottom > state.launcher_scroll + viewport_h {
                                        state.launcher_scroll = item_bottom - viewport_h;
                                    }
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Spotlight => {
                                let res_len = state.get_spotlight_results().len();
                                if res_len > 0 {
                                    state.selected_index = (state.selected_index + 1).min(res_len - 1);
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Clipboard => {
                                let len = state.clipboard_items.len();
                                if len > 0 && state.selected_index + 1 < len {
                                    state.selected_index += 1;
                                    let item_bottom = (state.selected_index as f64 + 1.0) * 62.0;
                                    let viewport_h = 480.0;
                                    if item_bottom > state.scroll_offset + viewport_h {
                                        state.scroll_offset = item_bottom - viewport_h;
                                    }
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Emoji => {
                                let matches_len = state.get_emoji_matches().len();
                                if matches_len > 0 {
                                    state.selected_index = (state.selected_index + 8).min(matches_len - 1);
                                    let row_bottom = (state.selected_index / 8 + 1) as f64 * 48.0;
                                    let viewport_h = 400.0;
                                    if row_bottom > state.emoji_scroll + viewport_h {
                                        state.emoji_scroll = row_bottom - viewport_h;
                                    }
                                    state.render_and_commit(qh);
                                }
                            }
                            UiMode::Keybinds => {
                                let total_rows = ((state.get_filtered_keybinds().len() + 1) / 2) as f64;
                                let item_h = 42.0;
                                let spacing = 6.0;
                                let (_cx, cy, _cw, ch) = state.card_bounds;
                                let list_start_y = cy as f64 + 78.0;
                                let viewport_h = (cy as f64 + ch as f64 - 20.0) - list_start_y;
                                let max_scroll = (total_rows * (item_h + spacing) - viewport_h).max(0.0);
                                state.keybinds_scroll = (state.keybinds_scroll + (item_h + spacing)).clamp(0.0, max_scroll);
                                state.render_and_commit(qh);
                            }
                            _ => {}
                        }
                    }
                    104 => {
                        // KEY_PAGEUP
                        if state.mode == UiMode::Keybinds {
                            let total_rows = ((state.get_filtered_keybinds().len() + 1) / 2) as f64;
                            let (_cx, cy, _cw, ch) = state.card_bounds;
                            let viewport_h = (cy as f64 + ch as f64 - 20.0) - (cy as f64 + 78.0);
                            let max_scroll = (total_rows * 48.0 - viewport_h).max(0.0);
                            state.keybinds_scroll = (state.keybinds_scroll - viewport_h).clamp(0.0, max_scroll);
                            state.render_and_commit(qh);
                        }
                    }
                    109 => {
                        // KEY_PAGEDOWN
                        if state.mode == UiMode::Keybinds {
                            let total_rows = ((state.get_filtered_keybinds().len() + 1) / 2) as f64;
                            let (_cx, cy, _cw, ch) = state.card_bounds;
                            let viewport_h = (cy as f64 + ch as f64 - 20.0) - (cy as f64 + 78.0);
                            let max_scroll = (total_rows * 48.0 - viewport_h).max(0.0);
                            state.keybinds_scroll = (state.keybinds_scroll + viewport_h).clamp(0.0, max_scroll);
                            state.render_and_commit(qh);
                        }
                    }
                    28 | 96 => {
                        // KEY_ENTER
                        match &state.mode {
                            UiMode::Launcher => {
                                let matches = search_apps(&state.apps, &state.search_query);
                                if let Some(app) = matches.get(state.selected_index).or_else(|| matches.first()) {
                                    eprintln!("[prism-ui] Launching '{}'...", app.name);
                                    let _ = app.launch();
                                }
                                state.exit_requested = true;
                            }
                            UiMode::Spotlight => {
                                let results = state.get_spotlight_results();
                                if let Some(item) = results.get(state.selected_index).or_else(|| results.first()) {
                                    match item {
                                        SpotlightResult::App(app) => {
                                            let _ = app.launch();
                                        }
                                        SpotlightResult::Folder(f) | SpotlightResult::File(f) => {
                                            let _ = Command::new("xdg-open").arg(&f.path).spawn().map(reap);
                                        }
                                    }
                                }
                                state.exit_requested = true;
                            }
                            UiMode::Emoji => {
                                let matches = state.get_emoji_matches();
                                if let Some(em) = matches.get(state.selected_index).or_else(|| matches.first()) {
                                    eprintln!("[prism-ui] Copying emoji '{}'...", em.emoji);
                                    let mut recents = state.emoji_recents.clone();
                                    recents.retain(|g| g != &em.emoji);
                                    recents.insert(0, em.emoji.clone());
                                    recents.truncate(40);
                                    state.emoji_recents = recents;
                                    save_emoji_recents(&state.emoji_recents);

                                    let _ = Command::new("wl-copy").arg(&em.emoji).spawn().map(reap);
                                    let _ = Command::new("wtype").arg(&em.emoji).spawn().map(reap);
                                    state.exit_requested = true;
                                }
                            }
                            UiMode::Clipboard => {
                                let filtered: Vec<&ClipboardItem> = state.clipboard_items
                                    .iter()
                                    .filter(|it| state.search_query.is_empty() || it.content.to_lowercase().contains(&state.search_query.to_lowercase()))
                                    .collect();
                                if let Some(item) = filtered.get(state.selected_index).or_else(|| filtered.first()) {
                                    prism_common::clipboard::touch_self_copy_lock();
                                    if item.is_image {
                                        if let Some(ref path) = item.image_path {
                                            let local = path.replace("file://", "");
                                            if let Ok(f) = std::fs::File::open(&local) {
                                                let _ = Command::new("wl-copy")
                                                    .arg("-t")
                                                    .arg("image/png")
                                                    .stdin(f)
                                                    .status();
                                            }
                                        }
                                    } else {
                                        if let Ok(mut child) = Command::new("wl-copy")
                                            .stdin(std::process::Stdio::piped())
                                            .spawn()
                                        {
                                            if let Some(mut stdin) = child.stdin.take() {
                                                use std::io::Write;
                                                let _ = stdin.write_all(item.content.as_bytes());
                                            }
                                            let _ = child.wait();
                                        }
                                    }
                                    let _ = Command::new("notify-send")
                                        .args(["-a", "Prism", "Clipboard", "Copied item to clipboard", "-i", "edit-copy"])
                                        .spawn().map(reap);
                                }
                                state.exit_requested = true;
                            }
                            UiMode::Power => {
                                let action = match state.selected_index {
                                    0 => prism_common::PowerAction::Lock,
                                    1 => prism_common::PowerAction::Logout,
                                    2 => prism_common::PowerAction::Reboot,
                                    _ => prism_common::PowerAction::Shutdown,
                                };
                                let _ = prism_common::power::execute_power(action);
                                state.exit_requested = true;
                            }
                            UiMode::NotificationCenter => {
                                if let Some(ssid) = state.nc_wifi_expanded_ssid.clone() {
                                    let pass = state.nc_wifi_password_input.clone();
                                    state.nc_wifi_expanded_ssid = None;
                                    state.nc_wifi_password_input.clear();
                                    state.request_status(StatusRequest::ConnectWifi { ssid, password: Some(pass) });
                                    state.render_and_commit(qh);
                                }
                            }
                            _ => {}
                        }
                    }
                    14 => {
                        // KEY_BACKSPACE: delete last char
                        if state.mode == UiMode::NotificationCenter && state.nc_wifi_expanded_ssid.is_some() {
                            state.nc_wifi_password_input.pop();
                            state.render_and_commit(qh);
                        } else {
                            match state.mode {
                                UiMode::Launcher | UiMode::Spotlight | UiMode::Clipboard | UiMode::Emoji | UiMode::Keybinds => {
                                    state.search_query.pop();
                                    state.selected_index = 0;
                                    if state.mode == UiMode::Spotlight {
                                        if let Some(tx) = &state.spotlight_tx {
                                            let _ = tx.send(state.search_query.clone());
                                        }
                                    } else if state.mode == UiMode::Launcher {
                                        state.launcher_scroll = 0.0;
                                    } else if state.mode == UiMode::Emoji {
                                        state.emoji_scroll = 0.0;
                                    } else if state.mode == UiMode::Keybinds {
                                        state.keybinds_scroll = 0.0;
                                    }
                                    state.render_and_commit(qh);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {
                        if state.mode == UiMode::NotificationCenter && state.nc_wifi_expanded_ssid.is_some() {
                            if let Some(ch) = typed {
                                state.nc_wifi_password_input.push(ch);
                                state.render_and_commit(qh);
                            }
                        } else {
                            match state.mode {
                                UiMode::Launcher | UiMode::Spotlight | UiMode::Clipboard | UiMode::Emoji | UiMode::Keybinds => {
                                    if let Some(ch) = typed {
                                        state.search_query.push(ch);
                                        state.selected_index = 0;
                                        if state.mode == UiMode::Spotlight {
                                            if let Some(tx) = &state.spotlight_tx {
                                                let _ = tx.send(state.search_query.clone());
                                            }
                                        } else if state.mode == UiMode::Launcher {
                                            state.launcher_scroll = 0.0;
                                        } else if state.mode == UiMode::Emoji {
                                            state.emoji_scroll = 0.0;
                                        } else if state.mode == UiMode::Keybinds {
                                            state.keybinds_scroll = 0.0;
                                        }
                                        state.render_and_commit(qh);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Convert a per-frame smoothing factor tuned at 60 Hz into one for an arbitrary frame time,
/// so exponential "lerp toward target" animations take the same wall-clock time at any refresh rate.
fn lerp_factor(k_at_60hz: f64, dt: f64) -> f64 {
    1.0 - (1.0 - k_at_60hz).powf(dt * 60.0)
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
        let fd = libc::memfd_create(b"prism-ui-shm\0".as_ptr() as *const libc::c_char, libc::MFD_CLOEXEC);
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

    /// Lower the Settings card and scrim below normal windows (and give up the keyboard)
    /// while a system dialog is open, or bring them back.
    fn apply_settings_layer(&mut self, qh: &QueueHandle<Self>) {
        let Some(lower) = self.settings.as_mut().and_then(|s| s.take_layer_request()) else { return };
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

    fn request_status(&self, req: StatusRequest) {
        if let Some(tx) = &self.status_tx {
            let _ = tx.send(req);
        }
    }

    /// Adopt a new snapshot from the status worker.
    fn apply_status(&mut self, snap: StatusSnapshot) {
        self.nc_wifi_networks = snap.wifi_networks.clone();
        self.nc_bt_devices = snap.bt_devices.clone();
        self.nc_audio_sinks = snap.audio_sinks.clone();
        self.status = snap;
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
        self.set_scrim_level(level, qh);
    }

    /// Set the dim to `level` (0..1) of the configured dim opacity by attaching a new 1x1
    /// buffer. The compositor just composites a solid rectangle; nothing is redrawn.
    fn set_scrim_level(&mut self, level: f64, qh: &QueueHandle<Self>) {
        let level = level.clamp(0.0, 1.0);
        if (level - self.scrim_shown).abs() < 0.004 {
            return;
        }
        let Some(surface) = self.scrim_surface.clone() else { return };
        if self.scrim_viewport.is_none() {
            return; // not configured yet
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
        }
    }

    /// Target height of the spotlight card for the current query/results.
    fn spotlight_target_height(&self) -> f64 {
        if self.search_query.trim().is_empty() {
            64.0
        } else {
            let flat_items = self.get_spotlight_results();
            if flat_items.is_empty() {
                150.0
            } else {
                let total_headers = self.get_spotlight_sections().len();
                (100.0 + (total_headers as f64 * 28.0) + (flat_items.len() as f64 * 42.0) + 16.0).min(560.0)
            }
        }
    }

    /// Screen-space rectangle the card surface should occupy right now. Mirrors the card
    /// geometry in `draw_frame`; the surface is exactly the card so compositor blur (which on
    /// MangoWM always covers the whole layer surface) stays behind the card only.
    fn desired_card_rect(&self) -> (i32, i32, i32, i32) {
        let sw = self.width as f64;
        let sh = self.height as f64;
        let centered = |w: f64, h: f64| {
            let w = w.min(sw - 40.0).max(1.0);
            let h = h.min(sh - 40.0).max(1.0);
            (((sw - w) / 2.0).round() as i32, ((sh - h) / 2.0).round() as i32, w as i32, h as i32)
        };
        match &self.mode {
            UiMode::Launcher => centered(960.0, 620.0),
            UiMode::Clipboard => centered(800.0, 600.0),
            UiMode::Emoji => centered(760.0, 560.0),
            UiMode::Settings => centered(settings::CARD_W, settings::CARD_H),
            UiMode::Keybinds => centered(1200.0, 700.0),
            UiMode::Power => {
                let w = (sw * 0.70).min(1400.0).max(700.0);
                let h = 360.0;
                (((sw - w) / 2.0).round() as i32, ((sh - h) / 2.0).round() as i32, w as i32, h as i32)
            }
            UiMode::Spotlight => {
                let w = 680.0_f64.min(sw - 40.0);
                // Grow at once, shrink only after the height animation has finished.
                let h = self.spotlight_height_anim.max(self.spotlight_target_height()).round();
                (((sw - w) / 2.0).round() as i32, 140, w as i32, h as i32)
            }
            UiMode::NotificationCenter => {
                let w = 400.0_f64.min(sw - 40.0);
                let h = (sh - 48.0).max(400.0);
                if self.transition.as_ref().map(|t| t.entering).unwrap_or(false) {
                    // While sliding in (OutBack overshoots ~13% of 400 px) the surface spans
                    // from the screen edge past the overshoot, then shrinks to the card.
                    (0, 24, (20.0 + w + 64.0) as i32, h as i32)
                } else {
                    (20, 24, w as i32, h as i32)
                }
            }
            UiMode::Wallpaper => {
                // Full-width strip (QML panelWidth = screen width), vertically centred.
                let h = wallpaper::PANEL_H.min(sh);
                (0, ((sh - h) / 2.0).round() as i32, sw as i32, h as i32)
            }
            _ => (0, 0, self.width as i32, self.height as i32),
        }
    }


    /// Apply messages from prismd to the toast stack (add / update / dismiss).
    fn drain_notification_messages(&mut self, qh: &QueueHandle<Self>) {
        let mut msgs = Vec::new();
        if let Some(rx) = &self.notif_socket_rx {
            msgs.extend(rx.try_iter());
        }
        if msgs.is_empty() {
            return;
        }
        for msg in msgs {
            match msg {
                prism_common::NotifOverlayMessage::Add { id, app_name, summary, body, timeout_ms } => {
                    if let Some(existing) = self.notif_toasts.iter_mut().find(|t| t.id == id && t.phase != ToastPhase::Finished) {
                        existing.summary = summary;
                        existing.body = body;
                        existing.created_at = std::time::Instant::now();
                        existing.timeout_ms = timeout_ms;
                        existing.phase = ToastPhase::Shown;
                    } else {
                        self.notif_toasts.push(PopupToast {
                            id,
                            app_name,
                            summary,
                            body,
                            created_at: std::time::Instant::now(),
                            timeout_ms,
                            slide_x: -420.0,
                            opacity: 0.0,
                            reveal: 0.0,
                            card_height: 84.0,
                            phase: ToastPhase::Entering,
                            hovered: false,
                        });
                        // Enforce cap: max 4 visible
                        let mut active = self.notif_toasts.iter().filter(|t| t.phase != ToastPhase::Leaving && t.phase != ToastPhase::Finished).count();
                        if active > 4 {
                            for t in self.notif_toasts.iter_mut() {
                                if t.phase != ToastPhase::Leaving && t.phase != ToastPhase::Finished {
                                    t.phase = ToastPhase::Leaving;
                                    active -= 1;
                                    if active <= 4 {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                prism_common::NotifOverlayMessage::Dismiss { id } => {
                    for t in self.notif_toasts.iter_mut() {
                        if id == 0 || t.id == id {
                            t.phase = ToastPhase::Leaving;
                        }
                    }
                }
            }
        }
        self.render_and_commit(qh);
    }

    /// Advance toast enter/leave animations by the real frame time. Called once per drawn frame.
    fn step_toasts(&mut self) {
        let dt = self.frame_dt;
        for toast in self.notif_toasts.iter_mut() {
            match toast.phase {
                ToastPhase::Entering => {
                    toast.slide_x += (10.0 - toast.slide_x) * lerp_factor(0.28, dt);
                    toast.opacity += (1.0 - toast.opacity) * lerp_factor(0.35, dt);
                    toast.reveal += (1.0 - toast.reveal) * lerp_factor(0.35, dt);
                    if (10.0 - toast.slide_x).abs() < 1.0 && (1.0 - toast.opacity).abs() < 0.03 {
                        toast.slide_x = 10.0;
                        toast.opacity = 1.0;
                        toast.reveal = 1.0;
                        toast.phase = ToastPhase::Shown;
                    }
                }
                ToastPhase::Shown => {
                    toast.slide_x = 10.0;
                    toast.opacity = 1.0;
                    toast.reveal = 1.0;
                }
                ToastPhase::Leaving => {
                    toast.slide_x += (-440.0 - toast.slide_x) * lerp_factor(0.28, dt);
                    toast.opacity += (0.0 - toast.opacity) * lerp_factor(0.28, dt);
                    if toast.opacity < 0.1 || toast.slide_x <= -380.0 {
                        toast.reveal += (0.0 - toast.reveal) * lerp_factor(0.25, dt);
                        if toast.reveal <= 0.05 {
                            toast.phase = ToastPhase::Finished;
                        }
                    }
                }
                ToastPhase::Finished => {}
            }
        }
        self.notif_toasts.retain(|t| t.phase != ToastPhase::Finished);
        if self.notif_toasts.is_empty() {
            self.exit_requested = true;
            return;
        }
        // Resize the surface when the stack height changes.
        let target_h = (self.notif_toasts.iter().map(|t| t.card_height + 12.0).sum::<f64>() as u32).max(1);
        if target_h != self.height {
            self.height = target_h;
            if let Some(ls) = &self.layer_surface {
                ls.set_size(self.width, self.height);
            }
        }
    }

    fn draw_frame(&mut self, qh: &QueueHandle<Self>) {
        let perf_t0 = std::time::Instant::now();
        self.draw_frame_inner(qh);
        if let Some(level) = self.scrim_pending.take() {
            self.set_scrim_level(level, qh);
        }
        if std::env::var_os("PRISM_PERF").is_some() {
            eprintln!("[perf] frame {:.2} ms", perf_t0.elapsed().as_secs_f64() * 1000.0);
        }
    }

    fn draw_frame_inner(&mut self, qh: &QueueHandle<Self>) {
        if let Some(rx) = &self.spotlight_rx {
            while let Ok(files) = rx.try_recv() {
                self.spotlight_files = files;
            }
        }

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

        if matches!(self.mode, UiMode::Notification { .. }) {
            self.step_toasts();
            if self.exit_requested {
                return;
            }
        }

        if self.card_surface {
            let want = self.desired_card_rect();
            if want != self.surf_rect {
                if self.pending_rect != Some(want) {
                    if let (Some(ls), Some(surf)) = (&self.layer_surface, &self.surface) {
                        ls.set_size(want.2.max(1) as u32, want.3.max(1) as u32);
                        ls.set_margin(want.1, 0, 0, want.0);
                        surf.commit();
                    }
                    self.pending_rect = Some(want);
                }
                // Draw once the compositor has configured the new size.
                self.needs_render = true;
                return;
            }
        }

        let (surface, shm) = match (&self.surface, &self.shm) {
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
            if let Some(slot) = create_buffer_slot(shm, qh, bw, bh, stride, size) {
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

            let (anim_offset, _anim_alpha, anim_done) = if let Some(anim) = &self.animation {
                anim.evaluate()
            } else {
                (0, 1.0, true)
            };

            // The buffer was cleared to transparent by memset above. The modal dim is a separate
            // scrim surface, so nothing full-screen is painted here.

            let (anim_tx, anim_ty, anim_scale, mut is_animating) = if let Some(anim) = &mut self.spring_anim {
                let still_running = anim.tick();
                let val = anim.value();
                // Only the OSD still uses a spring; card surfaces use the global transition.
                let res = (0.0_f64, val, 1.0_f64, still_running);
                if !still_running {
                    self.spring_anim = None;
                }
                res
            } else {
                (0.0, 0.0, 1.0, false)
            };

            cairo_backend::cairo_save(cr);
            if anim_tx != 0.0 || anim_ty != 0.0 {
                cairo_backend::cairo_translate(cr, anim_tx, anim_ty);
            }
            if (anim_scale - 1.0).abs() > 0.001 {
                let center_x = (width as f64) / 2.0;
                let center_y = (height as f64) / 2.0;
                cairo_backend::cairo_translate(cr, center_x, center_y);
                cairo_backend::cairo_scale(cr, anim_scale, anim_scale);
                cairo_backend::cairo_translate(cr, -center_x, -center_y);
            }

            if redraw_content {
            match &self.mode {
                // The desktop layer and its edit mode live in desktop/ (separate code path).
                UiMode::Desktop | UiMode::DesktopEdit => {}
                UiMode::Wallpaper => {
                    let sw = width as f64;
                    let py = ((height as f64 - wallpaper::PANEL_H) / 2.0).round();
                    self.card_bounds = (0, py as i32, sw as i32, wallpaper::PANEL_H as i32);
                    let (style, theme) = (self.style.clone(), self.theme.clone());
                    if let Some(v) = self.wallpaper.as_mut() {
                        v.draw(cr, 0.0, py, sw, &style, &theme);
                        if v.animating() {
                            is_animating = true;
                        }
                    }
                }
                UiMode::Settings => {
                    let (sw, sh) = (width as f64, height as f64);
                    let card = desktop::layout::Rect {
                        x: ((sw - settings::CARD_W) / 2.0).round(),
                        y: ((sh - settings::CARD_H) / 2.0).round(),
                        w: settings::CARD_W,
                        h: settings::CARD_H,
                    };
                    self.card_bounds = (card.x as i32, card.y as i32, card.w as i32, card.h as i32);
                    let (style, theme) = (self.style.clone(), self.theme.clone());
                    if let Some(sv) = self.settings.as_mut() {
                        sv.draw(cr, card, &style, &theme);
                    }
                }
                UiMode::Osd { kind, value } => {
                    let card_w = 320.0_f64.min(width as f64);
                    let card_h = 64.0_f64.min(height as f64);
                    let card_x = (width as f64 - card_w) / 2.0;
                    // Bottom-anchored sliding card matching Osd.qml
                    let card_y = (height as f64 - card_h - 60.0) + (anim_offset.max(0) as f64);

                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_bg);
                    let card_border = self.style.card_border;
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_border, 1.0);

                    let primary_color = self.style.accent;
                    let track_color = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.60);
                    let knob_color = cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 1.0);
                    let text_color = self.style.fg;

                    let icon = if kind.contains("bright") { "☀️" } else { "🔊" };
                    let label = format!("{} {} {:.0}%", icon, kind.to_uppercase(), value * 100.0);
                    cairo_backend::draw_pango_text(cr, card_x + 20.0, card_y + 14.0, &label, "Inter, Sans Bold 11", text_color, None);
                    cairo_backend::draw_slider(cr, card_x + 20.0, card_y + 34.0, card_w - 40.0, 16.0, *value as f64, primary_color, track_color, knob_color);
                }
                UiMode::Launcher => {
                    let card_w = 960.0_f64.min(width as f64 - 40.0);
                    let card_h = 620.0_f64.min(height as f64 - 40.0);
                    let card_x = ((width as f64 - card_w) / 2.0).round();
                    let card_y = ((height as f64 - card_h) / 2.0).round();
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, card_h as i32);

                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_bg);
                    let card_border = self.style.card_border;
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_border, 1.0);

                    // Left column: 60% of card width for wallpaper preview
                    let left_w = (card_w * 0.60).round();
                    cairo_backend::draw_wallpaper_panel(cr, card_x, card_y, left_w, card_h, radius);

                    // Right column: App search & results list
                    let right_x = card_x + left_w;
                    let right_w = card_w - left_w;
                    let pad = 20.0;
                    let content_x = right_x + pad;
                    let content_w = right_w - (pad * 2.0);

                    // Search field
                    let search_y = card_y + 20.0;
                    let search_h = 48.0;
                    let search_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.45);
                    let search_border = cairo_backend::hex_to_rgba(&self.theme.primary, 0.90);
                    let search_text = self.style.fg;
                    let placeholder_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70);

                    cairo_backend::draw_search_pill(
                        cr,
                        content_x,
                        search_y,
                        content_w,
                        search_h,
                        &self.search_query,
                        "Search apps…",
                        search_bg,
                        search_border,
                        search_text,
                        placeholder_color,
                    );

                    // Result count hint
                    let matches = search_apps(&self.apps, &self.search_query);
                    let count_text = format!("{} results", matches.len());
                    let count_y = search_y + search_h + 10.0;
                    cairo_backend::draw_pango_text(
                        cr,
                        content_x + 4.0,
                        count_y,
                        &count_text,
                        "Inter, Sans 11",
                        placeholder_color,
                        None,
                    );

                    // App list with viewport clipping and scrolling
                    let list_start_y = count_y + 22.0;
                    let item_h = 54.0;
                    let item_spacing = 4.0;
                    let viewport_h = (card_y + card_h - 20.0) - list_start_y;
                    let total_h = (matches.len() as f64) * (item_h + item_spacing);

                    cairo_backend::cairo_save(cr);
                    cairo_backend::cairo_rectangle(cr, content_x, list_start_y, content_w, viewport_h);
                    cairo_backend::cairo_clip(cr);

                    for (i, app) in matches.iter().enumerate() {
                        let item_y = list_start_y + (i as f64) * (item_h + item_spacing) - self.launcher_scroll;
                        if item_y + item_h < list_start_y || item_y > list_start_y + viewport_h {
                            continue;
                        }
                        let is_selected = i == self.selected_index;

                        if is_selected {
                            let pill_bg = cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 0.90);
                            cairo_backend::fill_rounded_rect(cr, content_x, item_y, content_w, item_h, 12.0, pill_bg);
                        }

                        let icon_size = 32;
                        let icon_x = content_x + 12.0;
                        let icon_y = item_y + (item_h - icon_size as f64) / 2.0;
                        let fallback_char = app.name.chars().next().unwrap_or('A').to_uppercase().to_string();
                        let icon_accent = cairo_backend::hex_to_rgba(&self.theme.primary, 0.90);
                        let icon_name = app.icon.as_deref().unwrap_or("");
                        cairo_backend::draw_app_icon(cr, icon_name, icon_x, icon_y, icon_size, &fallback_char, icon_accent);

                        let text_x = icon_x + icon_size as f64 + 12.0;
                        let text_w = (content_x + content_w - text_x - 8.0) as i32;

                        let (title_color, sub_color) = if is_selected {
                            (
                                cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 1.0),
                                cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 0.75),
                            )
                        } else {
                            (
                                cairo_backend::hex_to_rgba(&self.theme.on_surface, 0.95),
                                cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70),
                            )
                        };

                        cairo_backend::draw_pango_text(
                            cr,
                            text_x,
                            item_y + 8.0,
                            &app.name,
                            "Inter, Sans Bold 13",
                            title_color,
                            Some(text_w),
                        );

                        let desc = app.comment.as_deref().or_else(|| app.categories.first().map(|s| s.as_str())).unwrap_or("Application");
                        cairo_backend::draw_pango_text(
                            cr,
                            text_x,
                            item_y + 28.0,
                            desc,
                            "Inter, Sans 11",
                            sub_color,
                            Some(text_w),
                        );
                    }

                    cairo_backend::cairo_restore(cr);

                    // Draw scrollbar thumb if content overflows viewport
                    if total_h > viewport_h {
                        cairo_backend::draw_scrollbar_thumb(
                            cr,
                            content_x + content_w - 4.0,
                            list_start_y,
                            viewport_h,
                            total_h,
                            self.launcher_scroll,
                            4.0,
                            cairo_backend::hex_to_rgba(&self.theme.primary, 0.60),
                        );
                    }
                }
                UiMode::Spotlight => {
                    let card_w = 680.0_f64.min(width as f64 - 40.0);
                    let sections = self.get_spotlight_sections();
                    let flat_items = self.get_spotlight_results();

                    let target_h = if self.search_query.trim().is_empty() {
                        64.0
                    } else if flat_items.is_empty() {
                        150.0
                    } else {
                        let total_items = flat_items.len();
                        let total_headers = sections.len();
                        (100.0 + (total_headers as f64 * 28.0) + (total_items as f64 * 42.0) + 16.0).min(560.0)
                    };

                    if (self.spotlight_height_anim - target_h).abs() > 1.0 {
                        self.spotlight_height_anim += (target_h - self.spotlight_height_anim) * lerp_factor(0.40, self.frame_dt);
                        is_animating = true;
                    } else {
                        self.spotlight_height_anim = target_h;
                    }
                    let dynamic_h = self.spotlight_height_anim.round();

                    let card_x = ((width as f64 - card_w) / 2.0).round();
                    let card_y = 140.0;
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, dynamic_h as i32);

                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    let border_c = cairo_backend::hex_to_rgba(&self.theme.primary, 0.85);
                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, dynamic_h, radius, card_bg);
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, dynamic_h, radius, border_c, 1.5);

                    // Top Search input bar
                    let search_fg = self.style.fg;
                    let search_hint = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.65);
                    cairo_backend::draw_pango_text(cr, card_x + 20.0, card_y + 18.0, "🔍", "Inter, Sans 16", border_c, None);

                    let query_display = if self.search_query.is_empty() {
                        "Search applications, files and folders…"
                    } else {
                        &self.search_query
                    };
                    let query_color = if self.search_query.is_empty() { search_hint } else { search_fg };
                    cairo_backend::draw_pango_text(cr, card_x + 54.0, card_y + 18.0, query_display, "Inter, Sans Bold 15", query_color, Some((card_w - 90.0) as i32));

                    if !self.search_query.trim().is_empty() {
                        // Category filter chips
                        let all_apps_count = search_apps(&self.apps, &self.search_query).len();
                        let all_folders_count = self.spotlight_files.iter().filter(|f| f.is_dir).count();
                        let all_files_count = self.spotlight_files.iter().filter(|f| !f.is_dir).count();
                        let all_total = all_apps_count + all_folders_count + all_files_count;

                        let chip_data = [
                            ("All", all_total),
                            ("Apps", all_apps_count),
                            ("Folders", all_folders_count),
                            ("Files", all_files_count),
                        ];

                        let mut chip_x = card_x + 18.0;
                        let chip_y = card_y + 58.0;
                        let chip_h = 26.0;

                        for (i, (name, count)) in chip_data.iter().enumerate() {
                            let chip_w = 95.0;
                            let is_active = i == self.spotlight_tab;
                            let chip_bg = if is_active {
                                cairo_backend::hex_to_rgba(&self.theme.primary_container, 0.90)
                            } else {
                                cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.50)
                            };
                            cairo_backend::fill_rounded_rect(cr, chip_x, chip_y, chip_w, chip_h, 6.0, chip_bg);
                            if is_active {
                                cairo_backend::stroke_rounded_rect(cr, chip_x, chip_y, chip_w, chip_h, 6.0, border_c, 1.0);
                            }

                            let chip_fg = if is_active {
                                cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 1.0)
                            } else {
                                cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.85)
                            };
                            let label = format!("{} ({})", name, count);
                            cairo_backend::draw_pango_text(cr, chip_x + 8.0, chip_y + 5.0, &label, "Inter, Sans Bold 10", chip_fg, Some((chip_w - 12.0) as i32));

                            chip_x += chip_w + 8.0;
                        }

                        // Divider
                        cairo_backend::fill_rounded_rect(cr, card_x + 16.0, card_y + 90.0, card_w - 32.0, 1.0, 0.5, border_c);

                        let mut cur_y = card_y + 96.0;
                        let mut flat_idx = 0;

                        if flat_items.is_empty() {
                            cairo_backend::draw_pango_text(cr, card_x + 24.0, cur_y + 8.0, "No matching applications or files found", "Inter, Sans Italic 13", search_hint, None);
                        } else {
                            for sec in &sections {
                                // Section Ribbon
                                let sec_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.40);
                                cairo_backend::fill_rounded_rect(cr, card_x + 14.0, cur_y, card_w - 28.0, 22.0, 4.0, sec_bg);
                                let sec_title = format!("{} ({})", sec.title, sec.count);
                                cairo_backend::draw_pango_text(cr, card_x + 22.0, cur_y + 3.0, &sec_title, "Inter, Sans Bold 10", border_c, None);
                                cur_y += 26.0;

                                for item in &sec.items {
                                    let iy = cur_y;
                                    let item_h = 42.0;
                                    let is_selected = flat_idx == self.selected_index;

                                    if is_selected {
                                        let pill_bg = cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 0.90);
                                        cairo_backend::fill_rounded_rect(cr, card_x + 12.0, iy, card_w - 24.0, item_h - 4.0, 8.0, pill_bg);
                                    }

                                    match item {
                                        SpotlightResult::App(app) => {
                                            let icon_size = 24;
                                            let icon_name = app.icon.as_deref().unwrap_or("");
                                            let fallback = app.name.chars().next().unwrap_or('A').to_uppercase().to_string();
                                            cairo_backend::draw_app_icon(cr, icon_name, card_x + 20.0, iy + 6.0, icon_size, &fallback, border_c);

                                            let (fg, sub) = if is_selected {
                                                (cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 1.0), cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 0.70))
                                            } else {
                                                (search_fg, search_hint)
                                            };
                                            cairo_backend::draw_pango_text(cr, card_x + 54.0, iy + 8.0, &app.name, "Inter, Sans Bold 13", fg, Some((card_w - 200.0) as i32));
                                            cairo_backend::draw_pango_text(cr, card_x + card_w - 110.0, iy + 8.0, "[APPLICATION]", "Inter, Sans 10", sub, None);
                                        }
                                        SpotlightResult::Folder(f) => {
                                            let (fg, sub) = if is_selected {
                                                (cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 1.0), cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 0.70))
                                            } else {
                                                (search_fg, search_hint)
                                            };
                                            cairo_backend::draw_pango_text(cr, card_x + 20.0, iy + 8.0, "📁", "Inter, Sans 14", border_c, None);
                                            cairo_backend::draw_pango_text(cr, card_x + 54.0, iy + 8.0, &f.name, "Inter, Sans Bold 13", fg, Some((card_w - 180.0) as i32));
                                            cairo_backend::draw_pango_text(cr, card_x + card_w - 90.0, iy + 8.0, "[FOLDER]", "Inter, Sans 10", sub, None);
                                        }
                                        SpotlightResult::File(f) => {
                                            let (fg, sub) = if is_selected {
                                                (cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 1.0), cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 0.70))
                                            } else {
                                                (search_fg, search_hint)
                                            };
                                            cairo_backend::draw_pango_text(cr, card_x + 20.0, iy + 8.0, "📄", "Inter, Sans 14", border_c, None);
                                            cairo_backend::draw_pango_text(cr, card_x + 54.0, iy + 8.0, &f.name, "Inter, Sans Bold 13", fg, Some((card_w - 180.0) as i32));
                                            cairo_backend::draw_pango_text(cr, card_x + card_w - 70.0, iy + 8.0, "[FILE]", "Inter, Sans 10", sub, None);
                                        }
                                    }

                                    cur_y += item_h;
                                    flat_idx += 1;
                                }
                            }
                        }
                    }
                }
                UiMode::Clipboard => {
                    let card_w = 800.0_f64.min(width as f64 - 40.0);
                    let card_h = 600.0_f64.min(height as f64 - 40.0);
                    let card_x = ((width as f64 - card_w) / 2.0).round();
                    let card_y = ((height as f64 - card_h) / 2.0).round();
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, card_h as i32);

                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    let border_c = self.style.card_border;
                    let primary_c = self.style.accent;
                    let fg_color = self.style.fg;
                    let sub_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70);

                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_bg);
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, border_c, 1.0);

                    // Header
                    cairo_backend::draw_pango_text(cr, card_x + 24.0, card_y + 20.0, "Clipboard History", "Inter, Sans Bold 18", fg_color, None);

                    // Clear all button
                    let clear_x = card_x + card_w - 110.0;
                    let clear_y = card_y + 20.0;
                    cairo_backend::fill_rounded_rect(cr, clear_x, clear_y, 86.0, 28.0, 8.0, (primary_c.0, primary_c.1, primary_c.2, 0.20));
                    cairo_backend::draw_pango_text(cr, clear_x + 12.0, clear_y + 5.0, "Clear all", "Inter, Sans Bold 11", primary_c, None);

                    // Search field
                    let search_y = card_y + 60.0;
                    let search_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.45);
                    cairo_backend::draw_search_pill(
                        cr,
                        card_x + 24.0,
                        search_y,
                        card_w - 48.0,
                        44.0,
                        &self.search_query,
                        "Search clipboard entries…",
                        search_bg,
                        primary_c,
                        fg_color,
                        sub_color,
                    );

                    // Items list
                    let list_y = search_y + 56.0;
                    let item_h = 58.0;

                    let filtered: Vec<&ClipboardItem> = self.clipboard_items
                        .iter()
                        .filter(|it| self.search_query.is_empty() || it.content.to_lowercase().contains(&self.search_query.to_lowercase()))
                        .collect();

                    if filtered.is_empty() {
                        cairo_backend::draw_pango_text(cr, card_x + 24.0, list_y + 20.0, "No clipboard items found", "Inter, Sans Italic 13", sub_color, None);
                    } else {
                        let list_h = card_h - (list_y - card_y) - 20.0;
                        let max_scroll = ((filtered.len() as f64 * (item_h + 4.0)) - list_h).max(0.0);
                        self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll);

                        cairo_backend::cairo_save(cr);
                        cairo_backend::cairo_rectangle(cr, card_x + 10.0, list_y, card_w - 20.0, list_h);
                        cairo_backend::cairo_clip(cr);

                        for (i, it) in filtered.iter().enumerate() {
                            let iy = list_y - self.scroll_offset + (i as f64) * (item_h + 4.0);
                            if iy + item_h < list_y || iy > list_y + list_h {
                                continue;
                            }

                            let is_selected = i == self.selected_index;

                            if is_selected {
                                let pill_bg = cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 0.90);
                                cairo_backend::fill_rounded_rect(cr, card_x + 20.0, iy, card_w - 40.0, item_h, 12.0, pill_bg);
                            } else {
                                let row_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.25);
                                cairo_backend::fill_rounded_rect(cr, card_x + 20.0, iy, card_w - 40.0, item_h, 12.0, row_bg);
                            }

                            let (text_c, hint_c) = if is_selected {
                                (cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 1.0), cairo_backend::hex_to_rgba(&self.theme.inverse_primary, 0.70))
                            } else {
                                (fg_color, sub_color)
                            };

                            let idx_badge = format!("#{}", i + 1);
                            cairo_backend::draw_pango_text(cr, card_x + 36.0, iy + 10.0, &idx_badge, "Inter, Sans Bold 11", primary_c, None);

                            if it.is_image {
                                if let Some(ref path) = it.image_path {
                                    let local = path.replace("file://", "");
                                    cairo_backend::draw_image_clipped(cr, &local, card_x + 72.0, iy + 6.0, 46.0, 46.0, 6.0, true);
                                    cairo_backend::draw_pango_text(cr, card_x + 130.0, iy + 14.0, "PNG Image", "Inter, Sans Bold 13", text_c, None);
                                    let len_hint = "Image snapshot • Click to copy";
                                    cairo_backend::draw_pango_text(cr, card_x + 130.0, iy + 32.0, len_hint, "Inter, Sans 11", hint_c, None);
                                }
                            } else {
                                let preview = it.content.replace('\n', " ");
                                cairo_backend::draw_pango_text(cr, card_x + 72.0, iy + 10.0, &preview, "Inter, Sans Bold 13", text_c, Some((card_w - 180.0) as i32));

                                let len_hint = format!("{} chars • Click to copy", it.content.len());
                                cairo_backend::draw_pango_text(cr, card_x + 72.0, iy + 32.0, &len_hint, "Inter, Sans 11", hint_c, None);
                            }
                        }

                        cairo_backend::cairo_restore(cr);

                        // Render scrollbar thumb if content overflows
                        if max_scroll > 0.0 {
                            cairo_backend::draw_scrollbar_thumb(
                                cr,
                                card_x + card_w - 14.0,
                                list_y,
                                list_h,
                                filtered.len() as f64 * (item_h + 4.0),
                                self.scroll_offset,
                                4.0,
                                (primary_c.0, primary_c.1, primary_c.2, 0.40),
                            );
                        }
                    }
                }
                UiMode::Emoji => {
                    let card_w = 760.0_f64.min(width as f64 - 40.0);
                    let card_h = 560.0_f64.min(height as f64 - 40.0);
                    let card_x = ((width as f64 - card_w) / 2.0).round();
                    let card_y = ((height as f64 - card_h) / 2.0).round();
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, card_h as i32);

                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    let border_c = self.style.card_border;
                    let primary_c = self.style.accent;
                    let fg_color = self.style.fg;
                    let sub_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70);

                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_bg);
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, border_c, 1.0);

                    // Left vertical category rail (11 categories)
                    let rail_w = 148.0;
                    let rail_pad_y = 16.0;
                    for (i, (icon, name, _)) in EMOJI_CATEGORIES.iter().enumerate() {
                        let ty = card_y + rail_pad_y + (i as f64) * 44.0;
                        let is_sel = i == self.selected_tab;
                        if is_sel {
                            cairo_backend::fill_rounded_rect(cr, card_x + 12.0, ty, rail_w, 38.0, 10.0, (primary_c.0, primary_c.1, primary_c.2, 0.25));
                        }
                        let label = format!("{} {}", icon, name);
                        let fc = if is_sel { primary_c } else { fg_color };
                        cairo_backend::draw_pango_text(cr, card_x + 22.0, ty + 9.0, &label, "Inter, Sans Bold 11", fc, None);
                    }

                    // Right pane: Search pill
                    let right_x = card_x + rail_w + 24.0;
                    let right_w = card_w - rail_w - 40.0;
                    let search_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.45);
                    cairo_backend::draw_search_pill(
                        cr,
                        right_x,
                        card_y + 16.0,
                        right_w,
                        44.0,
                        &self.search_query,
                        "Search emoji glyphs…",
                        search_bg,
                        primary_c,
                        fg_color,
                        sub_color,
                    );

                    // Emoji Grid
                    let grid_y = card_y + 68.0;
                    let grid_h = card_h - 120.0;
                    let cell_w = (right_w - 12.0) / 8.0;
                    let cell_h = 48.0;
                    let matches = self.get_emoji_matches();

                    let total_rows = ((matches.len() as f64) / 8.0).ceil();
                    let total_h = total_rows * cell_h;
                    let max_scroll = (total_h - grid_h).max(0.0);
                    self.emoji_scroll = self.emoji_scroll.clamp(0.0, max_scroll);

                    if matches.is_empty() {
                        let hint = if self.selected_tab == 0 && self.search_query.trim().is_empty() {
                            "No recent emojis yet\nClick any emoji to copy, auto-type, and pin here"
                        } else {
                            "No emoji glyphs match your search query"
                        };
                        cairo_backend::draw_pango_text(cr, right_x + 20.0, grid_y + 40.0, hint, "Inter, Sans Italic 13", sub_color, None);
                    } else {
                        cairo_backend::cairo_save(cr);
                        cairo_backend::cairo_rectangle(cr, right_x, grid_y, right_w, grid_h);
                        cairo_backend::cairo_clip(cr);

                        for (i, em) in matches.iter().enumerate() {
                            let col = i % 8;
                            let row = i / 8;
                            let ex = right_x + (col as f64) * cell_w;
                            let ey = grid_y - self.emoji_scroll + (row as f64) * cell_h;

                            if ey + cell_h < grid_y || ey > grid_y + grid_h {
                                continue;
                            }

                            let is_sel = i == self.selected_index;
                            if is_sel {
                                cairo_backend::fill_rounded_rect(cr, ex + 2.0, ey + 2.0, cell_w - 4.0, cell_h - 4.0, 10.0, (primary_c.0, primary_c.1, primary_c.2, 0.30));
                                cairo_backend::stroke_rounded_rect(cr, ex + 2.0, ey + 2.0, cell_w - 4.0, cell_h - 4.0, 10.0, primary_c, 1.5);
                            }
                            cairo_backend::draw_pango_text(cr, ex + (cell_w / 2.0) - 12.0, ey + 8.0, &em.emoji, "Inter, Sans 20", fg_color, None);
                        }

                        cairo_backend::cairo_restore(cr);
                    }

                    // Scrollbar for emoji grid
                    if max_scroll > 0.0 {
                        cairo_backend::draw_scrollbar_thumb(
                            cr,
                            right_x + right_w - 6.0,
                            grid_y,
                            grid_h,
                            total_h,
                            self.emoji_scroll,
                            4.0,
                            (primary_c.0, primary_c.1, primary_c.2, 0.40),
                        );
                    }

                    // Bottom info strip: Active emoji name & description
                    let strip_y = card_y + card_h - 42.0;
                    let strip_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.30);
                    cairo_backend::fill_rounded_rect(cr, right_x, strip_y, right_w, 32.0, 8.0, strip_bg);

                    if let Some(active_em) = matches.get(self.selected_index) {
                        let info_text = format!("{}  {}  •  Enter to copy  •  Tab to switch category", active_em.emoji, active_em.name);
                        cairo_backend::draw_pango_text(cr, right_x + 12.0, strip_y + 8.0, &info_text, "Inter, Sans Bold 11", fg_color, Some((right_w - 24.0) as i32));
                    } else if matches.is_empty() {
                        cairo_backend::draw_pango_text(cr, right_x + 12.0, strip_y + 8.0, "No emoji glyphs found", "Inter, Sans Italic 11", sub_color, None);
                    }
                }
                UiMode::NotificationCenter => {
                    // Left-docked card matching NotificationCenter.qml
                    let card_w = 400.0_f64.min(width as f64 - 40.0);
                    let card_h = (height as f64 - 48.0).max(400.0);
                    let card_x = 20.0;
                    let card_y = 24.0;
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, card_h as i32);

                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_bg);
                    let card_border = self.style.card_border;
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_border, 1.0);

                    let inner_x = card_x + 20.0;
                    let inner_w = card_w - 40.0;

                    // ── Header Bar ──────────────────────────────────────────
                    let title_color = self.style.fg;
                    cairo_backend::draw_pango_text(cr, inner_x, card_y + 18.0, "Control Centre", "Inter, Sans Bold 18", title_color, None);

                    // Gear button
                    let gear_cx = inner_x + inner_w - 16.0;
                    let gear_cy = card_y + 30.0;
                    let gear_r = 16.0;
                    let gear_border = cairo_backend::hex_to_rgba(&self.theme.outline_variant, 0.60);
                    cairo_backend::stroke_circle(cr, gear_cx, gear_cy, gear_r, gear_border, 1.0);
                    cairo_backend::draw_pango_text(cr, gear_cx - 7.0, gear_cy - 9.0, "⚙", "Inter, Sans 13", title_color, None);

                    // Battery Indicator (if present)
                    if let Some(bat) = self.status.battery.clone() {
                        let bat_x = gear_cx - gear_r - 76.0;
                        let bat_y = gear_cy - 6.0;
                        cairo_backend::draw_battery_indicator(cr, bat_x, bat_y, bat.percent, bat.charging, title_color);
                    }

                    // ── Quick Tiles Row (4 morphing tiles) ──────────────────
                    let tile_y = card_y + 60.0;
                    let tile_h = 44.0;
                    let tile_gap = 8.0;
                    let wide_w = (inner_w - 3.0 * 44.0 - 3.0 * tile_gap).max(44.0);

                    let net_st = self.status.network.clone();
                    let bt_st = self.status.bluetooth.clone();
                    let notifs = self.status.notifications.clone();

                    let active_bg = cairo_backend::hex_to_rgba(&self.theme.primary_container, 0.90);
                    let inactive_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.35);
                    let active_fg = self.style.accent;
                    let inactive_fg = cairo_backend::hex_to_rgba(&self.theme.on_surface, 0.85);

                    let notifs_count = notifs.notifications.len();
                    let tile_meta = [
                        (
                            "󰂚",
                            "Notifications",
                            if self.nc_dnd_silent { "Silent".to_string() } else if notifs_count == 0 { "Empty".to_string() } else { format!("{} items", notifs_count) },
                            self.selected_tab == 0
                        ),
                        (
                            if net_st.kind == "ethernet" { "󰌘" } else { "󰖩" },
                            if net_st.kind == "ethernet" { "Ethernet" } else { net_st.ssid.as_deref().unwrap_or("Wi-Fi") },
                            if net_st.connected { "Connected".to_string() } else { "Off".to_string() },
                            self.selected_tab == 1
                        ),
                        (
                            "󰂯",
                            bt_st.name.as_deref().unwrap_or("Bluetooth"),
                            if bt_st.connected { "Connected".to_string() } else { "Off".to_string() },
                            self.selected_tab == 2
                        ),
                        (
                            "󰅶",
                            "Caffeinate",
                            if self.caffeinate_active { "Active".to_string() } else { "Off".to_string() },
                            self.caffeinate_active
                        ),
                    ];

                    let mut needs_anim_redraw = false;
                    let mut tile_x = inner_x;

                    for i in 0..4 {
                        let target_w = if (i == self.selected_tab && i < 3) || (i == 3 && self.caffeinate_active && self.selected_tab == 3) {
                            wide_w
                        } else {
                            44.0
                        };

                        let diff = target_w - self.nc_tile_widths[i];
                        if diff.abs() > 0.5 {
                            self.nc_tile_widths[i] += diff * lerp_factor(0.35, self.frame_dt);
                            needs_anim_redraw = true;
                        } else {
                            self.nc_tile_widths[i] = target_w;
                        }

                        let cur_w = self.nc_tile_widths[i];
                        let (icon, label, subtext, is_selected) = &tile_meta[i];
                        let bg = if *is_selected { active_bg } else { inactive_bg };
                        let fg = if *is_selected { active_fg } else { inactive_fg };

                        cairo_backend::fill_rounded_rect(cr, tile_x, tile_y, cur_w, tile_h, 12.0, bg);

                        if cur_w < 70.0 {
                            // Centered icon only
                            cairo_backend::draw_pango_text(cr, tile_x + (cur_w - 20.0) / 2.0, tile_y + 11.0, icon, "Inter, Sans 16", fg, None);
                        } else {
                            // Expanded tile: icon + title + subtitle
                            cairo_backend::draw_pango_text(cr, tile_x + 12.0, tile_y + 11.0, icon, "Inter, Sans 16", fg, None);
                            cairo_backend::draw_pango_text(cr, tile_x + 40.0, tile_y + 7.0, label, "Inter, Sans Bold 12", fg, Some((cur_w - 50.0) as i32));
                            let sub_fg = (fg.0, fg.1, fg.2, 0.70);
                            cairo_backend::draw_pango_text(cr, tile_x + 40.0, tile_y + 24.0, subtext, "Inter, Sans 10", sub_fg, Some((cur_w - 50.0) as i32));
                        }

                        tile_x += cur_w + tile_gap;
                    }

                    if needs_anim_redraw {
                        is_animating = true;
                    }

                    // ── Footer (Volume + Brightness + Audio Sink Menu) ───────
                    let footer_h = 100.0;
                    let footer_y = card_y + card_h - footer_h - 16.0;
                    let footer_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.20);
                    cairo_backend::fill_rounded_rect(cr, inner_x, footer_y, inner_w, footer_h, 14.0, footer_bg);

                    let slider_x = inner_x + 46.0;
                    let slider_w = inner_w - 92.0;
                    let primary_c = self.style.accent;
                    let track_c = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.50);
                    let knob_c = cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 1.0);

                    // Volume Row
                    let vol_y = footer_y + 12.0;
                    let (cur_vol, is_muted) = (self.status.volume, self.status.muted);
                    let vol_pct = (cur_vol as f64 / 100.0).clamp(0.0, 1.0);
                    let vol_icon = if is_muted || cur_vol == 0 { "󰝟" } else if cur_vol < 50 { "󰕿" } else { "󰕾" };
                    cairo_backend::draw_pango_text(cr, inner_x + 14.0, vol_y + 4.0, vol_icon, "Inter, Sans 16", title_color, None);
                    cairo_backend::draw_slider(cr, slider_x, vol_y + 8.0, slider_w, 16.0, vol_pct, primary_c, track_c, knob_c);

                    // Chevron button for audio sinks menu
                    let chev_x = inner_x + inner_w - 34.0;
                    let chev_y = vol_y + 2.0;
                    if self.nc_sink_menu_open {
                        let chev_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.60);
                        cairo_backend::fill_circle(cr, chev_x + 12.0, chev_y + 12.0, 14.0, chev_bg);
                    }
                    let chev_glyph = if self.nc_sink_menu_open { "▼" } else { "▲" };
                    cairo_backend::draw_pango_text(cr, chev_x + 7.0, chev_y + 5.0, chev_glyph, "Inter, Sans Bold 11", title_color, None);

                    // Brightness Row
                    let bright_y = footer_y + 54.0;
                    let cur_bright = self.status.brightness;
                    let bright_pct = (cur_bright as f64 / 100.0).clamp(0.0, 1.0);
                    cairo_backend::draw_pango_text(cr, inner_x + 14.0, bright_y + 4.0, "󰃠", "Inter, Sans 16", title_color, None);
                    cairo_backend::draw_slider(cr, slider_x, bright_y + 8.0, slider_w, 16.0, bright_pct, primary_c, track_c, knob_c);

                    // Audio Sinks Popover Menu (opens upward)
                    if self.nc_sink_menu_open {
                        let sink_row_h = 32.0;
                        let sink_menu_h = ((self.nc_audio_sinks.len() as f64) * sink_row_h + 16.0).min(180.0);
                        let sink_menu_y = footer_y - sink_menu_h - 8.0;

                        let menu_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.96);
                        cairo_backend::fill_rounded_rect(cr, inner_x, sink_menu_y, inner_w, sink_menu_h, 12.0, menu_bg);
                        cairo_backend::stroke_rounded_rect(cr, inner_x, sink_menu_y, inner_w, sink_menu_h, 12.0, card_border, 1.0);

                        for (idx, s) in self.nc_audio_sinks.iter().enumerate() {
                            let sy = sink_menu_y + 8.0 + (idx as f64) * sink_row_h;
                            if sy + sink_row_h > sink_menu_y + sink_menu_h { break; }

                            // Radio circle
                            let radio_cx = inner_x + 20.0;
                            let radio_cy = sy + sink_row_h / 2.0;
                            cairo_backend::stroke_circle(cr, radio_cx, radio_cy, 6.0, primary_c, 1.5);
                            if s.is_default {
                                cairo_backend::fill_circle(cr, radio_cx, radio_cy, 3.5, primary_c);
                            }

                            // Kind icon
                            let k_icon = match s.kind.as_str() {
                                "bluetooth" => "󰂯",
                                "hdmi" => "󰍹",
                                _ => "󰕾",
                            };
                            cairo_backend::draw_pango_text(cr, inner_x + 36.0, sy + 6.0, k_icon, "Inter, Sans 13", primary_c, None);

                            // Description
                            cairo_backend::draw_pango_text(cr, inner_x + 60.0, sy + 7.0, &s.description, "Inter, Sans 11", title_color, Some((inner_w - 74.0) as i32));
                        }
                    }

                    // ── Content Area between Quick Tiles & Footer ───────────
                    let content_top_y = tile_y + tile_h + 16.0;
                    let content_bot_y = if self.nc_sink_menu_open {
                        footer_y - (((self.nc_audio_sinks.len() as f64) * 32.0 + 16.0).min(180.0) + 16.0)
                    } else {
                        footer_y - 10.0
                    };
                    let content_h = (content_bot_y - content_top_y).max(50.0);
                    let sec_label_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.80);

                    match self.selected_tab {
                        0 => {
                            // Notifications Page
                            let notif_count = notifs.notifications.len();
                            let count_str = if notif_count == 0 { "No notifications".to_string() } else if notif_count == 1 { "1 notification".to_string() } else { format!("{} notifications", notif_count) };
                            cairo_backend::draw_pango_text(cr, inner_x, content_top_y, &count_str, "Inter, Sans Bold 12", sec_label_color, None);

                            // Silent DND Button
                            let clear_w = 64.0;
                            let clear_x = inner_x + inner_w - clear_w;
                            let silent_w = 64.0;
                            let silent_x = clear_x - silent_w - 8.0;

                            let silent_bg = if self.nc_dnd_silent {
                                cairo_backend::hex_to_rgba(&self.theme.primary, 0.85)
                            } else {
                                cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.40)
                            };
                            let silent_fg = if self.nc_dnd_silent {
                                (0.05, 0.08, 0.10, 1.0)
                            } else {
                                title_color
                            };
                            cairo_backend::fill_rounded_rect(cr, silent_x, content_top_y - 4.0, silent_w, 24.0, 6.0, silent_bg);
                            cairo_backend::draw_pango_text(cr, silent_x + 8.0, content_top_y + 1.0, "󰂛 Silent", "Inter, Sans Bold 10", silent_fg, None);

                            // Clear all button
                            let clear_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.40);
                            cairo_backend::fill_rounded_rect(cr, clear_x, content_top_y - 4.0, clear_w, 24.0, 6.0, clear_bg);
                            cairo_backend::draw_pango_text(cr, clear_x + 10.0, content_top_y + 1.0, "✕ Clear", "Inter, Sans Bold 10", title_color, None);

                            // List Area
                            let list_y = content_top_y + 32.0;
                            let list_h = content_h - 32.0;

                            if notifs.notifications.is_empty() {
                                let empty_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.60);
                                cairo_backend::draw_pango_text(cr, inner_x + 40.0, list_y + 50.0, if self.nc_dnd_silent { "Silent mode is on" } else { "No notifications in history" }, "Inter, Sans Italic 13", empty_color, None);
                            } else {
                                cairo_backend::cairo_save(cr);
                                cairo_backend::cairo_rectangle(cr, inner_x, list_y, inner_w, list_h);
                                cairo_backend::cairo_clip(cr);

                                // Group notifications by app_name
                                let mut groups: Vec<(String, Vec<&prism_common::notifications::Notification>)> = Vec::new();
                                for n in &notifs.notifications {
                                    if let Some((_, list)) = groups.iter_mut().find(|(k, _)| k == &n.app_name) {
                                        list.push(n);
                                    } else {
                                        groups.push((n.app_name.clone(), vec![n]));
                                    }
                                }

                                let mut cur_y = list_y - self.nc_notif_scroll;
                                for (app, items) in groups {
                                    let is_expanded = self.nc_expanded_group.as_deref() == Some(&app);
                                    let header_h = 56.0;
                                    let total_in_group = items.len();
                                    let latest = items[0];

                                    // Group card background
                                    let grp_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.35);
                                    let grp_h = if is_expanded { header_h + (items.len() as f64) * 52.0 + 8.0 } else { header_h };
                                    cairo_backend::fill_rounded_rect(cr, inner_x, cur_y, inner_w, grp_h, 10.0, grp_bg);

                                    // App initial badge
                                    let badge_bg = cairo_backend::hex_to_rgba(&self.theme.primary_container, 0.80);
                                    cairo_backend::fill_rounded_rect(cr, inner_x + 10.0, cur_y + 10.0, 36.0, 36.0, 8.0, badge_bg);
                                    let initial = app.chars().next().unwrap_or('A').to_uppercase().to_string();
                                    cairo_backend::draw_pango_text(cr, inner_x + 22.0, cur_y + 18.0, &initial, "Inter, Sans Bold 14", primary_c, None);

                                    // App Name & Timestamp
                                    cairo_backend::draw_pango_text(cr, inner_x + 54.0, cur_y + 10.0, &app, "Inter, Sans Bold 12", primary_c, Some((inner_w - 120.0) as i32));

                                    // Count pill
                                    let pill_x = inner_x + inner_w - 48.0;
                                    let pill_bg = cairo_backend::hex_to_rgba(&self.theme.outline_variant, 0.30);
                                    cairo_backend::fill_rounded_rect(cr, pill_x, cur_y + 10.0, 38.0, 20.0, 10.0, pill_bg);
                                    let pill_text = format!("{} {}", total_in_group, if is_expanded { "▲" } else { "▼" });
                                    cairo_backend::draw_pango_text(cr, pill_x + 6.0, cur_y + 12.0, &pill_text, "Inter, Sans 9", title_color, None);

                                    // Preview line
                                    let prev_text = format!("{}: {}", latest.summary, latest.body);
                                    let sub_c = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70);
                                    cairo_backend::draw_pango_text(cr, inner_x + 54.0, cur_y + 28.0, &prev_text, "Inter, Sans 11", sub_c, Some((inner_w - 110.0) as i32));

                                    if is_expanded {
                                        let mut sub_y = cur_y + header_h + 4.0;
                                        for item in items {
                                            let card_bg = cairo_backend::hex_to_rgba(&self.theme.surface, 0.50);
                                            cairo_backend::fill_rounded_rect(cr, inner_x + 10.0, sub_y, inner_w - 20.0, 48.0, 8.0, card_bg);

                                            cairo_backend::draw_pango_text(cr, inner_x + 20.0, sub_y + 6.0, &item.summary, "Inter, Sans Bold 11", title_color, Some((inner_w - 60.0) as i32));
                                            cairo_backend::draw_pango_text(cr, inner_x + 20.0, sub_y + 24.0, &item.body, "Inter, Sans 10", sub_c, Some((inner_w - 60.0) as i32));

                                            // Dismiss "✕" button
                                            let dis_x = inner_x + inner_w - 36.0;
                                            cairo_backend::draw_pango_text(cr, dis_x, sub_y + 14.0, "✕", "Inter, Sans Bold 11", sec_label_color, None);

                                            sub_y += 52.0;
                                        }
                                        cur_y = sub_y + 6.0;
                                    } else {
                                        cur_y += header_h + 8.0;
                                    }
                                }

                                cairo_backend::cairo_restore(cr);
                            }
                        }
                        1 => {
                            // Network Page
                            let mut net_y = content_top_y;

                            // Ethernet Card (if connected)
                            if net_st.kind == "ethernet" {
                                let eth_card_h = 58.0;
                                let eth_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.35);
                                cairo_backend::fill_rounded_rect(cr, inner_x, net_y, inner_w, eth_card_h, 10.0, eth_bg);
                                cairo_backend::draw_pango_text(cr, inner_x + 14.0, net_y + 18.0, "󰌘", "Inter, Sans 20", primary_c, None);
                                cairo_backend::draw_pango_text(cr, inner_x + 46.0, net_y + 10.0, "Ethernet Connected", "Inter, Sans Bold 13", title_color, None);
                                let eth_sub = format!("IP: {}", net_st.ip.as_deref().unwrap_or("Connected"));
                                cairo_backend::draw_pango_text(cr, inner_x + 46.0, net_y + 30.0, &eth_sub, "Inter, Sans 11", sec_label_color, None);
                                net_y += eth_card_h + 12.0;
                            }

                            // Wi-Fi Power Row
                            cairo_backend::draw_pango_text(cr, inner_x, net_y + 4.0, "Wi-Fi Power", "Inter, Sans Bold 13", title_color, None);
                            cairo_backend::draw_pango_text(cr, inner_x, net_y + 22.0, "Enable wireless networking", "Inter, Sans 11", sec_label_color, None);
                            cairo_backend::draw_toggle_switch(cr, inner_x + inner_w - 54.0, net_y + 6.0, net_st.connected, primary_c, track_c, knob_c);
                            net_y += 48.0;

                            // Networks Header + Rescan
                            cairo_backend::draw_pango_text(cr, inner_x, net_y + 4.0, "NETWORKS", "Inter, Sans Bold 11", sec_label_color, None);
                            let rescan_x = inner_x + inner_w - 60.0;
                            cairo_backend::draw_pango_text(cr, rescan_x, net_y + 4.0, "Rescan", "Inter, Sans Bold 11", primary_c, None);
                            net_y += 28.0;

                            // Wi-Fi Networks List
                            let list_h = (content_bot_y - net_y).max(40.0);
                            cairo_backend::cairo_save(cr);
                            cairo_backend::cairo_rectangle(cr, inner_x, net_y, inner_w, list_h);
                            cairo_backend::cairo_clip(cr);

                            if self.nc_wifi_networks.is_empty() {
                                let empty_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.60);
                                cairo_backend::draw_pango_text(cr, inner_x, net_y + 20.0, "No Wi-Fi networks found. Click Rescan.", "Inter, Sans Italic 12", empty_color, None);
                            } else {
                                let mut y_cur = net_y - self.nc_network_scroll;
                                for net in &self.nc_wifi_networks {
                                    let is_expanded = self.nc_wifi_expanded_ssid.as_deref() == Some(&net.ssid);
                                    let row_h = 42.0;

                                    let net_bg = if net.in_use {
                                        cairo_backend::hex_to_rgba(&self.theme.primary_container, 0.60)
                                    } else {
                                        cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.25)
                                    };
                                    cairo_backend::fill_rounded_rect(cr, inner_x, y_cur, inner_w, row_h, 8.0, net_bg);

                                    // Signal icon
                                    let sig_icon = if net.signal >= 75 { "󰤨" } else if net.signal >= 50 { "󰤥" } else if net.signal >= 25 { "󰤢" } else { "󰤟" };
                                    cairo_backend::draw_pango_text(cr, inner_x + 12.0, y_cur + 11.0, sig_icon, "Inter, Sans 14", primary_c, None);

                                    // SSID
                                    cairo_backend::draw_pango_text(cr, inner_x + 36.0, y_cur + 12.0, &net.ssid, "Inter, Sans Bold 12", title_color, Some((inner_w - 110.0) as i32));

                                    // Security lock
                                    if !net.security.is_empty() && net.security != "--" {
                                        let lock_x = inner_x + inner_w - 56.0;
                                        cairo_backend::draw_pango_text(cr, lock_x, y_cur + 12.0, "🔒", "Inter, Sans 11", sec_label_color, None);
                                    }

                                    // In-use checkmark
                                    if net.in_use {
                                        let chk_x = inner_x + inner_w - 28.0;
                                        cairo_backend::draw_pango_text(cr, chk_x, y_cur + 11.0, "✓", "Inter, Sans Bold 13", primary_c, None);
                                    }

                                    if is_expanded {
                                        // Expanded password input row
                                        let pass_y = y_cur + row_h + 4.0;
                                        let pass_h = 42.0;
                                        let pass_bg = cairo_backend::hex_to_rgba(&self.theme.surface, 0.70);
                                        cairo_backend::fill_rounded_rect(cr, inner_x + 10.0, pass_y, inner_w - 20.0, pass_h, 8.0, pass_bg);

                                        let pass_text = if self.nc_wifi_password_input.is_empty() {
                                            "Enter password...".to_string()
                                        } else {
                                            "•".repeat(self.nc_wifi_password_input.len())
                                        };
                                        let pass_c = if self.nc_wifi_password_input.is_empty() { sec_label_color } else { title_color };
                                        cairo_backend::draw_pango_text(cr, inner_x + 20.0, pass_y + 12.0, &pass_text, "Inter, Sans 11", pass_c, Some((inner_w - 110.0) as i32));

                                        // Connect button pill
                                        let conn_btn_x = inner_x + inner_w - 90.0;
                                        cairo_backend::fill_rounded_rect(cr, conn_btn_x, pass_y + 6.0, 70.0, 30.0, 6.0, primary_c);
                                        let conn_fg = (0.05, 0.08, 0.10, 1.0);
                                        cairo_backend::draw_pango_text(cr, conn_btn_x + 12.0, pass_y + 12.0, "Connect", "Inter, Sans Bold 11", conn_fg, None);

                                        y_cur += row_h + pass_h + 8.0;
                                    } else {
                                        y_cur += row_h + 4.0;
                                    }
                                }
                            }

                            cairo_backend::cairo_restore(cr);
                        }
                        2 => {
                            // Bluetooth Page
                            let mut bt_y = content_top_y;

                            // Bluetooth Power Row
                            cairo_backend::draw_pango_text(cr, inner_x, bt_y + 4.0, "Bluetooth Power", "Inter, Sans Bold 13", title_color, None);
                            cairo_backend::draw_pango_text(cr, inner_x, bt_y + 22.0, "Enable bluetooth wireless", "Inter, Sans 11", sec_label_color, None);
                            cairo_backend::draw_toggle_switch(cr, inner_x + inner_w - 54.0, bt_y + 6.0, bt_st.connected, primary_c, track_c, knob_c);
                            bt_y += 48.0;

                            // Devices Header + Scan
                            cairo_backend::draw_pango_text(cr, inner_x, bt_y + 4.0, "DEVICES", "Inter, Sans Bold 11", sec_label_color, None);
                            let scan_x = inner_x + inner_w - 50.0;
                            cairo_backend::draw_pango_text(cr, scan_x, bt_y + 4.0, "Scan", "Inter, Sans Bold 11", primary_c, None);
                            bt_y += 28.0;

                            // Devices List
                            let list_h = (content_bot_y - bt_y).max(40.0);
                            cairo_backend::cairo_save(cr);
                            cairo_backend::cairo_rectangle(cr, inner_x, bt_y, inner_w, list_h);
                            cairo_backend::cairo_clip(cr);

                            if self.nc_bt_devices.is_empty() {
                                let empty_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.60);
                                cairo_backend::draw_pango_text(cr, inner_x, bt_y + 20.0, "No Bluetooth devices found. Click Scan.", "Inter, Sans Italic 12", empty_color, None);
                            } else {
                                let mut y_cur = bt_y - self.nc_bt_scroll;
                                for dev in &self.nc_bt_devices {
                                    let row_h = 44.0;
                                    let row_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.25);
                                    cairo_backend::fill_rounded_rect(cr, inner_x, y_cur, inner_w, row_h, 8.0, row_bg);

                                    cairo_backend::draw_pango_text(cr, inner_x + 12.0, y_cur + 13.0, "󰂯", "Inter, Sans 14", primary_c, None);
                                    cairo_backend::draw_pango_text(cr, inner_x + 36.0, y_cur + 8.0, &dev.name, "Inter, Sans Bold 12", title_color, Some((inner_w - 130.0) as i32));
                                    let status_text = if dev.connected { "Connected" } else if dev.paired { "Paired" } else { &dev.mac };
                                    cairo_backend::draw_pango_text(cr, inner_x + 36.0, y_cur + 24.0, status_text, "Inter, Sans 10", sec_label_color, Some((inner_w - 130.0) as i32));

                                    // Action button
                                    let btn_x = inner_x + inner_w - 88.0;
                                    let (btn_lbl, btn_bg, btn_fg) = if dev.connected {
                                        ("Disconnect", self.style.card_border, title_color)
                                    } else if dev.paired {
                                        ("Connect", primary_c, (0.05, 0.08, 0.10, 1.0))
                                    } else {
                                        ("Pair", primary_c, (0.05, 0.08, 0.10, 1.0))
                                    };
                                    cairo_backend::fill_rounded_rect(cr, btn_x, y_cur + 8.0, 78.0, 28.0, 6.0, btn_bg);
                                    cairo_backend::draw_pango_text(cr, btn_x + 10.0, y_cur + 13.0, btn_lbl, "Inter, Sans Bold 10", btn_fg, None);

                                    y_cur += row_h + 6.0;
                                }
                            }

                            cairo_backend::cairo_restore(cr);
                        }
                        _ => {}
                    }
                }
                UiMode::Notification { .. } => {
                    let radius = self.style.radius;
                    let card_bg = self.style.card_bg;
                    let border_color = self.style.card_border;
                    let primary_color = self.style.accent;
                    let on_surface = self.style.fg;
                    let on_surface_var = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.85);
                    let on_surface_sub = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.65);

                    let card_w = 360.0_f64;
                    let mut current_y = 0.0;

                    for toast in &mut self.notif_toasts {
                        if toast.reveal <= 0.001 {
                            continue;
                        }

                        let text_w = (card_w - 58.0 - 36.0) as i32;
                        let (_, sum_h) = cairo_backend::measure_pango_text(cr, &toast.summary, "Inter, Sans Bold 13", text_w, Some(2));
                        let (_, b_h) = if toast.body.is_empty() {
                            (0, 0)
                        } else {
                            cairo_backend::measure_pango_text(cr, &toast.body, "Inter, Sans 11", text_w + 20, Some(5))
                        };
                        let exact_card_h = (12.0 + 18.0 + 4.0 + (sum_h as f64).max(18.0) + (if b_h > 0 { 6.0 + (b_h as f64).max(16.0) } else { 0.0 }) + 14.0).max(72.0);
                        toast.card_height = exact_card_h;

                        let card_h = toast.card_height;
                        let full_item_h = (card_h + 12.0) * toast.reveal;
                        let card_x = toast.slide_x;
                        let card_y = current_y;

                        let alpha = toast.opacity.clamp(0.0, 1.0);
                        let bg_c = (card_bg.0, card_bg.1, card_bg.2, card_bg.3 * alpha);
                        let border_c = (border_color.0, border_color.1, border_color.2, border_color.3 * alpha);
                        let prim_c = (primary_color.0, primary_color.1, primary_color.2, primary_color.3 * alpha);
                        let title_c = (on_surface.0, on_surface.1, on_surface.2, on_surface.3 * alpha);
                        let body_c = (on_surface_var.0, on_surface_var.1, on_surface_var.2, on_surface_var.3 * alpha);
                        let app_c = (on_surface_sub.0, on_surface_sub.1, on_surface_sub.2, on_surface_sub.3 * alpha);

                        cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, bg_c);
                        cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, border_c, 1.0);
                        cairo_backend::fill_rounded_rect(cr, card_x, card_y + 10.0, 4.0, (card_h - 20.0).max(10.0), 2.0, prim_c);

                        let icon_x = card_x + 14.0;
                        let icon_y = card_y + 14.0;
                        let icon_bg = (primary_color.0, primary_color.1, primary_color.2, 0.18 * alpha);
                        cairo_backend::fill_rounded_rect(cr, icon_x, icon_y, 36.0, 36.0, 18.0, icon_bg);
                        let initial_letter = toast.app_name.chars().next().unwrap_or('?').to_uppercase().to_string();
                        cairo_backend::draw_pango_text(cr, icon_x + 12.0, icon_y + 8.0, &initial_letter, "Inter, Sans Bold 14", prim_c, None);

                        let text_x = card_x + 58.0;
                        cairo_backend::draw_pango_text(cr, text_x, card_y + 12.0, &toast.app_name, "Inter, Sans Bold 11", app_c, Some(text_w - 30));

                        let sum_y = card_y + 30.0;
                        cairo_backend::draw_pango_text_wrapped(cr, text_x, sum_y, &toast.summary, "Inter, Sans Bold 13", title_c, text_w, Some(2));

                        if !toast.body.is_empty() {
                            let body_y = sum_y + (sum_h as f64).max(18.0) + 4.0;
                            cairo_backend::draw_pango_text_wrapped(cr, text_x, body_y, &toast.body, "Inter, Sans 11", body_c, text_w + 20, Some(5));
                        }

                        let close_x = card_x + card_w - 30.0;
                        let close_y = card_y + 10.0;
                        let px = self.pointer_x as f64;
                        let py = self.pointer_y as f64;
                        let is_over_close = px >= close_x && px <= close_x + 22.0 && py >= close_y && py <= close_y + 22.0;

                        let close_bg = if is_over_close {
                            (on_surface.0, on_surface.1, on_surface.2, 0.18 * alpha)
                        } else {
                            (0.0, 0.0, 0.0, 0.0)
                        };
                        cairo_backend::fill_rounded_rect(cr, close_x, close_y, 22.0, 22.0, 11.0, close_bg);
                        let close_fg = if is_over_close {
                            title_c
                        } else {
                            (on_surface_sub.0, on_surface_sub.1, on_surface_sub.2, 0.70 * alpha)
                        };
                        cairo_backend::draw_pango_text(cr, close_x + 6.0, close_y + 3.0, "✕", "Inter, Sans 11", close_fg, None);

                        current_y += full_item_h;
                    }
                }
                UiMode::Power => {
                    let card_w = (width as f64 * 0.70).min(1400.0).max(700.0);
                    let card_h = 360.0;
                    let card_x = ((width as f64 - card_w) / 2.0).round();
                    let card_y = ((height as f64 - card_h) / 2.0).round();
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, card_h as i32);

                    let radius = (self.config.window_radius as f64).clamp(8.0, 36.0);
                    let card_bg = self.style.card_bg;
                    let border_c = self.style.card_border;
                    let primary_c = self.style.accent;
                    let fg_color = self.style.fg;
                    let sub_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70);

                    cairo_backend::draw_grand_power_menu(
                        cr,
                        card_x,
                        card_y,
                        card_w,
                        card_h,
                        radius,
                        card_bg,
                        border_c,
                        self.selected_index,
                        primary_c,
                        fg_color,
                        sub_color,
                    );
                }
                UiMode::Keybinds => {
                    let card_w = 1200.0_f64.min(width as f64 - 40.0);
                    let card_h = 700.0_f64.min(height as f64 - 40.0);
                    let card_x = ((width as f64 - card_w) / 2.0).round();
                    let card_y = ((height as f64 - card_h) / 2.0).round();
                    self.card_bounds = (card_x as i32, card_y as i32, card_w as i32, card_h as i32);

                    let radius = (self.config.window_radius as f64).clamp(8.0, 36.0);
                    let card_bg = self.style.card_bg;
                    let border_c = self.style.card_border;
                    let primary_c = self.style.accent;
                    let fg_color = self.style.fg;
                    let sub_color = cairo_backend::hex_to_rgba(&self.theme.on_surface_variant, 0.70);

                    // Background & border
                    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, card_bg);
                    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, radius, border_c, 1.5);

                    // Header bar
                    let header_y = card_y + 20.0;
                    cairo_backend::draw_pango_text(cr, card_x + 28.0, header_y + 2.0, "KEYBOARD SHORTCUTS", "Inter, Sans Bold 17", primary_c, None);
                    let filtered_binds = self.get_filtered_keybinds();
                    let count_text = format!("{} shortcuts loaded", filtered_binds.len());
                    cairo_backend::draw_pango_text(cr, card_x + 28.0, header_y + 24.0, &count_text, "Inter, Sans 12", sub_color, None);

                    // Search bar on top right
                    let search_w = 340.0;
                    let search_h = 38.0;
                    let search_x = card_x + card_w - search_w - 28.0;
                    let search_bg = cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.50);
                    let search_border = if !self.search_query.is_empty() { primary_c } else { border_c };
                    cairo_backend::fill_rounded_rect(cr, search_x, header_y, search_w, search_h, 8.0, search_bg);
                    cairo_backend::stroke_rounded_rect(cr, search_x, header_y, search_w, search_h, 8.0, search_border, 1.0);
                    cairo_backend::draw_pango_text(cr, search_x + 12.0, header_y + 10.0, "🔍", "Inter, Sans 13", primary_c, None);

                    let search_text = if self.search_query.is_empty() { "Search shortcuts (e.g. workspace, terminal)..." } else { &self.search_query };
                    let search_text_c = if self.search_query.is_empty() { sub_color } else { fg_color };
                    cairo_backend::draw_pango_text(cr, search_x + 36.0, header_y + 10.0, search_text, "Inter, Sans 12", search_text_c, Some((search_w - 70.0) as i32));

                    if !self.search_query.is_empty() {
                        cairo_backend::draw_pango_text(cr, search_x + search_w - 26.0, header_y + 10.0, "✕", "Inter, Sans Bold 13", sub_color, None);
                    }

                    // Divider
                    let div_y = card_y + 68.0;
                    cairo_backend::fill_rounded_rect(cr, card_x + 24.0, div_y, card_w - 48.0, 1.0, 0.5, border_c);

                    // List area
                    let list_y = div_y + 10.0;
                    let list_h = card_h - (list_y - card_y) - 20.0;
                    let col_w = (card_w - 68.0) / 2.0;
                    let item_h = 42.0;
                    let spacing = 6.0;

                    let total_rows = ((filtered_binds.len() + 1) / 2) as f64;
                    let max_scroll = (total_rows * (item_h + spacing) - list_h).max(0.0);
                    let scroll = self.keybinds_scroll.clamp(0.0, max_scroll);

                    if filtered_binds.is_empty() {
                        cairo_backend::draw_pango_text(cr, card_x + 32.0, list_y + 30.0, "No shortcuts matched your search query", "Inter, Sans Italic 14", sub_color, None);
                    } else {
                        cairo_backend::cairo_save(cr);
                        cairo_backend::cairo_rectangle(cr, card_x + 20.0, list_y, card_w - 40.0, list_h);
                        cairo_backend::cairo_clip(cr);

                        for (i, b) in filtered_binds.iter().enumerate() {
                            let row = i / 2;
                            let col = i % 2;
                            let col_x = if col == 0 { card_x + 24.0 } else { card_x + 34.0 + col_w };
                            let iy = list_y - scroll + (row as f64) * (item_h + spacing);

                            // Skip off-screen items
                            if iy + item_h < list_y || iy > list_y + list_h {
                                continue;
                            }

                            let item_bg = if row % 2 == 0 {
                                cairo_backend::hex_to_rgba(&self.theme.surface, 0.50)
                            } else {
                                cairo_backend::hex_to_rgba(&self.theme.surface_variant, 0.35)
                            };
                            cairo_backend::fill_rounded_rect(cr, col_x, iy, col_w, item_h, 8.0, item_bg);

                            // Key combo pill
                            let combo = b.formatted_combo();
                            let pill_w = 180.0_f64.min(col_w * 0.40);
                            let pill_bg = cairo_backend::hex_to_rgba(&self.theme.primary_container, 0.90);
                            cairo_backend::fill_rounded_rect(cr, col_x + 6.0, iy + 5.0, pill_w, item_h - 10.0, 6.0, pill_bg);
                            let combo_c = cairo_backend::hex_to_rgba(&self.theme.on_primary_container, 1.0);
                            cairo_backend::draw_pango_text(cr, col_x + 12.0, iy + 11.0, &combo, "Inter, Sans Bold 11", combo_c, Some((pill_w - 12.0) as i32));

                            // Action text
                            let action_x = col_x + pill_w + 14.0;
                            let cat_w = 95.0;
                            let action_max_w = (col_w - pill_w - cat_w - 24.0).max(50.0);
                            cairo_backend::draw_pango_text(cr, action_x, iy + 11.0, &b.action, "Inter, Sans 12", fg_color, Some(action_max_w as i32));

                            // Category badge
                            let cat_x = col_x + col_w - cat_w - 8.0;
                            let cat_bg = cairo_backend::hex_to_rgba(&self.theme.outline_variant, 0.25);
                            cairo_backend::fill_rounded_rect(cr, cat_x, iy + 8.0, cat_w, item_h - 16.0, 4.0, cat_bg);
                            cairo_backend::draw_pango_text(cr, cat_x + 6.0, iy + 12.0, &b.category, "Inter, Sans 10", sub_color, Some((cat_w - 12.0) as i32));
                        }

                        cairo_backend::cairo_restore(cr);

                        // Render scrollbar thumb if overflowing
                        if max_scroll > 0.0 {
                            cairo_backend::draw_scrollbar_thumb(
                                cr,
                                card_x + card_w - 14.0,
                                list_y,
                                list_h,
                                total_rows * (item_h + spacing),
                                self.keybinds_scroll,
                                4.0,
                                (primary_c.0, primary_c.1, primary_c.2, 0.45),
                            );
                        }
                    }
                }
            }

            } // if redraw_content

            cairo_backend::cairo_restore(cr);
            cairo_backend::cairo_destroy(cr);

            if redraw_content {
                self.content_dirty = false;
                self.content_animating = use_cache && is_animating;
            }

            let mut pose = Pose::REST;
            if use_cache {
                if let Some(tr) = &self.transition {
                    pose = tr.sample();
                }
                if pose.done && !self.closing {
                    self.transition = None;
                }
                if let Some((img, _, _)) = self.card_cache {
                    let out = cairo_backend::cairo_create(cairo_surface);
                    cairo_backend::cairo_translate(out, pose.tx, pose.ty);
                    let scaled = (pose.scale - 1.0).abs() > 1e-3;
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
                    if pose.alpha >= 0.999 {
                        cairo_backend::cairo_paint(out);
                    } else {
                        cairo_backend::cairo_paint_with_alpha(out, pose.alpha.max(0.0));
                    }
                    cairo_backend::cairo_destroy(out);
                }
                // Fade the scrim with the card; applied after drawing (see draw_frame).
                self.scrim_pending = Some(pose.shown.clamp(0.0, 1.0));
                if self.closing && pose.done {
                    self.exit_requested = true;
                }
            }
            cairo_backend::cairo_surface_destroy(cairo_surface);

            surface.attach(Some(&buffer), 0, 0);

            // Every commit asks for a frame callback: it paces redraws to the display refresh
            // (vblank) and wakes us to continue animations or apply coalesced redraw requests.
            if self.frame_callback.is_none() {
                self.frame_callback = Some(surface.frame(qh, ()));
            }
            self.animating = is_animating || !anim_done
                || (use_cache && !pose.done)
                || self.notif_toasts.iter().any(|t| t.phase == ToastPhase::Entering || t.phase == ToastPhase::Leaving);
            self.needs_render = false;

            if anim_done && self.animation.as_ref().map(|a| a.is_leaving).unwrap_or(false) {
                self.exit_requested = true;
            }

            surface.damage_buffer(0, 0, bw, bh);
            surface.commit();
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

/// Globals bound once per process and shared by every overlay session.
pub struct Globals {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    single_pixel_mgr: Option<WpSinglePixelBufferManagerV1>,
    viewporter: Option<WpViewporter>,
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
globals_ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, ZwlrLayerShellV1, wl_seat::WlSeat, WpCursorShapeManagerV1, WpSinglePixelBufferManagerV1, WpViewporter);

/// Data that is expensive to load, kept between overlay openings.
#[derive(Default)]
struct Resources {
    apps: Option<(Vec<DesktopApp>, std::time::Instant)>,
    emojis: Option<Vec<EmojiItem>>,
}

impl Resources {
    fn apps(&mut self) -> Vec<DesktopApp> {
        // .desktop files rarely change; refresh in the background of a later opening.
        let stale = self.apps.as_ref().map(|(_, t)| t.elapsed().as_secs() > 300).unwrap_or(true);
        if stale {
            self.apps = Some((load_all_desktop_apps(), std::time::Instant::now()));
        }
        self.apps.as_ref().map(|(a, _)| a.clone()).unwrap_or_default()
    }

    fn emojis(&mut self) -> Vec<EmojiItem> {
        self.emojis.get_or_insert_with(load_emojis).clone()
    }
}

fn parse_mode(args: &[String]) -> UiMode {
    match args.first().map(|s| s.as_str()) {
        Some("osd") => {
            let kind = args.get(1).cloned().unwrap_or_else(|| "volume".to_string());
            let val_num: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50.0);
            UiMode::Osd { kind, value: val_num / 100.0 }
        }
        Some("notif") if args.len() > 1 => UiMode::Notification {
            summary: args.get(1).cloned().unwrap_or_else(|| "Notification".to_string()),
            body: args.get(2).cloned().unwrap_or_default(),
            app_name: args.get(3).cloned().unwrap_or_else(|| "System".to_string()),
        },
        Some(name) => mode_from_name(name).unwrap_or(UiMode::Launcher),
        None => UiMode::Launcher,
    }
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
        UiMode::NotificationCenter => "notif-center",
        UiMode::Notification { .. } => "toast",
        UiMode::Osd { .. } => "osd",
        UiMode::Desktop => "desktop",
        UiMode::DesktopEdit => "desktop-edit",
    }
}

/// One open overlay: its own event queue and state on the shared connection.
struct Overlay {
    queue: EventQueue<UiState>,
    state: UiState,
    notif_tx: Option<std::sync::mpsc::Sender<prism_common::NotifOverlayMessage>>,
    osd_tx: Option<std::sync::mpsc::Sender<(String, f32)>>,
}

impl Overlay {
    fn open(
        conn: &Connection,
        g: &Globals,
        mode: UiMode,
        toast_id: u32,
        res: &mut Resources,
        waker: &Waker,
    ) -> Result<Overlay, String> {
        let mut event_queue: EventQueue<UiState> = conn.new_event_queue();
        let qh = event_queue.handle();
        let cfg = Config::load();
        let theme = Theme::load();

        // Load only what this mode needs; apps and emojis stay cached in the daemon.
        let apps = if matches!(mode, UiMode::Launcher | UiMode::Spotlight) { res.apps() } else { Vec::new() };
        let clipboard_items = if matches!(mode, UiMode::Clipboard) { ClipboardStore::load().items } else { Vec::new() };
        let emojis = if matches!(mode, UiMode::Emoji) { res.emojis() } else { Vec::new() };

        let (initial_w, initial_h) = match &mode {
            UiMode::Osd { .. } => (320, 64),
            UiMode::Notification { .. } => (420, 160),
            _ => (0, 0),
        };

        let mut initial_toasts = Vec::new();
        let (notif_tx, notif_socket_rx) = if let UiMode::Notification { ref summary, ref body, ref app_name } = mode {
            initial_toasts.push(PopupToast {
                id: toast_id,
                app_name: app_name.clone(),
                summary: summary.clone(),
                body: body.clone(),
                created_at: std::time::Instant::now(),
                timeout_ms: 5000,
                slide_x: -420.0,
                opacity: 0.0,
                reveal: 0.0,
                card_height: 84.0,
                phase: ToastPhase::Entering,
                hovered: false,
            });
            let (tx, rx) = std::sync::mpsc::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let (osd_tx, osd_rx) = if matches!(mode, UiMode::Osd { .. }) {
            let (tx, rx) = std::sync::mpsc::channel::<(String, f32)>();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let initial_anim = match &mode {
            UiMode::Notification { .. } => Some(SlideAnimation::new_enter(250, 160)),
            UiMode::Osd { .. } => Some(SlideAnimation::new_enter(180, 40)),
            _ => None,
        };

        // Spotlight file search worker (exits when the overlay drops its sender).
        let (spotlight_tx, spotlight_worker_rx) = std::sync::mpsc::channel::<String>();
        let (spotlight_worker_tx, spotlight_rx) = std::sync::mpsc::channel::<Vec<FileResult>>();
        if matches!(mode, UiMode::Spotlight) {
            let spotlight_waker = waker.clone();
            std::thread::spawn(move || {
                while let Ok(query) = spotlight_worker_rx.recv() {
                    let mut latest = query;
                    while let Ok(newer) = spotlight_worker_rx.try_recv() {
                        latest = newer;
                    }
                    let q = latest.trim();
                    let files = if q.is_empty() { Vec::new() } else { search_files(q, 8) };
                    if spotlight_worker_tx.send(files).is_err() {
                        break;
                    }
                    spotlight_waker.wake();
                }
            });
        }

        let spring = match &mode {
            UiMode::Osd { .. } => Some(SpringAnimation::out_back(260.0, 60.0, 300, 2.0)),
            _ => None,
        };

        let style = Style::new(&cfg, &theme);
        cairo_backend::set_global_font(&style.font_family);
        let transition = match &mode {
            UiMode::NotificationCenter => Some(Transition::enter(Motion::SlideLeft { distance: 400.0 + 100.0 }, &style)),
            UiMode::Launcher | UiMode::Spotlight | UiMode::Wallpaper | UiMode::Clipboard | UiMode::Emoji
            | UiMode::Settings | UiMode::Power | UiMode::Keybinds => Some(Transition::enter(Motion::Scale, &style)),
            _ => None,
        };

        let keybinds = if matches!(mode, UiMode::Keybinds) { prism_common::keybinds::load_keybinds() } else { Vec::new() };
        let emoji_recents = if matches!(mode, UiMode::Emoji) { load_emoji_recents() } else { Vec::new() };

        let scope = StatusScope { control_center: matches!(mode, UiMode::NotificationCenter) };
        let status_handle = if scope.control_center { Some(status::spawn(scope, waker.clone())) } else { None };

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
            search_query: String::new(),
            selected_index: 0,
            selected_tab: 0,
            pointer_x: 0,
            pointer_y: 0,
            pointer_down: false,
            card_bounds: (0, 0, 0, 0),
            theme,
            config: cfg,
            animation: initial_anim,
            frame_callback: None,
            clipboard_items,
            emojis,
            scroll_offset: 0.0,
            launcher_scroll: 0.0,
            emoji_scroll: 0.0,
            keybinds_scroll: 0.0,
            keybinds,
            emoji_recents,
            spotlight_tab: 0,
            spotlight_height_anim: 64.0,
            cursor_shape_manager: g.cursor_shape_manager.clone(),
            cursor_shape_device: None,
            pointer_serial: 0,
            spring_anim: spring,
            spotlight_tx: Some(spotlight_tx),
            spotlight_rx: Some(spotlight_rx),
            spotlight_files: Vec::new(),
            caffeinate_active: false,
            nc_tile_widths: [216.0, 44.0, 44.0, 44.0],
            nc_expanded_group: None,
            nc_dnd_silent: false,
            nc_notif_scroll: 0.0,
            nc_wifi_networks: Vec::new(),
            nc_wifi_expanded_ssid: None,
            nc_wifi_password_input: String::new(),
            _nc_wifi_scanning: false,
            nc_network_scroll: 0.0,
            nc_bt_devices: Vec::new(),
            _nc_bt_scanning: false,
            nc_bt_scroll: 0.0,
            nc_sink_menu_open: false,
            nc_audio_sinks: Vec::new(),
            notif_toasts: initial_toasts,
            notif_socket_rx,
            needs_render: false,
            buffer_pool: Vec::new(),
            status: StatusSnapshot::default(),
            status_tx: status_handle.as_ref().map(|h| h.tx.clone()),
            status_rx: None,
            single_pixel_mgr: g.single_pixel_mgr.clone(),
            viewporter: g.viewporter.clone(),
            scrim_surface: None,
            scrim_layer: None,
            scrim_viewport: None,
            scrim_buffer: None,
            scrim_configured: false,
            card_surface: false,
            surf_rect: (0, 0, 0, 0),
            pending_rect: None,
            pointer_origin: (0, 0),
            animating: false,
            last_frame_at: None,
            frame_dt: 1.0 / 60.0,
            osd_rx,
            osd_hide_at: None,
            xkb_context: xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            style: style.clone(),
            transition,
            closing: false,
            content_dirty: true,
            content_animating: false,
            card_cache: None,
            scrim_shown: 0.0,
            scrim_pending: None,
            settings: None,
            wallpaper: None,
        };
        state.status_rx = status_handle.map(|h| h.rx);
        if matches!(state.mode, UiMode::Settings) {
            state.settings = Some(settings::SettingsView::new(&state.config, waker.clone()));
        }

        // Modal cards: a full-screen scrim (dim, click-outside) plus a card-sized surface.
        let uses_card_surface = matches!(
            state.mode,
            UiMode::Launcher | UiMode::Spotlight | UiMode::Wallpaper | UiMode::Clipboard | UiMode::Emoji
                | UiMode::Settings | UiMode::NotificationCenter | UiMode::Power | UiMode::Keybinds
        );
        if uses_card_surface {
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
        }

        if matches!(state.mode, UiMode::Wallpaper) {
            state.wallpaper = Some(wallpaper::WallpaperView::new(&state.config, state.width as f64, waker.clone()));
        }
        let surface = comp.create_surface(&qh, ());
        let (layer_type, namespace, anchor, keyboard_mode) = match state.mode {
            UiMode::Osd { .. } => (Layer::Overlay, "prism-osd", Anchor::Bottom, KeyboardInteractivity::None),
            UiMode::Notification { .. } => (Layer::Overlay, "prism-notification", Anchor::Top | Anchor::Left, KeyboardInteractivity::None),
            // The carousel has no card background; it must not be blurred.
            UiMode::Wallpaper => (Layer::Overlay, "prism-wallpaper", Anchor::Top | Anchor::Left, KeyboardInteractivity::Exclusive),
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
            if let UiMode::Notification { .. } = &state.mode {
                layer_surface.set_margin(20, 0, 0, 10);
            }
        }
        surface.commit();
        state.surface = Some(surface);
        state.layer_surface = Some(layer_surface);
        if matches!(state.mode, UiMode::Osd { .. }) {
            state.osd_hide_at = Some(std::time::Instant::now() + std::time::Duration::from_millis(OSD_VISIBLE_MS));
        }
        Ok(Overlay { queue: event_queue, state, notif_tx, osd_tx })
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

    /// Timers, background data and the exit transition. Returns the next deadline (ms, -1 =
    /// none) or None once the overlay has finished and should be torn down.
    fn tick(&mut self) -> Option<i32> {
        let qh = self.queue.handle();
        let state = &mut self.state;
        let now = std::time::Instant::now();

        if state.exit_requested && state.card_surface && !state.closing && state.style.animations && state.configured {
            state.closing = true;
            state.exit_requested = false;
            let motion = state.transition.as_ref().map(|t| t.motion).unwrap_or(Motion::Scale);
            state.transition = Some(Transition::exit(motion, &state.style));
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

        let snap = state.status_rx.as_ref().and_then(|rx| rx.try_iter().last());
        if let Some(snap) = snap {
            state.apply_status(snap);
            state.render_and_commit(&qh);
        }
        if state.settings.as_mut().map(|s| s.poll()).unwrap_or(false) {
            state.apply_settings_layer(&qh);
            state.render_and_commit(&qh);
        }
        if let Some((redraw, close)) = state.wallpaper.as_mut().map(|w| w.poll()) {
            if close {
                state.exit_requested = true;
            } else if redraw {
                state.render_and_commit(&qh);
            }
        }
        let files = state.spotlight_rx.as_ref().and_then(|rx| rx.try_iter().last());
        if let Some(files) = files {
            state.spotlight_files = files;
            state.render_and_commit(&qh);
        }

        let mut timeout_ms: i32 = -1;
        match state.mode {
            UiMode::Notification { .. } => {
                state.drain_notification_messages(&qh);
                for toast in state.notif_toasts.iter_mut() {
                    if toast.phase == ToastPhase::Shown
                        && !toast.hovered
                        && now.duration_since(toast.created_at).as_millis() >= toast.timeout_ms as u128
                    {
                        toast.phase = ToastPhase::Leaving;
                        state.needs_render = true;
                    }
                }
                if state.needs_render {
                    state.render_and_commit(&qh);
                }
                let next = state
                    .notif_toasts
                    .iter()
                    .filter(|t| t.phase == ToastPhase::Shown && !t.hovered)
                    .map(|t| (t.timeout_ms as i64 - now.duration_since(t.created_at).as_millis() as i64).max(0))
                    .min();
                if let Some(ms) = next {
                    timeout_ms = (ms as i32).max(1);
                }
            }
            UiMode::Osd { .. } => {
                let update = state.osd_rx.as_ref().and_then(|rx| rx.try_iter().last());
                if let Some((kind, value)) = update {
                    state.mode = UiMode::Osd { kind, value };
                    state.osd_hide_at = Some(now + std::time::Duration::from_millis(OSD_VISIBLE_MS));
                    if state.animation.as_ref().map(|a| a.is_leaving).unwrap_or(false) {
                        state.animation = None;
                    }
                    state.render_and_commit(&qh);
                }
                if let Some(hide_at) = state.osd_hide_at {
                    if now >= hide_at {
                        state.osd_hide_at = None;
                        state.animation = Some(SlideAnimation::new_leave(150, 40));
                        state.render_and_commit(&qh);
                    } else {
                        timeout_ms = (hide_at.duration_since(now).as_millis() as i32).max(1);
                    }
                }
            }
            _ => {}
        }
        if state.exit_requested { None } else { Some(timeout_ms) }
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
        if let Some(l) = s.layer_surface.take() {
            l.destroy();
        }
        if let Some(sf) = s.surface.take() {
            sf.destroy();
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
    if env::var("WAYLAND_DISPLAY").is_err() && env::var("WAYLAND_SOCKET").is_err() {
        eprintln!("Error: WAYLAND_DISPLAY is not set. prism-ui requires an active Wayland compositor.");
        exit(1);
    }
    let daemon = args.first().map(|s| s == "daemon").unwrap_or(false);
    if daemon && !acquire_instance_lock("prism-ui-daemon.lock") {
        eprintln!("[prism-ui] daemon already running. Exiting.");
        exit(0);
    }
    let mode = if daemon { None } else { Some(parse_mode(&args)) };
    if let Some(UiMode::Desktop) = mode {
        if !acquire_instance_lock("prism-desktop.lock") {
            eprintln!("[prism-ui] Desktop layer already running. Exiting.");
            exit(0);
        }
    }
    let toast_id = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    run(mode, toast_id);
}

/// The event loop for the daemon (`initial` = None) or a single standalone overlay.
fn run(initial: Option<UiMode>, toast_id: u32) {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to connect to Wayland compositor: {}", e);
            exit(1);
        }
    };
    let waker = Waker::new().expect("failed to create wake pipe");
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
    };
    let _registry = conn.display().get_registry(&gq.handle(), ());
    if gq.roundtrip(&mut globals).is_err() {
        exit(1);
    }

    let daemon = initial.is_none();
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
            Some(mode) => match Overlay::open(&conn, &globals, mode, toast_id, &mut res, &waker) {
                Ok(o) => overlays.push(o),
                Err(e) => {
                    eprintln!("[prism-ui] {}", e);
                    exit(1);
                }
            },
            None => {}
        }
    }

    let mut next_toast_id: u32 = 1;
    loop {
        // 1. Commands
        for cmd in cmd_rx.try_iter() {
            handle_command(cmd, &conn, &globals, &mut overlays, &mut edit, &mut res, &waker, &mut next_toast_id);
        }

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
        if edit.as_ref().map(|e| e.finished()).unwrap_or(false) {
            if let Some(e) = edit.take() {
                e.teardown();
            }
        }
        if !daemon && overlays.is_empty() && desktop.is_none() && edit.is_none() {
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
    }
    for o in overlays.drain(..) {
        o.teardown();
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
    res: &mut Resources,
    waker: &Waker,
    next_toast_id: &mut u32,
) {
    use prism_common::{NotifOverlayMessage, UiCommand};
    let open_modal = |name: &str, overlays: &mut Vec<Overlay>, res: &mut Resources| {
        // Close any other open modal (it plays its exit transition), then open this one.
        for o in overlays.iter_mut().filter(|o| o.is_modal() && !o.closing()) {
            o.close();
        }
        if let Some(mode) = mode_from_name(name) {
            match Overlay::open(conn, g, mode, 0, res, waker) {
                Ok(o) => overlays.push(o),
                Err(e) => eprintln!("[prism-ui] cannot open {}: {}", name, e),
            }
        }
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
                None => match Overlay::open(conn, g, UiMode::Osd { kind, value: v }, 0, res, waker) {
                    Ok(o) => overlays.push(o),
                    Err(e) => eprintln!("[prism-ui] cannot open OSD: {}", e),
                },
            }
        }
        UiCommand::Notify(msg) => {
            let existing = overlays.iter().find(|o| o.name() == "toast" && !o.closing()).and_then(|o| o.notif_tx.clone());
            match (existing, msg) {
                (Some(tx), m) => {
                    let _ = tx.send(m);
                    waker.wake();
                }
                (None, NotifOverlayMessage::Add { id, app_name, summary, body, .. }) => {
                    *next_toast_id = id;
                    match Overlay::open(conn, g, UiMode::Notification { summary, body, app_name }, id, res, waker) {
                        Ok(o) => overlays.push(o),
                        Err(e) => eprintln!("[prism-ui] cannot open toast: {}", e),
                    }
                }
                (None, NotifOverlayMessage::Dismiss { .. }) => {}
            }
        }
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

const OSD_VISIBLE_MS: u64 = 1800;


/// Hold an exclusive flock on `$XDG_RUNTIME_DIR/<name>` for the life of the process.
/// Returns false if another process already holds it.
fn acquire_instance_lock(name: &str) -> bool {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = Path::new(&dir).join(name);
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


