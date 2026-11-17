//! Screen colour picker (port of QML ColorPicker.qml).
//!
//! The overlay first maps transparent and hides the pointer; once a frame without the cursor
//! has been presented, the screen is frozen with `prism_capture` (cursor excluded; kept in
//! memory, never written to disk) and shown on a
//! full-screen overlay. A 180 px lens magnifies the pixels around the cursor 11x
//! (nearest-neighbour) with a crosshair on the sampled pixel, a ring tinted with its colour and
//! a hex label. Arrow keys nudge by one pixel. Click / Enter / Space copies the hex with
//! wl-copy (so it lands in clipboard history) and posts a notification; right-click, Escape
//! or Q cancels.

use std::process::{Command, Stdio};

use wayland_client::protocol::{wl_buffer, wl_callback, wl_keyboard, wl_pointer, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xkbcommon::xkb;

use crate::cairo_backend::{self as cb, Cairo, CairoSurface};
use crate::desktop::ShmBuf;
use crate::Globals;

const LENS: f64 = 180.0;
const ZOOM: f64 = 11.0;
const CAIRO_FILTER_NEAREST: i32 = 3;

/// A frozen screenshot as a cairo image.
struct Shot {
    img: *mut CairoSurface,
    w: i32,
    h: i32,
}

impl Drop for Shot {
    fn drop(&mut self) {
        unsafe { cb::cairo_surface_destroy(self.img) };
    }
}

impl Shot {
    /// The first output, at its native resolution, without the cursor.
    fn capture(conn: &Connection) -> Option<Shot> {
        let mut cap = prism_capture::Capturer::new(conn).ok()?;
        let out = cap.outputs().into_iter().next()?;
        let img = cap.capture_output(&out, false).ok()?;
        unsafe {
            let (w, h) = (img.width as i32, img.height as i32);
            let surf = cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h);
            let stride = cb::cairo_image_surface_get_stride(surf) as usize;
            let dst = std::slice::from_raw_parts_mut(cb::cairo_image_surface_get_data(surf), stride * h as usize);
            for y in 0..h as usize {
                let row = &img.data[y * img.stride as usize..y * img.stride as usize + w as usize * 4];
                dst[y * stride..y * stride + w as usize * 4].copy_from_slice(row);
            }
            cb::cairo_surface_mark_dirty(surf);
            Some(Shot { img: surf, w, h })
        }
    }

    fn pixel(&self, x: i32, y: i32) -> (u8, u8, u8) {
        let (x, y) = (x.clamp(0, self.w - 1), y.clamp(0, self.h - 1));
        unsafe {
            let stride = cb::cairo_image_surface_get_stride(self.img) as usize;
            let p = cb::cairo_image_surface_get_data(self.img).add(y as usize * stride + x as usize * 4);
            (*p.add(2), *p.add(1), *p)
        }
    }
}

pub struct Picker {
    queue: EventQueue<State>,
    st: State,
    conn: Connection,
}

/// Capture progress: the cursor has to be off screen before grim runs.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Waiting for pointer enter (counts presented frames as a fallback).
    WaitEnter(u32),
    /// Cursor hidden; true once a frame was committed after hiding it, whose callback means
    /// the cursor is gone from the screen.
    Hidden(bool),
    /// Capture on the next dispatch.
    Capture,
    Ready,
}

/// Frames to wait for pointer enter before capturing anyway (pointer on another output).
const ENTER_WAIT_FRAMES: u32 = 8;

struct State {
    shot: Option<Shot>,
    phase: Phase,
    enter_serial: Option<u32>,
    /// Pointer position in surface coordinates (before the capture exists).
    ptr: Option<(f64, f64)>,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    shm: wayland_client::protocol::wl_shm::WlShm,
    bufs: Vec<ShmBuf>,
    size: (i32, i32),
    configured: bool,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,
    cursor: (i32, i32),
    xkb_ctx: xkb::Context,
    xkb: Option<xkb::State>,
    done: bool,
}

impl Picker {
    /// Freeze the screen and show the picker; None when the capture fails.
    pub fn open(conn: &Connection, g: &Globals) -> Option<Picker> {
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let surface = g.compositor.as_ref()?.create_surface(&qh, ());
        let layer = g.layer_shell.as_ref()?.get_layer_surface(&surface, None, Layer::Overlay, "prism-picker".into(), &qh, ());
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();
        let seat = g.seat.as_ref()?;
        let pointer = seat.get_pointer(&qh, ());
        let cursor_dev = g.cursor_shape_manager.as_ref().map(|m| m.get_pointer(&pointer, &qh, ()));
        Some(Picker {
            conn: conn.clone(),
            queue,
            st: State {
                shot: None,
                phase: Phase::WaitEnter(0),
                enter_serial: None,
                ptr: None,
                surface,
                layer,
                keyboard: Some(seat.get_keyboard(&qh, ())),
                pointer: Some(pointer),
                cursor_dev,
                shm: g.shm.clone()?,
                bufs: Vec::new(),
                size: (0, 0),
                configured: false,
                frame_cb: None,
                dirty: true,
                cursor: (0, 0),
                xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
                xkb: None,
                done: false,
            },
        })
    }

    pub fn dispatch(&mut self) -> bool {
        let ok = self.queue.dispatch_pending(&mut self.st).is_ok();
        if self.st.phase == Phase::Capture && !self.st.done {
            self.st.capture(&self.conn, &self.queue.handle());
        }
        ok
    }

    pub fn finished(&self) -> bool {
        self.st.done
    }

    pub fn close(&mut self) {
        self.st.done = true;
    }

    /// Draw if needed; false once the picker is done.
    pub fn tick(&mut self) -> bool {
        let qh = self.queue.handle();
        if self.st.dirty && self.st.configured && self.st.frame_cb.is_none() {
            self.st.draw(&qh);
        }
        !self.st.done
    }

    pub fn teardown(mut self) {
        let s = &mut self.st;
        if let Some(d) = s.cursor_dev.take() {
            d.destroy();
        }
        if let Some(k) = s.keyboard.take() {
            k.release();
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

impl State {
    /// Surface position to screenshot pixel (they match unless the output is scaled).
    fn to_shot(&self, x: f64, y: f64) -> (i32, i32) {
        let Some(shot) = &self.shot else { return (0, 0) };
        let (sw, sh) = (self.size.0.max(1) as f64, self.size.1.max(1) as f64);
        (((x * shot.w as f64 / sw) as i32).clamp(0, shot.w - 1), ((y * shot.h as f64 / sh) as i32).clamp(0, shot.h - 1))
    }

    fn hex(&self) -> String {
        let Some(shot) = &self.shot else { return String::new() };
        let (r, g, b) = shot.pixel(self.cursor.0, self.cursor.1);
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// Freeze the screen (the cursor is hidden by now) and show the picker.
    fn capture(&mut self, conn: &Connection, qh: &QueueHandle<Self>) {
        self.phase = Phase::Ready;
        let Some(shot) = Shot::capture(conn) else {
            eprintln!("[prism-ui] colour picker: screen capture failed");
            self.done = true;
            return;
        };
        let centre = (shot.w / 2, shot.h / 2);
        self.shot = Some(shot);
        self.cursor = match self.ptr {
            Some((x, y)) => self.to_shot(x, y),
            None => centre,
        };
        if let (Some(d), Some(serial)) = (&self.cursor_dev, self.enter_serial) {
            d.set_shape(serial, CursorShape::Crosshair);
        }
        self.dirty = true;
        if self.frame_cb.is_none() {
            self.draw(qh);
        }
    }

    fn pick(&mut self) {
        if self.shot.is_none() {
            return;
        }
        let hex = self.hex();
        self.done = true;
        std::thread::spawn(move || {
            // Plain wl-copy (no self-copy lock): the colour belongs in clipboard history.
            if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(hex.as_bytes());
                }
                let _ = child.wait();
            }
            let _ = Command::new("notify-send")
                .args(["-a", "Prism", "-i", "color-picker", "Color Picker", &format!("Copied {hex} to clipboard")])
                .status();
        });
    }

    fn nudge(&mut self, dx: i32, dy: i32) {
        let Some(shot) = &self.shot else { return };
        self.cursor = ((self.cursor.0 + dx).clamp(0, shot.w - 1), (self.cursor.1 + dy).clamp(0, shot.h - 1));
        self.dirty = true;
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (w, h) = self.size;
        if w <= 0 || h <= 0 {
            return;
        }
        self.bufs.retain(|b| b.w == w && b.h == h);
        let idx = match self.bufs.iter().position(|b| !b.busy) {
            Some(i) => i,
            None if self.bufs.len() < 3 => match ShmBuf::new(&self.shm, qh, w, h, ()) {
                Some(b) => {
                    self.bufs.push(b);
                    self.bufs.len() - 1
                }
                None => return,
            },
            None => return,
        };
        self.bufs[idx].busy = true;
        unsafe {
            let surf = self.bufs[idx].cairo();
            let cr = cb::cairo_create(surf);
            self.paint(cr, w as f64, h as f64);
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, w, h);
        self.frame_cb = Some(self.surface.frame(qh, ()));
        self.surface.commit();
        self.dirty = false;
    }

    unsafe fn paint(&self, cr: *mut Cairo, w: f64, h: f64) {
        // Before the capture the overlay is fully transparent.
        let Some(shot) = &self.shot else { return };
        let (sx, sy) = (w / shot.w as f64, h / shot.h as f64);
        // The frozen screen.
        cb::cairo_save(cr);
        cb::cairo_scale(cr, sx, sy);
        cb::cairo_set_source_surface(cr, shot.img, 0.0, 0.0);
        cb::cairo_paint(cr);
        cb::cairo_restore(cr);

        let (px, py) = self.cursor;
        let (cx, cy) = ((px as f64 + 0.5) * sx, (py as f64 + 0.5) * sy);
        let (r, g, b) = shot.pixel(px, py);
        let col = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0);
        let lr = LENS / 2.0;

        // Lens: pixels around the cursor, 11x, centred on the sampled pixel.
        cb::cairo_save(cr);
        cb::cairo_new_path(cr);
        cb::cairo_arc(cr, cx, cy, lr - 1.0, 0.0, std::f64::consts::TAU);
        cb::cairo_clip(cr);
        cb::cairo_translate(cr, cx, cy);
        cb::cairo_scale(cr, ZOOM, ZOOM);
        cb::cairo_translate(cr, -(px as f64 + 0.5), -(py as f64 + 0.5));
        cb::cairo_set_source_surface(cr, shot.img, 0.0, 0.0);
        cb::cairo_pattern_set_filter(cb::cairo_get_source(cr), CAIRO_FILTER_NEAREST);
        cb::cairo_paint(cr);
        cb::cairo_restore(cr);

        // Rings: sampled colour, then thin dark outlines for contrast.
        cb::stroke_circle(cr, cx, cy, lr - 1.5, col, 3.0);
        cb::stroke_circle(cr, cx, cy, lr - 0.5, (0.0, 0.0, 0.0, 0.45), 1.0);
        cb::stroke_circle(cr, cx, cy, lr - 3.5, (0.0, 0.0, 0.0, 0.35), 1.0);

        // Crosshair: black outline under white lines, on the centre pixel.
        cb::fill_rounded_rect(cr, cx - 2.0, cy - 10.0, 4.0, 20.0, 1.0, (0.0, 0.0, 0.0, 1.0));
        cb::fill_rounded_rect(cr, cx - 10.0, cy - 2.0, 20.0, 4.0, 1.0, (0.0, 0.0, 0.0, 1.0));
        cb::fill_rounded_rect(cr, cx - 1.0, cy - 9.0, 2.0, 18.0, 0.5, (1.0, 1.0, 1.0, 1.0));
        cb::fill_rounded_rect(cr, cx - 9.0, cy - 1.0, 18.0, 2.0, 0.5, (1.0, 1.0, 1.0, 1.0));

        // Hex label with swatch, below the centre (above near the bottom edge).
        let hex = self.hex();
        let font = "monospace Bold 13px";
        let (tw, th) = cb::measure_pango_text(cr, &hex, font, 1000, Some(1));
        let (lw, lh) = (10.0 + 6.0 + tw as f64 + 16.0, th as f64 + 8.0);
        let lx = (cx - lw / 2.0).round();
        let ly = if cy > h - 70.0 { cy - 20.0 - lh } else { cy + 20.0 };
        cb::fill_rounded_rect(cr, lx, ly, lw, lh, 6.0, (0.08, 0.08, 0.08, 0.85));
        cb::stroke_rounded_rect(cr, lx + 0.5, ly + 0.5, lw - 1.0, lh - 1.0, 6.0, (1.0, 1.0, 1.0, 0.25), 1.0);
        let sw_y = ly + (lh - 10.0) / 2.0;
        cb::fill_rounded_rect(cr, lx + 8.0, sw_y, 10.0, 10.0, 2.0, col);
        cb::stroke_rounded_rect(cr, lx + 8.5, sw_y + 0.5, 9.0, 9.0, 2.0, (1.0, 1.0, 1.0, 0.4), 1.0);
        cb::draw_pango_text(cr, lx + 8.0 + 10.0 + 6.0, ly + 4.0, &hex, font, (1.0, 1.0, 1.0, 1.0), None);
    }
}

// ── Wayland events ───────────────────────────────────────────────────────

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                st.size = (width as i32, height as i32);
                st.configured = true;
                st.dirty = true;
                if st.frame_cb.is_none() {
                    st.draw(qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => st.done = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, pointer: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                st.enter_serial = Some(serial);
                st.ptr = Some((surface_x, surface_y));
                if st.shot.is_some() {
                    if let Some(d) = &st.cursor_dev {
                        d.set_shape(serial, CursorShape::Crosshair);
                    }
                    st.cursor = st.to_shot(surface_x, surface_y);
                } else {
                    // Hide the cursor so it is not in the capture; wait for a frame without it.
                    pointer.set_cursor(serial, None, 0, 0);
                    st.phase = Phase::Hidden(false);
                    if st.frame_cb.is_none() && st.configured {
                        st.draw(qh);
                        st.phase = Phase::Hidden(true);
                        return;
                    }
                }
                st.dirty = true;
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.ptr = Some((surface_x, surface_y));
                if st.shot.is_some() {
                    st.cursor = st.to_shot(surface_x, surface_y);
                    st.dirty = true;
                }
            }
            wl_pointer::Event::Button { button, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => match button {
                0x110 => st.pick(),
                0x111 => st.done = true,
                _ => {}
            },
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(st: &mut Self, _: &wl_keyboard::WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                let km = unsafe { xkb::Keymap::new_from_fd(&st.xkb_ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) };
                st.xkb = km.ok().flatten().map(|k| xkb::State::new(&k));
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(x) = st.xkb.as_mut() {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                let sym = st.xkb.as_ref().map(|x| x.key_get_one_sym(xkb::Keycode::new(key + 8)).raw()).unwrap_or(0);
                match sym {
                    0xff1b | 0x71 | 0x51 => st.done = true,           // Escape, q, Q
                    0xff0d | 0xff8d | 0x20 => st.pick(),               // Return, KP_Enter, space
                    0xff51 => st.nudge(-1, 0),
                    0xff53 => st.nudge(1, 0),
                    0xff52 => st.nudge(0, -1),
                    0xff54 => st.nudge(0, 1),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(st: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        st.frame_cb = None;
        if st.done {
            return;
        }
        match st.phase {
            // A frame without the cursor is on screen: capture (Picker::dispatch).
            Phase::Hidden(true) => st.phase = Phase::Capture,
            Phase::Hidden(false) => {
                st.draw(qh);
                st.phase = Phase::Hidden(true);
            }
            Phase::WaitEnter(n) if n + 1 >= ENTER_WAIT_FRAMES => st.phase = Phase::Capture,
            Phase::WaitEnter(n) => {
                st.phase = Phase::WaitEnter(n + 1);
                st.draw(qh); // keep frames coming while waiting for pointer enter
            }
            _ if st.dirty => st.draw(qh),
            _ => {}
        }
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
