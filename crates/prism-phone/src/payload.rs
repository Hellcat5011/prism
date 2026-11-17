//! Payloads (file bytes, album art, notification icons) travel on their own TLS socket. The
//! sender listens on a port in 1739..=1764 and names it in `payloadTransferInfo`; the receiver
//! connects. The listening side is the TLS server. Both ends must be the paired certificates.

use crate::core::Shared;
use crate::tls::{self, Tls};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

const PORTS: std::ops::RangeInclusive<u16> = 1739..=1764;

/// Address and pinned certificate of a connected device.
fn peer(shared: &Shared, id: &str) -> io::Result<(IpAddr, Vec<u8>, tls::TlsConfigs)> {
    let core = shared.lock().unwrap();
    let d = core.devices.get(id).ok_or_else(|| io::Error::other("device gone"))?;
    let cert = d.link.as_ref().map(|l| l.cert.clone()).ok_or_else(|| io::Error::other("device not connected"))?;
    Ok((d.addr.ok_or_else(|| io::Error::other("no address"))?, cert, core.tls.clone()))
}

/// Fetch a payload of `size` bytes (negative = until EOF) into `sink`. `progress(bytes)` is
/// called as data arrives; returning false cancels.
pub fn download(shared: &Shared, id: &str, port: u16, size: i64, sink: &mut dyn Write, mut progress: impl FnMut(u64) -> bool) -> io::Result<u64> {
    let (ip, cert, cfg) = peer(shared, id)?;
    let sock = TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_secs(10))?;
    tls::keepalive(&sock);
    let mut t = Tls::handshake(sock, &cfg, false)?;
    if t.peer_cert().map(|c| c.to_vec()) != Some(cert) {
        return Err(io::Error::other("payload from an unexpected certificate"));
    }
    t.sock.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got: u64 = 0;
    loop {
        let want = if size >= 0 { ((size as u64 - got) as usize).min(buf.len()) } else { buf.len() };
        if want == 0 {
            break;
        }
        let n = t.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        sink.write_all(&buf[..n])?;
        got += n as u64;
        if !progress(got) {
            return Err(io::Error::other("cancelled"));
        }
    }
    if size >= 0 && got != size as u64 {
        return Err(io::Error::other(format!("payload ended after {got} of {size} bytes")));
    }
    Ok(got)
}

/// Listen for the device to fetch a payload; returns the port at once. `body` runs on its own
/// thread with the connected TLS stream once the device connects (within 60 s).
pub fn serve(shared: &Shared, id: &str, body: impl FnOnce(&mut Tls) -> io::Result<()> + Send + 'static) -> io::Result<u16> {
    let (_ip, cert, cfg) = peer(shared, id)?;
    let listener = PORTS.clone().find_map(|p| TcpListener::bind(("0.0.0.0", p)).ok()).ok_or_else(|| io::Error::other("no free payload port in 1739-1764"))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        let sock = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    eprintln!("[prism-phoned] payload on {port}: no connection ({e})");
                    return;
                }
            }
        };
        let run = || -> io::Result<()> {
            sock.set_nonblocking(false)?;
            tls::keepalive(&sock);
            let mut t = Tls::handshake(sock, &cfg, true)?;
            if t.peer_cert().map(|c| c.to_vec()) != Some(cert) {
                return Err(io::Error::other("payload fetched by an unexpected certificate"));
            }
            t.sock.set_write_timeout(Some(Duration::from_secs(30)))?;
            body(&mut t)?;
            t.flush()?;
            t.conn.send_close_notify();
            let _ = t.flush();
            Ok(())
        };
        if let Err(e) = run() {
            eprintln!("[prism-phoned] payload on {port}: {e}");
        }
    });
    Ok(port)
}

/// Copy `src` into the payload stream; `progress(bytes)` returning false cancels.
pub fn pump(t: &mut Tls, src: &mut dyn Read, mut progress: impl FnMut(u64) -> bool) -> io::Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut sent = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok(sent);
        }
        t.write_all(&buf[..n])?;
        sent += n as u64;
        if !progress(sent) {
            return Err(io::Error::other("cancelled"));
        }
    }
}
