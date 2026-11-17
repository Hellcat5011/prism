//! Remote input from the phone (KDE Connect mousepad and presenter plugins), injected through
//! the compositor's virtual input protocols: `zwlr_virtual_pointer_v1` for the pointer and
//! `zwp_virtual_keyboard_v1` for keys. Text is typed with a keymap built on the fly that holds
//! every keysym needed so far, one per keycode (as `wtype` does), so any Unicode character can
//! be typed whatever the user's layout is.

use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_pointer, wl_registry, wl_seat};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;
use xkbcommon::xkb;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl Mods {
    /// Core X modifier bits (Shift, Lock, Control, Mod1, Mod2, Mod3, Mod4, Mod5), which the
    /// "complete" compat map in our keymap keeps.
    fn mask(self) -> u32 {
        (self.shift as u32) | (self.ctrl as u32) << 2 | (self.alt as u32) << 3 | (self.logo as u32) << 6
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ev {
    Motion(f64, f64),
    Button(u32, bool),
    Click(u32, u32),
    /// Scroll by (dx, dy) in pointer units; positive dy scrolls down.
    Scroll(f64, f64),
    Text(String, Mods),
    Keysym(u32, Mods),
}

/// The input thread, if the compositor offers the protocols.
pub fn start() -> Option<Sender<Ev>> {
    let (tx, rx) = channel();
    let (ok_tx, ok_rx) = channel();
    std::thread::spawn(move || match Injector::connect() {
        Ok(mut inj) => {
            let _ = ok_tx.send(true);
            inj.run(rx);
        }
        Err(e) => {
            eprintln!("[prism-phoned] remote input unavailable: {e}");
            let _ = ok_tx.send(false);
        }
    });
    ok_rx.recv().ok().filter(|ok| *ok).map(|_| tx)
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);

struct Injector {
    conn: Connection,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    /// Keysyms in the uploaded keymap; keysym `syms[i]` is evdev keycode `i + 1`.
    syms: Vec<u32>,
    epoch: Instant,
}

impl Injector {
    fn connect() -> Result<Injector, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|_| "no wl_seat".to_string())?;
        let pm: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).map_err(|_| "compositor lacks zwlr_virtual_pointer_manager_v1".to_string())?;
        let km: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).map_err(|_| "compositor lacks zwp_virtual_keyboard_manager_v1".to_string())?;
        let pointer = pm.create_virtual_pointer(Some(&seat), &qh, ());
        let keyboard = km.create_virtual_keyboard(&seat, &qh, ());
        queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
        let mut inj = Injector { conn, pointer, keyboard, syms: Vec::new(), epoch: Instant::now() };
        // Start with ASCII and the special keys so ordinary typing never re-uploads the keymap.
        let mut base: Vec<u32> = (0x20..0x7f).collect();
        base.extend(SPECIAL_KEYS.iter().filter_map(|k| *k));
        inj.upload(base)?;
        Ok(inj)
    }

    fn time(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    fn run(&mut self, rx: Receiver<Ev>) {
        for ev in rx {
            self.apply(ev);
            if self.conn.flush().is_err() {
                eprintln!("[prism-phoned] Wayland connection lost; remote input stops");
                return;
            }
        }
    }

    fn apply(&mut self, ev: Ev) {
        let t = self.time();
        match ev {
            Ev::Motion(dx, dy) => {
                self.pointer.motion(t, dx, dy);
                self.pointer.frame();
            }
            Ev::Button(b, down) => {
                self.pointer.button(t, b, if down { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released });
                self.pointer.frame();
            }
            Ev::Click(b, n) => {
                for _ in 0..n {
                    self.pointer.button(t, b, wl_pointer::ButtonState::Pressed);
                    self.pointer.frame();
                    self.pointer.button(t, b, wl_pointer::ButtonState::Released);
                    self.pointer.frame();
                }
            }
            Ev::Scroll(dx, dy) => {
                self.pointer.axis_source(wl_pointer::AxisSource::Finger);
                if dy != 0.0 {
                    self.pointer.axis(t, wl_pointer::Axis::VerticalScroll, dy);
                }
                if dx != 0.0 {
                    self.pointer.axis(t, wl_pointer::Axis::HorizontalScroll, dx);
                }
                self.pointer.frame();
            }
            Ev::Text(s, mods) => {
                for c in s.chars() {
                    let sym = xkb::utf32_to_keysym(c as u32).raw();
                    if sym != 0 {
                        self.tap(sym, mods);
                    }
                }
            }
            Ev::Keysym(sym, mods) => self.tap(sym, mods),
        }
    }

    fn tap(&mut self, sym: u32, mods: Mods) {
        let code = match self.syms.iter().position(|s| *s == sym) {
            Some(i) => i as u32 + 1,
            None => {
                let mut syms = self.syms.clone();
                if syms.len() >= 240 {
                    syms.truncate((0x7f - 0x20) + SPECIAL_KEYS.iter().flatten().count());
                }
                syms.push(sym);
                if let Err(e) = self.upload(syms) {
                    eprintln!("[prism-phoned] keymap: {e}");
                    return;
                }
                self.syms.len() as u32
            }
        };
        let t = self.time();
        let mask = mods.mask();
        if mask != 0 {
            self.keyboard.modifiers(mask, 0, 0, 0);
        }
        self.keyboard.key(t, code, 1);
        self.keyboard.key(t, code, 0);
        if mask != 0 {
            self.keyboard.modifiers(0, 0, 0, 0);
        }
    }

    fn upload(&mut self, syms: Vec<u32>) -> Result<(), String> {
        let text = keymap_text(&syms);
        let fd = unsafe { libc::memfd_create(c"prism-phone-keymap".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err("memfd_create failed".into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut f = std::fs::File::from(fd);
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        f.write_all(&[0]).map_err(|e| e.to_string())?;
        self.keyboard.keymap(1, f.as_fd(), text.len() as u32 + 1);
        self.syms = syms;
        Ok(())
    }
}

/// An XKB keymap with keysym `syms[i]` on keycode `i + 9`.
fn keymap_text(syms: &[u32]) -> String {
    let mut codes = String::new();
    let mut symbols = String::new();
    for (i, s) in syms.iter().enumerate() {
        let name = xkb::keysym_get_name(xkb::Keysym::new(*s));
        codes.push_str(&format!("    <K{i}> = {};\n", i + 9));
        symbols.push_str(&format!("    key <K{i}> {{ [ {name} ] }};\n"));
    }
    format!(
        "xkb_keymap {{\nxkb_keycodes \"prism\" {{\n    minimum = 8;\n    maximum = 255;\n{codes}}};\nxkb_types \"prism\" {{ include \"complete\" }};\nxkb_compatibility \"prism\" {{ include \"complete\" }};\nxkb_symbols \"prism\" {{\n{symbols}}};\n}};\n"
    )
}

/// KDE Connect `specialKey` codes (kdeconnect-kde mousepad plugin) to keysyms.
pub const SPECIAL_KEYS: [Option<u32>; 33] = [
    None,         // 0 invalid
    Some(0xff08), // 1 BackSpace
    Some(0xff09), // 2 Tab
    Some(0xff0a), // 3 Linefeed
    Some(0xff51), // 4 Left
    Some(0xff52), // 5 Up
    Some(0xff53), // 6 Right
    Some(0xff54), // 7 Down
    Some(0xff55), // 8 Page_Up
    Some(0xff56), // 9 Page_Down
    Some(0xff50), // 10 Home
    Some(0xff57), // 11 End
    Some(0xff0d), // 12 Return
    Some(0xffff), // 13 Delete
    Some(0xff1b), // 14 Escape
    Some(0xff15), // 15 Sys_Req
    Some(0xff14), // 16 Scroll_Lock
    None,         // 17
    None,         // 18
    None,         // 19
    None,         // 20
    Some(0xffbe), // 21 F1
    Some(0xffbf), // 22 F2
    Some(0xffc0), // 23 F3
    Some(0xffc1), // 24 F4
    Some(0xffc2), // 25 F5
    Some(0xffc3), // 26 F6
    Some(0xffc4), // 27 F7
    Some(0xffc5), // 28 F8
    Some(0xffc6), // 29 F9
    Some(0xffc7), // 30 F10
    Some(0xffc8), // 31 F11
    Some(0xffc9), // 32 F12
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_compiles() {
        let syms: Vec<u32> = vec![0x61, 0x41, 0xff0d, xkb::utf32_to_keysym('é' as u32).raw(), xkb::utf32_to_keysym('😀' as u32).raw()];
        let text = keymap_text(&syms);
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let km = xkb::Keymap::new_from_string(&ctx, text, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS).expect("keymap compiles");
        let st = xkb::State::new(&km);
        assert_eq!(st.key_get_one_sym(xkb::Keycode::new(9)).raw(), 0x61);
        assert_eq!(st.key_get_utf8(xkb::Keycode::new(12)), "é");
        assert_eq!(st.key_get_utf8(xkb::Keycode::new(13)), "😀");
        assert_eq!(Mods { ctrl: true, shift: true, ..Default::default() }.mask(), 0b101);
    }
}
