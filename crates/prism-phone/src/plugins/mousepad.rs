//! Remote input: the phone as touchpad and keyboard (`kdeconnect.mousepad.request`) and as a
//! presentation pointer (`kdeconnect.presenter`). Events go to `input.rs`.

use crate::core::Core;
use crate::input::{self, Ev, Mods, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, SPECIAL_KEYS};
use crate::packet::Packet;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct InputState {
    tx: Option<Sender<Ev>>,
    tried: bool,
    locked: bool,
    locked_checked: Option<Instant>,
    refused_logged: bool,
}

impl InputState {
    fn sender(&mut self) -> Option<&Sender<Ev>> {
        if !self.tried {
            self.tried = true;
            self.tx = input::start();
        }
        self.tx.as_ref()
    }

    /// Is the session locked? Asks prismd at most once a second.
    fn locked(&mut self) -> bool {
        if self.locked_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            self.locked_checked = Some(Instant::now());
            self.locked = prismd_says_locked();
        }
        self.locked
    }
}

/// Locked unless prismd clearly says otherwise: no answer must never open remote input.
fn prismd_says_locked() -> bool {
    use std::io::{BufRead, BufReader, Write};
    let Ok(mut s) = std::os::unix::net::UnixStream::connect(prism_common::get_socket_path()) else { return true };
    s.set_read_timeout(Some(Duration::from_millis(300))).ok();
    let cmd = prism_common::IpcCommand::Lock(prism_common::ipc::LockAction::Status);
    if writeln!(s, "{}", serde_json::to_string(&cmd).unwrap_or_default()).is_err() {
        return true;
    }
    let mut line = String::new();
    let _ = BufReader::new(s).read_line(&mut line);
    !reply_says_unlocked(&line)
}

/// prismd's `lock status` reply: `{"status":"ok","data":"unlocked, idle=false"}`.
fn reply_says_unlocked(line: &str) -> bool {
    line.contains("\"data\":\"unlocked")
}

/// Turn one mousepad packet into input events (kdeconnect-kde MousepadPlugin semantics).
pub fn events(p: &Packet) -> Vec<Ev> {
    let dx = p.float("dx").unwrap_or(0.0);
    let dy = p.float("dy").unwrap_or(0.0);
    let mods = Mods { shift: p.bool("shift"), ctrl: p.bool("ctrl"), alt: p.bool("alt"), logo: p.bool("super") };
    let key = p.str("key").unwrap_or_default();
    let special = p.int("specialKey").unwrap_or(0);
    let mut out = Vec::new();
    if p.bool("singleclick") {
        out.push(Ev::Click(BTN_LEFT, 1));
    } else if p.bool("doubleclick") {
        out.push(Ev::Click(BTN_LEFT, 2));
    } else if p.bool("middleclick") {
        out.push(Ev::Click(BTN_MIDDLE, 1));
    } else if p.bool("rightclick") {
        out.push(Ev::Click(BTN_RIGHT, 1));
    } else if p.bool("singlehold") {
        out.push(Ev::Button(BTN_LEFT, true));
    } else if p.bool("singlerelease") {
        out.push(Ev::Button(BTN_LEFT, false));
    } else if p.bool("scroll") {
        out.push(Ev::Scroll(-dx, -dy));
    } else if special > 0 {
        if let Some(Some(sym)) = SPECIAL_KEYS.get(special as usize) {
            out.push(Ev::Keysym(*sym, mods));
        }
    } else if !key.is_empty() {
        out.push(Ev::Text(key.to_string(), mods));
    } else if dx != 0.0 || dy != 0.0 {
        out.push(Ev::Motion(dx, dy));
    }
    out
}

pub fn handle(core: &mut Core, id: &str, p: &Packet) {
    if !core.cfg.phone_allow_input {
        return;
    }
    let state = &mut core.plugins.input;
    if !core.cfg.phone_input_when_locked && state.locked() {
        if !state.refused_logged {
            eprintln!("[prism-phoned] remote input refused while the session is locked");
            state.refused_logged = true;
        }
        return;
    }
    state.refused_logged = false;
    let evs = match p.kind.as_str() {
        "kdeconnect.presenter" => {
            if p.bool("stop") {
                Vec::new()
            } else {
                // Presenter deltas are fractions of the screen; about 1000 px per unit.
                vec![Ev::Motion(p.float("dx").unwrap_or(0.0) * 1000.0, p.float("dy").unwrap_or(0.0) * 1000.0)]
            }
        }
        _ => events(p),
    };
    if let Some(tx) = state.sender() {
        for e in evs {
            let _ = tx.send(e);
        }
    }
    if p.kind == "kdeconnect.mousepad.request" && p.bool("sendAck") {
        let mut body = p.body.clone();
        if let Some(o) = body.as_object_mut() {
            o.insert("isAck".into(), true.into());
        }
        core.send(id, Packet::new("kdeconnect.mousepad.echo", body));
    }
}

/// Tell the phone keyboard input is accepted (enables its keyboard in the remote input screen).
pub fn on_connected(core: &mut Core, id: &str) {
    let on = core.cfg.phone_allow_input;
    core.send(id, Packet::new("kdeconnect.mousepad.keyboardstate", serde_json::json!({ "state": on })));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(body: &str) -> Packet {
        Packet::parse(&format!(r#"{{"id":1,"type":"kdeconnect.mousepad.request","body":{body}}}"#)).unwrap()
    }

    #[test]
    fn only_a_clear_answer_counts_as_unlocked() {
        let reply = |r| serde_json::to_string(&r).unwrap();
        assert!(reply_says_unlocked(&reply(prism_common::ipc::IpcResponse::Ok(Some("unlocked, idle=false".into())))));
        assert!(!reply_says_unlocked(&reply(prism_common::ipc::IpcResponse::Ok(Some("locked, idle=false".into())))));
        assert!(!reply_says_unlocked(""));
        assert!(!reply_says_unlocked("{\"status\":\"error\",\"data\":\"busy\"}"));
    }

    #[test]
    fn maps_mousepad_packets() {
        assert_eq!(events(&pk(r#"{"dx":3.5,"dy":-2}"#)), vec![Ev::Motion(3.5, -2.0)]);
        assert_eq!(events(&pk(r#"{"singleclick":true}"#)), vec![Ev::Click(BTN_LEFT, 1)]);
        assert_eq!(events(&pk(r#"{"rightclick":true}"#)), vec![Ev::Click(BTN_RIGHT, 1)]);
        assert_eq!(events(&pk(r#"{"singlehold":true}"#)), vec![Ev::Button(BTN_LEFT, true)]);
        assert_eq!(events(&pk(r#"{"specialKey":12}"#)), vec![Ev::Keysym(0xff0d, Mods::default())]);
        assert_eq!(events(&pk(r#"{"key":"c","ctrl":true}"#)), vec![Ev::Text("c".into(), Mods { ctrl: true, ..Default::default() })]);
        assert_eq!(events(&pk(r#"{"specialKey":17}"#)), vec![]);
        assert!(matches!(events(&pk(r#"{"scroll":true,"dx":0,"dy":5}"#))[0], Ev::Scroll(_, _)));
    }
}
