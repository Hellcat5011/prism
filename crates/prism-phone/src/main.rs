//! prism-phoned: Prism's phone sync daemon. Speaks the KDE Connect protocol (v8) with the
//! official KDE Connect Android app. Supervised by prismd while `phone_enabled` is on; controlled
//! over `$XDG_RUNTIME_DIR/prism-phone.sock` (see `prism_common::phone`).

mod core;
mod ctl;
mod discovery;
mod identity;
mod input;
mod link;
mod notify;
mod packet;
mod payload;
mod plugins;
mod tls;
mod tray;
mod trust;

use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() {
    if let Err(e) = run() {
        eprintln!("[prism-phoned] {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let listener = ctl::bind()?;
    let dir = identity::data_dir();
    let ident = identity::load_or_create(&dir)?;
    let tls = tls::configs(&ident)?;
    let cfg = prism_common::Config::load();
    let tcp = discovery::bind_tcp().ok_or("no free TCP port in 1716-1764")?;
    let tcp_port = tcp.local_addr().map(|a| a.port()).unwrap_or(1716);
    let udp_port = std::env::var("PRISM_PHONE_UDP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(discovery::UDP_PORT);

    let mut core = core::Core::new(ident, tls, cfg, dir);
    core.tcp_port = tcp_port;
    core.udp_port = udp_port;
    eprintln!("[prism-phoned] {} ({}) on TCP {tcp_port}", core.name(), core.ident.id);
    let shared = Arc::new(Mutex::new(core));
    shared.lock().unwrap().me = Arc::downgrade(&shared);
    tray::sync(&shared.lock().unwrap());

    if let Err(e) = discovery::start_udp(shared.clone(), udp_port) {
        let msg = format!("{e}. Phones cannot find Prism: is kdeconnectd or GSConnect running?");
        eprintln!("[prism-phoned] {msg}");
        shared.lock().unwrap().error = Some(msg);
    }
    discovery::start_tcp(shared.clone(), tcp);
    discovery::start_announcer(shared.clone());
    notify::start(shared.clone());
    plugins::clipboard::start_watcher(shared.clone());
    {
        let sh = shared.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            sh.lock().unwrap().tick();
        });
    }
    ctl::serve(shared, listener);
    Ok(())
}
