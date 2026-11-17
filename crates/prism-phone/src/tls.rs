//! TLS for KDE Connect links. Certificates are self-signed, so the handshake accepts any peer
//! certificate (signatures are still checked) and trust is decided afterwards by comparing the
//! peer's certificate with the one pinned when it was paired (`core.rs`).
//!
//! Roles are reversed from the TCP roles: the side that opened the TCP connection is the TLS
//! server (kdeconnect-kde `LanLinkProvider`). Payload sockets: the side that listens is the TLS
//! server.

use crate::identity::Identity;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct TlsConfigs {
    pub server: Arc<ServerConfig>,
    pub client: Arc<ClientConfig>,
}

#[derive(Debug)]
struct AnyPeer(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AnyPeer {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

impl ClientCertVerifier for AnyPeer {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
    fn client_auth_mandatory(&self) -> bool {
        true
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

pub fn configs(id: &Identity) -> Result<TlsConfigs, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AnyPeer(provider.signature_verification_algorithms));
    let versions = &[&rustls::version::TLS13, &rustls::version::TLS12];
    let server = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(vec![id.cert.clone()], id.key.clone_key())
        .map_err(|e| e.to_string())?;
    let mut client = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![id.cert.clone()], id.key.clone_key())
        .map_err(|e| e.to_string())?;
    client.enable_sni = false;
    Ok(TlsConfigs { server: Arc::new(server), client: Arc::new(client) })
}

/// A TLS connection over a TCP socket, handshaken.
pub struct Tls {
    pub conn: rustls::Connection,
    pub sock: TcpStream,
}

impl Tls {
    /// Run the handshake (blocking, with a timeout) in the given role.
    pub fn handshake(mut sock: TcpStream, cfg: &TlsConfigs, as_server: bool) -> io::Result<Tls> {
        let mut conn: rustls::Connection = if as_server {
            rustls::ServerConnection::new(cfg.server.clone()).map_err(io::Error::other)?.into()
        } else {
            let name = ServerName::try_from("kdeconnect").map_err(io::Error::other)?;
            rustls::ClientConnection::new(cfg.client.clone(), name).map_err(io::Error::other)?.into()
        };
        sock.set_read_timeout(Some(Duration::from_secs(10)))?;
        sock.set_write_timeout(Some(Duration::from_secs(10)))?;
        while conn.is_handshaking() {
            conn.complete_io(&mut sock)?;
        }
        Ok(Tls { conn, sock })
    }

    pub fn peer_cert(&self) -> Option<CertificateDer<'static>> {
        self.conn.peer_certificates()?.first().map(|c| c.clone().into_owned())
    }

    /// Write everything, pushing TLS records out as rustls' plaintext buffer fills.
    pub fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let n = self.conn.writer().write(data)?;
            data = &data[n..];
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        while self.conn.wants_write() {
            self.conn.write_tls(&mut self.sock)?;
        }
        Ok(())
    }

    /// Read what is available from the socket (call when it is readable). Ok(false) = EOF.
    pub fn read_socket(&mut self) -> io::Result<bool> {
        let n = self.conn.read_tls(&mut self.sock)?;
        if n == 0 {
            return Ok(false);
        }
        self.conn.process_new_packets().map_err(io::Error::other)?;
        self.flush()?;
        Ok(true)
    }

    /// Plaintext already decrypted, appended to `buf`.
    pub fn drain_plaintext(&mut self, buf: &mut Vec<u8>) -> io::Result<()> {
        let mut chunk = [0u8; 16384];
        loop {
            match self.conn.reader().read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}

/// Blocking stream view (payload transfers).
impl Read for Tls {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.conn.reader().read(buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !self.read_socket()? {
                        return Ok(0);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) => return Err(e),
            }
        }
    }
}

impl Write for Tls {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Tls::write_all(self, buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Tls::flush(self)
    }
}

/// TCP keep-alive so a phone that vanished (Wi-Fi off) is noticed in about 30 s.
pub fn keepalive(sock: &TcpStream) {
    use std::os::fd::AsRawFd;
    let fd = sock.as_raw_fd();
    let set = |level, opt, v: libc::c_int| unsafe {
        libc::setsockopt(fd, level, opt, &v as *const _ as *const libc::c_void, std::mem::size_of::<libc::c_int>() as libc::socklen_t);
    };
    set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 10);
    set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 5);
    set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3);
    set(libc::IPPROTO_TCP, libc::TCP_NODELAY, 1);
}
