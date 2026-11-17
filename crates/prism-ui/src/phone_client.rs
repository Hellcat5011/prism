//! Live phone state for the Phone card and the desktop Phone widget: one thread subscribes to
//! prism-phoned (`prism_common::phone`), keeps the latest `PhoneState`, and wakes the main loop
//! when it changes. Reconnects every 2 s while phone sync is off or restarting.

use crate::waker::Waker;
use prism_common::phone::{get_phone_socket_path, phone_request, PhoneCommand, PhoneReply, PhoneState};
use std::io::{BufRead, BufReader, Write};
use std::sync::{Mutex, Once};
use std::time::Duration;

static STATE: Mutex<(u64, Option<PhoneState>)> = Mutex::new((0, None));
/// Last error from a command the user started (shown briefly in the card).
static LAST_ERROR: Mutex<(u64, Option<String>)> = Mutex::new((0, None));
static START: Once = Once::new();

pub fn start(waker: Waker) {
    START.call_once(|| {
        std::thread::spawn(move || loop {
            if let Ok(mut s) = std::os::unix::net::UnixStream::connect(get_phone_socket_path()) {
                let sub = serde_json::to_string(&PhoneCommand::Subscribe).unwrap_or_default();
                if writeln!(s, "{sub}").is_ok() {
                    for line in BufReader::new(s).lines().map_while(Result::ok) {
                        if let Ok(PhoneReply::State { state }) = serde_json::from_str(&line) {
                            let mut st = STATE.lock().unwrap();
                            if st.1.as_ref() != Some(&state) {
                                st.0 += 1;
                                st.1 = Some(state);
                                drop(st);
                                waker.wake();
                            }
                        }
                    }
                }
            }
            {
                let mut st = STATE.lock().unwrap();
                if st.1.is_some() {
                    st.0 += 1;
                    st.1 = None;
                    drop(st);
                    waker.wake();
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        });
    });
}

/// (generation, state); the generation changes whenever the state does. None = phone sync off.
pub fn snapshot() -> (u64, Option<PhoneState>) {
    STATE.lock().unwrap().clone()
}

pub fn generation() -> u64 {
    STATE.lock().unwrap().0 + LAST_ERROR.lock().unwrap().0
}

pub fn last_error() -> Option<String> {
    LAST_ERROR.lock().unwrap().1.clone()
}

/// Run a command off the UI thread; a failure is kept for the card to show.
pub fn command(cmd: PhoneCommand, waker: &Waker) {
    let waker = waker.clone();
    std::thread::spawn(move || {
        let err = match phone_request(&cmd) {
            Ok(PhoneReply::Error { message }) => Some(message),
            Err(e) => Some(e),
            _ => None,
        };
        let mut le = LAST_ERROR.lock().unwrap();
        le.0 += 1;
        le.1 = err;
        drop(le);
        waker.wake();
    });
}

pub fn clear_error() {
    LAST_ERROR.lock().unwrap().1 = None;
}
