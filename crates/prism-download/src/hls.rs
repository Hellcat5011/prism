//! HLS (m3u8) video downloads: the chosen variant of a master playlist (or a media playlist
//! as it is) and its audio rendition, segments fetched over several connections, AES-128
//! segments decrypted, then joined and remuxed into one MP4 with ffmpeg (stream copy).
//!
//! Each finished segment is a file in the work folder, so a paused or interrupted download
//! skips what it already has when started again. Live streams (no `#EXT-X-ENDLIST`) are
//! refused. The result is written to `out` (the caller's temporary file, MP4).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{agent, debug, get, Handle, Options, Request, Shared, State};

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub uri: String,
    pub bandwidth: u64,
    pub resolution: Option<(u32, u32)>,
    /// AUDIO group id.
    pub audio: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rendition {
    pub group: String,
    pub uri: String,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub uri: String,
    pub iv: Option<[u8; 16]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub uri: String,
    /// EXT-X-BYTERANGE: (offset, length).
    pub range: Option<(u64, u64)>,
    pub key: Option<Key>,
    /// Media sequence number (the IV when the key gives none).
    pub seq: u64,
    pub duration: f64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Media {
    pub segments: Vec<Segment>,
    /// EXT-X-MAP: the init segment of fragmented MP4 streams.
    pub map: Option<(String, Option<(u64, u64)>)>,
    pub ended: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Playlist {
    Master { variants: Vec<Variant>, audio: Vec<Rendition> },
    Media(Media),
}

/// `rel` against the playlist URL `base`.
pub fn resolve(base: &str, rel: &str) -> String {
    if rel.contains("://") {
        return rel.to_string();
    }
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    if let Some(r) = rel.strip_prefix("//") {
        return format!("{}{r}", &base[..scheme_end]);
    }
    let host_end = base[scheme_end..].find('/').map(|i| i + scheme_end).unwrap_or(base.len());
    if rel.starts_with('/') {
        return format!("{}{rel}", &base[..host_end]);
    }
    let path = base.split(['?', '#']).next().unwrap_or(base);
    let dir_end = path.rfind('/').filter(|&i| i >= host_end).map(|i| i + 1).unwrap_or(path.len());
    let mut out = format!("{}{}", &path[..dir_end.max(host_end)], if dir_end <= host_end { "/" } else { "" });
    out.push_str(rel);
    out
}

/// `KEY=value,KEY="quoted, value"` attribute lists.
fn attrs(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = s;
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_ascii_uppercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            value = r[..end].to_string();
            rest = r.get(end + 1..).unwrap_or("");
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        rest = rest.trim_start_matches(',');
        out.insert(key, value);
    }
    out
}

fn byterange(v: &str, next: &mut u64) -> (u64, u64) {
    let (len, off) = match v.split_once('@') {
        Some((l, o)) => (l.trim().parse().unwrap_or(0), o.trim().parse().unwrap_or(*next)),
        None => (v.trim().parse().unwrap_or(0), *next),
    };
    *next = off + len;
    (off, len)
}

fn hex_iv(v: &str) -> Option<[u8; 16]> {
    let h = v.trim().trim_start_matches("0x").trim_start_matches("0X");
    let h = format!("{h:0>32}");
    let mut iv = [0u8; 16];
    for (i, b) in iv.iter_mut().enumerate() {
        *b = u8::from_str_radix(h.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(iv)
}

pub fn parse(text: &str, base: &str) -> Result<Playlist, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err("not an HLS playlist".into());
    }
    let mut variants = Vec::new();
    let mut audio = Vec::new();
    let mut media = Media::default();
    let mut pending_variant: Option<Variant> = None;
    let mut key: Option<Key> = None;
    let mut seq = 0u64;
    let mut duration = 0.0;
    let mut range: Option<(u64, u64)> = None;
    let mut next_off = 0u64;
    for line in lines {
        if let Some(v) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(v);
            let resolution = a.get("RESOLUTION").and_then(|r| r.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
            pending_variant = Some(Variant { uri: String::new(), bandwidth: a.get("BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0), resolution, audio: a.get("AUDIO").cloned() });
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(v);
            if a.get("TYPE").map(String::as_str) == Some("AUDIO") {
                if let (Some(g), Some(u)) = (a.get("GROUP-ID"), a.get("URI")) {
                    audio.push(Rendition { group: g.clone(), uri: resolve(base, u), default: a.get("DEFAULT").map(String::as_str) == Some("YES") });
                }
            }
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("#EXT-X-KEY:") {
            let a = attrs(v);
            key = match a.get("METHOD").map(String::as_str) {
                Some("NONE") | None => None,
                Some("AES-128") => Some(Key { uri: resolve(base, a.get("URI").map(String::as_str).unwrap_or("")), iv: a.get("IV").and_then(|iv| hex_iv(iv)) }),
                Some(m) => return Err(format!("{m} encrypted streams (DRM) cannot be downloaded")),
            };
        } else if let Some(v) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attrs(v);
            let mut off = 0;
            media.map = a.get("URI").map(|u| (resolve(base, u), a.get("BYTERANGE").map(|b| byterange(b, &mut off))));
        } else if let Some(v) = line.strip_prefix("#EXTINF:") {
            duration = v.split(',').next().and_then(|d| d.trim().parse().ok()).unwrap_or(0.0);
        } else if let Some(v) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(byterange(v, &mut next_off));
        } else if line == "#EXT-X-ENDLIST" {
            media.ended = true;
        } else if !line.starts_with('#') {
            if let Some(mut v) = pending_variant.take() {
                v.uri = resolve(base, line);
                variants.push(v);
            } else {
                media.segments.push(Segment { uri: resolve(base, line), range: range.take(), key: key.clone(), seq, duration });
                seq += 1;
                duration = 0.0;
            }
        }
    }
    if !variants.is_empty() {
        return Ok(Playlist::Master { variants, audio });
    }
    Ok(Playlist::Media(media))
}

fn fetch_text(agent: &ureq::Agent, url: &str, req: &Request) -> Result<String, String> {
    let mut resp = get(agent, url, req, None)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("the server answered {status} for the playlist"));
    }
    resp.body_mut().with_config().limit(16 << 20).read_to_string().map_err(|e| e.to_string())
}

fn fetch_bytes(agent: &ureq::Agent, url: &str, req: &Request, range: Option<(u64, u64)>) -> Result<Vec<u8>, String> {
    let r = range.map(|(off, len)| (off, Some(off + len.max(1) - 1)));
    let mut resp = get(agent, url, req, r)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("the server answered {status}"));
    }
    let mut out = Vec::new();
    resp.body_mut().with_config().limit(1 << 30).reader().read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn decrypt(data: &[u8], key: &[u8], iv: [u8; 16]) -> Result<Vec<u8>, String> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    let k: [u8; 16] = key.get(..16).and_then(|k| k.try_into().ok()).ok_or("the stream's key is not 16 bytes")?;
    cbc::Decryptor::<aes::Aes128>::new(&k.into(), &iv.into()).decrypt_padded_vec_mut::<Pkcs7>(data).map_err(|_| "decrypting a segment failed (wrong key?)".to_string())
}

/// What one HLS download fetches: per track (video, then audio) the init segment and segments.
struct Plan {
    tracks: Vec<Media>,
}

/// Pick the variant (`variant` URL, else the highest bandwidth) and its audio, read the media
/// playlists.
fn plan(agent: &ureq::Agent, req: &Request, variant: Option<&str>) -> Result<Plan, String> {
    let text = fetch_text(agent, &req.url, req)?;
    let mut tracks = Vec::new();
    match parse(&text, &req.url)? {
        Playlist::Media(m) => tracks.push(m),
        Playlist::Master { variants, audio } => {
            let v = variant.and_then(|u| variants.iter().find(|v| v.uri == u)).or_else(|| variants.iter().max_by_key(|v| v.bandwidth)).ok_or("the playlist has no streams")?;
            for url in std::iter::once(v.uri.clone()).chain(v.audio.as_ref().and_then(|g| {
                let group: Vec<&Rendition> = audio.iter().filter(|r| &r.group == g).collect();
                group.iter().find(|r| r.default).or(group.first()).map(|r| r.uri.clone())
            })) {
                match parse(&fetch_text(agent, &url, req)?, &url)? {
                    Playlist::Media(m) => tracks.push(m),
                    Playlist::Master { .. } => return Err("nested master playlists are not supported".into()),
                }
            }
        }
    }
    if tracks.iter().any(|t| !t.ended) {
        return Err("live streams cannot be downloaded (the playlist has no end)".into());
    }
    if tracks.iter().all(|t| t.segments.is_empty()) {
        return Err("the playlist has no segments".into());
    }
    Ok(Plan { tracks })
}

fn seg_path(work: &Path, track: usize, idx: usize) -> PathBuf {
    work.join(format!("t{track}-{idx:06}.seg"))
}

/// Start (or continue: finished segments in `work` are kept) an HLS download into `out`.
/// `variant` is the variant playlist URL chosen from the master playlist at `req.url`.
pub fn start(req: Request, variant: Option<String>, work: &Path, out: &Path, opts: Options) -> Result<Handle, String> {
    std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    let shared = Arc::new(Shared {
        segs: Mutex::new(Vec::new()),
        total: AtomicU64::new(0),
        done: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        state: Mutex::new(State::Running),
        workers: AtomicUsize::new(0),
        receiving: AtomicUsize::new(0),
        cap: AtomicUsize::new(opts.connections.max(1)),
        budget: Mutex::new((Instant::now(), 0)),
    });
    let sh = shared.clone();
    let (work, out) = (work.to_path_buf(), out.to_path_buf());
    std::thread::Builder::new()
        .name("prism-hls".into())
        .spawn(move || {
            let r = run(&sh, &req, variant.as_deref(), &work, &out, &opts);
            *sh.state.lock().unwrap() = match r {
                Ok(true) => {
                    let _ = std::fs::remove_dir_all(&work);
                    State::Done
                }
                Ok(false) => State::Paused,
                Err(e) => State::Failed(e),
            };
        })
        .map_err(|e| e.to_string())?;
    Ok(Handle { shared })
}

/// Ok(true) when complete, Ok(false) when stopped.
fn run(sh: &Arc<Shared>, req: &Request, variant: Option<&str>, work: &Path, out: &Path, opts: &Options) -> Result<bool, String> {
    let agent = agent(opts.stall);
    let plan = plan(&agent, req, variant)?;
    // Jobs: (track, index); index 0 is the init segment slot (empty when there is none).
    let jobs: Vec<(usize, usize)> = plan.tracks.iter().enumerate().flat_map(|(t, m)| (0..=m.segments.len()).map(move |i| (t, i))).collect();
    let total_jobs = jobs.len();
    let have: Vec<bool> = jobs.iter().map(|&(t, i)| seg_path(work, t, i).exists()).collect();
    let done_jobs = Arc::new(AtomicUsize::new(have.iter().filter(|h| **h).count()));
    let on_disk: u64 = jobs.iter().zip(&have).filter(|(_, h)| **h).map(|(&(t, i), _)| std::fs::metadata(seg_path(work, t, i)).map(|m| m.len()).unwrap_or(0)).sum();
    sh.done.store(on_disk, Ordering::SeqCst);
    let todo: Vec<(usize, usize)> = jobs.iter().zip(&have).filter(|(_, h)| !**h).map(|(j, _)| *j).collect();
    let next = AtomicUsize::new(0);
    let keys: Mutex<HashMap<String, Vec<u8>>> = Mutex::new(HashMap::new());
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let estimate = |sh: &Shared| {
        let d = done_jobs.load(Ordering::SeqCst).max(1);
        sh.total.store(sh.done.load(Ordering::SeqCst) * total_jobs as u64 / d as u64, Ordering::SeqCst);
    };
    estimate(sh);
    std::thread::scope(|s| {
        for _ in 0..opts.connections.clamp(1, 16) {
            s.spawn(|| loop {
                if sh.stop.load(Ordering::SeqCst) || failure.lock().unwrap().is_some() {
                    return;
                }
                let k = next.fetch_add(1, Ordering::SeqCst);
                let Some(&(t, i)) = todo.get(k) else { return };
                let media = &plan.tracks[t];
                let (url, range, key, seq) = if i == 0 {
                    match &media.map {
                        Some((u, r)) => (u.clone(), *r, None, 0),
                        None => {
                            let _ = std::fs::write(seg_path(work, t, 0), b"");
                            done_jobs.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                    }
                } else {
                    let sg = &media.segments[i - 1];
                    (sg.uri.clone(), sg.range, sg.key.clone(), sg.seq)
                };
                let mut tries = 0;
                let data = loop {
                    sh.receiving.fetch_add(1, Ordering::SeqCst);
                    let r = fetch_bytes(&agent, &url, req, range).and_then(|d| match &key {
                        None => Ok(d),
                        Some(k) => {
                            let kb = {
                                let cached = keys.lock().unwrap().get(&k.uri).cloned();
                                match cached {
                                    Some(b) => b,
                                    None => {
                                        let b = fetch_bytes(&agent, &k.uri, req, None)?;
                                        keys.lock().unwrap().insert(k.uri.clone(), b.clone());
                                        b
                                    }
                                }
                            };
                            let iv = k.iv.unwrap_or_else(|| {
                                let mut iv = [0u8; 16];
                                iv[8..].copy_from_slice(&seq.to_be_bytes());
                                iv
                            });
                            decrypt(&d, &kb, iv)
                        }
                    });
                    sh.receiving.fetch_sub(1, Ordering::SeqCst);
                    match r {
                        Ok(d) => break Some(d),
                        Err(e) => {
                            tries += 1;
                            debug(|| format!("segment {t}/{i}: {e} (try {tries})"));
                            if tries > opts.retries || sh.stop.load(Ordering::SeqCst) {
                                if !sh.stop.load(Ordering::SeqCst) {
                                    failure.lock().unwrap().get_or_insert(format!("segment {i}: {e}"));
                                }
                                break None;
                            }
                            std::thread::sleep(Duration::from_millis(500 * (1 << tries.min(4))));
                        }
                    }
                };
                let Some(data) = data else { return };
                let path = seg_path(work, t, i);
                let tmp = path.with_extension("part");
                if std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
                    failure.lock().unwrap().get_or_insert("writing a segment failed".into());
                    return;
                }
                sh.done.fetch_add(data.len() as u64, Ordering::SeqCst);
                done_jobs.fetch_add(1, Ordering::SeqCst);
                estimate(sh);
                crate::throttle(sh, opts.limit, data.len() as u64);
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    if sh.stop.load(Ordering::SeqCst) {
        return Ok(false);
    }
    sh.total.store(sh.done.load(Ordering::SeqCst), Ordering::SeqCst);
    // Join each track's segments, then remux them into one MP4.
    let mut inputs = Vec::new();
    for (t, m) in plan.tracks.iter().enumerate() {
        let joined = work.join(format!("track{t}.{}", if m.map.is_some() { "mp4" } else { "ts" }));
        let mut f = std::fs::File::create(&joined).map_err(|e| e.to_string())?;
        for i in 0..=m.segments.len() {
            let mut part = std::fs::File::open(seg_path(work, t, i)).map_err(|e| e.to_string())?;
            std::io::copy(&mut part, &mut f).map_err(|e| e.to_string())?;
        }
        inputs.push(joined);
    }
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into(), "-nostdin".into(), "-y".into()];
    for i in &inputs {
        args.extend(["-i".into(), i.to_string_lossy().into_owned()]);
    }
    args.extend(["-map".into(), "0:v?".into(), "-map".into(), if inputs.len() > 1 { "1:a?".into() } else { "0:a?".into() }]);
    args.extend(["-c".into(), "copy".into(), "-movflags".into(), "+faststart".into(), "-f".into(), "mp4".into(), out.to_string_lossy().into_owned()]);
    let res = std::process::Command::new("ffmpeg").args(&args).stdin(std::process::Stdio::null()).output();
    match res {
        Ok(o) if o.status.success() => Ok(true),
        Ok(o) => Err(format!("ffmpeg: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("failed"))),
        Err(_) if inputs.len() == 1 => {
            // No ffmpeg: keep the joined stream as it is.
            std::fs::rename(&inputs[0], out).map_err(|e| e.to_string())?;
            Ok(true)
        }
        Err(e) => Err(format!("ffmpeg is needed to join video and audio: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(resolve("https://h.org/a/b/master.m3u8?t=1", "v/720.m3u8"), "https://h.org/a/b/v/720.m3u8");
        assert_eq!(resolve("https://h.org/a/master.m3u8", "/x/seg.ts"), "https://h.org/x/seg.ts");
        assert_eq!(resolve("https://h.org/a/master.m3u8", "//cdn.org/s.ts"), "https://cdn.org/s.ts");
        assert_eq!(resolve("https://h.org/master.m3u8", "s1.ts"), "https://h.org/s1.ts");
        assert_eq!(resolve("https://h.org", "s1.ts"), "https://h.org/s1.ts");
        assert_eq!(resolve("https://h.org/a/m.m3u8", "https://o.org/s.ts"), "https://o.org/s.ts");
    }

    #[test]
    fn master() {
        let t = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",DEFAULT=YES,URI=\"a/en.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"aud\"\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,AUDIO=\"aud\"\nhigh.m3u8\n";
        let Playlist::Master { variants, audio } = parse(t, "https://h.org/v/master.m3u8").unwrap() else { panic!() };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].resolution, Some((1920, 1080)));
        assert_eq!(variants[0].uri, "https://h.org/v/low.m3u8");
        assert_eq!(variants[0].audio.as_deref(), Some("aud"));
        assert_eq!(audio, vec![Rendition { group: "aud".into(), uri: "https://h.org/v/a/en.m3u8".into(), default: true }]);
    }

    #[test]
    fn media() {
        let t = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\",IV=0x0000000000000000000000000000000A\n#EXTINF:4.0,\ns1.m4s\n#EXT-X-BYTERANGE:100@0\n#EXTINF:4.0,\nall.m4s\n#EXT-X-BYTERANGE:50\n#EXTINF:2.5,\nall.m4s\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(t, "https://h.org/v/p.m3u8").unwrap() else { panic!() };
        assert!(m.ended);
        assert_eq!(m.map, Some(("https://h.org/v/init.mp4".into(), None)));
        assert_eq!(m.segments.len(), 3);
        assert_eq!(m.segments[0].seq, 7);
        assert_eq!(m.segments[1].range, Some((0, 100)));
        assert_eq!(m.segments[2].range, Some((100, 50)));
        assert_eq!(m.segments[0].key.as_ref().unwrap().iv.unwrap()[15], 10);
        assert!(parse("#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"x\"\n", "https://h").is_err());
    }

    #[test]
    fn aes_round_trip() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        let (key, iv) = ([7u8; 16], [3u8; 16]);
        let enc = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(b"segment bytes");
        assert_eq!(decrypt(&enc, &key, iv).unwrap(), b"segment bytes");
    }
}
