//! `cargo run --release -p prism-download --example fetch -- <url> <file>`: one download with
//! progress, for trying the engine against real servers (PRISM_DL_DEBUG=1 logs connections).
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let req = prism_download::Request { url: a[1].clone(), ..Default::default() };
    let out = std::path::Path::new(&a[2]);
    let h = if a[1].contains(".m3u8") {
        prism_download::hls::start(req, None, &out.with_extension("work"), out, Default::default()).expect("start")
    } else {
        let p = prism_download::probe(&req).expect("probe");
        eprintln!("{p:?}");
        prism_download::start(req, &p, out, None, Default::default()).expect("start")
    };
    let t = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let pr = h.progress();
        eprintln!("{:>5.1}s {:>6.1} MB  {} conn  {:?}", t.elapsed().as_secs_f64(), pr.done as f64 / 1e6, pr.connections, pr.state);
        if pr.state != prism_download::State::Running {
            break;
        }
    }
}
