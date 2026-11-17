//! Finding phones: our identity goes out over UDP (broadcast, plus unicast to paired devices'
//! last addresses); a phone's UDP identity makes us open TCP to it. Phones that hear our
//! broadcast connect to our TCP listener instead.

use crate::core::Shared;
use crate::packet::{Packet, PeerInfo};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::thread;
use std::time::Duration;

/// KDE Connect ports: UDP 1716 for discovery, TCP 1716..=1764 for links.
pub const UDP_PORT: u16 = 1716;
const TCP_PORTS: std::ops::RangeInclusive<u16> = 1716..=1764;

pub fn bind_tcp() -> Option<TcpListener> {
    TCP_PORTS.clone().find_map(|p| TcpListener::bind((Ipv4Addr::UNSPECIFIED, p)).ok())
}

pub fn start_tcp(shared: Shared, listener: TcpListener) {
    thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let sh = shared.clone();
            thread::spawn(move || crate::link::accept_in(sh, s));
        }
    });
}

/// Listen for phones' UDP identities. Err = port taken (another KDE Connect daemon).
pub fn start_udp(shared: Shared, port: u16) -> Result<(), String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(|e| format!("UDP port {port}: {e}"))?;
    thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            let Some(info) = std::str::from_utf8(&buf[..n]).ok().and_then(Packet::parse).and_then(|p| PeerInfo::from_identity(&p)) else { continue };
            let Some(port) = info.tcp_port else { continue };
            if shared.lock().unwrap().should_connect(&info, from.ip()) {
                let sh = shared.clone();
                thread::spawn(move || crate::link::connect_out(sh, SocketAddr::new(from.ip(), port), info));
            }
        }
    });
    Ok(())
}

/// Send our identity to the LAN broadcast address and to every paired device's last address.
pub fn announce(shared: &Shared) {
    let (line, port, targets) = {
        let core = shared.lock().unwrap();
        let targets: Vec<IpAddr> = core.devices.values().filter(|d| d.view.paired && d.link.is_none()).filter_map(|d| d.addr).collect();
        (core.identity_packet(true).to_line(), core.udp_port, targets)
    };
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return };
    let _ = sock.set_broadcast(true);
    let _ = sock.send_to(line.as_bytes(), (Ipv4Addr::BROADCAST, port));
    for ip in targets {
        let _ = sock.send_to(line.as_bytes(), (ip, port));
    }
}

/// Manual connect ("add device by IP"): send our identity to "host" (or "host:port", default
/// 1716) over UDP so the device connects back.
pub fn connect_addr(shared: &Shared, addr: &str) -> Result<String, String> {
    let addr = addr.trim();
    let target: SocketAddr = match addr.parse::<SocketAddr>() {
        Ok(sa) => sa,
        Err(_) => SocketAddr::new(addr.parse::<IpAddr>().map_err(|_| format!("not an IP address: {addr}"))?, UDP_PORT),
    };
    let line = shared.lock().unwrap().identity_packet(true).to_line();
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| e.to_string())?;
    sock.send_to(line.as_bytes(), target).map_err(|e| e.to_string())?;
    Ok(format!("sent our identity to {target}; the device should connect back"))
}

/// Announce at start, then every minute while a paired device is away.
pub fn start_announcer(shared: Shared) {
    thread::spawn(move || {
        announce(&shared);
        loop {
            thread::sleep(Duration::from_secs(60));
            let missing = shared.lock().unwrap().devices.values().any(|d| d.view.paired && d.link.is_none());
            if missing {
                announce(&shared);
            }
        }
    });
}
