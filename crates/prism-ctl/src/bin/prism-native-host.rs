//! Native messaging host for the Prism Downloads Firefox extension (`extension/`).
//!
//! Firefox starts it when the extension connects (manifest `prism_downloads.json` in
//! `native-messaging-hosts`, written by deploy/install.sh) and talks over stdin/stdout:
//! each message is a 32-bit native-endian length and that much JSON. A request is
//! `{"id": 1, "command": {"target": "download", "action": ...}}` (an `IpcCommand`); only
//! download commands pass, so the extension can never do anything else to the session. The
//! answer is `{"id": 1, "ok": true, "data": ...}` (`data` parsed when it is JSON) or
//! `{"id": 1, "ok": false, "error": "..."}`. `{"id": 1, "ping": true}` answers whether prismd
//! is reachable.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use prism_common::ipc::{get_socket_path, IpcCommand, IpcResponse};
use serde_json::{json, Value};

/// Firefox's limit for one message to the extension is 1 MB.
const MAX_IN: u32 = 4 << 20;

fn read_msg(r: &mut impl Read) -> Option<Value> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let len = u32::from_ne_bytes(len);
    if len > MAX_IN {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn write_msg(w: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let b = serde_json::to_vec(v).unwrap_or_default();
    w.write_all(&(b.len() as u32).to_ne_bytes())?;
    w.write_all(&b)?;
    w.flush()
}

fn ask(cmd: &IpcCommand) -> Result<Option<String>, String> {
    let mut s = UnixStream::connect(get_socket_path()).map_err(|_| "Prism is not running".to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
    let mut line = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    BufReader::new(s).read_line(&mut answer).map_err(|e| e.to_string())?;
    match serde_json::from_str::<IpcResponse>(&answer) {
        Ok(IpcResponse::Ok(data)) => Ok(data),
        Ok(IpcResponse::Error(e)) => Err(e),
        Err(_) => Err("unexpected answer from Prism".into()),
    }
}

fn handle(msg: &Value) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    if msg.get("ping").is_some() {
        return match ask(&IpcCommand::Ping) {
            Ok(_) => json!({"id": id, "ok": true, "data": {"version": env!("CARGO_PKG_VERSION")}}),
            Err(e) => json!({"id": id, "ok": false, "error": e}),
        };
    }
    let cmd = match msg.get("command").cloned().map(serde_json::from_value::<IpcCommand>) {
        Some(Ok(c @ IpcCommand::Download(_))) => c,
        Some(Ok(_)) => return json!({"id": id, "ok": false, "error": "only download commands are allowed"}),
        _ => return json!({"id": id, "ok": false, "error": "not a Prism command"}),
    };
    match ask(&cmd) {
        Ok(data) => {
            let data = data.map(|d| serde_json::from_str::<Value>(&d).unwrap_or(Value::String(d))).unwrap_or(Value::Null);
            json!({"id": id, "ok": true, "data": data})
        }
        Err(e) => json!({"id": id, "ok": false, "error": e}),
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    while let Some(msg) = read_msg(&mut input) {
        if write_msg(&mut output, &handle(&msg)).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trip() {
        let mut buf = Vec::new();
        write_msg(&mut buf, &json!({"id": 3, "ping": true})).unwrap();
        assert_eq!(u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize, buf.len() - 4);
        assert_eq!(read_msg(&mut &buf[..]), Some(json!({"id": 3, "ping": true})));
    }

    #[test]
    fn only_download_commands() {
        let v = handle(&json!({"id": 1, "command": {"target": "power", "action": "shutdown"}}));
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "only download commands are allowed");
        let v = handle(&json!({"id": 2, "command": "nonsense"}));
        assert_eq!(v["error"], "not a Prism command");
    }
}
