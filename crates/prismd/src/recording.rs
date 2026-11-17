//! Runs prism-rec for a recording job (SCREEN_CAPTURE_PLAN.md): picks the file name, starts the
//! recorder, tells prism-ui to show the indicator, stops it with SIGINT, and posts the result
//! as a notification (Open, Show in folder, Delete).

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
    let actions: Vec<&str> = if file { vec!["open", "Open", "folder", "Show in folder", "delete", "Delete"] } else { Vec::new() };
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
            _ => {}
        }
        return;
    }
}
