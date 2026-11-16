//! prism-lock: Prism's session lock screen and greetd greeter (port of QML Lockscreen.qml).
//!
//!   prism-lock               lock the session (ext-session-lock-v1)
//!   prism-lock --greeter     greetd greeter (fullscreen window, meant for `cage -s --`); without
//!                            greetd (no GREETD_SOCK) it is a preview window that Esc closes
//!   prism-lock --check-auth  check your password against PAM from a terminal (no Wayland)
//!
//! Safety rules for the lock (a broken lock client leaves the session locked by design):
//! * lock surfaces are created for every output right after the lock request, as the protocol
//!   requires, never only after `locked`;
//! * PAM runs on a worker thread so the Wayland connection keeps being serviced;
//! * nothing on the event path panics; errors exit with a non-zero code and prismd starts a new
//!   lock client, which takes over the still-locked session.
//!
//! Exit codes: 0 unlocked / logged in, 1 error, 3 compositor cannot lock (no retry).

mod auth;
#[allow(dead_code)]
mod cairo_backend;
mod greetd;
#[allow(dead_code)]
mod svg;

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::process::exit;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use cairo_backend::{self as cb, Cairo, CairoSurface};
use prism_common::{Config, Theme};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xkbcommon::xkb;

type Rgba = (f64, f64, f64, f64);

const DESIGN_H: f64 = 1440.0;
const FADE_IN_MS: f64 = 600.0;
const FADE_OUT_MS: f64 = 350.0;
const BOX_MS: f64 = 300.0;
const SHAKE_MS: f64 = 300.0;
const POP_MS: f64 = 120.0;
const INACTIVITY: Duration = Duration::from_secs(15);
const MASK_FONT: &str = "CaskaydiaCove Nerd Font";
const MASK_GLYPHS: [char; 4] = ['\u{f0d7}', '\u{f0d8}', '\u{f0d9}', '\u{f0da}'];
const LOCK_ICON: &str = "M7 10h10a3 3 0 0 1 3 3v6a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3v-6a3 3 0 0 1 3-3zM7 10V6a5 5 0 0 1 10 0v4";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Lock,
    Greeter,
}

fn out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn in_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

fn out_back(t: f64) -> f64 {
    let (c1, t) = (1.70158, t.clamp(0.0, 1.0));
    1.0 + (c1 + 1.0) * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

// ── Look ─────────────────────────────────────────────────────────────────

/// Colours and settings from the shared Prism config and matugen palette.
struct Look {
    fg: Rgba,
    primary: Rgba,
    error: Rgba,
    outline: Rgba,
    background: Rgba,
    right: bool,
    session_controls: bool,
    font: String,
    wallpaper: Option<String>,
}

impl Look {
    fn load(mode: Mode) -> Self {
        let cfg = Config::load();
        // The greeter user has no Prism config: use the palette synced into greetd's state dir.
        let synced = greetd::get_state_dir().join("colors.json");
        let theme = match mode {
            Mode::Greeter => Theme::load_from_path(&synced).unwrap_or_else(|_| Theme::load()),
            Mode::Lock => Theme::load(),
        };
        // QML Lockscreen.qml sets font.family "Inter" on every text.
        let font = "Inter".to_string();
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates: Vec<String> = match mode {
            Mode::Lock => vec![format!("{home}/.wa.jpg")],
            Mode::Greeter => vec![
                "/var/lib/greetd/prism/wallpaper.jpg".into(),
                "/var/lib/greetd/quickshell-greeter/wallpaper.jpg".into(),
            ],
        };
        let mut right = cfg.lockscreen_alignment == "right";
        if mode == Mode::Greeter {
            right = greetd::load_greeter_config().lockscreen_alignment == "right";
        }
        Look {
            fg: cb::hex_to_rgba(&theme.on_primary_container, 1.0),
            primary: cb::hex_to_rgba(&theme.primary, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            outline: cb::hex_to_rgba(&theme.outline_variant, 0.4),
            background: cb::hex_to_rgba(&theme.background, 1.0),
            right,
            session_controls: cfg.show_lockscreen_session_controls,
            font,
            wallpaper: candidates.into_iter().find(|p| std::path::Path::new(p).exists()),
        }
    }

    fn f(&self, weight: &str, px: f64) -> String {
        format!("{} {} {}px", self.font, weight, px.round().max(1.0))
    }
}

// ── Surfaces ─────────────────────────────────────────────────────────────

enum Role {
    Lock(ExtSessionLockSurfaceV1),
    Xdg(XdgSurface, XdgToplevel),
    Layer(ZwlrLayerSurfaceV1),
}

struct Buf {
    buffer: wl_buffer::WlBuffer,
    ptr: *mut u8,
    len: usize,
    busy: bool,
}

impl Drop for Buf {
    fn drop(&mut self) {
        self.buffer.destroy();
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

struct Surf {
    output: Option<u32>,
    surface: wl_surface::WlSurface,
    role: Role,
    size: (i32, i32),
    configured: bool,
    bufs: Vec<Buf>,
    /// Wallpaper + overlays rendered once per size.
    background: Option<(*mut CairoSurface, i32, i32)>,
    frame_cb: Option<wl_callback::WlCallback>,
    /// Hit rectangles from the last draw: (what, x, y, w, h).
    hits: Vec<(Hit, f64, f64, f64, f64)>,
}

impl Drop for Surf {
    fn drop(&mut self) {
        if let Some((bg, _, _)) = self.background.take() {
            unsafe { cb::cairo_surface_destroy(bg) };
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    User,
    Field,
    Go,
    Shutdown,
    Reboot,
    Session,
}

// ── State ────────────────────────────────────────────────────────────────

enum AuthResult {
    Ok,
    Failed(String),
}

struct App {
    mode: Mode,
    look: Look,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    wm_base: Option<XdgWmBase>,
    layer_shell: Option<ZwlrLayerShellV1>,
    outputs: Vec<(u32, wl_output::WlOutput)>,
    session_lock: Option<ExtSessionLockV1>,
    surfaces: Vec<Surf>,
    xkb_ctx: xkb::Context,
    xkb: Option<xkb::State>,
    repeat_rate: i32,
    repeat_delay: i32,
    repeat: Option<(u32, Instant)>,
    pointer_on: Option<wl_surface::WlSurface>,
    pointer_pos: (f64, f64),
    hover: Option<Hit>,

    user: String,
    /// Greeter: typing the user name (before the password).
    entering_user: bool,
    sessions: Vec<greetd::Session>,
    session_idx: usize,
    text: String,
    masks: Vec<(usize, Instant)>,
    password_mode: bool,
    box_anim: (f64, f64, Instant),
    last_input: Instant,
    checking: bool,
    failed_at: Option<Instant>,
    status: String,
    status_error: bool,
    opened: Instant,
    closing: Option<Instant>,
    done: bool,
    /// Greeter started without greetd (no GREETD_SOCK): a closable preview window.
    preview: bool,
    /// The Nerd Font with the mask glyphs is installed for this user (not for `greeter`).
    mask_font_ok: bool,
    error: Option<String>,
    auth_tx: Sender<AuthResult>,
    auth_rx: Receiver<AuthResult>,
    wake_fd: i32,
    wake_write: i32,
}

impl App {
    fn scale(&self, h: i32) -> f64 {
        (h as f64 / DESIGN_H).max(0.1)
    }

    fn any_animating(&self) -> bool {
        ms_since(self.opened) < FADE_IN_MS
            || self.closing.is_some()
            || ms_since(self.box_anim.2) < BOX_MS
            || self.failed_at.map(|t| ms_since(t) < SHAKE_MS).unwrap_or(false)
            || self.masks.iter().any(|(_, t)| ms_since(*t) < POP_MS)
            || self.checking
    }

    fn set_password_mode(&mut self, on: bool) {
        if self.password_mode == on {
            return;
        }
        let cur = self.box_value();
        self.password_mode = on;
        self.box_anim = (cur, if on { 1.0 } else { 0.0 }, Instant::now());
        if !on {
            self.clear_text();
        }
    }

    fn box_value(&self) -> f64 {
        let (from, to, start) = self.box_anim;
        from + (to - from) * out_cubic(ms_since(start) / BOX_MS)
    }

    fn clear_text(&mut self) {
        // Overwrite before dropping (best effort; the allocator may have copied it before).
        unsafe {
            for b in self.text.as_bytes_mut() {
                *b = 0;
            }
        }
        self.text.clear();
        self.masks.clear();
    }

    fn push_char(&mut self, c: char) {
        self.text.push(c);
        let prev = self.masks.last().map(|m| m.0);
        let seed = (Instant::now().elapsed().subsec_nanos() as usize ^ (self.text.len() * 2654435761)) % MASK_GLYPHS.len();
        let idx = if Some(seed) == prev { (seed + 1) % MASK_GLYPHS.len() } else { seed };
        self.masks.push((idx, Instant::now()));
    }

    fn submit(&mut self) {
        if self.checking || self.closing.is_some() {
            return;
        }
        if self.mode == Mode::Greeter && self.entering_user {
            let name = std::mem::take(&mut self.text).trim().to_string();
            self.masks.clear();
            if !name.is_empty() {
                self.user = name;
                self.entering_user = false;
                self.status.clear();
            }
            return;
        }
        if self.text.is_empty() {
            return;
        }
        if self.preview {
            self.clear_text();
            self.status = "Preview only: greetd is not running here, nothing was sent.".into();
            self.status_error = true;
            self.failed_at = Some(Instant::now());
            return;
        }
        self.checking = true;
        self.status = String::from("Checking…");
        self.status_error = false;
        let password = std::mem::take(&mut self.text);
        self.masks.clear();
        let user = self.user.clone();
        let tx = self.auth_tx.clone();
        let wake = self.wake_write;
        let mode = self.mode;
        let session = self.sessions.get(self.session_idx).map(|s| s.cmd.clone()).unwrap_or_else(|| vec![greetd::detect_default_session()]);
        std::thread::spawn(move || {
            let mut password = password;
            let result = match mode {
                Mode::Lock => {
                    if auth::authenticate(&user, &password) {
                        AuthResult::Ok
                    } else {
                        AuthResult::Failed("Authentication failed".into())
                    }
                }
                Mode::Greeter => {
                    let sock = std::env::var("GREETD_SOCK").unwrap_or_else(|_| "/run/greetd.sock".into());
                    match greetd::authenticate_and_start(&sock, &user, &password, &session) {
                        Ok(()) => {
                            let cfg = greetd::load_greeter_config();
                            if cfg.remember_last_user {
                                greetd::save_last_user(&user, &session);
                            }
                            AuthResult::Ok
                        }
                        Err(e) => AuthResult::Failed(e),
                    }
                }
            };
            unsafe {
                for b in password.as_bytes_mut() {
                    *b = 0;
                }
            }
            let _ = tx.send(result);
            unsafe { libc::write(wake, b"x".as_ptr() as *const libc::c_void, 1) };
        });
    }

    fn take_auth_result(&mut self) {
        while let Ok(r) = self.auth_rx.try_recv() {
            self.checking = false;
            match r {
                AuthResult::Ok => {
                    self.status.clear();
                    self.closing = Some(Instant::now());
                }
                AuthResult::Failed(msg) => {
                    eprintln!("[prism-lock] authentication failed");
                    self.status = msg;
                    self.status_error = true;
                    self.failed_at = Some(Instant::now());
                }
            }
        }
    }

    // ── Input ────────────────────────────────────────────────────────

    fn key(&mut self, raw: u32) {
        if self.checking || self.closing.is_some() {
            return;
        }
        let Some(state) = self.xkb.as_ref() else { return };
        let kc = xkb::Keycode::new(raw + 8);
        let sym = state.key_get_one_sym(kc);
        let text = state.key_get_utf8(kc);
        let ctrl = state.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE);
        self.last_input = Instant::now();
        if self.failed_at.is_some() && !self.checking {
            self.status.clear();
            self.status_error = false;
        }
        match sym.raw() {
            0xff1b if self.preview => {
                self.done = true;
                return;
            }
            0xff1b => {
                // Escape: leave the field (greeter: back to the user name).
                if self.mode == Mode::Greeter && !self.entering_user && self.text.is_empty() {
                    self.entering_user = true;
                    self.text = self.user.clone();
                    self.masks.clear();
                } else {
                    self.set_password_mode(false);
                }
                return;
            }
            0xff0d | 0xff8d => {
                if self.password_mode || self.mode == Mode::Greeter {
                    self.submit();
                } else {
                    self.set_password_mode(true);
                }
                return;
            }
            0xff08 => {
                if ctrl {
                    self.clear_text();
                } else if self.text.pop().is_some() {
                    self.masks.pop();
                }
                return;
            }
            0xff09 if self.mode == Mode::Greeter && !self.sessions.is_empty() => {
                self.session_idx = (self.session_idx + 1) % self.sessions.len();
                return;
            }
            _ => {}
        }
        if !self.password_mode {
            // The first key only opens the password field (as QML).
            self.set_password_mode(true);
            return;
        }
        if ctrl {
            if text == "\u{15}" || sym.raw() == 0x75 {
                self.clear_text();
            }
            return;
        }
        for c in text.chars().filter(|c| !c.is_control()) {
            self.push_char(c);
        }
    }

    fn click(&mut self) {
        if self.checking || self.closing.is_some() {
            return;
        }
        self.last_input = Instant::now();
        match self.hover {
            Some(Hit::Go) => self.submit(),
            Some(Hit::Shutdown) => spawn_detached(&["systemctl", "poweroff"]),
            Some(Hit::Reboot) => spawn_detached(&["systemctl", "reboot"]),
            Some(Hit::Session) if self.mode == Mode::Greeter && !self.sessions.is_empty() => {
                self.session_idx = (self.session_idx + 1) % self.sessions.len();
            }
            _ => self.set_password_mode(true),
        }
    }

    fn update_hover(&mut self) {
        let (x, y) = self.pointer_pos;
        let on = self.pointer_on.clone();
        self.hover = self
            .surfaces
            .iter()
            .find(|s| Some(&s.surface) == on.as_ref())
            .and_then(|s| s.hits.iter().find(|h| x >= h.1 && x < h.1 + h.3 && y >= h.2 && y < h.2 + h.4).map(|h| h.0));
    }

    // ── Surfaces ─────────────────────────────────────────────────────

    fn add_lock_surface(&mut self, name: u32, output: &wl_output::WlOutput, qh: &QueueHandle<Self>) {
        let (Some(lock), Some(comp)) = (&self.session_lock, &self.compositor) else { return };
        if self.surfaces.iter().any(|s| s.output == Some(name)) {
            return;
        }
        let surface = comp.create_surface(qh, ());
        let ls = lock.get_lock_surface(&surface, output, qh, ());
        self.surfaces.push(Surf::new(Some(name), surface, Role::Lock(ls)));
    }

    fn draw_all(&mut self, qh: &QueueHandle<Self>) {
        for i in 0..self.surfaces.len() {
            let s = &self.surfaces[i];
            if s.configured && s.frame_cb.is_none() {
                self.draw(i, qh);
            }
        }
    }

    fn draw(&mut self, i: usize, qh: &QueueHandle<Self>) {
        let Some(shm) = self.shm.clone() else { return };
        let (w, h) = self.surfaces[i].size;
        if w <= 0 || h <= 0 {
            return;
        }
        let s = &mut self.surfaces[i];
        let idx = match s.bufs.iter().position(|b| !b.busy) {
            Some(k) => k,
            None if s.bufs.len() < 3 => match new_buf(&shm, qh, w, h) {
                Some(b) => {
                    s.bufs.push(b);
                    s.bufs.len() - 1
                }
                None => return,
            },
            None => return,
        };
        if !matches!(s.background, Some((_, bw, bh)) if bw == w && bh == h) {
            if let Some((old, _, _)) = s.background.take() {
                unsafe { cb::cairo_surface_destroy(old) };
            }
            s.background = Some((render_background(&self.look, w, h), w, h));
        }
        let bg = s.background.unwrap().0;
        let ptr = s.bufs[idx].ptr;
        s.bufs[idx].busy = true;
        let hits = unsafe {
            let surf = cb::cairo_image_surface_create_for_data(ptr, cb::CAIRO_FORMAT_ARGB32, w, h, w * 4);
            let cr = cb::cairo_create(surf);
            let hits = self.paint(cr, bg, w, h);
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
            hits
        };
        let s = &mut self.surfaces[i];
        s.hits = hits;
        s.surface.attach(Some(&s.bufs[idx].buffer), 0, 0);
        s.surface.damage_buffer(0, 0, w, h);
        s.frame_cb = Some(s.surface.frame(qh, ()));
        s.surface.commit();
    }

    /// Draw one frame; returns hit rectangles.
    unsafe fn paint(&self, cr: *mut Cairo, bg: *mut CairoSurface, w: i32, h: i32) -> Vec<(Hit, f64, f64, f64, f64)> {
        let l = &self.look;
        let sc = self.scale(h);
        let px = |v: f64| (v * sc).round();
        let mut hits = Vec::new();
        let fade = match self.closing {
            Some(t) => 1.0 - in_out_cubic(ms_since(t) / FADE_OUT_MS),
            None => out_cubic(ms_since(self.opened) / FADE_IN_MS),
        };

        // Background colour, wallpaper, then the dark overlays fading in.
        let bgc = l.background;
        cb::cairo_set_source_rgba(cr, bgc.0, bgc.1, bgc.2, 1.0);
        cb::cairo_paint(cr);
        cb::cairo_set_source_surface(cr, bg, 0.0, 0.0);
        cb::cairo_paint(cr);
        // `bg` already has the overlays; while fading, blend the plain wallpaper back in.
        let content = ((fade - 0.25) / 0.75).clamp(0.0, 1.0);
        if content <= 0.0 && self.closing.is_none() {
            return hits;
        }
        cb::cairo_push_group(cr);

        let (w, hf) = (w as f64, h as f64);
        let col_w = px(600.0);
        let margin = px(120.0);
        let x0 = if l.right { w - margin - col_w } else { margin };
        let align = |tw: f64| if l.right { x0 + col_w - tw } else { x0 };
        let fg = l.fg;
        let a = |c: Rgba, v: f64| (c.0, c.1, c.2, c.3 * v);

        // Clock and date.
        let now = local_now();
        let hour = if now.0 % 12 == 0 { 12 } else { now.0 % 12 };
        let clock = format!("{:02}:{:02}", hour, now.1);
        let cf = l.f("Black", px(160.0));
        let (cw, ch) = text_size(cr, &clock, &cf);
        let mut y = px(120.0);
        cb::draw_pango_text(cr, align(cw), y, &clock, &cf, fg, None);
        y += ch + px(15.0);
        let date = date_label();
        let df = l.f("Bold", px(16.0));
        let dw = spaced_width(cr, &date, &df, px(8.0));
        draw_spaced(cr, align(dw), y, &date, &df, a(fg, 0.8), px(8.0));

        // User block, slightly below the centre.
        let block_w = px(400.0);
        let bx = if l.right { x0 + col_w - block_w } else { x0 };
        let balign = |tw: f64| if l.right { bx + block_w - tw } else { bx };
        let label_f = l.f("Bold", px(12.0));
        let user_f = l.f("Black", px(48.0));
        let (_, lh) = text_size(cr, "A", &label_f);
        let (_, uh) = text_size(cr, "A", &user_f);
        let field_h = px(55.0);
        let block_h = lh + px(5.0) + uh + px(15.0) + px(5.0) + field_h;
        let mut by = (hf - block_h) / 2.0 + px(80.0);
        let label = match (self.mode, self.entering_user) {
            (Mode::Greeter, true) => "WHO GOES THERE",
            _ => "CURRENT OPERATIVE",
        };
        let lw = spaced_width(cr, label, &label_f, px(4.0));
        draw_spaced(cr, balign(lw), by, label, &label_f, a(fg, 0.6), px(4.0));
        by += lh + px(5.0);
        let shown_user = if self.mode == Mode::Greeter && self.entering_user {
            if self.text.is_empty() { "USERNAME".to_string() } else { self.text.to_uppercase() }
        } else {
            self.user.to_uppercase()
        };
        let user_hover = self.hover == Some(Hit::User);
        let (uw, _) = text_size(cr, &shown_user, &user_f);
        let ucol = if user_hover { l.primary } else if self.entering_user && self.text.is_empty() { a(fg, 0.35) } else { fg };
        cb::draw_pango_text(cr, balign(uw), by, &shown_user, &user_f, ucol, Some(block_w as i32 + 200));
        if user_hover {
            cb::fill_rounded_rect(cr, balign(uw), by + uh - px(2.0), uw, px(2.0), 0.0, ucol);
        }
        hits.push((Hit::User, balign(uw), by, uw, uh));
        by += uh + px(15.0) + px(5.0);

        // Password field: a 55 px lock badge that widens to 400 px.
        if !(self.mode == Mode::Greeter && self.entering_user) {
            let t = self.box_value();
            let fw = field_h + (block_w - field_h) * t;
            let shake = self.failed_at.map(|f| shake_offset(ms_since(f)) * sc).unwrap_or(0.0);
            let fx = balign(fw) + shake;
            let radius = field_h / 2.0 + (px(8.0) - field_h / 2.0) * t;
            let border = if self.status_error { l.error } else if self.password_mode { l.primary } else { l.outline };
            cb::stroke_rounded_rect(cr, fx + 0.5, by + 0.5, fw - 1.0, field_h - 1.0, radius, border, 1.0);
            hits.push((Hit::Field, fx, by, fw, field_h));
            // Lock icon fades/shrinks away.
            let ia = 0.7 * (1.0 - t);
            if ia > 0.01 {
                let isz = px(32.0) * (1.0 - 0.5 * t);
                svg::draw_path(cr, LOCK_ICON, fx + (fw - isz) / 2.0, by + (field_h - isz) / 2.0, isz, a(fg, ia), Some(2.0));
            }
            if t > 0.01 {
                cb::cairo_save(cr);
                cb::rounded_rect_path(cr, fx, by, fw, field_h, radius);
                cb::cairo_clip(cr);
                let go_f = l.f("Bold", px(14.0));
                let go_w = spaced_width(cr, "GO", &go_f, px(2.0));
                let go_x = fx + fw - px(20.0) - go_w;
                let (_, go_h) = text_size(cr, "GO", &go_f);
                let go_a = if self.text.is_empty() { 0.4 } else { 1.0 } * t;
                draw_spaced(cr, go_x, by + (field_h - go_h) / 2.0, "GO", &go_f, a(fg, go_a), px(2.0));
                hits.push((Hit::Go, go_x - px(10.0), by, go_w + px(30.0), field_h));
                if self.text.is_empty() && !self.checking {
                    let pf = l.f("", px(16.0));
                    let (_, ph) = text_size(cr, "P", &pf);
                    cb::draw_pango_text(cr, fx + px(20.0), by + (field_h - ph) / 2.0, "Password...", &pf, a(fg, 0.4 * t), None);
                }
                // Masked characters pop in.
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, fx + px(20.0), by, (go_x - px(20.0) - (fx + px(20.0))).max(0.0), field_h);
                cb::cairo_clip(cr);
                let mf = if self.mask_font_ok { format!("{MASK_FONT} {}px", px(32.0)) } else { l.f("Black", px(28.0)) };
                let mut mx = fx + px(20.0);
                let total = self.masks.len();
                let (gw, gh) = text_size(cr, if self.mask_font_ok { "\u{f0d7}" } else { "•" }, &mf);
                let room = go_x - px(20.0) - mx;
                let step = gw + px(10.0);
                // Keep the newest glyphs visible.
                let shift = (total as f64 * step - room).max(0.0);
                mx -= shift;
                for (g, born) in &self.masks {
                    let p = (ms_since(*born) / POP_MS).clamp(0.0, 1.0);
                    let s = 0.7 + 0.3 * out_back(p);
                    let glyph = if self.mask_font_ok { MASK_GLYPHS[*g].to_string() } else { "•".to_string() };
                    cb::cairo_save(cr);
                    cb::cairo_translate(cr, mx + gw / 2.0, by + field_h / 2.0);
                    cb::cairo_scale(cr, s, s);
                    cb::draw_pango_text(cr, -gw / 2.0, -gh / 2.0, &glyph, &mf, a(fg, p.min(1.0) * t), None);
                    cb::cairo_restore(cr);
                    mx += step;
                }
                cb::cairo_restore(cr);
                cb::cairo_restore(cr);
            }
            by += field_h;
        } else {
            // Typing the user name: the caret sits after the name.
            by += field_h;
        }

        if self.preview {
            let pf = l.f("Bold", px(14.0));
            let msg = "GREETER PREVIEW  ·  GREETD IS NOT RUNNING  ·  ESC TO CLOSE";
            let pw = spaced_width(cr, msg, &pf, px(2.0));
            draw_spaced(cr, (w - pw) / 2.0, px(40.0), msg, &pf, l.error, px(2.0));
        }

        // Status line.
        if !self.status.is_empty() {
            let sf = l.f("", px(14.0));
            let (sw, _) = text_size(cr, &self.status, &sf);
            let c = if self.status_error { l.error } else { a(fg, 0.7) };
            cb::draw_pango_text(cr, balign(sw), by + px(5.0), &self.status, &sf, c, None);
        }

        // Bottom row: SHUTDOWN  REBOOT  • SESSION
        if l.session_controls || self.mode == Mode::Greeter {
            let bf = l.f("Bold", px(14.0));
            let session_name = match self.mode {
                Mode::Greeter => self.sessions.get(self.session_idx).map(|s| s.name.clone()).unwrap_or_else(greetd::detect_default_session),
                Mode::Lock => session_label(),
            }
            .to_uppercase();
            let items = [(Hit::Shutdown, "SHUTDOWN".to_string()), (Hit::Reboot, "REBOOT".to_string()), (Hit::Session, session_name)];
            let (_, bh) = text_size(cr, "A", &bf);
            let ry = hf - px(120.0) - bh;
            let gap = px(40.0);
            let widths: Vec<f64> = items.iter().map(|(k, t)| spaced_width(cr, t, &bf, px(2.0)) + if *k == Hit::Session { px(21.0) } else { 0.0 }).collect();
            let total: f64 = widths.iter().sum::<f64>() + gap * (items.len() as f64 - 1.0);
            let mut rx = if l.right { x0 + col_w - total } else { x0 };
            let order: Vec<usize> = if l.right { vec![2, 1, 0] } else { vec![0, 1, 2] };
            for k in order {
                let (hit, text) = &items[k];
                let hov = self.hover == Some(*hit);
                let col = if hov { l.primary } else if *hit == Hit::Session { a(fg, 0.6) } else { fg };
                let mut tx = rx;
                if *hit == Hit::Session {
                    let dot = px(6.0);
                    cb::fill_circle(cr, rx + dot / 2.0, ry + bh / 2.0, dot / 2.0, a(if hov { l.primary } else { fg }, 0.5));
                    tx += dot + px(15.0);
                }
                draw_spaced(cr, tx, ry, text, &bf, col, px(2.0));
                hits.push((*hit, rx - px(10.0), ry - px(10.0), widths[k] + px(20.0), bh + px(20.0)));
                rx += widths[k] + gap;
            }
        }

        cb::cairo_pop_group_to_source(cr);
        cb::cairo_paint_with_alpha(cr, content.max(0.0));
        hits
    }
}

impl Surf {
    fn new(output: Option<u32>, surface: wl_surface::WlSurface, role: Role) -> Self {
        Surf { output, surface, role, size: (0, 0), configured: false, bufs: Vec::new(), background: None, frame_cb: None, hits: Vec::new() }
    }
}

/// QML shake: -20, 20, -15, 15, -5, 0 px over 6 x 50 ms (design pixels).
fn shake_offset(ms: f64) -> f64 {
    let keys = [0.0, -20.0, 20.0, -15.0, 15.0, -5.0, 0.0];
    if ms >= SHAKE_MS {
        return 0.0;
    }
    let seg = (ms / 50.0) as usize;
    let t = (ms % 50.0) / 50.0;
    let (a, b) = (keys[seg], keys[(seg + 1).min(6)]);
    a + (b - a) * (1.0 - (1.0 - t) * (1.0 - t))
}

fn session_label() -> String {
    for k in ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP", "DESKTOP_SESSION"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    if std::env::var_os("MANGO_INSTANCE_SIGNATURE").is_some() {
        "MangoWM".into()
    } else {
        "Wayland".into()
    }
}

/// fontconfig knows `family` (fonts in another user's home are invisible to the greeter user).
fn font_installed(family: &str) -> bool {
    std::process::Command::new("fc-list")
        .arg(format!("{family}:"))
        .arg("family")
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(true)
}

fn spawn_detached(cmd: &[&str]) {
    if let Ok(mut c) = std::process::Command::new(cmd[0]).args(&cmd[1..]).spawn() {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

fn text_size(cr: *mut Cairo, text: &str, font: &str) -> (f64, f64) {
    let (w, h) = cb::measure_pango_text(cr, text, font, 100_000, Some(1));
    (w as f64, h as f64)
}

/// Width of `text` drawn with extra `spacing` after each character (letter-spacing).
fn spaced_width(cr: *mut Cairo, text: &str, font: &str, spacing: f64) -> f64 {
    text.chars().map(|c| text_size(cr, &c.to_string(), font).0 + spacing).sum::<f64>() - spacing.min(0.0)
}

fn draw_spaced(cr: *mut Cairo, mut x: f64, y: f64, text: &str, font: &str, color: Rgba, spacing: f64) {
    for c in text.chars() {
        let s = c.to_string();
        cb::draw_pango_text(cr, x, y, &s, font, color, None);
        x += text_size(cr, &s, font).0 + spacing;
    }
}

fn local_tm() -> libc::tm {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

fn local_now() -> (i32, i32) {
    let tm = local_tm();
    (tm.tm_hour, tm.tm_min)
}

/// QML "dddd / MMMM d" in upper case.
fn date_label() -> String {
    let tm = local_tm();
    const DAYS: [&str; 7] = ["SUNDAY", "MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY", "SATURDAY"];
    const MONTHS: [&str; 12] = ["JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST", "SEPTEMBER", "OCTOBER", "NOVEMBER", "DECEMBER"];
    format!("{} / {} {}", DAYS[tm.tm_wday.clamp(0, 6) as usize], MONTHS[tm.tm_mon.clamp(0, 11) as usize], tm.tm_mday)
}

/// Wallpaper (cover-cropped) with QML's 15 % black veil and the side gradient.
fn render_background(look: &Look, w: i32, h: i32) -> *mut CairoSurface {
    unsafe {
        let img = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h);
        let cr = cb::cairo_create(img);
        let bg = look.background;
        cb::cairo_set_source_rgba(cr, bg.0, bg.1, bg.2, 1.0);
        cb::cairo_paint(cr);
        let mut have_wall = false;
        if let Some(p) = &look.wallpaper {
            have_wall = cb::draw_image_cover(cr, p, 0.0, 0.0, w as f64, h as f64, 0.0);
        }
        if have_wall {
            cb::cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 0.15);
            cb::cairo_paint(cr);
        }
        let gw = w as f64 * 0.6;
        let (x0, x1) = if look.right { (w as f64, w as f64 - gw) } else { (0.0, gw) };
        let pat = cb::cairo_pattern_create_linear(x0, 0.0, x1, 0.0);
        cb::cairo_pattern_add_color_stop_rgba(pat, 0.0, 0.0, 0.0, 0.0, 1.0);
        cb::cairo_pattern_add_color_stop_rgba(pat, 1.0, 0.0, 0.0, 0.0, 0.0);
        cb::cairo_set_source(cr, pat);
        let rx = if look.right { w as f64 - gw } else { 0.0 };
        cb::cairo_rectangle(cr, rx, 0.0, gw, h as f64);
        cb::cairo_fill(cr);
        cb::cairo_pattern_destroy(pat);
        cb::cairo_destroy(cr);
        img
    }
}

fn new_buf(shm: &wl_shm::WlShm, qh: &QueueHandle<App>, w: i32, h: i32) -> Option<Buf> {
    let stride = w * 4;
    let len = (stride * h) as usize;
    unsafe {
        let fd = libc::memfd_create(c"prism-lock".as_ptr(), libc::MFD_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let file = std::fs::File::from_raw_fd(fd);
        if libc::ftruncate(fd, len as libc::off_t) < 0 {
            return None;
        }
        let ptr = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        if ptr == libc::MAP_FAILED {
            return None;
        }
        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        Some(Buf { buffer, ptr: ptr as *mut u8, len, busy: false })
    }
}

// ── Wayland events ───────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(app: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_registry::Event::Global { name, interface, version } => match interface.as_str() {
                "wl_compositor" => app.compositor = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_shm" => app.shm = Some(reg.bind(name, 1, qh, ())),
                "wl_seat" if app.seat.is_none() => app.seat = Some(reg.bind(name, version.min(5), qh, ())),
                "ext_session_lock_manager_v1" => app.lock_manager = Some(reg.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(reg.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => app.layer_shell = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_output" => {
                    let out: wl_output::WlOutput = reg.bind(name, version.min(3), qh, ());
                    app.add_lock_surface(name, &out, qh);
                    app.outputs.push((name, out));
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                app.outputs.retain(|(n, _)| *n != name);
                if let Some(i) = app.surfaces.iter().position(|s| s.output == Some(name)) {
                    let s = app.surfaces.remove(i);
                    if let Role::Lock(ls) = &s.role {
                        ls.destroy();
                    }
                    s.surface.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(app: &mut Self, seat: &wl_seat::WlSeat, ev: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = ev {
            if caps.contains(wl_seat::Capability::Keyboard) && app.keyboard.is_none() {
                app.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && app.pointer.is_none() {
                app.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(app: &mut Self, _: &wl_keyboard::WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                let km = unsafe { xkb::Keymap::new_from_fd(&app.xkb_ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) };
                app.xkb = km.ok().flatten().map(|k| xkb::State::new(&k));
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(x) = app.xkb.as_mut() {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                app.repeat_rate = rate;
                app.repeat_delay = delay;
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                app.key(key);
                let repeats = app.xkb.as_ref().map(|x| x.get_keymap().key_repeats(xkb::Keycode::new(key + 8))).unwrap_or(false);
                app.repeat = (repeats && app.repeat_rate > 0).then(|| (key, Instant::now() + Duration::from_millis(app.repeat_delay.max(0) as u64)));
            }
            wl_keyboard::Event::Key { key, .. } => {
                if app.repeat.map(|(k, _)| k == key).unwrap_or(false) {
                    app.repeat = None;
                }
            }
            wl_keyboard::Event::Leave { .. } => app.repeat = None,
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(app: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } => {
                app.pointer_on = Some(surface);
                app.pointer_pos = (surface_x, surface_y);
                app.update_hover();
            }
            wl_pointer::Event::Leave { .. } => {
                app.pointer_on = None;
                app.hover = None;
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                app.pointer_pos = (surface_x, surface_y);
                app.update_hover();
            }
            wl_pointer::Event::Button { state: WEnum::Value(wl_pointer::ButtonState::Released), .. } => app.click(),
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for App {
    fn event(app: &mut Self, _: &ExtSessionLockV1, ev: ext_session_lock_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            ext_session_lock_v1::Event::Locked => eprintln!("[prism-lock] session locked"),
            ext_session_lock_v1::Event::Finished => {
                // Denied (another locker is active) or revoked: nothing we can do.
                eprintln!("[prism-lock] compositor ended the lock");
                app.error = Some("lock finished by compositor".into());
                app.done = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for App {
    fn event(app: &mut Self, ls: &ExtSessionLockSurfaceV1, ev: ext_session_lock_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = ev {
            ls.ack_configure(serial);
            if let Some(i) = app.surfaces.iter().position(|s| matches!(&s.role, Role::Lock(l) if l == ls)) {
                app.surfaces[i].size = (width as i32, height as i32);
                app.surfaces[i].configured = true;
                app.surfaces[i].frame_cb = None;
                app.draw(i, qh);
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(app: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                if let Some(i) = app.surfaces.iter().position(|s| matches!(&s.role, Role::Layer(l) if l == ls)) {
                    app.surfaces[i].size = (width.max(1) as i32, height.max(1) as i32);
                    app.surfaces[i].configured = true;
                    app.surfaces[i].frame_cb = None;
                    app.draw(i, qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => app.done = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(_: &mut Self, base: &XdgWmBase, ev: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = ev {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for App {
    fn event(app: &mut Self, xs: &XdgSurface, ev: xdg_surface::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = ev {
            xs.ack_configure(serial);
            if let Some(i) = app.surfaces.iter().position(|s| matches!(&s.role, Role::Xdg(x, _) if x == xs)) {
                if app.surfaces[i].size.0 <= 0 {
                    app.surfaces[i].size = (1920, 1080);
                }
                app.surfaces[i].configured = true;
                app.surfaces[i].frame_cb = None;
                app.draw(i, qh);
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for App {
    fn event(app: &mut Self, tl: &XdgToplevel, ev: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                if let Some(s) = app.surfaces.iter_mut().find(|s| matches!(&s.role, Role::Xdg(_, t) if t == tl)) {
                    s.size = (width, height);
                }
            }
            // Under greetd the greeter never closes; a preview window does.
            xdg_toplevel::Event::Close if app.preview => app.done = true,
            xdg_toplevel::Event::Close => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(app: &mut Self, cb_obj: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        for s in app.surfaces.iter_mut() {
            if s.frame_cb.as_ref() == Some(cb_obj) {
                s.frame_cb = None;
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(app: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            for b in app.surfaces.iter_mut().flat_map(|s| s.bufs.iter_mut()) {
                if &b.buffer == buffer {
                    b.busy = false;
                }
            }
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_output::WlOutput, ExtSessionLockManagerV1, ZwlrLayerShellV1);

// ── Main ─────────────────────────────────────────────────────────────────

/// `--check-auth`: read a password from the terminal and run the same PAM check as the lock.
fn check_auth() -> ! {
    let user = std::env::var("USER").unwrap_or_default();
    eprint!("PAM service '{}', user '{}'. Password: ", auth::service(), user);
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    let tty = unsafe { libc::tcgetattr(0, &mut old) } == 0;
    if tty {
        let mut t = old;
        t.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
    }
    let mut pw = String::new();
    let _ = std::io::stdin().read_line(&mut pw);
    if tty {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &old) };
    }
    eprintln!();
    let pw = pw.trim_end_matches(['\n', '\r']);
    if auth::authenticate(&user, pw) {
        eprintln!("OK: the lock screen accepts this password.");
        exit(0);
    }
    eprintln!("FAILED: PAM rejected the password (see journalctl for pam messages).");
    exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check-auth") {
        check_auth();
    }
    let mode = if args.iter().any(|a| a == "--greeter") || std::env::var_os("GREETD_SOCK").is_some() { Mode::Greeter } else { Mode::Lock };
    match run(mode) {
        Ok(()) => exit(0),
        Err((code, msg)) => {
            eprintln!("[prism-lock] {msg}");
            exit(code);
        }
    }
}

fn run(mode: Mode) -> Result<(), (i32, String)> {
    let conn = Connection::connect_to_env().map_err(|e| (1, format!("cannot connect to Wayland: {e}")))?;
    let mut queue: EventQueue<App> = conn.new_event_queue();
    let qh = queue.handle();
    let (auth_tx, auth_rx) = channel();
    let mut pipe = [0i32; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err((1, "pipe2 failed".into()));
    }

    let sessions = if mode == Mode::Greeter { greetd::sessions() } else { Vec::new() };
    let mut session_idx = 0;
    if let Some(last) = greetd::last_session().filter(|_| mode == Mode::Greeter) {
        session_idx = sessions.iter().position(|s| s.cmd == last).unwrap_or(0);
    }
    let user = match mode {
        Mode::Lock => std::env::var("USER").unwrap_or_default(),
        Mode::Greeter => {
            let cfg = greetd::load_greeter_config();
            if cfg.remember_last_user { greetd::load_last_user().unwrap_or_default() } else { String::new() }
        }
    };
    let entering_user = mode == Mode::Greeter && user.is_empty();
    let now = Instant::now();
    let mut app = App {
        mode,
        look: Look::load(mode),
        compositor: None,
        shm: None,
        seat: None,
        keyboard: None,
        pointer: None,
        lock_manager: None,
        wm_base: None,
        layer_shell: None,
        outputs: Vec::new(),
        session_lock: None,
        surfaces: Vec::new(),
        xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb: None,
        repeat_rate: 25,
        repeat_delay: 600,
        repeat: None,
        pointer_on: None,
        pointer_pos: (0.0, 0.0),
        hover: None,
        user,
        entering_user,
        sessions,
        session_idx,
        text: String::new(),
        masks: Vec::new(),
        // The greeter starts in the field; the lock starts with the badge (QML).
        password_mode: mode == Mode::Greeter,
        box_anim: if mode == Mode::Greeter { (1.0, 1.0, now) } else { (0.0, 0.0, now) },
        last_input: now,
        checking: false,
        failed_at: None,
        status: String::new(),
        status_error: false,
        opened: now,
        closing: None,
        done: false,
        preview: mode == Mode::Greeter && std::env::var_os("GREETD_SOCK").is_none(),
        mask_font_ok: font_installed(MASK_FONT),
        error: None,
        auth_tx,
        auth_rx,
        wake_fd: pipe[0],
        wake_write: pipe[1],
    };

    let _registry = conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut app).map_err(|e| (1, format!("registry roundtrip: {e}")))?;
    if app.compositor.is_none() || app.shm.is_none() {
        return Err((1, "compositor lacks wl_compositor or wl_shm".into()));
    }

    match mode {
        Mode::Lock => {
            let Some(manager) = app.lock_manager.clone() else {
                // A layer surface is not a lock: other clients keep input. Refuse instead.
                return Err((3, "compositor does not support ext-session-lock-v1; not locking".into()));
            };
            app.session_lock = Some(manager.lock(&qh, ()));
            // Lock surfaces for every output now, before `locked` (protocol requirement).
            let outputs = app.outputs.clone();
            for (name, out) in &outputs {
                app.add_lock_surface(*name, out, &qh);
            }
        }
        Mode::Greeter => {
            let comp = app.compositor.clone().unwrap();
            let surface = comp.create_surface(&qh, ());
            // A normal fullscreen window: under cage it is the only window; in a desktop session
            // (preview) the window manager can still close it. Layer shell only as a last resort.
            let role = if let Some(base) = app.wm_base.clone() {
                let xs = base.get_xdg_surface(&surface, &qh, ());
                let tl = xs.get_toplevel(&qh, ());
                tl.set_title("prism-greeter".into());
                tl.set_app_id("prism-greeter".into());
                tl.set_fullscreen(None);
                Role::Xdg(xs, tl)
            } else if let Some(shell) = app.layer_shell.clone() {
                let ls = shell.get_layer_surface(&surface, None, Layer::Overlay, "prism-greeter".into(), &qh, ());
                ls.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
                ls.set_exclusive_zone(-1);
                ls.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                Role::Layer(ls)
            } else {
                return Err((1, "no xdg_wm_base or layer shell for the greeter".into()));
            };
            surface.commit();
            app.surfaces.push(Surf::new(None, surface, role));
        }
    }

    // Event loop: Wayland fd + auth wake pipe, with timers for the clock, key repeat and
    // animations. Errors end the loop with an error code (prismd starts a new lock client).
    loop {
        if app.done {
            break;
        }
        if let Some(t) = app.closing {
            if ms_since(t) >= FADE_OUT_MS {
                break;
            }
        }
        let now = Instant::now();
        if let Some((key, at)) = app.repeat {
            if now >= at && app.repeat_rate > 0 {
                app.repeat = Some((key, now + Duration::from_micros(1_000_000 / app.repeat_rate as u64)));
                app.key(key);
            }
        }
        // QML inactivityTimer: 15 s without input clears the field and closes it.
        if app.password_mode && app.mode == Mode::Lock && !app.checking && app.last_input.elapsed() >= INACTIVITY {
            app.set_password_mode(false);
        }
        app.draw_all(&qh);

        queue.flush().map_err(|e| (1, format!("flush: {e}")))?;
        let mut timeout = if app.any_animating() { 8 } else { 1000 };
        if let Some((_, at)) = app.repeat {
            timeout = timeout.min(at.saturating_duration_since(Instant::now()).as_millis() as i32 + 1);
        }
        if let Some(guard) = queue.prepare_read() {
            let mut fds = [
                libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: app.wake_fd, events: libc::POLLIN, revents: 0 },
            ];
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
            if ret > 0 && fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                    Err(e) => return Err((1, format!("Wayland connection lost: {e}"))),
                }
            } else {
                drop(guard);
            }
            if ret > 0 && fds[1].revents & libc::POLLIN != 0 {
                let mut b = [0u8; 64];
                unsafe { libc::read(app.wake_fd, b.as_mut_ptr() as *mut libc::c_void, b.len()) };
            }
        }
        queue.dispatch_pending(&mut app).map_err(|e| (1, format!("dispatch: {e}")))?;
        app.take_auth_result();
    }

    if let Some(e) = app.error.take() {
        return Err((1, e));
    }
    match mode {
        Mode::Lock => {
            if let Some(lock) = app.session_lock.take() {
                for s in app.surfaces.drain(..) {
                    if let Role::Lock(ls) = &s.role {
                        ls.destroy();
                    }
                    s.surface.destroy();
                }
                lock.unlock_and_destroy();
                queue.roundtrip(&mut app).map_err(|e| (1, format!("unlock roundtrip: {e}")))?;
                eprintln!("[prism-lock] unlocked");
            }
        }
        Mode::Greeter if app.preview => eprintln!("[prism-greet] preview closed"),
        Mode::Greeter => eprintln!("[prism-greet] session starting"),
    }
    Ok(())
}
