//! Control socket for prismd, prism-ctl and prism-ui (`prism_common::phone`).

use crate::core::Shared;
use prism_common::phone::{get_phone_socket_path, PhoneCommand, PhoneReply};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;

pub fn bind() -> Result<UnixListener, String> {
    let path = get_phone_socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err(format!("prism-phoned is already running ({})", path.display()));
    }
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn serve(shared: Shared, listener: UnixListener) {
    for s in listener.incoming().flatten() {
        let sh = shared.clone();
        thread::spawn(move || client(sh, s));
    }
}

fn client(shared: Shared, s: UnixStream) {
    let Ok(mut out) = s.try_clone() else { return };
    for line in BufReader::new(s).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<PhoneCommand>(&line) {
            Ok(PhoneCommand::Subscribe) => {
                if let Ok(c) = out.try_clone() {
                    shared.lock().unwrap().subscribe(c);
                }
                continue;
            }
            Ok(PhoneCommand::Refresh) => {
                crate::discovery::announce(&shared);
                PhoneReply::Ok { message: None }
            }
            Ok(PhoneCommand::Connect { addr }) => match crate::discovery::connect_addr(&shared, &addr) {
                Ok(m) => PhoneReply::Ok { message: Some(m) },
                Err(m) => PhoneReply::Error { message: m },
            },
            Ok(PhoneCommand::SendFiles { device, paths }) => crate::plugins::share::send_files(&shared, device, paths),
            Ok(cmd) => shared.lock().unwrap().command(cmd),
            Err(e) => PhoneReply::Error { message: format!("bad command: {e}") },
        };
        let mut l = serde_json::to_string(&reply).unwrap_or_default();
        l.push('\n');
        if out.write_all(l.as_bytes()).is_err() {
            return;
        }
    }
}
