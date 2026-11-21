//! Also the replay buffer: while `replay_enabled` is on, a prism-rec keeps the last seconds
//! of the screen in memory; `save_replay` (SIGUSR1) makes it write them, and its stdout line
//! becomes the same notification as a recording.
//!
//! Runs prism-rec for a recording job (SCREEN_CAPTURE_PLAN.md): picks the file name, starts the
//! recorder, tells prism-ui to show the indicator, stops it with SIGINT, and posts the result
//! as a notification (Open, Trim…, GIF, Delete). Trim opens prism-ui's Trim card; GIF converts
//! the recording with ffmpeg (two-pass palette, 15 fps, at most 960 px wide) next to it.

use prism_common::capture::{timestamp, RecordJob, RecordStatus};
use prism_common::Config;
use std::process::{Child, Command};
use std::sync::Mutex;

struct Running {
    child: Child,
    status: RecordStatus,
}

static CURRENT: Mutex<Option<Running>> = Mutex::new(None);

pub fn status() -> RecordStatus {
    CURRENT.lock().unwrap().as_ref().map(|r| r.status.clone()).unwrap_or_default()
}

/// Start recording; returns the file path.
pub fn start(mut job: RecordJob) -> Result<String, String> {
    let mut cur = CURRENT.lock().unwrap();
    if cur.is_some() {
        return Err("already recording".into());
    }
    let cfg = Config::load();
    if job.path.is_empty() {
        let dir = prism_common::config::expand_home(&cfg.capture_recording_dir);
        let ext = if cfg.capture_container == "mp4" { "mp4" } else { "mkv" };
        job.path = dir.join(format!("Recording_{}.{ext}", timestamp())).to_string_lossy().into_owned();
    }
    let json = serde_json::to_string(&job).map_err(|e| e.to_string())?;
    let child = Command::new(crate::resolve_sibling_bin("prism-rec")).arg(json).spawn().map_err(|e| format!("cannot start prism-rec: {e}"))?;
    let started_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let status = RecordStatus { recording: true, path: job.path.clone(), started_ms, audio: job.audio_desktop || job.audio_mic, target: Some(job.target.clone()) };
    eprintln!("[prismd] recording to {}", job.path);
    let _ = crate::send_ui(&prism_common::UiCommand::Recording(Some(status.clone())));
    *cur = Some(Running { child, status });
    drop(cur);
    std::thread::spawn(watch);
    Ok(job.path)
}

/// Ask the recorder to finish; false when nothing is recording.
pub fn stop() -> bool {
    match CURRENT.lock().unwrap().as_ref() {
        Some(r) => {
            unsafe { libc::kill(r.child.id() as i32, libc::SIGINT) };
            true
        }
        None => false,
    }
}

/// Wait for prism-rec to exit, then report.
fn watch() {
    let (ok, path) = loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut cur = CURRENT.lock().unwrap();
        let Some(r) = cur.as_mut() else { return };
        if let Ok(Some(st)) = r.child.try_wait() {
            let path = r.status.path.clone();
            *cur = None;
            break (st.success(), path);
        }
    };
    let _ = crate::send_ui(&prism_common::UiCommand::Recording(None));
    let exists = std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false);
    if ok && exists {
        eprintln!("[prismd] recording saved: {path}");
        notify("Recording saved", &path, true);
    } else {
        eprintln!("[prismd] recording failed ({path})");
        notify("Recording failed", "The recorder stopped with an error; see journalctl --user -u prismd.", false);
    }
}

fn notify(summary: &str, body: &str, file: bool) {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    const D: &str = "org.freedesktop.Notifications";
    let Ok(conn) = zbus::blocking::Connection::session() else { return };
    let Ok(proxy) = zbus::blocking::Proxy::new(&conn, D, "/org/freedesktop/Notifications", D) else { return };
    let Ok(signals) = proxy.receive_all_signals() else { return };
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from("prism-capture"));
    let gif = body.ends_with(".gif");
    let actions: Vec<&str> = match (file, gif) {
        (false, _) => Vec::new(),
        (true, true) => vec!["default", "Open", "folder", "Show in folder", "delete", "Delete"],
        (true, false) => vec!["default", "Open", "trim", "Trim\u{2026}", "gif", "GIF", "delete", "Delete"],
    };
    let icon = if file { "video-x-generic" } else { "dialog-error" };
    let Ok(reply) = conn.call_method(Some(D), "/org/freedesktop/Notifications", Some(D), "Notify", &("Screen Recording", 0u32, icon, summary, body, actions, hints, -1i32)) else { return };
    let Ok(id) = reply.body().deserialize::<u32>() else { return };
    if !file {
        return;
    }
    let path = std::path::PathBuf::from(body);
    for m in signals {
        let member = m.header().member().map(|s| s.to_string()).unwrap_or_default();
        if member == "NotificationClosed" && m.body().deserialize::<(u32, u32)>().is_ok_and(|(n, _)| n == id) {
            return;
        }
        if member != "ActionInvoked" {
            continue;
        }
        let Ok((n, action)) = m.body().deserialize::<(u32, String)>() else { continue };
        if n != id {
            continue;
        }
        let dir = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        match action.as_str() {
            "open" | "default" => {
                let _ = prism_common::apps::app_command("xdg-open", &[path.to_string_lossy().into_owned()]).spawn();
            }
            "folder" => {
                let _ = prism_common::apps::app_command("xdg-open", &[dir]).spawn();
            }
            "delete" => {
                let _ = std::fs::remove_file(&path);
            }
            "trim" => {
                let _ = crate::send_ui(&prism_common::UiCommand::Trim(path.to_string_lossy().into_owned()));
            }
            "gif" => {
                let src = path.clone();
                std::thread::spawn(move || export_gif(&src));
            }
            _ => {}
        }
        return;
    }
}

/// Convert a recording to an animated GIF beside it and say how it went.
fn export_gif(src: &std::path::Path) {
    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Recording".into());
    let mut out = src.with_file_name(format!("{stem}.gif"));
    let mut n = 2;
    while out.exists() {
        out = src.with_file_name(format!("{stem}_{n}.gif"));
        n += 1;
    }
    let filter = "fps=15,scale='min(960,iw)':-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle";
    let result = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(src)
        .args(["-vf", filter, "-loop", "0"])
        .arg(&out)
        .stdin(std::process::Stdio::null())
        .output();
    match result {
        Ok(o) if o.status.success() => {
            eprintln!("[prismd] GIF saved: {}", out.display());
            notify("GIF saved", &out.to_string_lossy(), true);
        }
        Ok(o) => {
            let _ = std::fs::remove_file(&out);
            let msg = String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("ffmpeg failed").to_string();
            notify("GIF export failed", &msg, false);
        }
        Err(e) => notify("GIF export failed", &format!("ffmpeg: {e}"), false),
    }
}

// ── Replay buffer ───────────────────────────────────────────────────────

struct Replay {
    child: Child,
    /// The job it runs (a changed config restarts it).
    job: String,
}

static REPLAY: Mutex<Option<Replay>> = Mutex::new(None);
/// The replay job wanted now (None: off), for the supervisor.
static REPLAY_WANT: Mutex<Option<String>> = Mutex::new(None);
static REPLAY_SUPERVISOR: std::sync::Once = std::sync::Once::new();

fn replay_job(cfg: &Config) -> String {
    let dir = prism_common::config::expand_home(&cfg.capture_recording_dir);
    let ext = if cfg.capture_container == "mp4" { "mp4" } else { "mkv" };
    let job = RecordJob {
        target: prism_common::capture::RecordTarget::Output { name: String::new() },
        fps: if cfg.capture_fps >= 45 { 60 } else { 30 },
        cursor: cfg.capture_cursor,
        audio_desktop: cfg.capture_audio_desktop,
        audio_mic: false,
        codec: cfg.capture_codec.clone(),
        quality: cfg.capture_quality.clone(),
        path: dir.join(format!("Replay.{ext}")).to_string_lossy().into_owned(),
        replay_secs: cfg.replay_seconds.clamp(5, 600),
    };
    serde_json::to_string(&job).unwrap_or_default()
}

/// Start, restart or stop the replay buffer to match the config.
pub fn apply_replay(cfg: &Config) {
    *REPLAY_WANT.lock().unwrap() = cfg.replay_enabled.then(|| replay_job(cfg));
    REPLAY_SUPERVISOR.call_once(|| {
        std::thread::Builder::new().name("prism-replay".into()).spawn(supervise_replay).ok();
    });
}

/// Save the replay buffer; false when it is off.
pub fn save_replay() -> bool {
    match REPLAY.lock().unwrap().as_ref() {
        Some(r) => {
            unsafe { libc::kill(r.child.id() as i32, libc::SIGUSR1) };
            true
        }
        None => false,
    }
}

fn supervise_replay() {
    let mut failures = 0u32;
    loop {
        let want = REPLAY_WANT.lock().unwrap().clone();
        let mut cur = REPLAY.lock().unwrap();
        // Exited by itself?
        if let Some(r) = cur.as_mut() {
            if let Ok(Some(st)) = r.child.try_wait() {
                eprintln!("[prismd] replay buffer stopped ({st})");
                *cur = None;
                failures += 1;
            }
        }
        let running = cur.as_ref().map(|r| r.job.clone());
        if running.is_some() && running != want {
            if let Some(mut r) = cur.take() {
                unsafe { libc::kill(r.child.id() as i32, libc::SIGINT) };
                let _ = r.child.wait();
                eprintln!("[prismd] replay buffer off");
            }
        }
        if cur.is_none() && failures < 5 {
            if let Some(job) = want {
                match Command::new(crate::resolve_sibling_bin("prism-rec")).arg(&job).stdout(std::process::Stdio::piped()).spawn() {
                    Ok(mut child) => {
                        eprintln!("[prismd] replay buffer on");
                        if let Some(out) = child.stdout.take() {
                            std::thread::spawn(move || {
                                use std::io::BufRead;
                                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                                    if let Some(rest) = line.strip_prefix("SAVED ") {
                                        let path = rest.rsplit_once(' ').map(|(p, _)| p).unwrap_or(rest).to_string();
                                        std::thread::spawn(move || notify("Replay saved", &path, true));
                                    } else if let Some(why) = line.strip_prefix("FAILED ") {
                                        let why = why.to_string();
                                        std::thread::spawn(move || notify("Replay not saved", &why, false));
                                    }
                                }
                            });
                        }
                        *cur = Some(Replay { child, job });
                    }
                    Err(e) => {
                        eprintln!("[prismd] replay buffer: cannot start prism-rec: {e}");
                        failures += 1;
                    }
                }
            }
        }
        if cur.is_none() && REPLAY_WANT.lock().unwrap().is_none() {
            failures = 0;
        }
        drop(cur);
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
