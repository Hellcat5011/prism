//! One KDE Connect link: TCP, the plain-text identity, TLS (roles reversed from TCP), the v8
//! identity exchange inside TLS, then a poll loop reading packets and writing queued ones.

use crate::core::{LinkHandle, Shared};
use crate::packet::{Packet, PeerInfo, MAX_LINE, PROTOCOL_VERSION};
use crate::tls::{self, Tls};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc::{channel, TryRecvError};
use std::time::{Duration, Instant};

/// We open the TCP connection after the peer's UDP identity (`peer`): send our identity in plain
/// text, act as the TLS server, and send our identity again inside TLS. The peer (TLS client)
/// sends nothing more about itself; its UDP identity is what we go by (kdeconnectd 26.08 does
/// the same).
pub fn connect_out(shared: Shared, addr: SocketAddr, peer: PeerInfo) {
    let run = || -> io::Result<()> {
        let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        tls::keepalive(&sock);
        let ident = shared.lock().unwrap().identity_packet(true);
        sock.write_all(ident.to_line().as_bytes())?;
        let cfg = shared.lock().unwrap().tls.clone();
        let mut t = Tls::handshake(sock, &cfg, true)?;
        let ident = shared.lock().unwrap().identity_packet(false);
        t.write_all(ident.to_line().as_bytes())?;
        run_secure(&shared, t, addr, peer.clone(), false)
    };
    if let Err(e) = run() {
        eprintln!("[prism-phoned] link to {addr} failed: {e}");
    }
}

/// The peer opened the TCP connection: read its plain-text identity, act as the TLS client, and
/// take the identity it sends again inside TLS (falling back to the plain-text one).
pub fn accept_in(shared: Shared, mut sock: TcpStream) {
    let addr = match sock.peer_addr() {
        Ok(a) => a,
        Err(_) => return,
    };
    let mut run = || -> io::Result<()> {
        tls::keepalive(&sock);
        sock.set_read_timeout(Some(Duration::from_secs(10)))?;
        let line = read_plain_line(&mut sock)?;
        debug("plain <", &line);
        let info = Packet::parse(&line).and_then(|p| PeerInfo::from_identity(&p)).ok_or_else(|| io::Error::other("no identity packet"))?;
        if info.protocol < PROTOCOL_VERSION {
            return Err(io::Error::other(format!("{} speaks protocol {}, need {PROTOCOL_VERSION} (update KDE Connect)", info.name, info.protocol)));
        }
        let cfg = shared.lock().unwrap().tls.clone();
        let t = Tls::handshake(sock.try_clone()?, &cfg, false)?;
        run_secure(&shared, t, addr, info, true)
    };
    if let Err(e) = run() {
        eprintln!("[prism-phoned] link from {addr} failed: {e}");
    }
}

/// `PRISM_PHONE_DEBUG=1` logs every packet line.
pub fn debug(dir: &str, line: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("PRISM_PHONE_DEBUG").is_some()) {
        eprintln!("[prism-phoned] {dir} {}", line.chars().take(600).collect::<String>());
    }
}

/// Byte by byte, so no TLS bytes are swallowed.
fn read_plain_line(sock: &mut TcpStream) -> io::Result<String> {
    let mut out = Vec::with_capacity(512);
    let mut b = [0u8; 1];
    loop {
        if sock.read(&mut b)? == 0 {
            return Err(io::Error::other("closed before identity"));
        }
        if b[0] == b'\n' {
            return String::from_utf8(out).map_err(io::Error::other);
        }
        out.push(b[0]);
        if out.len() > MAX_LINE {
            return Err(io::Error::other("identity line too long"));
        }
    }
}

fn poll2(a: i32, b: i32, timeout_ms: i32) -> io::Result<(i16, i16)> {
    let mut fds = [libc::pollfd { fd: a, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: b, events: libc::POLLIN, revents: 0 }];
    let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok((0, 0)) } else { Err(e) };
    }
    Ok((fds[0].revents, fds[1].revents))
}

/// Next complete line in `buf`, if any.
fn take_line(buf: &mut Vec<u8>) -> Option<String> {
    let pos = buf.iter().position(|&b| b == b'\n')?;
    let line: Vec<u8> = buf.drain(..=pos).collect();
    Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned())
}

fn run_secure(shared: &Shared, mut t: Tls, addr: SocketAddr, known: PeerInfo, expect_identity: bool) -> io::Result<()> {
    let cert = t.peer_cert().ok_or_else(|| io::Error::other("peer sent no certificate"))?.to_vec();
    let mut buf = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(if expect_identity { 3 } else { 0 });
    let sock_fd = t.sock.as_raw_fd();
    let info = loop {
        if let Some(line) = take_line(&mut buf) {
            debug("tls <", &line);
            match Packet::parse(&line) {
                Some(p) => match PeerInfo::from_identity(&p) {
                    Some(info) if info.id == known.id => break info,
                    Some(_) => return Err(io::Error::other("identity inside TLS names another device")),
                    None => {
                        // Not an identity: keep it for the packet loop.
                        let mut rest = line.into_bytes();
                        rest.push(b'\n');
                        rest.extend_from_slice(&buf);
                        buf = rest;
                        break known;
                    }
                },
                None => continue,
            }
        }
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as i32;
        if left <= 0 {
            break known;
        }
        let (r, _) = poll2(sock_fd, -1, left)?;
        if r != 0 {
            if !t.read_socket()? {
                return Err(io::Error::other("closed during identity exchange"));
            }
            t.drain_plaintext(&mut buf)?;
        }
    };
    if info.protocol < PROTOCOL_VERSION || !crate::identity::valid_device_id(&info.id) {
        return Err(io::Error::other(format!("unsupported peer {} (protocol {})", info.id, info.protocol)));
    }

    let mut pipe = [0i32; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (wake_r, wake_w) = unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    let (tx, rx) = channel::<Packet>();
    let id = info.id.clone();
    let serial = {
        let mut core = shared.lock().unwrap();
        let serial = core.next_serial();
        let handle = LinkHandle { serial, tx, wake: wake_w, cert };
        core.link_up(info, handle.cert.clone(), addr.ip(), handle).map_err(io::Error::other)?;
        serial
    };
    t.sock.set_read_timeout(None)?;

    let result = (|| -> io::Result<()> {
        loop {
            let (rs, rw) = poll2(sock_fd, wake_r.as_raw_fd(), -1)?;
            if rw != 0 {
                let mut junk = [0u8; 256];
                let n = unsafe { libc::read(wake_r.as_raw_fd(), junk.as_mut_ptr() as *mut libc::c_void, junk.len()) };
                if n == 0 {
                    return Ok(()); // handle dropped: replaced or shut down
                }
                loop {
                    match rx.try_recv() {
                        Ok(p) => {
                            let l = p.to_line();
                            debug(">", l.trim_end());
                            t.write_all(l.as_bytes())?
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return Ok(()),
                    }
                }
            }
            if rs != 0 {
                if !t.read_socket()? {
                    return Ok(());
                }
                t.drain_plaintext(&mut buf)?;
                while let Some(line) = take_line(&mut buf) {
                    debug("<", &line);
                    if let Some(p) = Packet::parse(&line) {
                        shared.lock().unwrap().on_packet(&id, p);
                    }
                }
                if buf.len() > MAX_LINE {
                    return Err(io::Error::other("packet too large"));
                }
            }
        }
    })();
    shared.lock().unwrap().link_down(&id, serial);
    if let Err(e) = &result {
        eprintln!("[prism-phoned] link {id} ended: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines() {
        let mut b = b"{\"a\":1}\n{\"b\"".to_vec();
        assert_eq!(take_line(&mut b).as_deref(), Some("{\"a\":1}"));
        assert_eq!(take_line(&mut b), None);
        b.extend_from_slice(b":2}\n");
        assert_eq!(take_line(&mut b).as_deref(), Some("{\"b\":2}"));
    }
}
