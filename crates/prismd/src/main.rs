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
mod depth;
mod gamma;
mod idle;
mod notifd;
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
    depth: depth::Depth,
    idle: Option<idle::IdleHandle>,
    apps: Vec<DesktopApp>,
    is_idle: Arc<AtomicBool>,
    clipboard: ClipboardStore,
    notifications: NotificationStore,
    emojis: Vec<EmojiItem>,
    /// Silent mode: notifications are stored but only critical ones pop up.
    silent: bool,
}

/// Caffeinate (idle inhibitor held by prism-ui). Global so the prism-ui supervisor can
/// re-apply it when prism-ui restarts.
static CAFFEINATE: AtomicBool = AtomicBool::new(false);

fn set_caffeinate(on: bool) -> Result<(), String> {
    CAFFEINATE.store(on, Ordering::SeqCst);
    send_ui(&prism_common::UiCommand::Caffeinate(on)).map(|_| ())
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
                    if CAFFEINATE.load(Ordering::SeqCst) {
                        // Re-apply once the new prism-ui has bound its socket.
                        thread::spawn(|| {
                            for _ in 0..20 {
                                thread::sleep(Duration::from_millis(250));
                                if !CAFFEINATE.load(Ordering::SeqCst) || set_caffeinate(true).is_ok() {
                                    break;
                                }
                            }
                        });
                    }
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

/// Receives notifications from the notification server (notifd.rs).
struct NotifHandler {
    state: std::sync::Weak<Mutex<DaemonState>>,
}

/// Popup message for a stored notification.
fn popup_for(n: &Notification) -> prism_common::NotifOverlayMessage {
    prism_common::NotifOverlayMessage::Add {
        id: n.id,
        app_name: n.app_name.clone(),
        summary: n.summary.clone(),
        body: n.body.clone(),
        timeout_ms: n.timeout_ms as u64,
        icon: n.icon.clone(),
        image: n.image.clone(),
        urgency: n.urgency,
        actions: n.actions.clone(),
        desktop_entry: n.desktop_entry.clone(),
    }
}

impl notifd::Handler for NotifHandler {
    fn notify(&self, i: notifd::Incoming) -> u32 {
        let Some(state) = self.state.upgrade() else { return 0 };
        let mut n = Notification::new(0, i.app_name, i.summary, i.body);
        n.icon = i.icon;
        n.urgency = i.urgency;
        n.timeout_ms = i.timeout_ms;
        n.actions = i.actions;
        n.desktop_entry = i.desktop_entry;
        n.image = i.image;
        let (popup, silent) = {
            let mut st = state.lock().unwrap();
            if i.transient {
                n.id = if i.replaces_id != 0 { i.replaces_id } else { st.notifications.reserve_id() };
            } else {
                n.id = st.notifications.upsert(i.replaces_id, n.clone());
            }
            (popup_for(&n), st.silent)
        };
        // Silent mode keeps critical notifications (QML silentAllowsCritical).
        if !silent || n.urgency >= 2 {
            send_notif(popup);
        }
        n.id
    }

    fn close(&self, id: u32) {
        let Some(state) = self.state.upgrade() else { return };
        send_notif(prism_common::NotifOverlayMessage::Dismiss { id });
        state.lock().unwrap().notifications.remove(id);
        notifd::emit_closed(id, notifd::CLOSED_BY_CALL);
    }
}

/// Stop the old Python notification bridge (replaced by notifd.rs) if it is still running.
/// Matches only `python* .../prism-dbus-notif.py` processes of this user (a `pkill -f` pattern
/// would also hit shells or editors that merely mention the file name).
fn stop_python_bridge() {
    let uid = unsafe { libc::getuid() };
    let Ok(dir) = std::fs::read_dir("/proc") else { return };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let owner = std::fs::metadata(e.path()).map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or(u32::MAX);
        if owner != uid {
            continue;
        }
        let Ok(cmd) = std::fs::read(e.path().join("cmdline")) else { continue };
        let args: Vec<&[u8]> = cmd.split(|b| *b == 0).filter(|a| !a.is_empty()).collect();
        let is_python = args.first().map(|a| a.rsplit(|b| *b == b'/').next().unwrap_or(a).starts_with(b"python")).unwrap_or(false);
        let is_bridge = args.get(1).map(|a| a.ends_with(b"/prism-dbus-notif.py") || *a == b"prism-dbus-notif.py").unwrap_or(false);
        if is_python && is_bridge {
            eprintln!("[prismd/notif] stopping the old Python bridge (pid {pid})");
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

/// Collect popups / lockscreen that exited on their own (Esc, click-outside, unlock) so they do
/// not linger as zombies until the next IPC request. try_wait() reaps only our own children.
fn start_popup_watcher(state: Arc<Mutex<DaemonState>>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        if let Ok(mut st) = state.lock() {
            let _ = &mut st;
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
/// A lock is in progress (a supervisor thread owns the prism-lock process).
/// Screen brightness factor while dimmed before auto-lock.
const IDLE_DIM: f64 = 0.3;

/// Last `prism-ctl index rebuild`: (status, message); status is idle, running, success or failed.
static INDEX_STATE: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

static LOCK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Lock the session. A supervisor thread runs prism-lock until it exits 0 (unlocked). If it
/// dies any other way the compositor keeps the session locked with nothing on screen, so a
/// new prism-lock is started to take the lock over. Exit code 3 means the compositor cannot
/// lock at all; that is not retried.
fn lock_screen(_st: &mut DaemonState) -> Result<&'static str, String> {
    if LOCK_ACTIVE.swap(true, Ordering::SeqCst) {
        return Ok("already_locked");
    }
    eprintln!("[prismd] Spawning prism-lock...");
    let mut child = match spawn_prism_lock(&[]) {
        Ok(c) => c,
        Err(e) => {
            LOCK_ACTIVE.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    thread::spawn(move || {
        let mut quick_failures = 0;
        loop {
            let started = std::time::Instant::now();
            let status = child.wait();
            let code = status.as_ref().ok().and_then(|s| s.code());
            match code {
                Some(0) => break,
                Some(3) => {
                    eprintln!("[prismd] prism-lock: compositor cannot lock the session");
                    break;
                }
                _ => {
                    eprintln!("[prismd] prism-lock exited without unlocking ({status:?}); restarting it");
                    quick_failures = if started.elapsed() < Duration::from_secs(5) { quick_failures + 1 } else { 0 };
                    if quick_failures >= 10 {
                        eprintln!("[prismd] prism-lock keeps failing; giving up (unlock from a TTY: run prism-lock with WAYLAND_DISPLAY set)");
                        break;
                    }
                    thread::sleep(Duration::from_millis(300));
                    match spawn_prism_lock(&[]) {
                        Ok(c) => child = c,
                        Err(e) => {
                            eprintln!("[prismd] cannot restart prism-lock: {e}");
                            break;
                        }
                    }
                }
            }
        }
        LOCK_ACTIVE.store(false, Ordering::SeqCst);
    });
    Ok("lock_invoked")
}

fn idle_settings(cfg: &Config) -> idle::IdleSettings {
    idle::IdleSettings { auto_lock: cfg.manage_idle, timeout_mins: cfg.idle_timeout }
}

/// Push `st.config` to the services that follow it live.
/// Blur level last sent to the compositor (0 = none yet).
static BLUR_LEVEL: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Niri / Hyprland blur strength from `blur_strength` (QML applyNiriBlur), only when it changed.
fn apply_blur(cfg: &Config) {
    let level = prism_common::blur::level(cfg.blur_strength);
    if prism_common::blur::compositor().is_none() || BLUR_LEVEL.swap(level, Ordering::SeqCst) == level {
        return;
    }
    if let Err(e) = prism_common::blur::apply(level) {
        eprintln!("[prismd] applying blur strength failed: {e}");
    }
}

/// prism-phoned should run (config `phone_enabled`).
static PHONE_WANTED: AtomicBool = AtomicBool::new(false);
static PHONE_SUPERVISOR: std::sync::Once = std::sync::Once::new();

/// Run prism-phoned while `phone_enabled` is on: start it, restart it if it exits, stop it when
/// the setting goes off. A running one is told to re-read the config.
fn apply_phone(cfg: &Config) {
    let was = PHONE_WANTED.swap(cfg.phone_enabled, Ordering::SeqCst);
    if was && cfg.phone_enabled {
        thread::spawn(|| {
            let _ = prism_common::phone::phone_request(&prism_common::phone::PhoneCommand::ReloadConfig);
        });
    }
    PHONE_SUPERVISOR.call_once(|| {
        thread::spawn(|| {
            let mut child: Option<Child> = None;
            let mut started = std::time::Instant::now();
            loop {
                let wanted = PHONE_WANTED.load(Ordering::SeqCst);
                if let Some(c) = child.as_mut() {
                    if !wanted {
                        let _ = c.kill();
                        let _ = c.wait();
                        child = None;
                        eprintln!("[prismd] phone sync stopped");
                    } else if let Ok(Some(status)) = c.try_wait() {
                        eprintln!("[prismd] prism-phoned exited ({status})");
                        child = None;
                        // Back off when it dies at once (e.g. ports taken).
                        if started.elapsed() < Duration::from_secs(5) {
                            thread::sleep(Duration::from_secs(10));
                        }
                    }
                } else if wanted {
                    match Command::new(resolve_sibling_bin("prism-phoned")).spawn() {
                        Ok(c) => {
                            eprintln!("[prismd] phone sync started");
                            started = std::time::Instant::now();
                            child = Some(c);
                        }
                        Err(e) => {
                            eprintln!("[prismd] cannot start prism-phoned: {e}");
                            thread::sleep(Duration::from_secs(10));
                        }
                    }
                }
                thread::sleep(Duration::from_secs(1));
            }
        });
    });
}

fn apply_config(st: &DaemonState) {
    apply_blur(&st.config);
    apply_phone(&st.config);
    st.bluelight.update_config(&st.config);
    st.depth.update_config(&st.config);
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
            Ok(IpcCommand::Phone(cmd)) => match prism_common::phone::phone_request(&cmd) {
                Ok(prism_common::phone::PhoneReply::Ok { message }) => IpcResponse::Ok(message),
                Ok(prism_common::phone::PhoneReply::Error { message }) => IpcResponse::Error(message),
                Ok(prism_common::phone::PhoneReply::State { state }) => IpcResponse::Ok(serde_json::to_string(&state).ok()),
                Err(e) => IpcResponse::Error(e),
            },
            Ok(IpcCommand::PhoneCard(action)) => {
                let cmd = match action.as_str() {
                    "open" => prism_common::UiCommand::Open("phone".into()),
                    "close" => prism_common::UiCommand::Close,
                    _ => prism_common::UiCommand::Toggle("phone".into()),
                };
                match send_ui(&cmd) {
                    Ok(_) => IpcResponse::Ok(None),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Help) => IpcResponse::Ok(Some("Prism IPC server active".to_string())),

            Ok(IpcCommand::Lock(LockAction::Status)) => {
                let st = state.lock().unwrap();
                let idle = st.is_idle.load(Ordering::SeqCst);
                let locked = if LOCK_ACTIVE.load(Ordering::SeqCst) { "locked" } else { "unlocked" };
                IpcResponse::Ok(Some(format!("{locked}, idle={}", idle)))
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
                let mut notif = Notification::new(0, app_name, summary, body);
                let silent = {
                    let mut st = state.lock().unwrap();
                    notif.id = st.notifications.add(notif.clone());
                    st.silent
                };
                let id = notif.id;
                if !silent {
                    send_notif(popup_for(&notif));
                }
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
                let ids: Vec<u32> = st.notifications.notifications.iter().map(|n| n.id).collect();
                st.notifications.clear();
                drop(st);
                for id in ids {
                    notifd::emit_closed(id, notifd::CLOSED_DISMISSED);
                }
                IpcResponse::Ok(Some("Notifications cleared".to_string()))
            }
            Ok(IpcCommand::Notif(NotifAction::Silent(want))) => {
                let mut st = state.lock().unwrap();
                st.silent = want.unwrap_or(!st.silent);
                IpcResponse::Ok(Some(format!("silent:{}", st.silent)))
            }
            Ok(IpcCommand::Notif(NotifAction::ClearApp(app))) => {
                let mut st = state.lock().unwrap();
                let key = app.to_lowercase();
                let ids: Vec<u32> = st.notifications.notifications.iter().filter(|n| n.app_name.to_lowercase() == key).map(|n| n.id).collect();
                for id in &ids {
                    send_notif(prism_common::NotifOverlayMessage::Dismiss { id: *id });
                    st.notifications.remove(*id);
                    notifd::emit_closed(*id, notifd::CLOSED_DISMISSED);
                }
                IpcResponse::Ok(Some(format!("cleared:{}", ids.len())))
            }
            Ok(IpcCommand::Notif(NotifAction::Invoke { id, action })) => {
                // Tell the app, then close the notification (spec: unless "resident").
                notifd::emit_action(id, &action);
                send_notif(prism_common::NotifOverlayMessage::Dismiss { id });
                state.lock().unwrap().notifications.remove(id);
                notifd::emit_closed(id, notifd::CLOSED_DISMISSED);
                IpcResponse::Ok(Some(format!("invoked:{id}:{action}")))
            }
            Ok(IpcCommand::Notif(NotifAction::State)) => {
                let st = state.lock().unwrap();
                let flags = prism_common::ipc::DaemonFlags { silent: st.silent, caffeinate: CAFFEINATE.load(Ordering::SeqCst) };
                IpcResponse::Ok(serde_json::to_string(&flags).ok())
            }
            Ok(IpcCommand::Caffeinate(want)) => {
                let on = want.unwrap_or(!CAFFEINATE.load(Ordering::SeqCst));
                match set_caffeinate(on) {
                    Ok(()) => IpcResponse::Ok(Some(format!("caffeinate:{}", on))),
                    Err(e) => IpcResponse::Error(e),
                }
            }
            Ok(IpcCommand::Notif(NotifAction::Dismiss(id))) => {
                send_notif(prism_common::NotifOverlayMessage::Dismiss { id });
                let mut st = state.lock().unwrap();
                if st.notifications.remove(id) {
                    notifd::emit_closed(id, notifd::CLOSED_DISMISSED);
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

            Ok(IpcCommand::ColorPicker(action)) => {
                // The picker overlay lives in prism-ui (picker.rs); it copies the colour itself.
                let what = match action {
                    ColorPickerAction::Close => "close",
                    ColorPickerAction::Toggle => "toggle",
                    ColorPickerAction::Open | ColorPickerAction::Pick => "open",
                };
                match send_ui(&prism_common::UiCommand::ColorPicker(what.to_string())) {
                    Ok(_) => IpcResponse::Ok(Some(format!("colorpicker_{what}"))),
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

            Ok(IpcCommand::Index(action)) => {
                use prism_common::ipc::IndexAction;
                let (mut status, message) = INDEX_STATE.lock().unwrap().clone();
                if status.is_empty() {
                    status = "idle".into();
                }
                match action {
                    IndexAction::Rebuild if status == "running" => IpcResponse::Error("Index rebuild already running".into()),
                    IndexAction::Rebuild => {
                        *INDEX_STATE.lock().unwrap() = ("running".into(), "Rebuilding index".into());
                        std::thread::spawn(|| {
                            let r = prism_common::files::rebuild_index(|| {});
                            *INDEX_STATE.lock().unwrap() = match r {
                                Ok(()) => ("success".into(), "Index updated".into()),
                                Err(e) => ("failed".into(), e),
                            };
                        });
                        IpcResponse::Ok(Some("Index rebuild started".into()))
                    }
                    IndexAction::Status => IpcResponse::Ok(Some(format!(
                        "status={status}, last_indexed={}",
                        prism_common::files::last_indexed().unwrap_or_else(|| "never".into())
                    ))),
                    IndexAction::Message => IpcResponse::Ok(Some(message)),
                    IndexAction::LastIndexed => IpcResponse::Ok(prism_common::files::last_indexed()),
                    IndexAction::LastError => IpcResponse::Ok(Some(if status == "failed" { message } else { String::new() })),
                }
            }
            Ok(IpcCommand::Depth(DepthAction::Status)) => {
                let st = prism_common::depth::get_depth_status();
                IpcResponse::Ok(Some(format!(
                    "model_available={}, mask_available={}, mask_path={:?}",
                    st.has_model, st.has_mask, st.mask_path
                )))
            }
            Ok(IpcCommand::Depth(DepthAction::Generate)) => {
                let st = state.lock().unwrap();
                if !st.config.depth_enabled {
                    IpcResponse::Error("The depth effect is turned off".to_string())
                } else if prism_common::wallpaper::get_current_wallpaper().is_none() {
                    IpcResponse::Error("No wallpaper recorded to generate depth for".to_string())
                } else {
                    // Slow on a cache miss: the worker publishes the mask when it is ready.
                    st.depth.generate();
                    IpcResponse::Ok(Some("Generating depth mask".to_string()))
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

    let gamma = gamma::start();
    let bluelight = BlueLight::start(&cfg, gamma.clone());
    let depth = depth::Depth::start(&cfg);
    let apps = load_all_desktop_apps();
    println!("Loaded {} desktop application entries.", apps.len());

    // Prism's cache holds clipboard and notification history: owner-only.
    if let Ok(home) = std::env::var("HOME") {
        let _ = prism_common::clipboard::private_dir(&std::path::PathBuf::from(home).join(".cache/prism"));
    }
    let mut clipboard = ClipboardStore::load();
    // Re-apply the sensitivity filter to stored history and tighten file permissions.
    let purged = clipboard.purge_sensitive();
    if purged > 0 {
        eprintln!("[prismd/clipboard] removed {purged} stored entries that look sensitive");
    }
    let _ = clipboard.save();
    println!("Loaded {} clipboard history entries.", clipboard.items.len());

    let notifications = NotificationStore::load();
    notifications.save(); // rewrite with owner-only permissions
    println!("Loaded {} notification entries.", notifications.notifications.len());

    let emojis = prism_common::load_emojis();
    println!("Loaded {} emoji entries.", emojis.len());

    let is_idle = Arc::new(AtomicBool::new(false));
    let idle_start = idle_settings(&cfg);

    apply_blur(&cfg);
    apply_phone(&cfg);
    let state = Arc::new(Mutex::new(DaemonState {
        config: cfg,
        theme,
        bluelight,
        depth,
        idle: None,
        apps,
        is_idle: Arc::clone(&is_idle),
        clipboard,
        notifications,
        emojis,
        silent: false,
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
        // QML dimmed the screens 30 s before the lock; done in the gamma ramp (instant, every
        // output) instead of DDC brightness writes, which stall the compositor.
        Box::new(move |dim| {
            if let Some(g) = &gamma {
                g.dim(if dim { IDLE_DIM } else { 1.0 });
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
        stop_python_bridge();
        notifd::start(Arc::new(NotifHandler { state: Arc::downgrade(&state) }));
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
