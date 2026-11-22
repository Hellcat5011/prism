//! Downloads watcher (`downloads_watch`): follows the download folders with inotify: the XDG
//! Downloads folder, the folders Firefox, Chromium-based browsers and brisk are set to save to
//! (read from their settings, checked again every minute) and `downloads_extra_dirs`. A download
//! is a file being written under a temporary name (Firefox `.part`, Chromium `.crdownload`,
//! Safari-style `.download`, ...) or a folder in brisk's `Brisk/Temp`. While any is in
//! progress prism-ui shows a pill with its size and speed (`UiCommand::Downloads`, polled once
//! a second). When one ends and a finished file lands in the folder within a few seconds
//! (renamed from the temporary name, or written by brisk), a notification offers Open and Show
//! in Folder. Other files arriving in the folder (saved documents, unpacked archives) are left
//! alone, and a download that ends with nothing arriving was cancelled.
//!
//! Temporary files do not say how big a download will be. Browsers do say how far along their
//! downloads are, for docks, through `com.canonical.Unity.LauncherEntry` `Update` signals
//! (Firefox and Chromium-based browsers): that becomes the pill's progress bar, matched by the
//! temporary name (`.part` Firefox and its forks, `.crdownload` Chromium and the rest).

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use prism_common::ipc::DownloadProgress;
use prism_common::{Config, UiCommand};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Folders from Settings to watch as well.
static EXTRA: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// The config changed: look for the folders again.
static ROOTS_DIRTY: AtomicBool = AtomicBool::new(true);
static START: std::sync::Once = std::sync::Once::new();

/// Temporary names of downloads in progress.
const TEMP_EXT: [&str; 5] = ["part", "crdownload", "download", "partial", "opdownload"];
/// An ended download and a file arriving belong together within this time.
const PAIR_WINDOW: Duration = Duration::from_secs(5);
/// Folders followed below the Downloads folder (depth) and at most this many.
const MAX_DEPTH: usize = 4;
const MAX_WATCHES: usize = 512;

/// Dock progress by application (the LauncherEntry `app_uri`): (0 to 1, when it came).
static LAUNCHER: std::sync::Mutex<Vec<(String, f64, Instant)>> = std::sync::Mutex::new(Vec::new());
/// Dock progress older than this is not trusted (the app may have quit mid-download).
const LAUNCHER_STALE: Duration = Duration::from_secs(10);

/// Prism's own downloads for the pill (dl.rs), shown next to the browsers'.
static ENGINE: std::sync::Mutex<Vec<DownloadProgress>> = std::sync::Mutex::new(Vec::new());
/// Written when ENGINE changes, so the watcher shows it at once.
static WAKE_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
/// Files Prism's downloads just put in place: never announced a second time as a browser's.
static OWN: std::sync::Mutex<Vec<(PathBuf, Instant)>> = std::sync::Mutex::new(Vec::new());

pub fn set_engine(list: Vec<DownloadProgress>) {
    let mut e = ENGINE.lock().unwrap();
    if *e == list {
        return;
    }
    let appeared = e.is_empty() && !list.is_empty();
    *e = list;
    drop(e);
    let fd = WAKE_FD.load(Ordering::SeqCst);
    if appeared && fd >= 0 {
        let one: u64 = 1;
        unsafe { libc::write(fd, &one as *const u64 as *const libc::c_void, 8) };
    }
}

/// Call before moving a finished download of Prism's into place.
pub fn own(path: &Path) {
    let mut o = OWN.lock().unwrap();
    o.retain(|(_, t)| t.elapsed() < Duration::from_secs(30));
    o.push((path.to_path_buf(), Instant::now()));
}

fn is_own(path: &Path) -> bool {
    OWN.lock().unwrap().iter().any(|(p, t)| p == path && t.elapsed() < Duration::from_secs(30))
}

/// Follow the LauncherEntry `Update` signals on the session bus.
fn watch_launchers() {
    use zbus::message::Type as MsgType;
    use zbus::zvariant::OwnedValue;
    let run = || -> Result<(), String> {
        let e = |e: zbus::Error| e.to_string();
        let conn = zbus::blocking::Connection::session().map_err(e)?;
        let rule = zbus::MatchRule::builder()
            .msg_type(MsgType::Signal)
            .interface("com.canonical.Unity.LauncherEntry")
            .map_err(e)?
            .member("Update")
            .map_err(e)?
            .build();
        for msg in zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64)).map_err(e)? {
            let Ok(msg) = msg else { continue };
            let Ok((uri, props)) = msg.body().deserialize::<(String, HashMap<String, OwnedValue>)>() else { continue };
            let progress = props.get("progress").and_then(|v| f64::try_from(v).ok());
            let visible = props.get("progress-visible").and_then(|v| bool::try_from(v).ok());
            let mut list = LAUNCHER.lock().unwrap();
            match (visible, progress) {
                (Some(false), _) => list.retain(|(u, _, _)| *u != uri),
                (_, Some(p)) => {
                    list.retain(|(u, _, _)| *u != uri);
                    list.push((uri, p.clamp(0.0, 1.0), Instant::now()));
                }
                _ => {}
            }
        }
        Ok(())
    };
    loop {
        if let Err(e) = run() {
            eprintln!("[prismd/downloads] dock progress: {e}");
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// The dock progress that belongs to the download kept under `key`, if a browser gives one.
fn dock_progress(key: &Path) -> Option<f64> {
    let mut list = LAUNCHER.lock().unwrap();
    list.retain(|(_, _, t)| t.elapsed() < LAUNCHER_STALE);
    if list.len() == 1 {
        return Some(list[0].1);
    }
    let firefox = |u: &str| ["firefox", "librewolf", "floorp", "zen", "waterfox"].iter().any(|n| u.to_lowercase().contains(n));
    let ext = key.extension().and_then(|e| e.to_str()).unwrap_or("");
    let found: Vec<f64> = list.iter().filter(|(u, _, _)| if ext == "part" { firefox(u) } else { ext == "crdownload" && !firefox(u) }).map(|(_, p, _)| *p).collect();
    (found.len() == 1).then(|| found[0])
}

pub fn apply(cfg: &Config) {
    ENABLED.store(cfg.downloads_watch, Ordering::SeqCst);
    *EXTRA.lock().unwrap() = cfg.downloads_extra_dirs.clone();
    ROOTS_DIRTY.store(true, Ordering::SeqCst);
    // Runs even with the watcher off: it also shows Prism's own downloads in the pill.
    START.call_once(|| {
        std::thread::Builder::new().name("prism-downloads".into()).spawn(run).ok();
        std::thread::Builder::new().name("prism-dock-progress".into()).spawn(watch_launchers).ok();
    });
}

/// XDG_DOWNLOAD_DIR from user-dirs.dirs, else ~/Downloads.
pub fn download_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let dirs = std::fs::read_to_string(home.join(".config/user-dirs.dirs")).unwrap_or_default();
    for line in dirs.lines() {
        if let Some(v) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") {
            let v = v.trim().trim_matches('"').replace("$HOME", &home.to_string_lossy());
            if !v.is_empty() && v != home.to_string_lossy() {
                return PathBuf::from(v);
            }
        }
    }
    home.join("Downloads")
}

/// What a path is to the watcher.
#[derive(Debug, PartialEq)]
enum Kind {
    /// Part of a download in progress, tracked under this key (the temp file, or brisk's
    /// per-download temp folder).
    Temp(PathBuf),
    /// A finished file could be this.
    File,
    Ignore,
}

/// The folders to follow: (download folders, brisk's temporary folder).
#[derive(Clone, Debug, Default, PartialEq)]
struct Roots {
    dirs: Vec<PathBuf>,
    brisk_temp: Option<PathBuf>,
}

fn classify(roots: &Roots, path: &Path) -> Kind {
    // brisk: <temp>/<download>/... (one folder per download).
    if let Some(t) = &roots.brisk_temp {
        if let Ok(rel) = path.strip_prefix(t) {
            return match rel.iter().next() {
                Some(first) => Kind::Temp(t.join(first)),
                None => Kind::Ignore,
            };
        }
    }
    let Some(rel) = roots.dirs.iter().filter_map(|r| path.strip_prefix(r).ok()).min_by_key(|r| r.iter().count()) else { return Kind::Ignore };
    let parts: Vec<&std::ffi::OsStr> = rel.iter().collect();
    if parts.iter().any(|p| p.as_bytes().first() == Some(&b'.')) {
        return Kind::Ignore;
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // Prism's own downloads report themselves (dl.rs), and the segments of a video it
    // downloads (`<name>.prismdl.d/`) are parts, not downloads.
    if ext == "prismdl" || is_own(path) || parts.iter().any(|p| p.as_bytes().ends_with(b".prismdl.d")) {
        return Kind::Ignore;
    }
    if TEMP_EXT.contains(&ext.as_str()) {
        return Kind::Temp(path.to_path_buf());
    }
    Kind::File
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// Firefox: `browser.download.dir` of every profile (when set to save there).
fn firefox_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in [home().join(".mozilla/firefox"), home().join(".config/mozilla/firefox"), home().join(".librewolf"), home().join(".floorp")] {
        for prof in std::fs::read_dir(base).into_iter().flatten().flatten() {
            let prefs = std::fs::read_to_string(prof.path().join("prefs.js")).unwrap_or_default();
            for line in prefs.lines() {
                if let Some(rest) = line.trim().strip_prefix("user_pref(\"browser.download.dir\",") {
                    if let Some(v) = rest.split('"').nth(1) {
                        out.push(PathBuf::from(v.replace("\\\\", "\\")));
                    }
                }
            }
        }
    }
    out
}

/// Chromium-based browsers: `download.default_directory` of their profiles.
fn chromium_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for app in ["google-chrome", "google-chrome-beta", "chromium", "BraveSoftware/Brave-Browser", "vivaldi", "microsoft-edge", "thorium"] {
        let base = home().join(".config").join(app);
        for prof in std::fs::read_dir(&base).into_iter().flatten().flatten() {
            let Ok(text) = std::fs::read(prof.path().join("Preferences")) else { continue };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&text) else { continue };
            if let Some(d) = v.pointer("/download/default_directory").and_then(|d| d.as_str()) {
                out.push(PathBuf::from(d));
            }
        }
    }
    out
}

/// brisk: folders in its settings (a Hive box): the save folder and the temporary one.
fn brisk_dirs() -> (Vec<PathBuf>, Option<PathBuf>) {
    let Ok(data) = std::fs::read(home().join(".local/share/io.github.BrisklyDev.Brisk/settings.hive")) else { return (Vec::new(), None) };
    let mut dirs = Vec::new();
    let mut temp = None;
    for run in data.split(|b| !(0x20..0x7f).contains(b)) {
        let Ok(s) = std::str::from_utf8(run) else { continue };
        let Some(i) = s.find('/') else { continue };
        let p = PathBuf::from(&s[i..]);
        if p.components().count() < 3 || !p.is_dir() {
            continue;
        }
        if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("temp")) {
            temp = Some(p);
        } else {
            dirs.push(p);
        }
    }
    (dirs, temp)
}

/// Every folder downloads may land in now.
fn find_roots() -> Roots {
    let (brisk, brisk_temp) = brisk_dirs();
    let extra: Vec<PathBuf> = EXTRA.lock().unwrap().iter().map(|d| prism_common::config::expand_home(d)).collect();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in std::iter::once(download_dir()).chain(firefox_dirs()).chain(chromium_dirs()).chain(brisk).chain(extra) {
        // Never the whole home folder (or /): too much to follow.
        if d.is_dir() && d != home() && d.components().count() > 2 && !dirs.iter().any(|x| d.starts_with(x)) {
            dirs.retain(|x| !x.starts_with(&d));
            dirs.push(d);
        }
    }
    Roots { dirs, brisk_temp }
}

/// Shown name of a download in progress.
fn temp_name(key: &Path) -> String {
    let name = key.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match key.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        Some(e) if TEMP_EXT.contains(&e.as_str()) => name[..name.len() - e.len() - 1].to_string(),
        _ => name,
    }
}

/// Bytes of a file, or of every file in a folder (two levels).
fn size_of(p: &Path) -> Option<u64> {
    let md = std::fs::metadata(p).ok()?;
    if md.is_file() {
        return Some(md.len());
    }
    let mut total = 0;
    for e in std::fs::read_dir(p).ok()?.flatten() {
        let Ok(m) = e.metadata() else { continue };
        if m.is_file() {
            total += m.len();
        } else if m.is_dir() {
            for f in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
                total += f.metadata().map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0);
            }
        }
    }
    Some(total)
}

struct Active {
    name: String,
    /// (when, bytes) over the last few seconds, for the rate.
    samples: VecDeque<(Instant, u64)>,
}

impl Active {
    fn progress(&self) -> DownloadProgress {
        let bytes = self.samples.back().map(|s| s.1).unwrap_or(0);
        let rate = match (self.samples.front(), self.samples.back()) {
            (Some(a), Some(b)) if b.0 > a.0 => ((b.1.saturating_sub(a.1)) as f64 / (b.0 - a.0).as_secs_f64()) as u64,
            _ => 0,
        };
        DownloadProgress { name: self.name.clone(), bytes, rate, progress: None, id: None }
    }
}

struct Watcher {
    fd: i32,
    roots: Roots,
    wds: HashMap<i32, PathBuf>,
    active: HashMap<PathBuf, Active>,
    /// Downloads that ended, waiting for their file.
    ended: Vec<Instant>,
    /// Files that arrived, waiting for a download to end.
    arrived: Vec<(PathBuf, Instant)>,
    shown: Vec<DownloadProgress>,
}

const MASK: u32 = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_DELETE | libc::IN_CLOSE_WRITE | libc::IN_DELETE_SELF;

impl Watcher {
    fn add_tree(&mut self, dir: &Path, depth: usize) {
        if self.wds.len() >= MAX_WATCHES || depth > MAX_DEPTH {
            return;
        }
        let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else { return };
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
        if wd < 0 {
            return;
        }
        self.wds.insert(wd, dir.to_path_buf());
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let hidden = e.file_name().as_bytes().first() == Some(&b'.');
            if !hidden && e.file_type().is_ok_and(|t| t.is_dir()) {
                self.add_tree(&e.path(), depth + 1);
            }
        }
        // A download already running in it (prismd restarted mid-download).
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if let Kind::Temp(key) = classify(&self.roots.clone(), &e.path()) {
                self.track(key);
            }
        }
    }

    /// Follow these folders (and stop following the ones no longer wanted).
    fn set_roots(&mut self, roots: Roots) {
        if roots == self.roots {
            return;
        }
        for (wd, _) in self.wds.drain() {
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
        self.roots = roots.clone();
        for d in roots.dirs.iter().chain(roots.brisk_temp.iter()) {
            self.add_tree(d, 0);
        }
        let list: Vec<String> = roots.dirs.iter().chain(roots.brisk_temp.iter()).map(|d| d.display().to_string()).collect();
        eprintln!("[prismd/downloads] watching {} ({} folders)", list.join(", "), self.wds.len());
    }

    fn depth(&self, p: &Path) -> usize {
        self.roots.dirs.iter().chain(self.roots.brisk_temp.iter()).filter_map(|r| p.strip_prefix(r).ok()).map(|r| r.iter().count()).min().unwrap_or(0)
    }

    fn track(&mut self, key: PathBuf) {
        let name = temp_name(&key);
        self.active.entry(key).or_insert_with(|| Active { name, samples: VecDeque::new() });
    }

    /// A download is over: pair it with a file that already arrived, or wait for one.
    fn end(&mut self, key: &Path) {
        if self.active.remove(key).is_none() {
            return;
        }
        let now = Instant::now();
        self.arrived.retain(|(_, t)| now - *t < PAIR_WINDOW);
        if let Some(i) = self.arrived.iter().position(|(p, _)| size_of(p).unwrap_or(0) > 0) {
            let (p, _) = self.arrived.remove(i);
            finished(p);
        } else {
            self.ended.push(now);
        }
    }

    fn arrive(&mut self, path: PathBuf) {
        let now = Instant::now();
        self.ended.retain(|t| now - *t < PAIR_WINDOW);
        // Firefox makes an empty placeholder under the final name when a download starts.
        let size = size_of(&path).unwrap_or(0);
        if !self.ended.is_empty() && size > 0 {
            self.ended.remove(0);
            finished(path);
        } else {
            self.arrived.retain(|(p, t)| now - *t < PAIR_WINDOW && *p != path);
            self.arrived.push((path, now));
        }
    }

    fn event(&mut self, wd: i32, mask: u32, name: &[u8]) {
        let Some(dir) = self.wds.get(&wd).cloned() else { return };
        if mask & libc::IN_DELETE_SELF != 0 {
            self.wds.remove(&wd);
            return;
        }
        let path = dir.join(std::ffi::OsStr::from_bytes(name));
        let is_dir = mask & libc::IN_ISDIR != 0;
        let kind = classify(&self.roots, &path);
        if is_dir && mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 && !name.ends_with(b".prismdl.d") {
            let depth = self.depth(&path);
            self.add_tree(&path, depth);
        }
        match kind {
            Kind::Temp(key) => {
                if mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                    self.track(key);
                } else if mask & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0 && key == path {
                    // The temp file, or brisk's whole temp folder, went away.
                    self.end(&key);
                }
            }
            Kind::File if !is_dir && mask & (libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE) != 0 => self.arrive(path),
            _ => {}
        }
    }

    /// Once a second: sizes and rates, ended downloads whose event was missed, and the pill.
    fn sample(&mut self) {
        let now = Instant::now();
        let gone: Vec<PathBuf> = self.active.keys().filter(|k| !k.exists()).cloned().collect();
        for k in gone {
            self.end(&k);
        }
        for (k, a) in self.active.iter_mut() {
            let bytes = size_of(k).unwrap_or(0);
            a.samples.push_back((now, bytes));
            while a.samples.len() > 4 {
                a.samples.pop_front();
            }
        }
        let mut list: Vec<DownloadProgress> = self.active.iter().map(|(k, a)| DownloadProgress { progress: dock_progress(k), ..a.progress() }).collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        if !ENABLED.load(Ordering::SeqCst) {
            list.clear();
        }
        list.extend(ENGINE.lock().unwrap().iter().cloned());
        if list != self.shown {
            self.shown = list.clone();
            let _ = crate::send_ui(&UiCommand::Downloads(list));
        }
    }
}

fn run() {
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        eprintln!("[prismd/downloads] inotify unavailable");
        return;
    }
    let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    WAKE_FD.store(wake, Ordering::SeqCst);
    let mut w = Watcher { fd, roots: Roots::default(), wds: HashMap::new(), active: HashMap::new(), ended: Vec::new(), arrived: Vec::new(), shown: Vec::new() };
    let mut buf = vec![0u8; 16 * 1024];
    let mut last_sample = Instant::now();
    let mut last_roots: Option<Instant> = None;
    loop {
        // Folders can change in the browsers' or brisk's settings (or in Prism's).
        if ROOTS_DIRTY.swap(false, Ordering::SeqCst) || last_roots.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            last_roots = Some(Instant::now());
            w.set_roots(find_roots());
        }
        let engine = !ENGINE.lock().unwrap().is_empty();
        let busy = !w.active.is_empty() || !w.shown.is_empty() || engine;
        let mut pfds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 }];
        let timeout = if busy { 1000 } else { 5_000 };
        let r = unsafe { libc::poll(pfds.as_mut_ptr(), 2, timeout) };
        if r > 0 && pfds[1].revents & libc::POLLIN != 0 {
            let mut v = 0u64;
            unsafe { libc::read(wake, &mut v as *mut u64 as *mut libc::c_void, 8) };
            last_sample = Instant::now() - Duration::from_secs(1);
        }
        if r > 0 && pfds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            let mut off = 0usize;
            let head = std::mem::size_of::<libc::inotify_event>();
            while n > 0 && off + head <= n as usize {
                let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const libc::inotify_event) };
                let name = &buf[off + head..off + head + ev.len as usize];
                let name = name.split(|b| *b == 0).next().unwrap_or(&[]);
                if !name.is_empty() || ev.mask & libc::IN_DELETE_SELF != 0 {
                    w.event(ev.wd, ev.mask, name);
                }
                off += head + ev.len as usize;
            }
        }
        if (!w.active.is_empty() || !w.shown.is_empty() || !ENGINE.lock().unwrap().is_empty()) && last_sample.elapsed() >= Duration::from_millis(900) {
            last_sample = Instant::now();
            w.sample();
        }
    }
}

/// A browser's download finished (when the watcher is on).
fn finished(path: PathBuf) {
    if ENABLED.load(Ordering::SeqCst) {
        announce(path);
    }
}

/// One session bus connection for every "Download finished" notification: each waits for its
/// answer, so a connection apiece would pile up open sockets on the bus.
fn session_bus() -> Option<zbus::blocking::Connection> {
    static BUS: std::sync::Mutex<Option<zbus::blocking::Connection>> = std::sync::Mutex::new(None);
    let mut bus = BUS.lock().unwrap();
    // The bus may have restarted since.
    let alive = bus.as_ref().is_some_and(|c| c.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus.Peer"), "Ping", &()).is_ok());
    if !alive {
        *bus = zbus::blocking::Connection::session().ok();
    }
    bus.clone()
}

/// "Download finished" with Open and Show in Folder, and the pill's "Download completed".
pub fn announce(path: PathBuf) {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let _ = crate::send_ui(&UiCommand::DownloadDone(name));
    std::thread::spawn(move || {
        let Some(session) = session_bus() else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let body = format!("{name}\n{}", human(size_of(&path).unwrap_or(0)));
        let answer = crate::obex::ask_as(&session, "Downloads", "Download finished", &body, "folder-download", &["default", "Open", "open", "Open", "folder", "Show in Folder"], None);
        match answer.as_deref() {
            Some("open") | Some("default") => open_file(&path),
            Some("folder") => show_in_folder(&path),
            _ => {}
        }
    });
}

pub fn open_file(path: &Path) {
    let _ = prism_common::apps::app_command("xdg-open", &[path.to_string_lossy().to_string()]).spawn();
}

/// The file manager selects the file when it can (FileManager1), else opens the folder.
pub fn show_in_folder(path: &Path) {
    let uri = format!("file://{}", path.to_string_lossy());
    let shown = zbus::blocking::Connection::session()
        .and_then(|s| s.call_method(Some("org.freedesktop.FileManager1"), "/org/freedesktop/FileManager1", Some("org.freedesktop.FileManager1"), "ShowItems", &(vec![uri.as_str()], "")))
        .is_ok();
    if !shown {
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let _ = prism_common::apps::app_command("xdg-open", &[folder.to_string_lossy().to_string()]).spawn();
    }
}

/// 1.4 GB, 230 MB, 12 KB.
pub fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_files_and_brisk_folders() {
        let root = Path::new("/home/u/Downloads");
        let roots = Roots { dirs: vec![root.to_path_buf(), PathBuf::from("/data/dl")], brisk_temp: Some(root.join("Brisk/Temp")) };
        let r = &roots;
        assert_eq!(classify(r, &root.join("a.zip.part")), Kind::Temp(root.join("a.zip.part")));
        assert_eq!(classify(r, &root.join("x/Unconfirmed 1.crdownload")), Kind::Temp(root.join("x/Unconfirmed 1.crdownload")));
        assert_eq!(classify(r, &root.join("Brisk/Temp/abc/seg1")), Kind::Temp(root.join("Brisk/Temp/abc")));
        assert_eq!(classify(r, &root.join("Brisk/Temp/abc")), Kind::Temp(root.join("Brisk/Temp/abc")));
        assert_eq!(classify(r, &root.join("Brisk/Temp")), Kind::Ignore);
        assert_eq!(classify(r, &root.join("a.zip")), Kind::File);
        assert_eq!(classify(r, Path::new("/data/dl/b.iso.part")), Kind::Temp(PathBuf::from("/data/dl/b.iso.part")));
        assert_eq!(classify(r, Path::new("/elsewhere/b.iso")), Kind::Ignore);
        assert_eq!(classify(r, &root.join(".cache/a.part")), Kind::Ignore);
        assert_eq!(temp_name(&root.join("a.zip.part")), "a.zip");
        assert_eq!(temp_name(&root.join("Brisk/Temp/abc")), "abc");
    }
}
