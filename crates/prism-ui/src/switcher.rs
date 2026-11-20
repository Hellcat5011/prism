//! Window switcher (Alt+Tab): the compositor binds Alt+Tab to `prism-ctl switcher next`; the
//! first press opens a card of the open windows, most recently used first, with the previous
//! window selected; more presses (or Tab / arrows here) move on, and letting go of Alt switches
//! to the selected window. Esc cancels; a click picks a window. Thumbnails are captured on a
//! worker thread (ext-image-copy-capture of each window) and fill in as they arrive.

use std::sync::mpsc::{channel, Receiver};

use prism_capture::Image;
use wayland_client::protocol::{wl_buffer, wl_callback, wl_keyboard, wl_pointer, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xkbcommon::xkb;

use crate::cairo_backend::{self as cb, CairoSurface};
use crate::desktop::layout::Rect;
use crate::desktop::text::{draw_in_box, Align, TextStyle};
use crate::desktop::ShmBuf;
use crate::ui::Style;
use crate::waker::Waker;
use crate::wintrack::Window;
use crate::Globals;

const ITEM_W: f64 = 220.0;
const THUMB_H: f64 = 128.0;
const ITEM_H: f64 = THUMB_H + 44.0;
const GAP: f64 = 12.0;
const PAD: f64 = 18.0;
/// Most windows shown at once; the row scrolls past this.
const MAX_SHOWN: usize = 6;
/// Thumbnails are scaled down to at most this size.
const THUMB_MAX: (u32, u32) = (440, 256);

pub enum Outcome {
    Switch(Window),
    Cancel,
}

pub struct Switcher {
    queue: EventQueue<State>,
    st: State,
}

struct Thumb {
    img: Image,
}

struct State {
    windows: Vec<Window>,
    selected: usize,
    first: usize,
    thumbs: Vec<Option<Thumb>>,
    thumb_rx: Receiver<(usize, Image)>,
    style: Style,
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    shm: wayland_client::protocol::wl_shm::WlShm,
    bufs: Vec<ShmBuf>,
    size: (i32, i32),
    configured: bool,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,
    xkb_ctx: xkb::Context,
    xkb: Option<xkb::State>,
    /// The keyboard arrived (Alt is judged only after that).
    focused: bool,
    /// Alt (or Super) was seen held: letting go of it switches. Opened without one (from a
    /// terminal), the card waits for Enter, Esc or a click.
    held_seen: bool,
    from_key: bool,
    /// The next modifiers event is the first since the keyboard arrived.
    first_mods: bool,
    hits: Vec<(Rect, usize)>,
    ptr: (f64, f64),
    outcome: Option<Outcome>,
}

fn card_size(n: usize) -> (i32, i32) {
    let shown = n.clamp(1, MAX_SHOWN) as f64;
    ((PAD * 2.0 + shown * ITEM_W + (shown - 1.0) * GAP) as i32, (PAD * 2.0 + ITEM_H) as i32)
}

impl Switcher {
    /// Open with `windows` (most recent first); `back` selects from the end (Alt+Shift+Tab).
    /// `from_key`: opened by the Alt+Tab binding, so Alt already let go when the card gets the
    /// keyboard (a quick tap) switches at once.
    pub fn open(conn: &Connection, g: &Globals, windows: Vec<Window>, back: bool, from_key: bool, waker: Waker) -> Option<Switcher> {
        if windows.is_empty() {
            return None;
        }
        let queue: EventQueue<State> = conn.new_event_queue();
        let qh = queue.handle();
        let cfg = prism_common::Config::load();
        let style = Style::new(&cfg, &prism_common::Theme::load());
        let surface = g.compositor.as_ref()?.create_surface(&qh, ());
        let ns = if style.blur { "prism-modal" } else { "prism-modal-solid" };
        let layer = g.layer_shell.as_ref()?.get_layer_surface(&surface, None, Layer::Overlay, ns.into(), &qh, ());
        let size = card_size(windows.len());
        layer.set_size(size.0 as u32, size.1 as u32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_exclusive_zone(-1);
        surface.commit();
        let seat = g.seat.as_ref()?;
        let n = windows.len();
        // The previous window first (Alt+Tab goes back to it), or the last one going backwards.
        let selected = if n < 2 { 0 } else if back { n - 1 } else { 1 };
        let (tx, rx) = channel();
        let wanted: Vec<(String, String)> = windows.iter().map(|w| (w.app_id.clone(), w.title.clone())).collect();
        std::thread::Builder::new()
            .name("prism-switcher-thumbs".into())
            .spawn(move || {
                let Ok(conn) = Connection::connect_to_env() else { return };
                let Ok(mut cap) = prism_capture::Capturer::new(&conn) else { return };
                let list = cap.windows();
                for (i, (app, title)) in wanted.iter().enumerate() {
                    let Some(w) = list.iter().find(|w| &w.app_id == app && &w.title == title).or_else(|| list.iter().find(|w| &w.app_id == app)) else { continue };
                    if let Ok(img) = cap.capture_window(w, false) {
                        if tx.send((i, shrink(&img))).is_err() {
                            return;
                        }
                        waker.wake();
                    }
                }
            })
            .ok();
        let mut st = State {
            thumbs: (0..n).map(|_| None).collect(),
            windows,
            selected,
            first: 0,
            thumb_rx: rx,
            style,
            surface,
            layer,
            keyboard: Some(seat.get_keyboard(&qh, ())),
            pointer: Some(seat.get_pointer(&qh, ())),
            shm: g.shm.clone()?,
            bufs: Vec::new(),
            size,
            configured: false,
            frame_cb: None,
            dirty: true,
            xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb: None,
            focused: false,
            held_seen: false,
            from_key,
            first_mods: true,
            hits: Vec::new(),
            ptr: (-1.0, -1.0),
            outcome: None,
        };
        st.keep_visible();
        Some(Switcher { queue, st })
    }

    /// Another Alt+Tab (next) or Alt+Shift+Tab (prev) while open.
    pub fn step(&mut self, forward: bool) {
        self.st.step(forward);
    }

    pub fn cancel(&mut self) {
        self.st.outcome = Some(Outcome::Cancel);
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.st).is_ok()
    }

    pub fn finished(&self) -> bool {
        self.st.outcome.is_some()
    }

    pub fn tick(&mut self) {
        while let Ok((i, img)) = self.st.thumb_rx.try_recv() {
            if let Some(slot) = self.st.thumbs.get_mut(i) {
                *slot = Some(Thumb { img });
                self.st.dirty = true;
            }
        }
        let qh = self.queue.handle();
        if self.st.configured && self.st.dirty && self.st.frame_cb.is_none() {
            self.st.draw(&qh);
        }
    }

    pub fn teardown(mut self) -> Outcome {
        let s = &mut self.st;
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
        self.st.outcome.take().unwrap_or(Outcome::Cancel)
    }
}

/// Scale an image down (box filter) to fit THUMB_MAX.
fn shrink(img: &Image) -> Image {
    let f = ((img.width as f64 / THUMB_MAX.0 as f64).max(img.height as f64 / THUMB_MAX.1 as f64)).ceil().max(1.0) as u32;
    if f == 1 {
        return img.clone();
    }
    let (w, h) = (img.width / f, img.height / f);
    let mut data = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            for dy in 0..f {
                let row = ((y * f + dy) * img.stride) as usize;
                for dx in 0..f {
                    let p = row + ((x * f + dx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += img.data[p + c] as u32;
                    }
                }
            }
            let o = ((y * w + x) * 4) as usize;
            for c in 0..4 {
                data[o + c] = (acc[c] / (f * f)) as u8;
            }
        }
    }
    Image { width: w, height: h, stride: w * 4, data }
}

impl State {
    fn step(&mut self, forward: bool) {
        let n = self.windows.len();
        self.selected = if forward { (self.selected + 1) % n } else { (self.selected + n - 1) % n };
        self.keep_visible();
        self.dirty = true;
    }

    fn keep_visible(&mut self) {
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + MAX_SHOWN {
            self.first = self.selected + 1 - MAX_SHOWN;
        }
    }

    fn pick(&mut self) {
        if let Some(w) = self.windows.get(self.selected) {
            self.outcome = Some(Outcome::Switch(w.clone()));
        }
    }

    /// Alt or Super held (whichever the compositor's binding uses).
    fn held(&self) -> bool {
        self.xkb.as_ref().is_some_and(|x| x.mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE) || x.mod_name_is_active(xkb::MOD_NAME_LOGO, xkb::STATE_MODS_EFFECTIVE))
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (sw, sh) = self.size;
        self.bufs.retain(|b| b.w == sw && b.h == sh);
        let idx = match self.bufs.iter().position(|b| !b.busy) {
            Some(i) => i,
            None if self.bufs.len() < 2 => match ShmBuf::new(&self.shm, qh, sw, sh, ()) {
                Some(b) => {
                    self.bufs.push(b);
                    self.bufs.len() - 1
                }
                None => return,
            },
            None => return,
        };
        self.bufs[idx].busy = true;
        self.dirty = false;
        self.hits.clear();
        unsafe {
            let surf = self.bufs[idx].cairo();
            let cr = cb::cairo_create(surf);
            cb::cairo_set_operator(cr, 0);
            cb::cairo_paint(cr);
            cb::cairo_set_operator(cr, 2);
            let (w, h) = (sw as f64, sh as f64);
            crate::ui::card_surface(cr, 0.0, 0.0, w, h, &self.style);
            let fg = self.style.fg;
            let shown = self.windows.len().min(MAX_SHOWN);
            for k in 0..shown {
                let i = self.first + k;
                let Some(win) = self.windows.get(i) else { break };
                let x = PAD + k as f64 * (ITEM_W + GAP);
                let item = Rect { x, y: PAD, w: ITEM_W, h: ITEM_H };
                let sel = i == self.selected;
                let hover = item.contains(self.ptr.0, self.ptr.1);
                if sel || hover {
                    cb::fill_rounded_rect(cr, item.x - 6.0, item.y - 6.0, item.w + 12.0, item.h + 12.0, 14.0, (fg.0, fg.1, fg.2, if sel { 0.16 } else { 0.07 }));
                }
                if sel {
                    cb::stroke_rounded_rect(cr, item.x - 5.0, item.y - 5.0, item.w + 10.0, item.h + 10.0, 13.0, self.style.accent, 2.0);
                }
                // Thumbnail (or a placeholder until it arrives), fitted into the box.
                let tb = Rect { x, y: PAD, w: ITEM_W, h: THUMB_H };
                cb::fill_rounded_rect(cr, tb.x, tb.y, tb.w, tb.h, 10.0, (0.0, 0.0, 0.0, 0.25));
                if let Some(Some(t)) = self.thumbs.get_mut(i) {
                    let (iw, ih) = (t.img.width as f64, t.img.height as f64);
                    let s = (tb.w / iw).min(tb.h / ih);
                    let (dw, dh) = (iw * s, ih * s);
                    let (dx, dy) = (tb.x + (tb.w - dw) / 2.0, tb.y + (tb.h - dh) / 2.0);
                    let img_surf: *mut CairoSurface = cb::cairo_image_surface_create_for_data(t.img.data.as_mut_ptr(), cb::CAIRO_FORMAT_ARGB32, t.img.width as i32, t.img.height as i32, t.img.stride as i32);
                    cb::cairo_save(cr);
                    cb::rounded_rect_path(cr, dx, dy, dw, dh, 8.0);
                    cb::cairo_clip(cr);
                    cb::cairo_translate(cr, dx, dy);
                    cb::cairo_scale(cr, s, s);
                    cb::cairo_set_source_surface(cr, img_surf, 0.0, 0.0);
                    cb::cairo_paint(cr);
                    cb::cairo_restore(cr);
                    cb::cairo_surface_destroy(img_surf);
                } else {
                    crate::launcher::draw_icon(cr, &win.app_id, "application-x-executable", tb.x + tb.w / 2.0 - 24.0, tb.y + tb.h / 2.0 - 24.0, 48.0);
                }
                // Icon and title under it.
                let ty = PAD + THUMB_H + 10.0;
                crate::launcher::draw_icon(cr, &win.app_id, "application-x-executable", x, ty + 3.0, 22.0);
                let title = if win.title.is_empty() { win.app_id.as_str() } else { win.title.as_str() };
                let family = format!("{} Medium", self.style.font_family);
                draw_in_box(cr, title, &TextStyle { family: &family, px: 13.0, bold: false, color: fg }, x + 30.0, ty, ITEM_W - 30.0, 28.0, Align::Start);
                self.hits.push((item, i));
            }
            // More windows than shown: small arrows at the ends.
            if self.first > 0 {
                cb::draw_pango_text(cr, 4.0, h / 2.0 - 10.0, "\u{2039}", "Inter, Sans Bold 18px", (fg.0, fg.1, fg.2, 0.6), None);
            }
            if self.first + shown < self.windows.len() {
                cb::draw_pango_text(cr, w - 12.0, h / 2.0 - 10.0, "\u{203a}", "Inter, Sans Bold 18px", (fg.0, fg.1, fg.2, 0.6), None);
            }
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        self.surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        self.surface.damage_buffer(0, 0, sw, sh);
        self.frame_cb = Some(self.surface.frame(qh, ()));
        self.surface.commit();
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(st: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                st.configured = true;
                if width > 0 && height > 0 {
                    st.size = (width as i32, height as i32);
                }
                st.dirty = true;
                if st.frame_cb.is_none() {
                    st.draw(qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => st.outcome = Some(Outcome::Cancel),
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
            wl_keyboard::Event::Enter { .. } => st.focused = true,
            wl_keyboard::Event::Leave { .. } => {
                if st.outcome.is_none() {
                    st.outcome = Some(Outcome::Cancel);
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(x) = st.xkb.as_mut() {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                // Alt let go: switch. A quick tap (already up on arrival) switches too.
                let first = std::mem::take(&mut st.first_mods) && st.focused;
                if st.held() {
                    st.held_seen = true;
                } else if st.focused && (st.held_seen || (first && st.from_key)) && st.outcome.is_none() {
                    st.pick();
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                let sym = st.xkb.as_ref().map(|x| x.key_get_one_sym(xkb::Keycode::new(key + 8)).raw()).unwrap_or(0);
                let shift = st.xkb.as_ref().is_some_and(|x| x.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE));
                use xkb::keysyms as k;
                match sym {
                    k::KEY_Escape => st.outcome = Some(Outcome::Cancel),
                    k::KEY_Return | k::KEY_KP_Enter | k::KEY_space => st.pick(),
                    k::KEY_Tab | k::KEY_ISO_Left_Tab => st.step(!(shift || sym == k::KEY_ISO_Left_Tab)),
                    k::KEY_Right | k::KEY_Down => st.step(true),
                    k::KEY_Left | k::KEY_Up => st.step(false),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.ptr = (surface_x, surface_y);
                st.dirty = true;
            }
            wl_pointer::Event::Leave { .. } => {
                st.ptr = (-1.0, -1.0);
                st.dirty = true;
            }
            wl_pointer::Event::Button { button: 0x110, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => {
                if let Some(&(_, i)) = st.hits.iter().find(|(r, _)| r.contains(st.ptr.0, st.ptr.1)) {
                    st.selected = i;
                    st.pick();
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
ignore_events!(wl_surface::WlSurface, wl_shm_pool::WlShmPool);
