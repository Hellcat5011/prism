//! `cargo run --example probe -- <out.raw>`: list outputs and windows, capture the first output
//! as raw BGRA (prints width height), and time it.
fn main() {
    let conn = wayland_client::Connection::connect_to_env().unwrap();
    let mut c = prism_capture::Capturer::new(&conn).unwrap();
    for o in c.outputs() {
        println!("output {} {}x{}+{}+{}", o.name, o.width, o.height, o.x, o.y);
    }
    println!("windows: {}", c.windows().len());
    for w in c.windows().iter().take(3) {
        println!("  {:?} {:?}", w.app_id, w.title.chars().take(40).collect::<String>());
    }
    let out = c.outputs()[0].clone();
    let t = std::time::Instant::now();
    let img = c.capture_output(&out, false).unwrap();
    println!("captured {}x{} in {:?}", img.width, img.height, t.elapsed());
    std::fs::write(std::env::args().nth(1).unwrap(), &img.data).unwrap();
    if let Some(w) = c.windows().first().cloned() {
        let t = std::time::Instant::now();
        match c.capture_window(&w, false) {
            Ok(i) => println!("window {:?}: {}x{} in {:?}", w.app_id, i.width, i.height, t.elapsed()),
            Err(e) => println!("window capture: {e}"),
        }
    }
}
