//! Prism's download engine: segmented HTTP downloads with connections added on the fly.
//!
//! A download starts on one connection. A controller adds connections while there are fewer
//! than `Options::connections`, each taking the second half of the largest range still to
//! fetch. A connection that finishes its range takes half of the largest remaining one, so
//! finished connections keep helping until the file is complete. Ranges are written in place
//! (`pwrite`) into one preallocated temporary file; a connection that receives nothing for
//! `Options::stall` is dropped and started again from where it stopped.
//!
//! Two connections can briefly write the same bytes after a split (the old one read past the
//! new boundary). The bytes are the same, so this is harmless.
//!
//! The ranges still to fetch (`Handle::remaining`) are what a paused or interrupted download
//! needs to continue; `start` takes them back. Servers without range support get one
//! connection and restart from the beginning.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ureq::ResponseExt;

pub mod hls;

/// What to fetch and how the browser would have fetched it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub url: String,
    #[serde(default)]
    pub referer: Option<String>,
    /// `Cookie` header value.
    #[serde(default)]
    pub cookie: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Other headers to send.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// What the server says about the file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// After redirects.
    pub url: String,
    pub size: Option<u64>,
    /// The server answers byte ranges.
    pub ranges: bool,
    /// From Content-Disposition, else the URL; never empty, safe as a file name.
    pub name: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Most connections at once.
    pub connections: usize,
    /// No range smaller than this is split.
    pub min_split: u64,
    /// A connection that receives nothing for this long is restarted.
    pub stall: Duration,
    /// Failed requests in a row (per connection) before the download fails.
    pub retries: u32,
    /// Bytes per second over all connections; 0 = no limit.
    pub limit: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options { connections: 8, min_split: 1 << 20, stall: Duration::from_secs(20), retries: 5, limit: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Running,
    Paused,
    Done,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub state: State,
    /// Bytes fetched (of the whole file, including what earlier runs fetched).
    pub done: u64,
    pub size: Option<u64>,
    /// Connections receiving now.
    pub connections: usize,
}

/// `PRISM_DL_DEBUG=1`: log requests and connection errors.
pub(crate) fn debug(msg: impl FnOnce() -> String) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("PRISM_DL_DEBUG").is_some()) {
        eprintln!("[prism-dl] {}", msg());
    }
}

pub(crate) fn agent(stall: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(10)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(stall))
        .max_idle_connections_per_host(16)
        .build()
        .into()
}

pub(crate) fn get(agent: &ureq::Agent, url: &str, req: &Request, range: Option<(u64, Option<u64>)>) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut b = agent.get(url).header("Accept-Encoding", "identity");
    if let Some(r) = &req.referer {
        b = b.header("Referer", r);
    }
    if let Some(c) = &req.cookie {
        b = b.header("Cookie", c);
    }
    if let Some(ua) = &req.user_agent {
        b = b.header("User-Agent", ua);
    }
    for (k, v) in &req.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    if let Some((from, to)) = range {
        let v = match to {
            Some(to) => format!("bytes={from}-{to}"),
            None => format!("bytes={from}-"),
        };
        b = b.header("Range", v);
    }
    b.call().map_err(|e| e.to_string())
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Ask for the first byte: a 206 with Content-Range gives the size and range support at once
/// (HEAD is often refused or wrong for signed links).
pub fn probe(req: &Request) -> Result<Probe, String> {
    let agent = agent(Duration::from_secs(30));
    let resp = get(&agent, &req.url, req, Some((0, Some(0))))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("the server answered {status}"));
    }
    let url = resp.get_uri().to_string();
    let (size, ranges) = if status == 206 {
        let total = header(&resp, "content-range").and_then(|v| v.rsplit('/').next().and_then(|t| t.trim().parse::<u64>().ok()));
        (total, total.is_some())
    } else {
        (header(&resp, "content-length").and_then(|v| v.parse().ok()), false)
    };
    let name = header(&resp, "content-disposition").and_then(|d| disposition_name(&d)).or_else(|| url_name(&url)).map(|n| sanitize(&n)).filter(|n| !n.is_empty()).unwrap_or_else(|| "download".into());
    Ok(Probe {
        url,
        size,
        ranges,
        name,
        etag: header(&resp, "etag"),
        last_modified: header(&resp, "last-modified"),
        content_type: header(&resp, "content-type"),
    })
}

/// `filename*=UTF-8''a%20b.zip` (preferred) or `filename="a b.zip"`.
pub fn disposition_name(d: &str) -> Option<String> {
    let mut plain = None;
    for part in d.split(';').map(str::trim) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename*" {
            let v = v.splitn(3, '\'').nth(2).unwrap_or(v);
            return Some(percent_decode(v.trim_matches('"')));
        }
        if k == "filename" {
            plain = Some(v.trim_matches('"').to_string());
        }
    }
    plain
}

/// The last path segment of a URL, decoded.
pub fn url_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let after_host = path.split_once("://").map(|(_, r)| r).unwrap_or(path);
    let (_, path) = after_host.split_once('/')?;
    let last = path.rsplit('/').next()?;
    (!last.is_empty()).then(|| percent_decode(last))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A name that is safe in one folder: no separators or control characters, no leading dots,
/// at most 200 bytes.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    s = s.trim().trim_start_matches('.').trim().to_string();
    while s.len() > 200 {
        s.pop();
    }
    s
}

/// `dir/name`, or `dir/name (1).ext` and up when taken (also by a download's temporary file).
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let taken = |p: &Path| p.exists() || temp_path(p).exists();
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    // Keep ".tar.gz" together.
    let (stem, ext) = match stem.strip_suffix(".tar") {
        Some(s) => (s, format!(".tar{ext}")),
        None => (stem, ext.to_string()),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !taken(p)).unwrap()
}

/// Where a download is written until it is complete.
pub fn temp_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".prismdl");
    PathBuf::from(s)
}

/// One range still to fetch: `pos..end`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seg {
    pos: u64,
    end: u64,
    /// A connection is fetching it.
    taken: bool,
}

pub(crate) struct Shared {
    segs: Mutex<Vec<Seg>>,
    /// Size of the whole download when known (HLS: an estimate that firms up), else 0.
    total: AtomicU64,
    done: AtomicU64,
    stop: AtomicBool,
    state: Mutex<State>,
    workers: AtomicUsize,
    receiving: AtomicUsize,
    /// Connections allowed now: lowered each time one gives up while others still work
    /// (servers that refuse or drop extra connections).
    cap: AtomicUsize,
    /// Bytes allowed so far by the speed limit, and when the window started.
    budget: Mutex<(Instant, u64)>,
}

pub struct Handle {
    pub(crate) shared: Arc<Shared>,
}

impl Handle {
    pub fn progress(&self) -> Progress {
        Progress {
            state: self.shared.state.lock().unwrap().clone(),
            done: self.shared.done.load(Ordering::SeqCst),
            size: Some(self.shared.total.load(Ordering::SeqCst)).filter(|t| *t > 0),
            connections: self.shared.receiving.load(Ordering::SeqCst),
        }
    }

    /// Stop every connection; the state becomes Paused once they have all ended.
    pub fn pause(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// Ranges still to fetch (for `start` after a pause or a restart).
    pub fn remaining(&self) -> Vec<(u64, u64)> {
        remaining(&self.shared)
    }
}

fn remaining(sh: &Shared) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = sh.segs.lock().unwrap().iter().filter(|s| s.pos < s.end).map(|s| (s.pos, s.end)).collect();
    v.sort();
    v
}

/// Start (or continue) fetching `probe.url` into `temp`. `resume` is `Handle::remaining` from
/// an earlier run of the same file; None starts from the beginning. The caller renames `temp`
/// when the state is Done.
pub fn start(req: Request, probe: &Probe, temp: &Path, resume: Option<Vec<(u64, u64)>>, opts: Options) -> Result<Handle, String> {
    let ranged = probe.ranges && probe.size.is_some();
    let size = probe.size;
    let file = std::fs::OpenOptions::new().create(true).write(true).truncate(!ranged || resume.is_none()).open(temp).map_err(|e| format!("{}: {e}", temp.display()))?;
    let segs: Vec<Seg> = match (ranged, size, resume) {
        (true, Some(size), Some(r)) => r.into_iter().filter(|(a, b)| a < b && *b <= size).map(|(pos, end)| Seg { pos, end, taken: false }).collect(),
        (true, Some(size), None) => {
            file.set_len(size).map_err(|e| e.to_string())?;
            vec![Seg { pos: 0, end: size, taken: false }]
        }
        // Unknown size or no ranges: one connection to the end of the body.
        _ => vec![Seg { pos: 0, end: u64::MAX, taken: false }],
    };
    let left: u64 = segs.iter().map(|s| s.end.saturating_sub(s.pos)).sum();
    let done = match size {
        Some(size) if ranged => size.saturating_sub(left),
        _ => 0,
    };
    let shared = Arc::new(Shared {
        segs: Mutex::new(segs),
        total: AtomicU64::new(size.unwrap_or(0)),
        done: AtomicU64::new(done),
        stop: AtomicBool::new(false),
        state: Mutex::new(State::Running),
        workers: AtomicUsize::new(0),
        receiving: AtomicUsize::new(0),
        cap: AtomicUsize::new(opts.connections.max(1)),
        budget: Mutex::new((Instant::now(), 0)),
    });
    let file = Arc::new(file);
    let url = probe.url.clone();
    let sh = shared.clone();
    std::thread::Builder::new()
        .name("prism-dl".into())
        .spawn(move || control(sh, file, url, req, ranged, opts))
        .map_err(|e| e.to_string())?;
    Ok(Handle { shared })
}

/// Adds connections, then waits for them and settles the state.
fn control(sh: Arc<Shared>, file: Arc<File>, url: String, req: Request, ranged: bool, opts: Options) {
    let agent = agent(opts.stall);
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let spawn = |seg: usize| {
        let (sh, file, url, req, agent, failure, opts) = (sh.clone(), file.clone(), url.clone(), req.clone(), agent.clone(), failure.clone(), opts.clone());
        sh.workers.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            if let Err(e) = worker(&sh, &file, &agent, &url, &req, seg, ranged, &opts) {
                // Others still running carry on with fewer connections; the last one fails.
                let others = sh.workers.load(Ordering::SeqCst) > 1;
                if others {
                    let cap = sh.cap.load(Ordering::SeqCst);
                    sh.cap.store(cap.saturating_sub(1).max(1), Ordering::SeqCst);
                } else {
                    failure.lock().unwrap().get_or_insert(e);
                }
            }
            sh.workers.fetch_sub(1, Ordering::SeqCst);
        });
    };
    // Every range left from an earlier run gets a connection (up to the limit).
    let n = sh.segs.lock().unwrap().len();
    for i in 0..n.min(opts.connections.max(1)) {
        sh.segs.lock().unwrap()[i].taken = true;
        spawn(i);
    }
    let mut last_add = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let workers = sh.workers.load(Ordering::SeqCst);
        if workers == 0 {
            break;
        }
        // Add one only while every connection is receiving (the last one added is flowing).
        let flowing = sh.receiving.load(Ordering::SeqCst) >= workers;
        if !ranged || !flowing || sh.stop.load(Ordering::SeqCst) || workers >= sh.cap.load(Ordering::SeqCst) || last_add.elapsed() < Duration::from_millis(400) {
            continue;
        }
        // One more connection at a time while it pays: a free range, or a split of the largest.
        if let Some(i) = claim(&sh, opts.min_split) {
            last_add = Instant::now();
            spawn(i);
        }
    }
    let _ = file.sync_all();
    let left = remaining(&sh);
    let state = if let Some(e) = failure.lock().unwrap().take() {
        State::Failed(e)
    } else if left.is_empty() || (!ranged && !sh.stop.load(Ordering::SeqCst)) {
        State::Done
    } else if !sh.stop.load(Ordering::SeqCst) {
        State::Failed("every connection failed".into())
    } else {
        State::Paused
    };
    *sh.state.lock().unwrap() = state;
}

/// A range for a new connection: an untaken one, else the second half of the largest taken
/// one when that is at least two `min_split`s. Marks it taken and returns its index.
fn claim(sh: &Shared, min_split: u64) -> Option<usize> {
    let mut segs = sh.segs.lock().unwrap();
    if let Some(i) = segs.iter().position(|s| !s.taken && s.pos < s.end) {
        segs[i].taken = true;
        return Some(i);
    }
    let (i, len) = segs.iter().enumerate().filter(|(_, s)| s.taken).map(|(i, s)| (i, s.end.saturating_sub(s.pos))).max_by_key(|(_, l)| *l)?;
    if len < 2 * min_split.max(1) {
        return None;
    }
    let mid = segs[i].pos + len / 2;
    let end = segs[i].end;
    segs[i].end = mid;
    segs.push(Seg { pos: mid, end, taken: true });
    Some(segs.len() - 1)
}

/// Wait until the speed limit allows `n` more bytes.
pub(crate) fn throttle(sh: &Shared, limit: u64, n: u64) {
    if limit == 0 {
        return;
    }
    let wait = {
        let mut b = sh.budget.lock().unwrap();
        if b.0.elapsed() > Duration::from_secs(2) {
            *b = (Instant::now(), 0);
        }
        b.1 += n;
        let due = Duration::from_secs_f64(b.1 as f64 / limit as f64);
        due.saturating_sub(b.0.elapsed())
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
}

/// Fetch range `seg`, then keep taking more until nothing is left to take.
#[allow(clippy::too_many_arguments)]
fn worker(sh: &Shared, file: &File, agent: &ureq::Agent, url: &str, req: &Request, mut seg: usize, ranged: bool, opts: &Options) -> Result<(), String> {
    let mut failures = 0u32;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if sh.stop.load(Ordering::SeqCst) {
            sh.segs.lock().unwrap()[seg].taken = false;
            return Ok(());
        }
        let (pos, end) = {
            let s = sh.segs.lock().unwrap()[seg];
            (s.pos, s.end)
        };
        if pos >= end {
            // Done with this one: help with the largest range left, or stop.
            sh.segs.lock().unwrap()[seg].taken = false;
            match claim(sh, opts.min_split) {
                Some(i) if ranged => {
                    seg = i;
                    continue;
                }
                Some(i) => {
                    sh.segs.lock().unwrap()[i].taken = false;
                    return Ok(());
                }
                None => return Ok(()),
            }
        }
        match fetch(sh, file, agent, url, req, seg, pos, end, ranged, opts, &mut buf) {
            Ok(()) => failures = 0,
            // Too many connections for this server: fewer from now on. An extra connection
            // gives its range back; the only one waits as long as the server asks.
            Err(Fail::Busy(wait)) => {
                let receiving = sh.receiving.load(Ordering::SeqCst);
                sh.cap.store(receiving.max(1), Ordering::SeqCst);
                debug(|| format!("range {pos}-{end}: server busy, cap {}", receiving.max(1)));
                if sh.workers.load(Ordering::SeqCst) > 1 {
                    sh.segs.lock().unwrap()[seg].taken = false;
                    return Ok(());
                }
                failures += 1;
                if failures > opts.retries * 3 {
                    sh.segs.lock().unwrap()[seg].taken = false;
                    return Err("the server keeps answering that it is busy".into());
                }
                let until = Instant::now() + wait.unwrap_or(Duration::from_secs(2 * failures.min(15) as u64));
                while Instant::now() < until && !sh.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            Err(Fail::Other(e)) => {
                failures += 1;
                debug(|| format!("range {pos}-{end}: {e} (failure {failures})"));
                if failures > opts.retries {
                    sh.segs.lock().unwrap()[seg].taken = false;
                    return Err(e);
                }
                if !ranged {
                    // Starting over: forget what this run had written.
                    sh.done.store(0, Ordering::SeqCst);
                    sh.segs.lock().unwrap()[seg].pos = 0;
                }
                let backoff = Duration::from_millis(500 * (1 << failures.min(5)));
                let until = Instant::now() + backoff;
                while Instant::now() < until && !sh.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

enum Fail {
    /// 429 or 503, with Retry-After when given.
    Busy(Option<Duration>),
    Other(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Other(s)
    }
}

impl From<&str> for Fail {
    fn from(s: &str) -> Self {
        Fail::Other(s.to_string())
    }
}

/// One request for `pos..end` of range `seg`, read until the range (which may shrink while
/// reading) is complete or the connection fails.
#[allow(clippy::too_many_arguments)]
fn fetch(sh: &Shared, file: &File, agent: &ureq::Agent, url: &str, req: &Request, seg: usize, pos: u64, end: u64, ranged: bool, opts: &Options, buf: &mut [u8]) -> Result<(), Fail> {
    let range = if ranged { Some((pos, Some(end - 1))) } else { None };
    debug(|| format!("GET {range:?}"));
    let mut resp = get(agent, url, req, range)?;
    let status = resp.status().as_u16();
    if status == 429 || status == 503 {
        let wait = header(&resp, "retry-after").and_then(|v| v.parse::<u64>().ok()).map(|s| Duration::from_secs(s.min(120)));
        return Err(Fail::Busy(wait));
    }
    if ranged && status != 206 {
        return Err(format!("the server answered {status} to a range request").into());
    }
    if !ranged && !(200..300).contains(&status) {
        return Err(format!("the server answered {status}").into());
    }
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    sh.receiving.fetch_add(1, Ordering::SeqCst);
    struct Receiving<'a>(&'a AtomicUsize);
    impl Drop for Receiving<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _r = Receiving(&sh.receiving);
    let mut at = pos;
    loop {
        if sh.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let n = match reader.read(buf) {
            Ok(0) if !ranged => {
                // The whole body arrived.
                sh.segs.lock().unwrap()[seg].end = at;
                return Ok(());
            }
            Ok(0) => return Err("the connection closed early".into()),
            Ok(n) => n as u64,
            Err(e) => return Err(e.to_string().into()),
        };
        // The range may have been split since: write only what is still ours.
        let end_now = sh.segs.lock().unwrap()[seg].end;
        let take = n.min(end_now.saturating_sub(at));
        if take > 0 {
            file.write_all_at(&buf[..take as usize], at).map_err(|e| format!("writing: {e}"))?;
            at += take;
            let mut segs = sh.segs.lock().unwrap();
            segs[seg].pos = at.min(segs[seg].end);
            drop(segs);
            sh.done.fetch_add(take, Ordering::SeqCst);
            throttle(sh, opts.limit, take);
        }
        if at >= end_now {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn names() {
        assert_eq!(disposition_name("attachment; filename=\"a b.zip\"").as_deref(), Some("a b.zip"));
        assert_eq!(disposition_name("attachment; filename=x.bin; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf").as_deref(), Some("résumé.pdf"));
        assert_eq!(url_name("https://h.org/dl/My%20File.iso?token=1").as_deref(), Some("My File.iso"));
        assert_eq!(url_name("https://h.org/"), None);
        assert_eq!(url_name("https://h.org"), None);
        assert_eq!(sanitize("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("prism-dl-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.tar.gz"), b"").unwrap();
        std::fs::write(temp_path(&dir.join("b.zip")), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.tar.gz"), dir.join("a (1).tar.gz"));
        assert_eq!(unique_path(&dir, "b.zip"), dir.join("b (1).zip"));
        assert_eq!(unique_path(&dir, "c"), dir.join("c"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A tiny HTTP/1.1 server for `data`: answers Range requests (when `ranges`), with an
    /// optional delay per 64 KiB so several connections get a chance.
    fn serve(data: Arc<Vec<u8>>, ranges: bool, delay: Duration) -> String {
        serve_max(data, ranges, delay, usize::MAX)
    }

    /// Like `serve`, answering 503 to requests beyond `max` at once.
    fn serve_max(data: Arc<Vec<u8>>, ranges: bool, delay: Duration, max: usize) -> String {
        let busy = Arc::new(AtomicUsize::new(0));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let data = data.clone();
                let busy = busy.clone();
                std::thread::spawn(move || loop {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut range = None;
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 {
                            return;
                        }
                        if h == "\r\n" {
                            break;
                        }
                        let lower = h.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("range: bytes=") {
                            let (a, b) = v.trim().split_once('-').unwrap();
                            let a: u64 = a.parse().unwrap();
                            let b: u64 = if b.is_empty() { data.len() as u64 - 1 } else { b.parse().unwrap() };
                            range = Some((a, b.min(data.len() as u64 - 1)));
                        }
                    }
                    if busy.fetch_add(1, Ordering::SeqCst) >= max {
                        busy.fetch_sub(1, Ordering::SeqCst);
                        let _ = s.write_all(b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    struct Done(Arc<AtomicUsize>);
                    impl Drop for Done {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _done = Done(busy.clone());
                    let (head, body) = match range {
                        Some((a, b)) if ranges => (
                            format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"test file.bin\"\r\n\r\n", data.len(), b - a + 1),
                            &data[a as usize..=b as usize],
                        ),
                        _ => (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()), &data[..]),
                    };
                    if s.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    for c in body.chunks(64 * 1024) {
                        if s.write_all(c).is_err() {
                            return;
                        }
                        std::thread::sleep(delay);
                    }
                });
            }
        });
        format!("http://{addr}/files/thing.bin")
    }

    fn data(n: usize) -> Arc<Vec<u8>> {
        Arc::new((0..n).map(|i| (i * 31 % 251) as u8).collect())
    }

    fn wait(h: &Handle) -> Progress {
        let t = Instant::now();
        loop {
            let p = h.progress();
            if p.state != State::Running || t.elapsed() > Duration::from_secs(30) {
                return p;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prism-dl-{}-{name}", std::process::id()))
    }

    #[test]
    fn segmented_download_matches() {
        let d = data(6 << 20);
        let url = serve(d.clone(), true, Duration::from_millis(5));
        let req = Request { url, ..Default::default() };
        let p = probe(&req).unwrap();
        assert_eq!(p.size, Some(d.len() as u64));
        assert!(p.ranges);
        assert_eq!(p.name, "test file.bin");
        let out = tmp("seg");
        let h = start(req, &p, &out, None, Options { connections: 6, min_split: 256 << 10, ..Default::default() }).unwrap();
        let mut most = 0;
        while h.progress().state == State::Running {
            most = most.max(h.progress().connections);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(wait(&h).state, State::Done);
        assert!(most > 1, "only {most} connection(s) were used");
        assert_eq!(std::fs::read(&out).unwrap(), *d);
        std::fs::remove_file(&out).unwrap();
    }

    #[test]
    fn pause_and_resume() {
        let d = data(4 << 20);
        let url = serve(d.clone(), true, Duration::from_millis(10));
        let req = Request { url, ..Default::default() };
        let p = probe(&req).unwrap();
        let out = tmp("resume");
        let opts = Options { connections: 4, min_split: 256 << 10, ..Default::default() };
        let h = start(req.clone(), &p, &out, None, opts.clone()).unwrap();
        while h.progress().done < (1 << 20) {
            std::thread::sleep(Duration::from_millis(5));
        }
        h.pause();
        assert_eq!(wait(&h).state, State::Paused);
        let left = h.remaining();
        assert!(!left.is_empty());
        let h = start(req, &p, &out, Some(left), opts).unwrap();
        assert_eq!(wait(&h).state, State::Done);
        assert_eq!(h.progress().done, d.len() as u64);
        assert_eq!(std::fs::read(&out).unwrap(), *d);
        std::fs::remove_file(&out).unwrap();
    }

    #[test]
    fn server_refusing_extra_connections() {
        let d = data(3 << 20);
        let url = serve_max(d.clone(), true, Duration::from_millis(5), 2);
        let req = Request { url, ..Default::default() };
        let p = probe(&req).unwrap();
        let out = tmp("refuse");
        let h = start(req, &p, &out, None, Options { connections: 6, min_split: 128 << 10, retries: 1, ..Default::default() }).unwrap();
        assert_eq!(wait(&h).state, State::Done);
        assert_eq!(std::fs::read(&out).unwrap(), *d);
        std::fs::remove_file(&out).unwrap();
    }

    #[test]
    fn server_without_ranges() {
        let d = data(300_000);
        let url = serve(d.clone(), false, Duration::ZERO);
        let req = Request { url, ..Default::default() };
        let p = probe(&req).unwrap();
        assert!(!p.ranges);
        assert_eq!(p.name, "thing.bin");
        let out = tmp("plain");
        let h = start(req, &p, &out, None, Options::default()).unwrap();
        assert_eq!(wait(&h).state, State::Done);
        assert_eq!(std::fs::read(&out).unwrap(), *d);
        std::fs::remove_file(&out).unwrap();
    }

    #[test]
    fn missing_file_fails() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let e = probe(&Request { url: format!("http://{addr}/x"), ..Default::default() }).unwrap_err();
        assert!(e.contains("404"), "{e}");
    }
}
