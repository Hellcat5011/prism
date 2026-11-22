//! Prism's download manager over `prism_download` (the engine): a queue of downloads added by
//! `prism-ctl download add` and the Firefox extension (through `prism-native-host`), at most
//! `download_max_active` at once, each with up to `download_connections` connections.
//!
//! The list lives in `$XDG_DATA_HOME/prism/downloads.json` with each download's remaining
//! byte ranges, so downloads that were running when prismd stopped continue where they were
//! (when the server still has the same file: same size and ETag / Last-Modified), and paused or
//! failed ones can be resumed. A download is written to `<name>.prismdl` next to its target
//! and renamed when complete; then the downloads pill says so and a notification offers Open
//! and Show in Folder (`downloads::announce`). The pill shows running downloads with their
//! progress (`downloads::set_engine`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use prism_common::ipc::{DownloadAsk, DownloadInfo, DownloadProgress, DownloadRequest};
use prism_common::Config;
use prism_download as engine;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
struct Entry {
    info: DownloadInfo,
    req: DownloadRequest,
    /// What the server said when it started (to check a resumed download is the same file).
    #[serde(default)]
    probe: Option<engine::Probe>,
    /// Byte ranges still to fetch, for resuming.
    #[serde(default)]
    remaining: Option<Vec<(u64, u64)>>,
    #[serde(skip)]
    handle: Option<engine::Handle>,
    /// (when, bytes) for the rate.
    #[serde(skip)]
    samples: VecDeque<(Instant, u64)>,
    /// Cancel once the engine has stopped.
    #[serde(skip)]
    cancel: bool,
    /// Forget once the engine has stopped.
    #[serde(skip)]
    remove: bool,
    /// Waiting for the confirmation card: what to show (once the server was asked).
    #[serde(skip)]
    ask: Option<DownloadAsk>,
}

#[derive(Default)]
struct Manager {
    entries: Vec<Entry>,
    loaded: bool,
    dirty: bool,
    last_save: Option<Instant>,
}

static MGR: LazyLock<Mutex<Manager>> = LazyLock::new(|| Mutex::new(Manager::default()));
/// The download the confirmation card shows now (one at a time).
static ASKING: Mutex<Option<u64>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();
static START: std::sync::Once = std::sync::Once::new();

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn store_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    base.join("prism/downloads.json")
}

fn download_dir(cfg: &Config) -> PathBuf {
    let d = cfg.download_dir.trim();
    if d.is_empty() {
        return crate::downloads::download_dir();
    }
    match d.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(d),
    }
}

impl Manager {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let Ok(text) = std::fs::read_to_string(store_path()) else { return };
        let Ok(mut entries) = serde_json::from_str::<Vec<Entry>>(&text) else {
            eprintln!("[prismd/dl] {} is not readable; starting with an empty list", store_path().display());
            return;
        };
        // Unconfirmed downloads were never accepted.
        entries.retain(|e| e.info.state != "confirm");
        // Downloads that were running when prismd stopped continue.
        for e in entries.iter_mut() {
            if matches!(e.info.state.as_str(), "running" | "probing") {
                e.info.state = "queued".into();
            }
            e.info.rate = 0;
            e.info.connections = 0;
        }
        self.entries = entries;
    }

    fn save(&mut self) {
        self.dirty = false;
        self.last_save = Some(Instant::now());
        let path = store_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_string(&self.entries) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn get(&mut self, id: u64) -> Result<&mut Entry, String> {
        self.entries.iter_mut().find(|e| e.info.id == id).ok_or_else(|| format!("no download {id}"))
    }
}

fn ensure_started() {
    START.call_once(|| {
        MGR.lock().unwrap().load();
        let _ = std::thread::Builder::new().name("prism-dl-manager".into()).spawn(run);
    });
}

/// Called at startup: continues the downloads that were running, and registers
/// prism-native-host with the installed Firefox-family browsers.
pub fn init() {
    ensure_started();
    WAKE.notify_all();
    std::thread::spawn(register_native_host);
}

/// The browser's native messaging manifest for the Prism Downloads extension. Firefox (even
/// with its profile in ~/.config/mozilla, checked with Firefox 160) reads
/// ~/.mozilla/native-messaging-hosts; LibreWolf ~/.librewolf/native-messaging-hosts. Written
/// only for browsers that are installed, and only when it differs (the host moved).
fn register_native_host() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let on_path = |bin: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()));
    let firefox = on_path("firefox") || home.join(".mozilla/firefox").is_dir() || home.join(".config/mozilla/firefox").is_dir();
    let librewolf = on_path("librewolf") || home.join(".librewolf").is_dir();
    let host = crate::resolve_sibling_bin("prism-native-host");
    if !host.exists() {
        return;
    }
    let text = include_str!("../../../deploy/native-messaging/prism_downloads.json").replace("@BIN_DIR@/prism-native-host", &host.to_string_lossy());
    for (wanted, dir) in [(firefox, home.join(".mozilla/native-messaging-hosts")), (librewolf, home.join(".librewolf/native-messaging-hosts"))] {
        let file = dir.join("prism_downloads.json");
        if !wanted || std::fs::read_to_string(&file).is_ok_and(|t| t == text) {
            continue;
        }
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &text)) {
            Ok(()) => eprintln!("[prismd/dl] registered prism-native-host in {}", file.display()),
            Err(e) => eprintln!("[prismd/dl] cannot write {}: {e}", file.display()),
        }
    }
}

pub fn add(mut req: DownloadRequest) -> Result<u64, String> {
    if req.kind == "hls" {
        req.name = Some(video_name(req.name.as_deref()));
    }
    let url = req.url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http and https links can be downloaded".into());
    }
    ensure_started();
    let cfg = Config::load();
    let ask = req.ask && cfg.download_confirm;
    let mut m = MGR.lock().unwrap();
    let id = m.entries.iter().map(|e| e.info.id).max().unwrap_or(0) + 1;
    let name = req.name.as_deref().map(engine::sanitize).filter(|n| !n.is_empty()).or_else(|| engine::url_name(url).map(|n| engine::sanitize(&n))).unwrap_or_else(|| "download".into());
    let state = if ask { "confirm" } else { "queued" };
    let info = DownloadInfo { id, name, url: url.to_string(), state: state.into(), added: now_secs(), ..Default::default() };
    m.entries.insert(0, Entry { info, req: req.clone(), ..Default::default() });
    m.dirty = true;
    drop(m);
    if ask {
        std::thread::spawn(move || prepare_ask(id, req, cfg));
    } else {
        WAKE.notify_all();
    }
    Ok(id)
}

/// Ask the server about a download waiting for confirmation, then offer it.
fn prepare_ask(id: u64, req: DownloadRequest, cfg: Config) {
    let ereq = engine::Request { url: req.url.clone(), referer: req.referer.clone(), cookie: req.cookie.clone(), user_agent: req.user_agent.clone(), headers: req.headers.clone() };
    // A video stream has no single file to ask about.
    let probed = if req.kind == "hls" {
        Ok(engine::Probe { name: video_name(req.name.as_deref()), url: req.url.clone(), ..Default::default() })
    } else {
        engine::probe(&ereq)
    };
    let dir = req.dir.clone().map(PathBuf::from).unwrap_or_else(|| download_dir(&cfg));
    {
        let mut m = MGR.lock().unwrap();
        let Ok(e) = m.get(id) else { return };
        let (name, size, error) = match &probed {
            Ok(p) => (req.name.as_deref().map(engine::sanitize).filter(|n| !n.is_empty()).unwrap_or_else(|| p.name.clone()), p.size, None),
            Err(err) => (e.info.name.clone(), None, Some(err.clone())),
        };
        e.info.name = name.clone();
        e.info.size = size;
        e.ask = Some(DownloadAsk { id, name, size, url: req.url.clone(), dir: dir.to_string_lossy().into_owned(), error });
    }
    present_next();
}

/// Show the confirmation card for the oldest download waiting for it, unless one is shown.
fn present_next() {
    let mut asking = ASKING.lock().unwrap();
    if asking.is_some() {
        return;
    }
    let next = {
        let m = MGR.lock().unwrap();
        m.entries.iter().rev().find(|e| e.info.state == "confirm").and_then(|e| e.ask.clone())
    };
    if let Some(ask) = next {
        *asking = Some(ask.id);
        drop(asking);
        if let Err(e) = crate::send_ui(&prism_common::UiCommand::DownloadConfirm(ask.clone())) {
            // No UI to ask in: start it as it is.
            eprintln!("[prismd/dl] cannot show the confirmation card ({e}); starting {}", ask.name);
            *ASKING.lock().unwrap() = None;
            let _ = start(ask.id, None, None);
        }
    }
}

/// The card answered (Download, or Cancel through `cancel`): show the next one.
fn answered(id: u64) {
    let mut asking = ASKING.lock().unwrap();
    if *asking == Some(id) {
        *asking = None;
        drop(asking);
        std::thread::spawn(present_next);
    }
}

/// The confirmation card closed without Download (a no-op once it has started).
pub fn decline(id: u64) -> Result<(), String> {
    let waiting = {
        let mut m = MGR.lock().unwrap();
        m.load();
        m.get(id).map(|e| e.info.state == "confirm").unwrap_or(false)
    };
    if waiting {
        cancel(id, true)
    } else {
        answered(id);
        Ok(())
    }
}

/// The confirmation card's Download.
pub fn start(id: u64, name: Option<String>, dir: Option<String>) -> Result<(), String> {
    let mut m = MGR.lock().unwrap();
    m.load();
    let e = m.get(id)?;
    if e.info.state != "confirm" {
        return Err(format!("the download is {}", e.info.state));
    }
    if let Some(n) = name.map(|n| engine::sanitize(&n)).filter(|n| !n.is_empty()) {
        e.info.name = n.clone();
        e.req.name = Some(n);
    }
    if let Some(d) = dir.filter(|d| !d.trim().is_empty()) {
        e.req.dir = Some(prism_common::config::expand_home(d.trim()).to_string_lossy().into_owned());
    }
    e.info.state = "queued".into();
    e.ask = None;
    m.dirty = true;
    drop(m);
    answered(id);
    WAKE.notify_all();
    Ok(())
}

pub fn pause(id: u64) -> Result<(), String> {
    let mut m = MGR.lock().unwrap();
    m.load();
    let e = m.get(id)?;
    match e.info.state.as_str() {
        "running" => {
            if let Some(h) = &e.handle {
                h.pause();
            }
        }
        "queued" => e.info.state = "paused".into(),
        "probing" => return Err("the download is just starting; pause it in a moment".into()),
        s => return Err(format!("the download is {s}")),
    }
    m.dirty = true;
    Ok(())
}

pub fn resume(id: u64) -> Result<(), String> {
    let mut m = MGR.lock().unwrap();
    m.load();
    let e = m.get(id)?;
    match e.info.state.as_str() {
        "paused" | "failed" => {
            e.info.state = "queued".into();
            e.info.error = None;
        }
        s => return Err(format!("the download is {s}")),
    }
    m.dirty = true;
    drop(m);
    ensure_started();
    WAKE.notify_all();
    Ok(())
}

fn delete_temp(e: &Entry) {
    if !e.info.path.is_empty() {
        let temp = engine::temp_path(Path::new(&e.info.path));
        let _ = std::fs::remove_dir_all(work_dir(&temp));
        let _ = std::fs::remove_file(temp);
    }
}

/// An HLS download's segments, next to its temporary file.
fn work_dir(temp: &Path) -> PathBuf {
    let mut s = temp.as_os_str().to_owned();
    s.push(".d");
    PathBuf::from(s)
}

/// A video's file name: the given one (or the page's) with `.mp4`.
fn video_name(name: Option<&str>) -> String {
    let n = name.map(engine::sanitize).filter(|n| !n.is_empty()).unwrap_or_else(|| "video".into());
    let stem = n.strip_suffix(".m3u8").or_else(|| n.strip_suffix(".mp4")).unwrap_or(&n);
    format!("{stem}.mp4")
}

/// `begin` for HLS: no probe (the playlist is read by the engine); continues when the
/// segments from an earlier run are still there.
fn begin_hls(id: u64, req: DownloadRequest, path: String) {
    let cfg = Config::load();
    let mut m = MGR.lock().unwrap();
    let active = m.entries.iter().filter(|e| e.handle.is_some()).count() + 1;
    let Ok(e) = m.get(id) else { return };
    if e.cancel {
        delete_temp(e);
        e.info.state = "cancelled".into();
        if e.remove {
            m.entries.retain(|e| e.info.id != id);
        }
        m.dirty = true;
        return;
    }
    let resume = !path.is_empty() && work_dir(&engine::temp_path(Path::new(&path))).is_dir();
    let target = if resume {
        PathBuf::from(&path)
    } else {
        let dir = req.dir.as_deref().map(PathBuf::from).unwrap_or_else(|| download_dir(&cfg));
        let _ = std::fs::create_dir_all(&dir);
        engine::unique_path(&dir, &video_name(req.name.as_deref()))
    };
    let temp = engine::temp_path(&target);
    let opts = engine::Options { connections: cfg.download_connections.clamp(1, 16) as usize, limit: cfg.download_speed_limit_kb as u64 * 1024 / active as u64, ..Default::default() };
    let ereq = engine::Request { url: req.url.clone(), referer: req.referer.clone(), cookie: req.cookie.clone(), user_agent: req.user_agent.clone(), headers: req.headers.clone() };
    e.info.name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    e.info.path = target.to_string_lossy().into_owned();
    match engine::hls::start(ereq, req.variant.clone(), &work_dir(&temp), &temp, opts) {
        Ok(h) => {
            e.handle = Some(h);
            e.info.state = "running".into();
            e.samples.clear();
        }
        Err(err) => {
            e.info.state = "failed".into();
            e.info.error = Some(err);
        }
    }
    m.dirty = true;
}

pub fn cancel(id: u64, forget: bool) -> Result<(), String> {
    let mut m = MGR.lock().unwrap();
    m.load();
    let e = m.get(id)?;
    if let Some(h) = &e.handle {
        // Running: the manager finishes the job once the connections have stopped.
        h.pause();
        e.cancel = true;
        e.remove = forget;
    } else if e.info.state == "probing" {
        e.cancel = true;
        e.remove = forget;
    } else {
        let confirming = e.info.state == "confirm";
        if e.info.state != "done" {
            delete_temp(e);
            e.remaining = None;
            e.info.state = "cancelled".into();
        }
        // Declined in the confirmation card: nothing to keep.
        if forget || confirming {
            m.entries.retain(|e| e.info.id != id);
        }
    }
    m.dirty = true;
    drop(m);
    answered(id);
    WAKE.notify_all();
    Ok(())
}

/// Open a finished download, or show it (or its folder) in the file manager.
pub fn open(id: u64, folder: bool) -> Result<(), String> {
    let mut m = MGR.lock().unwrap();
    m.load();
    let e = m.get(id)?;
    let path = PathBuf::from(&e.info.path);
    if folder {
        let target = if path.exists() { path } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
        drop(m);
        std::thread::spawn(move || crate::downloads::show_in_folder(&target));
        return Ok(());
    }
    if e.info.state != "done" || !path.exists() {
        return Err("the download is not finished".into());
    }
    crate::downloads::open_file(&path);
    Ok(())
}

pub fn clear() {
    let mut m = MGR.lock().unwrap();
    m.load();
    m.entries.retain(|e| !matches!(e.info.state.as_str(), "done" | "failed" | "cancelled"));
    m.dirty = true;
}

/// `DownloadAction::Inspect`: what an HLS playlist offers.
pub fn inspect(req: DownloadRequest) -> Result<serde_json::Value, String> {
    let ereq = engine::Request { url: req.url, referer: req.referer, cookie: req.cookie, user_agent: req.user_agent, headers: req.headers };
    Ok(match engine::hls::inspect(&ereq)? {
        engine::hls::Playlist::Master { variants, .. } => serde_json::json!({
            "variants": variants.iter().map(|v| serde_json::json!({"url": v.uri, "height": v.resolution.map_or(0, |r| r.1), "bandwidth": v.bandwidth})).collect::<Vec<_>>(),
            "duration": 0,
        }),
        engine::hls::Playlist::Media(m) => serde_json::json!({"variants": [], "duration": m.segments.iter().map(|s| s.duration).sum::<f64>()}),
    })
}

pub fn list() -> Vec<DownloadInfo> {
    let mut m = MGR.lock().unwrap();
    m.load();
    m.entries.iter().map(|e| e.info.clone()).collect()
}

/// Probe, choose the file name and start the engine (on its own thread: probing waits for
/// the network).
fn begin(id: u64) {
    let (req, saved, remaining, path) = {
        let mut m = MGR.lock().unwrap();
        let Ok(e) = m.get(id) else { return };
        (e.req.clone(), e.probe.clone(), e.remaining.clone(), e.info.path.clone())
    };
    if req.kind == "hls" {
        return begin_hls(id, req, path);
    }
    let ereq = engine::Request { url: req.url.clone(), referer: req.referer.clone(), cookie: req.cookie.clone(), user_agent: req.user_agent.clone(), headers: req.headers.clone() };
    let probed = engine::probe(&ereq);
    let cfg = Config::load();
    let mut m = MGR.lock().unwrap();
    let active = m.entries.iter().filter(|e| e.handle.is_some()).count() + 1;
    let Ok(e) = m.get(id) else { return };
    let finish_cancel = |e: &mut Entry| {
        delete_temp(e);
        e.info.state = "cancelled".into();
    };
    if e.cancel {
        finish_cancel(e);
        let forget = e.remove;
        if forget {
            m.entries.retain(|e| e.info.id != id);
        }
        m.dirty = true;
        return;
    }
    let probe = match probed {
        Ok(p) => p,
        Err(err) => {
            e.info.state = "failed".into();
            e.info.error = Some(err);
            m.dirty = true;
            return;
        }
    };
    // Continue only when the server still has the same file.
    let same = |a: &engine::Probe| a.size == probe.size && a.ranges && probe.ranges && a.etag == probe.etag && a.last_modified == probe.last_modified;
    let resume = match (&saved, remaining) {
        (Some(a), Some(r)) if same(a) && !path.is_empty() && engine::temp_path(Path::new(&path)).exists() => Some(r),
        _ => None,
    };
    let target = if resume.is_some() {
        PathBuf::from(&path)
    } else {
        if !path.is_empty() {
            let _ = std::fs::remove_file(engine::temp_path(Path::new(&path)));
        }
        let dir = req.dir.as_deref().map(PathBuf::from).unwrap_or_else(|| download_dir(&cfg));
        let _ = std::fs::create_dir_all(&dir);
        let name = req.name.as_deref().map(engine::sanitize).filter(|n| !n.is_empty()).unwrap_or_else(|| probe.name.clone());
        engine::unique_path(&dir, &name)
    };
    let opts = engine::Options {
        connections: cfg.download_connections.clamp(1, 32) as usize,
        limit: cfg.download_speed_limit_kb as u64 * 1024 / active as u64,
        ..Default::default()
    };
    e.info.name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    e.info.path = target.to_string_lossy().into_owned();
    e.info.size = probe.size;
    match engine::start(ereq, &probe, &engine::temp_path(&target), resume, opts) {
        Ok(h) => {
            e.info.done = h.progress().done;
            e.handle = Some(h);
            e.info.state = "running".into();
            e.probe = Some(probe);
            e.samples.clear();
        }
        Err(err) => {
            e.info.state = "failed".into();
            e.info.error = Some(err);
        }
    }
    m.dirty = true;
}

/// A finished download: move it into place and say so.
fn complete(e: &mut Entry) {
    let target = PathBuf::from(&e.info.path);
    let temp = engine::temp_path(&target);
    let dest = if target.exists() { engine::unique_path(target.parent().unwrap_or(Path::new("/")), &e.info.name) } else { target };
    crate::downloads::own(&dest);
    match std::fs::rename(&temp, &dest) {
        Ok(()) => {
            e.info.name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            e.info.path = dest.to_string_lossy().into_owned();
            e.info.state = "done".into();
            e.info.size = Some(e.info.done);
            e.remaining = None;
            crate::downloads::announce(dest);
        }
        Err(err) => {
            e.info.state = "failed".into();
            e.info.error = Some(format!("moving the finished file into place: {err}"));
        }
    }
}

fn run() {
    loop {
        let to_begin: Vec<u64>;
        let busy;
        {
            let mut m = MGR.lock().unwrap();
            let max = Config::load().download_max_active.max(1) as usize;
            let now = Instant::now();
            let mut changed = false;
            for e in m.entries.iter_mut() {
                let Some(h) = &e.handle else { continue };
                let p = h.progress();
                e.info.done = p.done;
                // Video streams learn their size as segments arrive (an estimate from them).
                if p.size.is_some() {
                    e.info.size = p.size;
                }
                e.info.connections = p.connections as u32;
                e.samples.push_back((now, p.done));
                while e.samples.len() > 8 {
                    e.samples.pop_front();
                }
                e.info.rate = match (e.samples.front(), e.samples.back()) {
                    (Some(a), Some(b)) if b.0 > a.0 => ((b.1.saturating_sub(a.1)) as f64 / (b.0 - a.0).as_secs_f64()) as u64,
                    _ => 0,
                };
                if p.state == engine::State::Running {
                    continue;
                }
                e.remaining = Some(h.remaining());
                e.handle = None;
                e.info.rate = 0;
                e.info.connections = 0;
                changed = true;
                if e.cancel {
                    delete_temp(e);
                    e.remaining = None;
                    e.info.state = "cancelled".into();
                    continue;
                }
                match p.state {
                    engine::State::Done => complete(e),
                    engine::State::Failed(err) => {
                        e.info.state = "failed".into();
                        e.info.error = Some(err);
                        // Nothing fetched yet (the playlist was refused): leave no empty folder.
                        if !e.info.path.is_empty() {
                            let _ = std::fs::remove_dir(work_dir(&engine::temp_path(Path::new(&e.info.path))));
                        }
                    }
                    _ => e.info.state = "paused".into(),
                }
            }
            let before = m.entries.len();
            m.entries.retain(|e| !(e.remove && e.handle.is_none() && e.info.state == "cancelled"));
            changed |= m.entries.len() != before;
            let active = m.entries.iter().filter(|e| matches!(e.info.state.as_str(), "running" | "probing")).count();
            // Oldest first.
            let queued: Vec<u64> = m.entries.iter().rev().filter(|e| e.info.state == "queued").map(|e| e.info.id).take(max.saturating_sub(active)).collect();
            for id in &queued {
                if let Ok(e) = m.get(*id) {
                    e.info.state = "probing".into();
                }
            }
            to_begin = queued;
            changed |= !to_begin.is_empty();
            if changed {
                m.dirty = true;
            }
            busy = m.entries.iter().any(|e| matches!(e.info.state.as_str(), "running" | "probing" | "queued"));
            // The pill.
            let pill: Vec<DownloadProgress> = m
                .entries
                .iter()
                .filter(|e| matches!(e.info.state.as_str(), "running" | "probing"))
                .map(|e| DownloadProgress { name: e.info.name.clone(), bytes: e.info.done, rate: e.info.rate, progress: e.info.size.filter(|s| *s > 0).map(|s| (e.info.done as f64 / s as f64).min(1.0)).or((e.req.kind == "hls").then_some(0.0)), id: Some(e.info.id) })
                .collect();
            crate::downloads::set_engine(pill);
            let save_due = m.last_save.is_none_or(|t| t.elapsed() >= Duration::from_secs(5));
            if m.dirty || (busy && save_due) {
                // Remaining ranges of running downloads, so a crash loses little.
                for e in m.entries.iter_mut() {
                    if let Some(h) = &e.handle {
                        e.remaining = Some(h.remaining());
                    }
                }
                m.save();
            }
            if !busy && to_begin.is_empty() {
                let _unused = WAKE.wait_timeout(m, Duration::from_secs(30)).unwrap();
                continue;
            }
        }
        for id in to_begin {
            std::thread::spawn(move || begin(id));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
