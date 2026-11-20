//! MPRIS bridge for MPD (replaces mpd-mpris): serves `org.mpris.MediaPlayer2.mpd` on the
//! session bus while `mpd_mpris_enabled` is on, so media keys, the desktop media widget and
//! phone sync can control MPD like any other player.
//!
//! One MPD connection sits in `idle player mixer options` and refreshes the state on every
//! change; commands open a short-lived second connection. Album art comes from MPD itself
//! (`readpicture`, then `albumart`) and is cached in `$XDG_RUNTIME_DIR/prism-mpd-art/`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prism_common::Config;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const NAME: &str = "org.mpris.MediaPlayer2.mpd";
const PATH: &str = "/org/mpris/MediaPlayer2";

// ── MPD protocol ─────────────────────────────────────────────────────────

trait Io: Read + Write + Send {
    fn set_timeout(&self, t: Option<Duration>);
}
impl Io for TcpStream {
    fn set_timeout(&self, t: Option<Duration>) {
        let _ = self.set_read_timeout(t);
    }
}
impl Io for UnixStream {
    fn set_timeout(&self, t: Option<Duration>) {
        let _ = self.set_read_timeout(t);
    }
}

struct Mpd {
    r: BufReader<Box<dyn Io>>,
}

type Pairs = Vec<(String, String)>;

impl Mpd {
    fn connect(host: &str, port: u16, password: &str) -> Result<Mpd, String> {
        let s: Box<dyn Io> = if host.starts_with('/') || host.starts_with('@') {
            Box::new(UnixStream::connect(host).map_err(|e| format!("{host}: {e}"))?)
        } else {
            use std::net::ToSocketAddrs;
            let addr = (host, port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
            let s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| format!("{host}:{port}: {e}"))?;
            let _ = s.set_nodelay(true);
            Box::new(s)
        };
        s.set_timeout(Some(Duration::from_secs(5)));
        let mut m = Mpd { r: BufReader::new(s) };
        let mut hello = String::new();
        m.r.read_line(&mut hello).map_err(|e| e.to_string())?;
        if !hello.starts_with("OK MPD") {
            return Err(format!("not an MPD server: {}", hello.trim()));
        }
        if !password.is_empty() {
            m.cmd(&format!("password {}", quote(password)))?;
        }
        Ok(m)
    }

    fn send(&mut self, cmd: &str) -> Result<(), String> {
        let w = self.r.get_mut();
        w.write_all(cmd.as_bytes()).and_then(|_| w.write_all(b"\n")).map_err(|e| e.to_string())
    }

    /// Read `key: value` lines up to `OK`.
    fn read_pairs(&mut self) -> Result<Pairs, String> {
        let mut out = Vec::new();
        loop {
            let mut line = String::new();
            if self.r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("MPD closed the connection".into());
            }
            let line = line.trim_end_matches('\n');
            if line == "OK" {
                return Ok(out);
            }
            if let Some(err) = line.strip_prefix("ACK ") {
                return Err(err.to_string());
            }
            if let Some((k, v)) = line.split_once(": ") {
                out.push((k.to_string(), v.to_string()));
            }
        }
    }

    fn cmd(&mut self, cmd: &str) -> Result<Pairs, String> {
        self.send(cmd)?;
        self.read_pairs()
    }

    /// A binary command (`albumart`, `readpicture`): the whole picture, chunk by chunk.
    /// `Ok(None)` means the song has no such picture; `Err` means the transfer failed.
    fn picture(&mut self, cmd: &str, uri: &str) -> Result<Option<Vec<u8>>, String> {
        let err = |e: std::io::Error| e.to_string();
        let mut data = Vec::new();
        loop {
            self.send(&format!("{cmd} {} {}", quote(uri), data.len()))?;
            let mut size = 0usize;
            let chunk: usize;
            loop {
                let mut line = String::new();
                if self.r.read_line(&mut line).map_err(err)? == 0 {
                    return Err("MPD closed the connection".into());
                }
                let line = line.trim_end_matches('\n');
                if line == "OK" || line.starts_with("ACK ") {
                    if data.is_empty() {
                        return Ok(None);
                    }
                    return Err(format!("{cmd} ended early: {line}"));
                }
                if let Some(v) = line.strip_prefix("size: ") {
                    size = v.parse().map_err(|_| format!("bad size: {v}"))?;
                } else if let Some(v) = line.strip_prefix("binary: ") {
                    chunk = v.parse().map_err(|_| format!("bad chunk: {v}"))?;
                    break;
                }
            }
            let start = data.len();
            data.resize(start + chunk, 0);
            self.r.read_exact(&mut data[start..]).map_err(err)?;
            let mut rest = String::new();
            self.r.read_line(&mut rest).map_err(err)?; // newline after the data
            rest.clear();
            self.r.read_line(&mut rest).map_err(err)?; // OK
            if data.len() >= size {
                return Ok((!data.is_empty()).then_some(data));
            }
            if chunk == 0 {
                return Err(format!("{cmd} ended early at {} of {size} bytes", data.len()));
            }
        }
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn get<'a>(p: &'a Pairs, k: &str) -> Option<&'a str> {
    p.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
}

// ── State ────────────────────────────────────────────────────────────────

#[derive(Default, Clone, PartialEq)]
struct Song {
    id: String,
    file: String,
    title: String,
    artists: Vec<String>,
    album: String,
    album_artists: Vec<String>,
    track: Option<i32>,
    length_us: i64,
    art: Option<String>,
}

#[derive(Default)]
struct State {
    /// "Playing", "Paused" or "Stopped".
    status: String,
    song: Song,
    /// Elapsed time at `at`, in microseconds.
    elapsed_us: i64,
    at: Option<Instant>,
    volume: Option<f64>,
    repeat: bool,
    single: bool,
    random: bool,
}

impl State {
    fn position(&self) -> i64 {
        let extra = match (self.status.as_str(), self.at) {
            ("Playing", Some(at)) => at.elapsed().as_micros() as i64,
            _ => 0,
        };
        self.elapsed_us + extra
    }
}

#[derive(Clone)]
struct Target {
    host: String,
    port: u16,
    password: String,
}

impl Target {
    fn from(cfg: &Config) -> Target {
        Target { host: cfg.mpd_host.clone(), port: cfg.mpd_port, password: cfg.mpd_password.clone() }
    }

    /// Run one command on a fresh connection.
    fn run(&self, cmd: &str) -> zbus::fdo::Result<()> {
        Mpd::connect(&self.host, self.port, &self.password)
            .and_then(|mut m| m.cmd(cmd))
            .map(|_| ())
            .map_err(|e| zbus::fdo::Error::Failed(format!("MPD: {e}")))
    }
}

fn read_state(m: &mut Mpd) -> Result<State, String> {
    let st = m.cmd("status")?;
    let cur = m.cmd("currentsong")?;
    let status = match get(&st, "state") {
        Some("play") => "Playing",
        Some("pause") => "Paused",
        _ => "Stopped",
    };
    let secs = |k: &str, p: &Pairs| get(p, k).and_then(|v| v.parse::<f64>().ok());
    let all = |k: &str| cur.iter().filter(|(key, _)| key == k).map(|(_, v)| v.clone()).collect::<Vec<_>>();
    let length = secs("duration", &st).or_else(|| secs("duration", &cur)).or_else(|| secs("Time", &cur)).unwrap_or(0.0);
    let file = get(&cur, "file").unwrap_or("").to_string();
    let title = get(&cur, "Title").map(str::to_string).unwrap_or_else(|| file.rsplit('/').next().unwrap_or("").to_string());
    let song = Song {
        id: get(&cur, "Id").unwrap_or("").to_string(),
        file,
        title,
        artists: all("Artist"),
        album: get(&cur, "Album").unwrap_or("").to_string(),
        album_artists: all("AlbumArtist"),
        track: get(&cur, "Track").and_then(|t| t.split('/').next()?.parse().ok()),
        length_us: (length * 1e6) as i64,
        art: None,
    };
    let volume = get(&st, "volume").and_then(|v| v.parse::<f64>().ok()).filter(|v| *v >= 0.0).map(|v| v / 100.0);
    Ok(State {
        status: status.into(),
        song,
        elapsed_us: (secs("elapsed", &st).unwrap_or(0.0) * 1e6) as i64,
        at: Some(Instant::now()),
        volume,
        repeat: get(&st, "repeat") == Some("1"),
        single: get(&st, "single") == Some("1"),
        random: get(&st, "random") == Some("1"),
    })
}

fn art_dir() -> PathBuf {
    prism_common::runtime::dir().join("prism-mpd-art")
}

/// Cached cover for `file`, fetched from MPD when missing.
fn fetch_art(t: &Target, file: &str) -> Option<String> {
    use std::hash::{Hash, Hasher};
    if file.is_empty() || file.contains("://") {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file.hash(&mut h);
    let dir = art_dir();
    let path = dir.join(format!("{:016x}", h.finish()));
    let none = dir.join(format!("{:016x}.none", h.finish()));
    if path.exists() {
        return Some(format!("file://{}", path.display()));
    }
    if none.exists() {
        return None;
    }
    let _ = prism_common::clipboard::private_dir(&dir);
    let mut m = Mpd::connect(&t.host, t.port, &t.password).ok()?;
    // MPD re-reads the song for every chunk and sends 8 KiB by default, so a large embedded
    // cover takes half a minute. Servers older than 0.22.4 refuse this and keep the default.
    let _ = m.cmd("binarylimit 1048576");
    let found = match m.picture("readpicture", file) {
        Ok(None) => m.picture("albumart", file),
        other => other,
    };
    match found {
        Ok(Some(bytes)) => {
            // Renamed into place: readers never see a partly written cover.
            let tmp = dir.join(format!("{:016x}.{:?}.part", h.finish(), std::thread::current().id()));
            std::fs::write(&tmp, bytes).ok()?;
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
            Some(format!("file://{}", path.display()))
        }
        Ok(None) => {
            let _ = std::fs::write(&none, b"");
            None
        }
        // Not remembered as "no cover": the next play of this song tries again.
        Err(e) => {
            eprintln!("[prismd/mpd] cover of {file}: {e}");
            None
        }
    }
}

// ── D-Bus ────────────────────────────────────────────────────────────────

struct Root;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        "Music Player Daemon".into()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "mpd".into()
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["file".into()]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    state: Arc<Mutex<State>>,
    target: Arc<Mutex<Target>>,
}

impl Player {
    fn run(&self, cmd: &str) -> zbus::fdo::Result<()> {
        let t = self.target.lock().unwrap().clone();
        t.run(cmd)
    }
}

fn ov(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("plain value")
}

fn track_path(id: &str) -> String {
    if id.is_empty() {
        "/org/mpris/MediaPlayer2/TrackList/NoTrack".into()
    } else {
        format!("/org/mpd/Track/{id}")
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) -> zbus::fdo::Result<()> {
        self.run("next")
    }
    fn previous(&self) -> zbus::fdo::Result<()> {
        self.run("previous")
    }
    fn pause(&self) -> zbus::fdo::Result<()> {
        self.run("pause 1")
    }
    fn play(&self) -> zbus::fdo::Result<()> {
        let paused = self.state.lock().unwrap().status == "Paused";
        self.run(if paused { "pause 0" } else { "play" })
    }
    fn play_pause(&self) -> zbus::fdo::Result<()> {
        let status = self.state.lock().unwrap().status.clone();
        self.run(match status.as_str() {
            "Playing" => "pause 1",
            "Paused" => "pause 0",
            _ => "play",
        })
    }
    fn stop(&self) -> zbus::fdo::Result<()> {
        self.run("stop")
    }
    fn seek(&self, offset: i64) -> zbus::fdo::Result<()> {
        let sign = if offset < 0 { "-" } else { "+" };
        self.run(&format!("seekcur {sign}{:.3}", offset.unsigned_abs() as f64 / 1e6))
    }
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) -> zbus::fdo::Result<()> {
        let (id, len) = {
            let s = self.state.lock().unwrap();
            (track_path(&s.song.id), s.song.length_us)
        };
        if track_id.as_str() != id || position < 0 || (len > 0 && position > len) {
            return Ok(());
        }
        self.run(&format!("seekcur {:.3}", position as f64 / 1e6))
    }
    fn open_uri(&self, _uri: String) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("OpenUri".into()))
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.state.lock().unwrap().status.clone()
    }
    #[zbus(property)]
    fn loop_status(&self) -> String {
        let s = self.state.lock().unwrap();
        match (s.repeat, s.single) {
            (true, true) => "Track",
            (true, false) => "Playlist",
            _ => "None",
        }
        .into()
    }
    #[zbus(property)]
    fn set_loop_status(&mut self, v: String) -> zbus::fdo::Result<()> {
        let (repeat, single) = match v.as_str() {
            "Track" => (1, 1),
            "Playlist" => (1, 0),
            _ => (0, 0),
        };
        self.run(&format!("command_list_begin\nrepeat {repeat}\nsingle {single}\ncommand_list_end"))
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.state.lock().unwrap().random
    }
    #[zbus(property)]
    fn set_shuffle(&mut self, v: bool) -> zbus::fdo::Result<()> {
        self.run(&format!("random {}", v as u8))
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let s = self.state.lock().unwrap();
        let song = &s.song;
        let mut m = HashMap::new();
        if let Ok(p) = ObjectPath::try_from(track_path(&song.id)) {
            m.insert("mpris:trackid".into(), ov(Value::from(p)));
        }
        if song.id.is_empty() {
            return m;
        }
        m.insert("mpris:length".into(), ov(Value::from(song.length_us)));
        m.insert("xesam:title".into(), ov(Value::from(song.title.clone())));
        m.insert("xesam:url".into(), ov(Value::from(song.file.clone())));
        if !song.artists.is_empty() {
            m.insert("xesam:artist".into(), ov(Value::from(song.artists.clone())));
        }
        if !song.album.is_empty() {
            m.insert("xesam:album".into(), ov(Value::from(song.album.clone())));
        }
        if !song.album_artists.is_empty() {
            m.insert("xesam:albumArtist".into(), ov(Value::from(song.album_artists.clone())));
        }
        if let Some(n) = song.track {
            m.insert("xesam:trackNumber".into(), ov(Value::from(n)));
        }
        if let Some(a) = &song.art {
            m.insert("mpris:artUrl".into(), ov(Value::from(a.clone())));
        }
        m
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.state.lock().unwrap().volume.unwrap_or(0.0)
    }
    #[zbus(property)]
    fn set_volume(&mut self, v: f64) -> zbus::fdo::Result<()> {
        self.run(&format!("setvol {}", (v.clamp(0.0, 1.0) * 100.0).round() as u32))
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.state.lock().unwrap().position()
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.state.lock().unwrap().song.length_us > 0
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

// ── Supervisor ───────────────────────────────────────────────────────────

static WANTED: AtomicBool = AtomicBool::new(false);
static TARGET: Mutex<Option<Target>> = Mutex::new(None);
static SUPERVISOR: std::sync::Once = std::sync::Once::new();

/// Serve the bridge while `mpd_mpris_enabled` is on; follows host/port changes.
pub fn apply(cfg: &Config) {
    WANTED.store(cfg.mpd_mpris_enabled, Ordering::SeqCst);
    *TARGET.lock().unwrap() = Some(Target::from(cfg));
    if !cfg.mpd_mpris_enabled {
        return;
    }
    SUPERVISOR.call_once(|| {
        std::thread::Builder::new().name("prism-mpd".into()).spawn(supervise).ok();
    });
}

fn current_target() -> Option<Target> {
    TARGET.lock().unwrap().clone()
}

fn same(a: &Target, b: &Target) -> bool {
    a.host == b.host && a.port == b.port && a.password == b.password
}

fn supervise() {
    let mut logged = false;
    loop {
        if !WANTED.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let Some(t) = current_target() else { continue };
        match serve(&t) {
            Ok(()) => logged = false,
            Err(e) => {
                if !logged {
                    eprintln!("[prismd/mpd] {e}; retrying");
                    logged = true;
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

/// Connect, own the bus name and follow MPD until it goes away or the bridge is turned off or
/// retargeted. Dropping the connection releases the name.
fn serve(t: &Target) -> Result<(), String> {
    let mut idle = Mpd::connect(&t.host, t.port, &t.password)?;
    let state = Arc::new(Mutex::new(read_state(&mut idle)?));
    let target = Arc::new(Mutex::new(t.clone()));
    let conn = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(NAME))
        .and_then(|b| b.serve_at(PATH, Root))
        .and_then(|b| b.serve_at(PATH, Player { state: Arc::clone(&state), target }))
        .and_then(|b| b.build())
        .map_err(|e| format!("D-Bus {NAME}: {e}"))?;
    eprintln!("[prismd/mpd] serving {NAME} for MPD at {}:{}", t.host, t.port);
    let iface = conn.object_server().interface::<_, Player>(PATH).map_err(|e| e.to_string())?;
    let load_art = |file: String| {
        let t = t.clone();
        let state = Arc::clone(&state);
        let conn = conn.clone();
        std::thread::spawn(move || {
            let art = fetch_art(&t, &file);
            let mut s = state.lock().unwrap();
            if s.song.file == file && s.song.art != art {
                s.song.art = art;
                drop(s);
                if let Ok(iface) = conn.object_server().interface::<_, Player>(PATH) {
                    let _ = zbus::block_on(iface.get().metadata_changed(iface.signal_emitter()));
                }
            }
        });
    };
    load_art(state.lock().unwrap().song.file.clone());

    idle.r.get_ref().set_timeout(Some(Duration::from_secs(1)));
    let mut line = String::new();
    idle.send("idle player mixer options").map_err(|e| e.to_string())?;
    loop {
        if !WANTED.load(Ordering::SeqCst) || !current_target().is_some_and(|c| same(&c, t)) {
            eprintln!("[prismd/mpd] bridge stopped");
            return Ok(());
        }
        match idle.r.read_line(&mut line) {
            Ok(0) => return Err("MPD closed the connection".into()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e.to_string()),
        }
        let l = std::mem::take(&mut line);
        let l = l.trim_end();
        if l.starts_with("ACK ") {
            return Err(l.to_string());
        }
        if l != "OK" {
            continue; // "changed: <subsystem>"
        }
        // Something changed: re-read and announce what differs.
        idle.r.get_ref().set_timeout(Some(Duration::from_secs(5)));
        let new = read_state(&mut idle)?;
        idle.r.get_ref().set_timeout(Some(Duration::from_secs(1)));
        let (song_changed, status_changed, loop_changed, shuffle_changed, volume_changed, seeked) = {
            let mut s = state.lock().unwrap();
            let song_changed = s.song.id != new.song.id || s.song.file != new.song.file || s.song.title != new.song.title;
            let expected = s.position();
            let seeked = !song_changed && (new.elapsed_us - expected).abs() > 1_500_000;
            let flags = (
                song_changed || s.song.length_us != new.song.length_us,
                s.status != new.status,
                s.repeat != new.repeat || s.single != new.single,
                s.random != new.random,
                s.volume != new.volume,
                seeked,
            );
            let art = if song_changed { None } else { s.song.art.take() };
            *s = new;
            s.song.art = art;
            flags
        };
        let file = state.lock().unwrap().song.file.clone();
        if song_changed {
            load_art(file);
        }
        let pos = state.lock().unwrap().position();
        zbus::block_on(async {
            let p = iface.get();
            let e = iface.signal_emitter();
            if song_changed {
                let _ = p.metadata_changed(e).await;
                let _ = p.can_seek_changed(e).await;
            }
            if status_changed {
                let _ = p.playback_status_changed(e).await;
            }
            if loop_changed {
                let _ = p.loop_status_changed(e).await;
            }
            if shuffle_changed {
                let _ = p.shuffle_changed(e).await;
            }
            if volume_changed {
                let _ = p.volume_changed(e).await;
            }
            if seeked || song_changed {
                let _ = Player::seeked(e, pos).await;
            }
        });
        idle.send("idle player mixer options").map_err(|e| e.to_string())?;
    }
}
