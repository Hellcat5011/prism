#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

mod bluelight;
mod config_watch;
mod gamma;
mod idle;
mod wake;

use bluelight::BlueLight;
use prism_common::apps::{load_all_desktop_apps, search_apps, DesktopApp};
use prism_common::clipboard::ClipboardStore;
use prism_common::emoji::EmojiItem;
use prism_common::ipc::*;
use prism_common::notifications::{Notification, NotificationStore};
use prism_common::{Config, Theme};

struct DaemonState {
    config: Config,
    theme: Theme,
    bluelight: BlueLight,
    idle: Option<idle::IdleHandle>,
    apps: Vec<DesktopApp>,
    is_idle: Arc<AtomicBool>,
    clipboard: ClipboardStore,
    notifications: NotificationStore,
    emojis: Vec<EmojiItem>,
    /// The running lockscreen, so repeated lock requests never stack instances.
    lock_child: Option<Child>,
}

fn resolve_sibling_bin(bin_name: &str) -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join(bin_name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let local_bin = std::path::PathBuf::from(home).join(".local/bin").join(bin_name);
        if local_bin.exists() {
            return local_bin;
        }
    }
    std::path::PathBuf::from(bin_name)
}

fn spawn_prism_ui(args: &[&str]) -> Result<std::process::Child, String> {
    let bin_path = resolve_sibling_bin("prism-ui");
    let mut cmd = Command::new(&bin_path);
    cmd.args(args);

    // Ensure PATH has ~/.local/bin and standard bin dirs
    if let Ok(curr_path) = std::env::var("PATH") {
        if !curr_path.contains(".local/bin") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/vic".to_string());
            cmd.env("PATH", format!("{}/.local/bin:{}", home, curr_path));
        }
    }

    match cmd.spawn() {
        Ok(child) => {
            eprintln!("[prismd] Spawned '{}' with args {:?}", bin_path.display(), args);
            Ok(child)
        }
        Err(e) => {
            let err = format!("Failed to spawn '{}' with args {:?}: {}", bin_path.display(), args, e);
            eprintln!("[prismd] ERROR: {}", err);
            Err(err)
        }
    }
}

fn spawn_prism_lock(args: &[&str]) -> Result<std::process::Child, String> {
    let bin_path = resolve_sibling_bin("prism-lock");
    let mut cmd = Command::new(&bin_path);
    cmd.args(args);
    match cmd.spawn() {
        Ok(child) => {
            eprintln!("[prismd] Spawned '{}' with args {:?}", bin_path.display(), args);
            Ok(child)
        }
        Err(e) => {
            let err = format!("Failed to spawn '{}': {}", bin_path.display(), e);
            eprintln!("[prismd] ERROR: {}", err);
            Err(err)
        }
    }
}

/// Wait for a fire-and-forget child on a helper thread so it never lingers as a zombie.
/// (Replaces the old global `waitpid(-1)` reaper, which raced with popup tracking and
/// stole exit statuses from children owned by other threads.)
fn reap(child: Child) {
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
}

fn child_running(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

/// Terminate a child we own and collect its exit status.
fn terminate(mut child: Child) {
    if child_running(&mut child) {
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
    }
    let _ = child.wait();
}

/// Send a command to the resident `prism-ui daemon`, starting it if it is not up yet.
fn send_ui(cmd: &prism_common::UiCommand) -> Result<String, String> {
    let json = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    let path = prism_common::get_ui_socket_path();
    for attempt in 0..30 {
        if let Ok(mut stream) = UnixStream::connect(&path) {
            return writeln!(stream, "{}", json).map(|_| "ok".to_string()).map_err(|e| e.to_string());
        }
        if attempt == 0 {
            ensure_ui_daemon();
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("prism-ui daemon is not reachable at {}", path.display()))
}

static UI_SUPERVISOR: std::sync::Once = std::sync::Once::new();

/// Keep exactly one `prism-ui daemon` running (it hosts every overlay and the desktop widgets);
/// restart it if it exits. The daemon itself refuses to start twice (flock).
fn ensure_ui_daemon() {
    UI_SUPERVISOR.call_once(|| {
        thread::spawn(|| loop {
            let started = std::time::Instant::now();
            match spawn_prism_ui(&["daemon"]) {
                Ok(mut child) => {
                    let _ = child.wait();
                }
                Err(e) => eprintln!("[prismd] {}", e),
            }
            // Back off if it exits immediately (e.g. another daemon already holds the lock).
            let quick = started.elapsed() < Duration::from_secs(3);
            thread::sleep(Duration::from_secs(if quick { 5 } else { 1 }));
        });
    });
}

fn open_popup(_st: &mut DaemonState, mode_arg: &str) -> Result<String, String> {
    send_ui(&prism_common::UiCommand::Open(mode_arg.to_string())).map(|_| format!("{}_opened", mode_arg))
}

fn toggle_or_spawn_popup(_st: &mut DaemonState, mode_arg: &str) -> Result<String, String> {
    send_ui(&prism_common::UiCommand::Toggle(mode_arg.to_string())).map(|_| format!("{}_toggled", mode_arg))
}

fn close_active_popup(_st: &mut DaemonState) -> String {
    match send_ui(&prism_common::UiCommand::Close) {
        Ok(_) => "popup_closed".to_string(),
        Err(e) => e,
    }
}

fn show_osd(kind: &str, value: &str) {
    let value: f32 = value.parse().unwrap_or(0.0);
    let _ = send_ui(&prism_common::UiCommand::Osd { kind: kind.to_string(), value });
}

fn send_notif(msg: prism_common::NotifOverlayMessage) {
    let _ = send_ui(&prism_common::UiCommand::Notify(msg));
}

fn start_clipboard_daemon() {
    if std::env::var("WAYLAND_DISPLAY").is_err() && std::env::var("WAYLAND_SOCKET").is_err() {
        return;
    }

    // Terminate any stale watchers
    let _ = Command::new("pkill")
        .args(["-f", "wl-paste .*capture-"])
        .status();

    eprintln!("[prismd/clipboard] Spawning clipboard background watchers...");

    // 1. Text watcher thread
    thread::spawn(|| {
        loop {
            let mut cmd = Command::new("wl-paste");
            cmd.args(["-t", "text/plain", "--watch", "prism-ctl", "clipboard", "capture-text"]);
            if let Ok(mut child) = cmd.spawn() {
                let _ = child.wait();
            }
            thread::sleep(Duration::from_secs(2));
        }
    });

    // 2. Image watcher thread
    thread::spawn(|| {
        loop {
            let mut cmd = Command::new("wl-paste");
            cmd.args(["-t", "image/png", "--watch", "prism-ctl", "clipboard", "capture-image"]);
            if let Ok(mut child) = cmd.spawn() {
                let _ = child.wait();
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}

fn start_dbus_notif_daemon() {
    let script_paths = [
        std::path::PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            format!("{}/.local/share", std::env::var("HOME").unwrap_or_default())
        }))
        .join("prism/scripts/prism-dbus-notif.py"),
        std::path::PathBuf::from("./scripts/prism-dbus-notif.py"),
    ];

    let found_script = script_paths.iter().find(|p| p.exists()).cloned();
    if let Some(script) = found_script {
        eprintln!("[prismd/notif] Spawning D-Bus Notification daemon: {:?}", script);
        let _ = Command::new("pkill")
            .args(["-f", "prism-dbus-notif.py"])
            .status();

        thread::spawn(move || {
            loop {
                let mut cmd = Command::new("python3");
                cmd.arg(&script);
                if let Ok(mut child) = cmd.spawn() {
                    let _ = child.wait();
                }
                thread::sleep(Duration::from_secs(3));
            }
        });
    } else {
        eprintln!("[prismd/notif] prism-dbus-notif.py not found, skipping D-Bus server");
    }
}

/// Collect popups / lockscreen that exited on their own (Esc, click-outside, unlock) so they do
/// not linger as zombies until the next IPC request. try_wait() reaps only our own children.
fn start_popup_watcher(state: Arc<Mutex<DaemonState>>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        if let Ok(mut st) = state.lock() {
            let lock_exited = st.lock_child.as_mut().map(|c| !child_running(c)).unwrap_or(false);
            if lock_exited {
                st.lock_child = None;
            }
        }
    });
}

fn start_clipboard_pruner(state: Arc<Mutex<DaemonState>>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(900)); // 15 minutes
            if let Ok(mut st) = state.lock() {
                st.clipboard.prune_older_than(86400); // 24 hours
            }
        }
    });
}

/// Start prism-lock unless one is already running.
fn lock_screen(st: &mut DaemonState) -> Result<&'static str, String> {
    if st.lock_child.as_mut().map(child_running).unwrap_or(false) {
        return Ok("already_locked");
    }
    if let Some(mut old) = st.lock_child.take() {
        let _ = old.wait(); // already exited; collect its status
    }
    eprintln!("[prismd] Spawning prism-lock...");
    st.lock_child = Some(spawn_prism_lock(&[])?);
    Ok("lock_invoked")
}

fn idle_settings(cfg: &Config) -> idle::IdleSettings {
    idle::IdleSettings { auto_lock: cfg.manage_idle, timeout_mins: cfg.idle_timeout }
}

/// Push `st.config` to the services that follow it live.
fn apply_config(st: &DaemonState) {
    st.bluelight.update_config(&st.config);
    if let Some(idle) = &st.idle {
        idle.update(idle_settings(&st.config));
    }
}

/// Persist a config change made by prismd itself (IPC toggles) and apply it. The watcher
/// sees the write too, but finds the config unchanged by then.
fn save_and_apply_config(st: &mut DaemonState) {
    if let Err(e) = st.config.save() {
        eprintln!("[prismd] saving config failed: {}", e);
    }
    apply_config(st);
}

fn handle_client(mut stream: UnixStream, state: Arc<Mutex<DaemonState>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("Failed to clone socket"));
    let mut line = String::new();

    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            break;
        }

        let cmd: Result<IpcCommand, _> = serde_json::from_str(&line);
        let resp = match cmd {
            Ok(IpcCommand::Ping) => IpcResponse::Ok(Some("pong".to_string())),
            Ok(IpcCommand::Help) => IpcResponse::Ok(Some("Prism IPC server active".to_string())),

            Ok(IpcCommand::Lock(LockAction::Status)) => {
                let st = state.lock().unwrap();
                let idle = st.is_idle.load(Ordering::SeqCst);
                IpcResponse::Ok(Some(format!("unlocked, idle={}", idle)))
            }
            Ok(IpcCommand::Lock(LockAction::Lock)) => {
                let mut st = state.lock().unwrap();
                match lock_screen(&mut st) {
                    Ok(msg) => IpcResponse::Ok(Some(msg.to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Launcher(LauncherAction::Query(query))) => {
                let st = state.lock().unwrap();
                let matches = search_apps(&st.apps, &query);
                let count = matches.len();
                let summary: Vec<String> = matches
                    .into_iter()
                    .take(10)
                    .map(|a| format!("{} ({})", a.name, a.id))
                    .collect();
                IpcResponse::Ok(Some(format!("Matches ({} total):\n{}", count, summary.join("\n"))))
            }

            Ok(IpcCommand::Launcher(LauncherAction::List)) => {
                let st = state.lock().unwrap();
                let count = st.apps.len();
                let summary: Vec<String> = st.apps
                    .iter()
                    .take(15)
                    .map(|a| format!("{} [{}]", a.name, a.id))
                    .collect();
                IpcResponse::Ok(Some(format!("Installed apps ({} total):\n{}", count, summary.join("\n"))))
            }

            Ok(IpcCommand::Launcher(LauncherAction::Run(id))) => {
                let st = state.lock().unwrap();
                if let Some(app) = st.apps.iter().find(|a| a.id.eq_ignore_ascii_case(&id)) {
                    match app.launch() {
                        Ok(_) => IpcResponse::Ok(Some(format!("Launched {}", app.name))),
                        Err(e) => IpcResponse::Error(format!("Failed to launch {}: {}", app.name, e)),
                    }
                } else {
                    IpcResponse::Error(format!("No desktop application found with id '{}'", id))
                }
            }

            Ok(IpcCommand::Clipboard(ClipboardAction::List)) => {
                let st = state.lock().unwrap();
                let count = st.clipboard.items.len();
                let summary: Vec<String> = st.clipboard
                    .items
                    .iter()
                    .take(10)
                    .enumerate()
                    .map(|(i, item)| {
                        let preview = if item.content.len() > 50 {
                            format!("{}...", &item.content[0..47])
                        } else {
                            item.content.clone()
                        };
                        format!("[{}] {}", i, preview.replace('\n', " "))
                    })
                    .collect();
                IpcResponse::Ok(Some(format!("Clipboard history ({} items):\n{}", count, summary.join("\n"))))
            }

            Ok(IpcCommand::Clipboard(ClipboardAction::Get(idx))) => {
                let st = state.lock().unwrap();
                if let Some(item) = st.clipboard.items.get(idx) {
                    IpcResponse::Ok(Some(item.content.clone()))
                } else {
                    IpcResponse::Error(format!("No clipboard entry at index {}", idx))
                }
            }

            Ok(IpcCommand::Clipboard(ClipboardAction::Add(text))) => {
                let mut st = state.lock().unwrap();
                if st.clipboard.add_text(text) {
                    IpcResponse::Ok(Some("added".to_string()))
                } else {
                    IpcResponse::Ok(Some("filtered_or_duplicate".to_string()))
                }
            }
            Ok(IpcCommand::Clipboard(ClipboardAction::AddImage(bytes))) => {
                let mut st = state.lock().unwrap();
                if st.clipboard.add_image(&bytes) {
                    IpcResponse::Ok(Some("image_added".to_string()))
                } else {
                    IpcResponse::Ok(Some("image_failed".to_string()))
                }
            }

            Ok(IpcCommand::Clipboard(ClipboardAction::Clear)) => {
                let mut st = state.lock().unwrap();
                st.clipboard.clear();
                IpcResponse::Ok(Some("cleared".to_string()))
            }
            Ok(IpcCommand::Clipboard(ClipboardAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "clipboard") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Clipboard(ClipboardAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "clipboard") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Clipboard(ClipboardAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }

            Ok(IpcCommand::Wallpaper(WallpaperAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "wallpaper") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Wallpaper(WallpaperAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "wallpaper") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Wallpaper(WallpaperAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }
            Ok(IpcCommand::Wallpaper(WallpaperAction::Set { path, scheme, mode })) => {
                match prism_common::wallpaper::set_wallpaper(&path, None, scheme.as_deref(), mode.as_deref()) {
                    Ok(_) => {
                        let mut st = state.lock().unwrap();
                        st.theme = Theme::load();
                        IpcResponse::Ok(Some(format!("Wallpaper set: {}", path.display())))
                    }
                    Err(e) => IpcResponse::Error(format!("Failed to set wallpaper: {}", e)),
                }
            }
            Ok(IpcCommand::Wallpaper(WallpaperAction::Get)) => {
                if let Some(wp) = prism_common::wallpaper::get_current_wallpaper() {
                    IpcResponse::Ok(Some(wp.display().to_string()))
                } else {
                    IpcResponse::Ok(Some("No wallpaper recorded".to_string()))
                }
            }
            Ok(IpcCommand::Theme(ThemeAction::Reload)) => {
                let mut st = state.lock().unwrap();
                st.theme = Theme::load();
                IpcResponse::Ok(Some("Theme palette reloaded".to_string()))
            }
            Ok(IpcCommand::Theme(ThemeAction::GetPalette)) => {
                let st = state.lock().unwrap();
                let json = serde_json::to_string_pretty(&st.theme).unwrap_or_default();
                IpcResponse::Ok(Some(json))
            }

            Ok(IpcCommand::Media(MediaAction::Status)) => {
                if let Some(track) = prism_common::media::query_current_media() {
                    IpcResponse::Ok(Some(format!(
                        "[{}] {} - {} ({})",
                        track.status, track.artist, track.title, track.player
                    )))
                } else {
                    IpcResponse::Ok(Some("No active media playback".to_string()))
                }
            }
            Ok(IpcCommand::Media(MediaAction::PlayPause)) => {
                match prism_common::media::play_pause() {
                    Ok(_) => IpcResponse::Ok(Some("Toggled playback".to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Media(MediaAction::Next)) => {
                match prism_common::media::next() {
                    Ok(_) => IpcResponse::Ok(Some("Skipped to next track".to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Media(MediaAction::Previous)) => {
                match prism_common::media::previous() {
                    Ok(_) => IpcResponse::Ok(Some("Skipped to previous track".to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Media(MediaAction::Stop)) => {
                match prism_common::media::stop() {
                    Ok(_) => IpcResponse::Ok(Some("Stopped playback".to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Launcher(LauncherAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "launcher") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Launcher(LauncherAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "launcher") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Launcher(LauncherAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }

            Ok(IpcCommand::Search(SearchAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "search") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Search(SearchAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "search") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Search(SearchAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }

            Ok(IpcCommand::Search(SearchAction::SetQuery(query))) => {
                let st = state.lock().unwrap();
                let app_matches = search_apps(&st.apps, &query);
                let file_matches = prism_common::search_files(&query, 10);

                let mut sections = Vec::new();
                if !app_matches.is_empty() {
                    sections.push("== Applications ==".to_string());
                    for a in app_matches.iter().take(5) {
                        sections.push(format!("  • {} ({})", a.name, a.id));
                    }
                }
                if !file_matches.is_empty() {
                    sections.push("== Files & Folders ==".to_string());
                    for f in file_matches.iter().take(8) {
                        let kind = if f.is_dir { "[DIR]" } else { "[FILE]" };
                        sections.push(format!("  • {} {} ({})", kind, f.name, f.path));
                    }
                }

                if sections.is_empty() {
                    IpcResponse::Ok(Some(format!("No results for '{}'", query)))
                } else {
                    IpcResponse::Ok(Some(sections.join("\n")))
                }
            }

            Ok(IpcCommand::Notif(NotifAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "notif-center") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Notif(NotifAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "notif-center") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Notif(NotifAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }
            Ok(IpcCommand::Notif(NotifAction::Send { app_name, summary, body })) => {
                let notif = Notification::new(0, app_name.clone(), summary.clone(), body.clone());
                let id = {
                    let mut st = state.lock().unwrap();
                    st.notifications.add(notif)
                };
                send_notif(prism_common::NotifOverlayMessage::Add { id, app_name, summary, body, timeout_ms: 5000 });
                IpcResponse::Ok(Some(format!("notification_id:{}", id)))
            }
            Ok(IpcCommand::Notif(NotifAction::List)) => {
                let st = state.lock().unwrap();
                if st.notifications.notifications.is_empty() {
                    IpcResponse::Ok(Some("No notifications in history".to_string()))
                } else {
                    let mut lines = Vec::new();
                    for n in st.notifications.notifications.iter().take(20) {
                        lines.push(format!("[#{}] [{}] {} - {}", n.id, n.app_name, n.summary, n.body));
                    }
                    IpcResponse::Ok(Some(lines.join("\n")))
                }
            }
            Ok(IpcCommand::Notif(NotifAction::Clear)) => {
                send_notif(prism_common::NotifOverlayMessage::Dismiss { id: 0 });
                let mut st = state.lock().unwrap();
                st.notifications.clear();
                IpcResponse::Ok(Some("Notifications cleared".to_string()))
            }
            Ok(IpcCommand::Notif(NotifAction::Dismiss(id))) => {
                send_notif(prism_common::NotifOverlayMessage::Dismiss { id });
                let mut st = state.lock().unwrap();
                if st.notifications.remove(id) {
                    IpcResponse::Ok(Some(format!("Dismissed notification #{}", id)))
                } else {
                    IpcResponse::Error(format!("Notification #{} not found", id))
                }
            }

            Ok(IpcCommand::Osd(OsdAction::VolumeDelta(delta))) => {
                match prism_common::hardware::change_volume(delta) {
                    Ok(new_vol) => {
                        show_osd("volume", &new_vol.to_string());
                        IpcResponse::Ok(Some(format!("volume:{}%", new_vol)))
                    }
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Osd(OsdAction::VolumeMute)) => {
                match prism_common::hardware::toggle_mute() {
                    Ok(muted) => {
                        let (vol, _) = prism_common::hardware::get_volume().unwrap_or((0, false));
                        let display_vol = if muted { 0 } else { vol };
                        show_osd("volume", &display_vol.to_string());
                        IpcResponse::Ok(Some(format!("muted:{}", muted)))
                    }
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Osd(OsdAction::Volume(val))) => {
                if let Ok(pct) = val.parse::<u32>() {
                    let _ = prism_common::hardware::set_volume(pct);
                }
                eprintln!("[prismd] Showing Volume OSD: {}", val);
                show_osd("volume", &val);
                IpcResponse::Ok(Some(format!("osd_volume:{}", val)))
            }
            Ok(IpcCommand::Osd(OsdAction::BrightnessDelta(delta))) => {
                match prism_common::hardware::change_brightness(delta) {
                    Ok(new_bri) => {
                        show_osd("brightness", &new_bri.to_string());
                        IpcResponse::Ok(Some(format!("brightness:{}%", new_bri)))
                    }
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Osd(OsdAction::Brightness(val))) => {
                if let Ok(pct) = val.parse::<u32>() {
                    let _ = prism_common::hardware::set_brightness(pct);
                }
                eprintln!("[prismd] Showing Brightness OSD: {}", val);
                show_osd("brightness", &val);
                IpcResponse::Ok(Some(format!("osd_brightness:{}", val)))
            }

            Ok(IpcCommand::BlueLight(BlueLightAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                st.config.blue_light_enabled = !st.config.blue_light_enabled;
                let enabled = st.config.blue_light_enabled;
                save_and_apply_config(&mut st);
                IpcResponse::Ok(Some(format!("enabled:{}", enabled)))
            }
            Ok(IpcCommand::BlueLight(BlueLightAction::ManualToggle)) => {
                let mut st = state.lock().unwrap();
                st.config.blue_light_manual_on = !st.config.blue_light_manual_on;
                let manual_on = st.config.blue_light_manual_on;
                save_and_apply_config(&mut st);
                IpcResponse::Ok(Some(format!("manual_on:{}", manual_on)))
            }
            Ok(IpcCommand::BlueLight(BlueLightAction::Dump)) => {
                let st = state.lock().unwrap();
                let data = serde_json::to_string(&st.bluelight.status()).unwrap_or_default();
                IpcResponse::Ok(Some(data))
            }

            Ok(IpcCommand::ColorPicker(ColorPickerAction::Pick)) => {
                match prism_common::colorpicker::pick_color() {
                    Ok(sample) => {
                        let mut st = state.lock().unwrap();
                        st.clipboard.add_text(sample.hex.clone());
                        IpcResponse::Ok(Some(format!(
                            "{} (RGB: {},{},{} | HSL: {}°,{}%,{}%) [copied to clipboard]",
                            sample.hex, sample.rgb.0, sample.rgb.1, sample.rgb.2,
                            sample.hsl.0, sample.hsl.1, sample.hsl.2
                        )))
                    }
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Emoji(EmojiAction::Search(q))) => {
                let st = state.lock().unwrap();
                let matches = prism_common::search_emojis(&st.emojis, &q, None, 10);
                if matches.is_empty() {
                    IpcResponse::Ok(Some(format!("No emojis found for '{}'", q)))
                } else {
                    let mut lines = Vec::new();
                    for m in matches {
                        lines.push(format!("{}  {} ({})", m.emoji, m.name, m.category));
                    }
                    IpcResponse::Ok(Some(lines.join("\n")))
                }
            }
            Ok(IpcCommand::Emoji(EmojiAction::Copy(glyph))) => {
                let mut st = state.lock().unwrap();
                st.clipboard.add_text(glyph.clone());
                IpcResponse::Ok(Some(format!("Copied '{}' to clipboard", glyph)))
            }
            Ok(IpcCommand::Emoji(EmojiAction::List)) => {
                let st = state.lock().unwrap();
                let mut lines = Vec::new();
                for m in st.emojis.iter().take(20) {
                    lines.push(format!("{}  {}", m.emoji, m.name));
                }
                IpcResponse::Ok(Some(lines.join("\n")))
            }
            Ok(IpcCommand::Emoji(EmojiAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "emoji") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Power(PowerAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "power") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Power(PowerAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "power") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Power(PowerAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }
            Ok(IpcCommand::Power(act)) => {
                match prism_common::power::execute_power(act) {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Settings(SettingsAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "settings") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Settings(SettingsAction::Open) | IpcCommand::Settings(SettingsAction::OpenTab(_))) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "settings") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Settings(SettingsAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }

            Ok(IpcCommand::DesktopEdit(DesktopEditAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                let _ = &mut st;
                match send_ui(&prism_common::UiCommand::EditMode("toggle".into())) {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::DesktopEdit(DesktopEditAction::Open)) => {
                let mut st = state.lock().unwrap();
                let _ = &mut st;
                match send_ui(&prism_common::UiCommand::EditMode("open".into())) {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::DesktopEdit(DesktopEditAction::Close) | IpcCommand::DesktopEdit(DesktopEditAction::Discard)) => {
                match send_ui(&prism_common::UiCommand::EditMode("close".into())) {
                    Ok(_) => IpcResponse::Ok(Some("edit_mode_closed".to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::DesktopEdit(DesktopEditAction::Status)) => {
                IpcResponse::Ok(Some("edit mode state is owned by prism-ui".to_string()))
            }
            Ok(IpcCommand::DesktopEdit(DesktopEditAction::Reset)) => {
                let mut cfg = prism_common::Config::load();
                cfg.desktop_layout = prism_common::config::DesktopLayoutConfig::default();
                let _ = cfg.save();
                IpcResponse::Ok(Some("desktop_layout_reset".to_string()))
            }
            Ok(IpcCommand::DesktopEdit(DesktopEditAction::Save)) => {
                IpcResponse::Ok(Some("saved".to_string()))
            }

            Ok(IpcCommand::Network(NetworkAction::Status)) => {
                let st = prism_common::network::get_network_status();
                if st.connected {
                    IpcResponse::Ok(Some(format!(
                        "Connected via {} (SSID: {}, Signal: {}%, IP: {})",
                        st.kind,
                        st.ssid.unwrap_or_else(|| "N/A".to_string()),
                        st.signal,
                        st.ip.unwrap_or_else(|| "N/A".to_string())
                    )))
                } else {
                    IpcResponse::Ok(Some("Network disconnected".to_string()))
                }
            }
            Ok(IpcCommand::Network(NetworkAction::ToggleWifi)) => {
                match prism_common::network::toggle_wifi() {
                    Ok(enabled) => IpcResponse::Ok(Some(format!("wifi_enabled:{}", enabled))),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Bluetooth(BluetoothAction::Status)) => {
                let st = prism_common::network::get_bluetooth_status();
                if st.connected {
                    IpcResponse::Ok(Some(format!(
                        "Connected: {} (Battery: {:?}%)",
                        st.name.unwrap_or_else(|| "Device".to_string()),
                        st.battery.unwrap_or(0)
                    )))
                } else {
                    IpcResponse::Ok(Some("Bluetooth disconnected".to_string()))
                }
            }
            Ok(IpcCommand::Bluetooth(BluetoothAction::Toggle)) => {
                match prism_common::network::toggle_bluetooth() {
                    Ok(powered) => IpcResponse::Ok(Some(format!("bluetooth_powered:{}", powered))),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(IpcCommand::Keybinds(KeybindsAction::List)) => {
                let binds = prism_common::keybinds::load_keybinds();
                let mut lines = Vec::new();
                for b in binds {
                    lines.push(format!("[{}] {} + {} -> {}", b.category, b.mods, b.key, b.action));
                }
                IpcResponse::Ok(Some(lines.join("\n")))
            }
            Ok(IpcCommand::Keybinds(KeybindsAction::Toggle)) => {
                let mut st = state.lock().unwrap();
                match toggle_or_spawn_popup(&mut st, "keybinds") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Keybinds(KeybindsAction::Open)) => {
                let mut st = state.lock().unwrap();
                match open_popup(&mut st, "keybinds") {
                    Ok(msg) => IpcResponse::Ok(Some(msg)),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Keybinds(KeybindsAction::Close)) => {
                let mut st = state.lock().unwrap();
                let msg = close_active_popup(&mut st);
                IpcResponse::Ok(Some(msg))
            }

            Ok(IpcCommand::Depth(DepthAction::Status)) => {
                let st = prism_common::depth::get_depth_status();
                IpcResponse::Ok(Some(format!(
                    "model_available={}, mask_available={}, mask_path={:?}",
                    st.has_model, st.has_mask, st.mask_path
                )))
            }
            Ok(IpcCommand::Depth(DepthAction::Generate)) => {
                if let Some(wp) = prism_common::wallpaper::get_current_wallpaper() {
                    match prism_common::depth::generate_depth(&wp) {
                        Ok(msg) => IpcResponse::Ok(Some(msg)),
                        Err(e) => IpcResponse::Error(e),
                    }
                } else {
                    IpcResponse::Error("No wallpaper recorded to generate depth for".to_string())
                }
            }
            Ok(IpcCommand::Depth(DepthAction::Clear)) => {
                match prism_common::depth::clear_depth() {
                    Ok(_) => IpcResponse::Ok(Some("Depth masks cleared".to_string())),
                    Err(e) => IpcResponse::Error(e),
                }
            }

            Ok(cmd) => {
                eprintln!("[prismd] Received command: {:?}", cmd);
                IpcResponse::Ok(Some("dispatched".to_string()))
            }
            Err(e) => IpcResponse::Error(format!("Invalid command: {}", e)),
        };

        if let Ok(mut payload) = serde_json::to_string(&resp) {
            payload.push('\n');
            let _ = stream.write_all(payload.as_bytes());
        }
        line.clear();
    }
}

fn main() {
    println!("Starting Prism Daemon (prismd)...");

    // Claim the IPC socket before starting anything else. A second prismd (e.g. started by
    // both systemd and the compositor) must exit here without spawning desktop widgets,
    // clipboard watchers or the D-Bus bridge, otherwise those children are orphaned.
    let socket_path = get_socket_path();
    if socket_path.exists() {
        // Probe if another instance is actively responding
        if UnixStream::connect(&socket_path).is_ok() {
            eprintln!("[prismd] Another active prismd instance is already running on '{}'. Exiting.", socket_path.display());
            std::process::exit(0);
        }
        // Stale socket from crash/reboot: safely remove
        let _ = fs::remove_file(&socket_path);
    }

    if let Some(parent) = socket_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind IPC socket at '{}': {}", socket_path.display(), e);
            std::process::exit(1);
        }
    };

    let cfg = Config::load();
    let theme = Theme::load();
    println!("Loaded config (idle timeout: {}m, blur: {})", cfg.idle_timeout, cfg.blur_enabled);
    println!("Loaded theme background: {}", theme.background);

    let bluelight = BlueLight::start(&cfg);
    let apps = load_all_desktop_apps();
    println!("Loaded {} desktop application entries.", apps.len());

    let clipboard = ClipboardStore::load();
    println!("Loaded {} clipboard history entries.", clipboard.items.len());

    let notifications = NotificationStore::load();
    println!("Loaded {} notification entries.", notifications.notifications.len());

    let emojis = prism_common::load_emojis();
    println!("Loaded {} emoji entries.", emojis.len());

    let is_idle = Arc::new(AtomicBool::new(false));
    let idle_start = idle_settings(&cfg);

    let state = Arc::new(Mutex::new(DaemonState {
        config: cfg,
        theme,
        bluelight,
        idle: None,
        apps,
        is_idle: Arc::clone(&is_idle),
        clipboard,
        notifications,
        emojis,
        lock_child: None,
    }));

    let idle_state = Arc::downgrade(&state);
    let idle = idle::start_idle_monitor(
        idle_start,
        is_idle,
        Box::new(move || {
            if let Some(state) = idle_state.upgrade() {
                if let Err(e) = lock_screen(&mut state.lock().unwrap()) {
                    eprintln!("[prismd/idle] lock failed: {}", e);
                }
            }
        }),
    );
    {
        let mut st = state.lock().unwrap();
        st.idle = idle;
        let watch_state = Arc::clone(&state);
        config_watch::start(&st.config, move |cfg| {
            let mut st = watch_state.lock().unwrap();
            st.config = cfg;
            apply_config(&st);
        });
    }

    // Autostart persistent desktop widgets layer surface if Wayland environment is active
    if std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("WAYLAND_SOCKET").is_ok() {
        // One resident prism-ui hosts the desktop widgets and every overlay.
        eprintln!("[prismd] Starting resident prism-ui daemon...");
        ensure_ui_daemon();
        start_clipboard_daemon();
        start_clipboard_pruner(Arc::clone(&state));
        start_popup_watcher(Arc::clone(&state));
        start_dbus_notif_daemon();
    }

    println!("Prism IPC listening on '{}'", socket_path.display());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let st = Arc::clone(&state);
                std::thread::spawn(move || {
                    handle_client(s, st);
                });
            }
            Err(e) => eprintln!("Error accepting IPC connection: {}", e),
        }
    }
}
