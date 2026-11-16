use std::env;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::process::exit;

mod auth;
mod cairo_backend;
mod greetd;

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

#[derive(PartialEq, Clone, Copy, Debug)]
enum PromptMode {
    Password,
    Username,
}

#[derive(Clone, PartialEq)]
enum SurfaceKind {
    SessionLock(ExtSessionLockSurfaceV1),
    Layer(ZwlrLayerSurfaceV1),
    Xdg(XdgToplevel, XdgSurface),
}

struct SurfaceState {
    surface: wl_surface::WlSurface,
    kind: SurfaceKind,
    width: u32,
    height: u32,
    current_buffer: Option<wl_buffer::WlBuffer>,
    prev_buffer: Option<wl_buffer::WlBuffer>,
}

impl Drop for SurfaceState {
    fn drop(&mut self) {
        if let Some(b) = self.current_buffer.take() {
            b.destroy();
        }
        if let Some(b) = self.prev_buffer.take() {
            b.destroy();
        }
    }
}

struct LockState {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    session_lock: Option<ExtSessionLockV1>,
    layer_shell: Option<ZwlrLayerShellV1>,
    xdg_wm_base: Option<XdgWmBase>,
    outputs: Vec<wl_output::WlOutput>,
    surfaces: Vec<SurfaceState>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    user: String,
    password_buffer: String,
    status_message: Option<String>,
    shift_pressed: bool,
    caps_lock: bool,
    locked: bool,
    unlocked: bool,
    is_greeter: bool,
    greeter_cfg: greetd::GreeterConfig,
    active_prompt: PromptMode,
}

impl Dispatch<wl_registry::WlRegistry, ()> for LockState {
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
                state.seat = Some(seat);
            } else if interface == "ext_session_lock_manager_v1" {
                eprintln!("[prism-lock] Bound ext_session_lock_manager_v1");
                state.lock_manager = Some(registry.bind::<ExtSessionLockManagerV1, _, _>(name, 1, qh, ()));
            } else if interface == "zwlr_layer_shell_v1" {
                eprintln!("[prism-lock] Bound zwlr_layer_shell_v1");
                state.layer_shell = Some(registry.bind::<ZwlrLayerShellV1, _, _>(name, 1, qh, ()));
            } else if interface == "xdg_wm_base" {
                eprintln!("[prism-lock] Bound xdg_wm_base");
                state.xdg_wm_base = Some(registry.bind::<XdgWmBase, _, _>(name, 1, qh, ()));
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for LockState {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: wayland_protocols::xdg::shell::client::xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for LockState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: wayland_protocols::xdg::shell::client::xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.render_all(qh);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for LockState {
    fn event(
        state: &mut Self,
        toplevel: &XdgToplevel,
        event: wayland_protocols::xdg::shell::client::xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wayland_protocols::xdg::shell::client::xdg_toplevel::Event::Configure { width, height, .. } => {
                let w = if width > 0 { width as u32 } else { 1920 };
                let h = if height > 0 { height as u32 } else { 1080 };
                if let Some(s) = state.surfaces.iter_mut().find(|s| match &s.kind {
                    SurfaceKind::Xdg(tl, _) => tl == toplevel,
                    _ => false,
                }) {
                    s.width = w;
                    s.height = h;
                }
                state.render_all(qh);
            }
            wayland_protocols::xdg::shell::client::xdg_toplevel::Event::Close => {
                if state.is_greeter {
                    state.unlocked = true;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for LockState {
    fn event(
        _: &mut Self,
        _: &ExtSessionLockManagerV1,
        _: <ExtSessionLockManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<ZwlrLayerShellV1, ()> for LockState {
    fn event(
        _: &mut Self,
        _: &ZwlrLayerShellV1,
        _: <ZwlrLayerShellV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<ExtSessionLockV1, ()> for LockState {
    fn event(
        state: &mut Self,
        session_lock: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                eprintln!("[prism-lock] Session lock confirmed by compositor!");
                state.locked = true;

                if let Some(comp) = &state.compositor {
                    for output in &state.outputs {
                        let surface = comp.create_surface(qh, ());
                        let lock_surface = session_lock.get_lock_surface(&surface, output, qh, ());
                        state.surfaces.push(SurfaceState {
                            surface,
                            kind: SurfaceKind::SessionLock(lock_surface),
                            width: 0,
                            height: 0,
                            current_buffer: None,
                            prev_buffer: None,
                        });
                    }
                }
            }
            ext_session_lock_v1::Event::Finished => {
                eprintln!("[prism-lock] Session lock destroyed by compositor.");
                state.unlocked = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for LockState {
    fn event(
        state: &mut Self,
        lock_surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_surface_v1::Event::Configure { serial, width, height } => {
                lock_surface.ack_configure(serial);
                let w = width;
                let h = height;

                if let Some(s) = state.surfaces.iter_mut().find(|s| match &s.kind {
                    SurfaceKind::SessionLock(ls) => ls == lock_surface,
                    _ => false,
                }) {
                    s.width = w;
                    s.height = h;
                }

                state.render_all(qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for LockState {
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
                let w = if width > 0 { width } else { 1920 };
                let h = if height > 0 { height } else { 1080 };

                if let Some(s) = state.surfaces.iter_mut().find(|s| match &s.kind {
                    SurfaceKind::Layer(ls) => ls == layer_surface,
                    _ => false,
                }) {
                    s.width = w;
                    s.height = h;
                }

                state.render_all(qh);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                state.unlocked = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for LockState {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: <wl_compositor::WlCompositor as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_surface::WlSurface, ()> for LockState {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: <wl_surface::WlSurface as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for LockState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: <wl_shm::WlShm as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for LockState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: <wl_shm_pool::WlShmPool as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for LockState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: <wl_buffer::WlBuffer as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_output::WlOutput, ()> for LockState {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: <wl_output::WlOutput as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for LockState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let WEnum::Value(caps) = capabilities {
                if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                    eprintln!("[prism-lock] Seat announced Keyboard capability, binding wl_keyboard...");
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                }
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for LockState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => {
                eprintln!("[prism-lock] Keyboard focus gained!");
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_locked, .. } => {
                state.shift_pressed = (mods_depressed & 1) != 0;
                state.caps_lock = (mods_locked & 2) != 0;
            }
            wl_keyboard::Event::Key { key, state: key_state, .. } => {
                if key == 42 || key == 54 {
                    // KEY_LEFTSHIFT / KEY_RIGHTSHIFT
                    state.shift_pressed = matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                    return;
                }
                if key == 58 && matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed)) {
                    // KEY_CAPSLOCK toggle
                    state.caps_lock = !state.caps_lock;
                    state.render_all(qh);
                    return;
                }

                if let WEnum::Value(wl_keyboard::KeyState::Pressed) = key_state {
                    match key {
                        28 | 96 => {
                            // KEY_ENTER / KEY_KPENTER: Submit authentication
                            if state.is_greeter {
                                if state.active_prompt == PromptMode::Username {
                                    let entered = std::mem::take(&mut state.password_buffer);
                                    if !entered.trim().is_empty() {
                                        state.user = entered.trim().to_string();
                                    }
                                    state.active_prompt = PromptMode::Password;
                                    state.status_message = None;
                                    state.render_all(qh);
                                } else {
                                    let sock_path = env::var("GREETD_SOCK").unwrap_or_else(|_| "/var/run/greetd.sock".to_string());
                                    let password = std::mem::take(&mut state.password_buffer);
                                    let session_cmd = vec![state.greeter_cfg.default_session.clone()];
                                    match greetd::authenticate_and_start(&sock_path, &state.user, &password, &session_cmd) {
                                        Ok(()) => {
                                            eprintln!("[prism-greet] Login successful! Starting session {:?}...", session_cmd);
                                            if state.greeter_cfg.remember_last_user {
                                                greetd::save_last_user(&state.user, &session_cmd);
                                            }
                                            run_greeter_exit();
                                            exit(0);
                                        }
                                        Err(err) => {
                                            state.status_message = Some(format!("Error: {}", err));
                                            state.password_buffer.clear();
                                            state.render_all(qh);
                                        }
                                    }
                                }
                            } else {
                                let password = std::mem::take(&mut state.password_buffer);
                                if auth::authenticate(&state.user, &password) {
                                    eprintln!("[prism-lock] Authentication successful! Unlocking session...");
                                    if let Some(session_lock) = &state.session_lock {
                                        session_lock.unlock_and_destroy();
                                    }
                                    state.unlocked = true;
                                } else {
                                    state.status_message = Some("Authentication failed. Try again.".to_string());
                                    state.password_buffer.clear();
                                    state.render_all(qh);
                                }
                            }
                        }
                        14 => {
                            // KEY_BACKSPACE: Remove last character
                            state.password_buffer.pop();
                            state.status_message = None;
                            state.render_all(qh);
                        }
                        1 => {
                            // KEY_ESC: Clear password buffer
                            state.password_buffer.clear();
                            state.status_message = None;
                            if state.is_greeter {
                                state.active_prompt = PromptMode::Username;
                            }
                            state.render_all(qh);
                        }
                        code => {
                            if let Some(ch) = evdev_to_char(code, state.shift_pressed, state.caps_lock) {
                                state.password_buffer.push(ch);
                                state.status_message = None;
                                state.render_all(qh);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl LockState {
    fn render_all(&mut self, qh: &QueueHandle<Self>) {
        let shm = match &self.shm {
            Some(s) => s.clone(),
            None => return,
        };

        let targets: Vec<(usize, i32, i32)> = self.surfaces
            .iter()
            .enumerate()
            .filter(|(_, s)| s.width > 0 && s.height > 0)
            .map(|(i, s)| (i, s.width as i32, s.height as i32))
            .collect();

        for (idx, w, h) in targets {
            if let Some(buf) = create_rendered_buffer(&shm, w, h, self, qh) {
                self.surfaces[idx].surface.attach(Some(&buf), 0, 0);
                self.surfaces[idx].surface.damage_buffer(0, 0, w, h);
                self.surfaces[idx].surface.commit();

                if let Some(old) = self.surfaces[idx].prev_buffer.take() {
                    old.destroy();
                }
                self.surfaces[idx].prev_buffer = self.surfaces[idx].current_buffer.take();
                self.surfaces[idx].current_buffer = Some(buf);
            }
        }
    }
}

fn render_lock_frame_cairo(
    cr: *mut cairo_backend::Cairo,
    width: i32,
    height: i32,
    state: &LockState,
) {
    // 1. Draw static wallpaper ~/.wa.jpg + dark overlay
    cairo_backend::draw_wallpaper_background(cr, width, height);

    let (time_str, date_str) = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let mut time_buf = [0u8; 16];
        let mut date_buf = [0u8; 64];
        let time_fmt = std::ffi::CString::new("%H:%M").unwrap();
        let date_fmt = std::ffi::CString::new("%A, %B %d").unwrap();
        libc::strftime(time_buf.as_mut_ptr() as *mut libc::c_char, time_buf.len(), time_fmt.as_ptr(), &tm);
        libc::strftime(date_buf.as_mut_ptr() as *mut libc::c_char, date_buf.len(), date_fmt.as_ptr(), &tm);
        (
            std::ffi::CStr::from_ptr(time_buf.as_ptr() as *const libc::c_char).to_string_lossy().into_owned(),
            std::ffi::CStr::from_ptr(date_buf.as_ptr() as *const libc::c_char).to_string_lossy().into_owned(),
        )
    };

    let theme = prism_common::Theme::load();
    let bg_color = cairo_backend::hex_to_rgba(&theme.background, 0.85);
    let border_color = cairo_backend::hex_to_rgba(&theme.outline_variant, 0.45);
    let primary_color = cairo_backend::hex_to_rgba(&theme.primary, 1.0);
    let on_surface = cairo_backend::hex_to_rgba(&theme.on_surface, 1.0);
    let sub_color = cairo_backend::hex_to_rgba(&theme.on_surface_variant, 0.80);

    // Large Digital Clock above card in upper area
    let clock_y = ((height as f64) * 0.18).max(40.0);
    cairo_backend::draw_pango_text(cr, (width as f64 / 2.0) - 100.0, clock_y, &time_str, "Inter, Sans Bold 64", primary_color, None);
    cairo_backend::draw_pango_text(cr, (width as f64 / 2.0) - 100.0, clock_y + 80.0, &date_str, "Inter, Sans 15", sub_color, None);

    // Centered Auth Card
    let card_w = 420.0_f64.min(width as f64 - 40.0);
    let card_h = 240.0;
    let card_x = ((width as f64 - card_w) / 2.0).round();
    let card_y = clock_y + 140.0;

    cairo_backend::fill_rounded_rect(cr, card_x, card_y, card_w, card_h, 20.0, bg_color);
    cairo_backend::stroke_rounded_rect(cr, card_x, card_y, card_w, card_h, 20.0, border_color, 1.0);

    // User Avatar circle
    let avatar_cx = card_x + card_w / 2.0;
    let avatar_cy = card_y + 45.0;
    let avatar_r = 26.0;
    let avatar_bg = cairo_backend::hex_to_rgba(&theme.primary_container, 0.90);
    cairo_backend::fill_circle(cr, avatar_cx, avatar_cy, avatar_r, avatar_bg);
    cairo_backend::stroke_circle(cr, avatar_cx, avatar_cy, avatar_r, primary_color, 1.5);

    let initial = state.user.chars().next().unwrap_or('U').to_uppercase().to_string();
    cairo_backend::draw_pango_text(cr, avatar_cx - 7.0, avatar_cy - 12.0, &initial, "Inter, Sans Bold 18", primary_color, None);

    // Username label
    let user_label = if state.is_greeter && state.active_prompt == PromptMode::Username {
        "Select User".to_string()
    } else {
        state.user.clone()
    };
    cairo_backend::draw_pango_text(cr, card_x + 30.0, card_y + 80.0, &user_label, "Inter, Sans Bold 14", on_surface, Some((card_w - 60.0) as i32));

    // Input pill
    let input_w = card_w - 60.0;
    let input_h = 44.0;
    let input_x = card_x + 30.0;
    let input_y = card_y + 110.0;
    let input_bg = cairo_backend::hex_to_rgba(&theme.surface_variant, 0.45);
    let input_border = if state.password_buffer.is_empty() {
        border_color
    } else {
        primary_color
    };
    cairo_backend::fill_rounded_rect(cr, input_x, input_y, input_w, input_h, 12.0, input_bg);
    cairo_backend::stroke_rounded_rect(cr, input_x, input_y, input_w, input_h, 12.0, input_border, 1.5);

    if state.password_buffer.is_empty() {
        let placeholder = if state.is_greeter && state.active_prompt == PromptMode::Username {
            "Type username…"
        } else {
            "Type password…"
        };
        cairo_backend::draw_pango_text(cr, input_x + 16.0, input_y + 12.0, placeholder, "Inter, Sans 13", sub_color, None);
    } else {
        let bullets: String = "● ".repeat(state.password_buffer.len().min(24));
        cairo_backend::draw_pango_text(cr, input_x + 16.0, input_y + 12.0, &bullets, "Inter, Sans Bold 14", primary_color, None);
    }

    // Status or Hint
    if let Some(err) = &state.status_message {
        let err_color = (0.95, 0.35, 0.35, 1.0);
        cairo_backend::draw_pango_text(cr, card_x + 30.0, card_y + 168.0, err, "Inter, Sans Bold 11", err_color, Some((card_w - 60.0) as i32));
    } else {
        let hint = "Press Enter to unlock | Esc to clear";
        cairo_backend::draw_pango_text(cr, card_x + 30.0, card_y + 168.0, hint, "Inter, Sans 11", sub_color, Some((card_w - 60.0) as i32));
    }
}

fn create_rendered_buffer(
    shm: &wl_shm::WlShm,
    width: i32,
    height: i32,
    state: &LockState,
    qh: &QueueHandle<LockState>,
) -> Option<wl_buffer::WlBuffer> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let stride = width * 4;
    let size = (stride * height) as usize;

    unsafe {
        let fd = libc::memfd_create(b"prism-lock-shm\0".as_ptr() as *const libc::c_char, libc::MFD_CLOEXEC);
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

        let cairo_surface = cairo_backend::cairo_image_surface_create_for_data(
            ptr as *mut u8,
            cairo_backend::CAIRO_FORMAT_ARGB32,
            width,
            height,
            stride,
        );
        let cr = cairo_backend::cairo_create(cairo_surface);

        render_lock_frame_cairo(cr, width, height, state);

        cairo_backend::cairo_destroy(cr);
        cairo_backend::cairo_surface_destroy(cairo_surface);

        libc::munmap(ptr, size);

        let file = File::from_raw_fd(fd);
        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        Some(buffer)
    }
}

fn run_greeter_exit() {
    if let Ok(raw) = env::var("GREETER_EXIT_CMD") {
        if let Ok(cmd_args) = serde_json::from_str::<Vec<String>>(&raw) {
            if !cmd_args.is_empty() {
                let _ = std::process::Command::new(&cmd_args[0]).args(&cmd_args[1..]).output();
                return;
            }
        }
    }

    // In Cage kiosk mode, exiting the process terminates Cage cleanly.
    // If running in a nested compositor session, cleanly signal the respective compositor:
    if env::var("MANGO_INSTANCE_SIGNATURE").is_ok() {
        let _ = std::process::Command::new("mmsg").arg("quit").output();
    } else if env::var("NIRI_SOCKET").is_ok() {
        let _ = std::process::Command::new("niri").args(["msg", "action", "quit", "--skip-confirmation"]).output();
    } else if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        let _ = std::process::Command::new("hyprctl").args(["dispatch", "exit"]).output();
    } else if env::var("SWAYSOCK").is_ok() {
        let _ = std::process::Command::new("swaymsg").arg("exit").output();
    }
}

fn evdev_to_char(code: u32, shift: bool, caps: bool) -> Option<char> {
    let letter_upper = shift ^ caps;
    match code {
        // Numbers row
        2 => Some(if shift { '!' } else { '1' }),
        3 => Some(if shift { '@' } else { '2' }),
        4 => Some(if shift { '#' } else { '3' }),
        5 => Some(if shift { '$' } else { '4' }),
        6 => Some(if shift { '%' } else { '5' }),
        7 => Some(if shift { '^' } else { '6' }),
        8 => Some(if shift { '&' } else { '7' }),
        9 => Some(if shift { '*' } else { '8' }),
        10 => Some(if shift { '(' } else { '9' }),
        11 => Some(if shift { ')' } else { '0' }),
        12 => Some(if shift { '_' } else { '-' }),
        13 => Some(if shift { '+' } else { '=' }),

        // QWERTY top row
        16 => Some(if letter_upper { 'Q' } else { 'q' }),
        17 => Some(if letter_upper { 'W' } else { 'w' }),
        18 => Some(if letter_upper { 'E' } else { 'e' }),
        19 => Some(if letter_upper { 'R' } else { 'r' }),
        20 => Some(if letter_upper { 'T' } else { 't' }),
        21 => Some(if letter_upper { 'Y' } else { 'y' }),
        22 => Some(if letter_upper { 'U' } else { 'u' }),
        23 => Some(if letter_upper { 'I' } else { 'i' }),
        24 => Some(if letter_upper { 'O' } else { 'o' }),
        25 => Some(if letter_upper { 'P' } else { 'p' }),
        26 => Some(if shift { '{' } else { '[' }),
        27 => Some(if shift { '}' } else { ']' }),

        // Home row
        30 => Some(if letter_upper { 'A' } else { 'a' }),
        31 => Some(if letter_upper { 'S' } else { 's' }),
        32 => Some(if letter_upper { 'D' } else { 'd' }),
        33 => Some(if letter_upper { 'F' } else { 'f' }),
        34 => Some(if letter_upper { 'G' } else { 'g' }),
        35 => Some(if letter_upper { 'H' } else { 'h' }),
        36 => Some(if letter_upper { 'J' } else { 'j' }),
        37 => Some(if letter_upper { 'K' } else { 'k' }),
        38 => Some(if letter_upper { 'L' } else { 'l' }),
        39 => Some(if shift { ':' } else { ';' }),
        40 => Some(if shift { '"' } else { '\'' }),
        41 => Some(if shift { '~' } else { '`' }),
        43 => Some(if shift { '|' } else { '\\' }),

        // Bottom row
        44 => Some(if letter_upper { 'Z' } else { 'z' }),
        45 => Some(if letter_upper { 'X' } else { 'x' }),
        46 => Some(if letter_upper { 'C' } else { 'c' }),
        47 => Some(if letter_upper { 'V' } else { 'v' }),
        48 => Some(if letter_upper { 'B' } else { 'b' }),
        49 => Some(if letter_upper { 'N' } else { 'n' }),
        50 => Some(if letter_upper { 'M' } else { 'm' }),
        51 => Some(if shift { '<' } else { ',' }),
        52 => Some(if shift { '>' } else { '.' }),
        53 => Some(if shift { '?' } else { '/' }),
        57 => Some(' '),

        // Keypad
        55 => Some('*'),
        71 => Some('7'),
        72 => Some('8'),
        73 => Some('9'),
        74 => Some('-'),
        75 => Some('4'),
        76 => Some('5'),
        77 => Some('6'),
        78 => Some('+'),
        79 => Some('1'),
        80 => Some('2'),
        81 => Some('3'),
        82 => Some('0'),
        83 => Some('.'),
        98 => Some('/'),

        _ => None,
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let is_greeter = args.iter().any(|a| a == "--greeter") || env::var("GREETD_SOCK").is_ok();

    let greeter_cfg = if is_greeter {
        greetd::load_greeter_config()
    } else {
        greetd::GreeterConfig::default()
    };

    let user = if is_greeter {
        if greeter_cfg.remember_last_user {
            greetd::load_last_user().unwrap_or_else(|| env::var("USER").unwrap_or_default())
        } else {
            String::new()
        }
    } else {
        env::var("USER").unwrap_or_else(|_| "user".to_string())
    };

    let active_prompt = if user.is_empty() {
        PromptMode::Username
    } else {
        PromptMode::Password
    };

    if env::var("WAYLAND_DISPLAY").is_err() && env::var("WAYLAND_SOCKET").is_err() {
        eprintln!("Error: WAYLAND_DISPLAY is not set. prism-lock requires an active Wayland compositor.");
        exit(1);
    }

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to connect to Wayland compositor: {}", e);
            exit(1);
        }
    };
    let display = conn.display();
    let mut event_queue: EventQueue<LockState> = conn.new_event_queue();
    let qh = event_queue.handle();

    let mut state = LockState {
        compositor: None,
        shm: None,
        lock_manager: None,
        session_lock: None,
        layer_shell: None,
        xdg_wm_base: None,
        outputs: Vec::new(),
        surfaces: Vec::new(),
        seat: None,
        keyboard: None,
        user,
        password_buffer: String::new(),
        status_message: None,
        shift_pressed: false,
        caps_lock: false,
        locked: false,
        unlocked: false,
        is_greeter,
        greeter_cfg,
        active_prompt,
    };

    let _registry = display.get_registry(&qh, ());

    // Initial registry sync
    event_queue.roundtrip(&mut state).expect("Registry roundtrip failed");
    event_queue.roundtrip(&mut state).expect("Globals roundtrip failed");

    if is_greeter {
        if let Some(layer_shell) = &state.layer_shell {
            eprintln!("[prism-lock] Greeter mode: Using zwlr_layer_shell_v1 overlay...");
            let comp = state.compositor.clone().expect("Missing wl_compositor");
            let surface = comp.create_surface(&qh, ());
            let layer_surface = layer_shell.get_layer_surface(
                &surface,
                None,
                Layer::Overlay,
                "prism-greeter".to_string(),
                &qh,
                (),
            );
            layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer_surface.set_exclusive_zone(-1);
            layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            surface.commit();

            state.surfaces.push(SurfaceState {
                surface,
                kind: SurfaceKind::Layer(layer_surface),
                width: 1920,
                height: 1080,
                current_buffer: None,
                prev_buffer: None,
            });
        } else if let Some(wm_base) = &state.xdg_wm_base {
            eprintln!("[prism-lock] Greeter mode: Creating fullscreen XDG window for Cage kiosk...");
            let wm_base = wm_base.clone();
            let comp = state.compositor.clone().expect("Missing wl_compositor");
            let surface = comp.create_surface(&qh, ());
            let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
            let toplevel = xdg_surface.get_toplevel(&qh, ());
            toplevel.set_title("prism-greeter".to_string());
            toplevel.set_app_id("prism-greeter".to_string());
            toplevel.set_fullscreen(None);
            surface.commit();

            state.surfaces.push(SurfaceState {
                surface,
                kind: SurfaceKind::Xdg(toplevel, xdg_surface),
                width: 1920,
                height: 1080,
                current_buffer: None,
                prev_buffer: None,
            });
        } else {
            eprintln!("Error: Greeter requires zwlr_layer_shell_v1 or xdg_wm_base.");
            exit(2);
        }
    } else if let Some(manager) = &state.lock_manager {
        eprintln!("[prism-lock] Requesting session lock via ext_session_lock_manager_v1...");
        let session_lock = manager.lock(&qh, ());
        state.session_lock = Some(session_lock);
    } else if let Some(layer_shell) = &state.layer_shell {
        eprintln!("[prism-lock] No session lock manager found; using zwlr_layer_shell_v1 (kiosk mode)...");
        let comp = state.compositor.clone().expect("Missing wl_compositor");
        let surface = comp.create_surface(&qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Overlay,
            "prism-greeter".to_string(),
            &qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();

        state.surfaces.push(SurfaceState {
            surface,
            kind: SurfaceKind::Layer(layer_surface),
            width: 1920,
            height: 1080,
            current_buffer: None,
            prev_buffer: None,
        });
    } else if let Some(wm_base) = &state.xdg_wm_base {
        eprintln!("[prism-lock] Using xdg_wm_base fallback window...");
        let comp = state.compositor.clone().expect("Missing wl_compositor");
        let surface = comp.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title("prism-lock".to_string());
        toplevel.set_app_id("prism-lock".to_string());
        toplevel.set_fullscreen(None);
        surface.commit();

        state.surfaces.push(SurfaceState {
            surface,
            kind: SurfaceKind::Xdg(toplevel, xdg_surface),
            width: 1920,
            height: 1080,
            current_buffer: None,
            prev_buffer: None,
        });
    } else {
        eprintln!("Error: Compositor does not support ext_session_lock_manager_v1, zwlr_layer_shell_v1, or xdg_wm_base.");
        exit(2);
    }

    // Main event dispatch loop
    while !state.unlocked {
        event_queue.blocking_dispatch(&mut state).expect("Wayland event dispatch error");
    }

    let _ = conn.flush();
    eprintln!("[prism-lock] Lock/greeter session finished cleanly. Exiting.");
}
