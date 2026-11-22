use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::exit;

use prism_common::ipc::*;

fn print_usage() {
    eprintln!(
        r#"Prism Control CLI (prism-ctl)

Usage: prism-ctl <target> [action] [args...]

Targets & Commands:
  launcher [toggle|open|close]
  settings [toggle|open|close|tab <name|number>]
  search [toggle|open|close|query <text>|results <text>]
  wallpaper [set <path> [scheme] [mode]|get|toggle|open|close]
  media [status|play-pause|next|prev|stop]
  theme [reload|palette]
  notif [list|clear|send <summary> [body]|toggle|open|close|silent [on|off]|state]
  caffeinate [on|off]
  setup                             the first-time setup guide
  deps [--quiet]                    missing programs and services (exit 1: something needed)
  notes [toggle|open|close]         the Notes card
  timer [start <5m|90s|1h30m>|add <1m>|pause|resume|cancel|status]
  stopwatch [toggle|reset]
  switcher [next|prev|show|close]   window switcher (bind next/prev to Alt+Tab / Alt+Shift+Tab)
  gamemode [on|off]                 silent, performance profile, no animations or blur
  osd volume [+<step>%|-<step>%|mute|<value>]
  osd brightness [+<step>%|-<step>%|<value>]
  clipboard [toggle|open|close]
  emoji [search <query>|copy <glyph>|list|toggle]
  colorpicker [pick|toggle]
  power [lock|logout|suspend|sleep|hibernate|reboot|shutdown|toggle]
  desktop [edit|open|close|reset|status]
  edit-mode [open|close|reset|status]
  network [status|toggle-wifi]
  bluetooth [status|toggle]
  keybinds [list|toggle|open|close|edit|rules]   edit: Shortcuts editor, rules: Window Rules
  lock [lock|status]
  screenshot [toggle|region|window|text|screen [--no-save] [--no-clipboard]|open <mode>|close|record|stop|status
             |replay [save|on|off|toggle]]   replay buffer: save the last seconds
  phone [toggle|open|close|status|refresh|connect <ip>|pair|unpair|accept|reject [device]
         |ping|ring|clipboard [text]|media [play|pause|next|previous]
         |send <file>...|send --text <text>|send --url <url>]
  bluelight [toggle|manual_toggle|dump]
  depth [status|generate|clear]
  index [rebuild|status]
  download [toggle|open|close|list|add <url> [--name <file>] [--dir <folder>] [--referer <url>] [--ask] [--hls [--variant <url>]]
           |start <id> [--name <file>] [--dir <folder>]
           |pause <id>|resume <id>|cancel <id>|remove <id>|open <id>|show <id>|clear]
  ping

See man prism-ctl for every command.
"#
    );
}

#[allow(dead_code)]
fn parse_target_action(arg: Option<&str>) -> TargetAction {
    match arg {
        Some("open") => TargetAction::Open,
        Some("close") => TargetAction::Close,
        _ => TargetAction::Toggle,
    }
}

fn parse_command(args: &[String]) -> Result<IpcCommand, String> {
    if args.is_empty() {
        return Err("No target specified".to_string());
    }

    let target = args[0].as_str();
    let action_str = args.get(1).map(|s| s.as_str());

    match target {
        "launcher" => {
            let act = match action_str {
                Some("open") => LauncherAction::Open,
                Some("close") => LauncherAction::Close,
                Some("query") => {
                    let q = args.get(2).cloned().unwrap_or_default();
                    LauncherAction::Query(q)
                }
                Some("run") => {
                    let id = args.get(2).cloned().unwrap_or_default();
                    LauncherAction::Run(id)
                }
                Some("list") => LauncherAction::List,
                _ => LauncherAction::Toggle,
            };
            Ok(IpcCommand::Launcher(act))
        }
        "wallpaper" => {
            let act = match action_str {
                Some("set") => {
                    let path = args.get(2).map(std::path::PathBuf::from).unwrap_or_default();
                    let scheme = args.get(3).cloned();
                    let mode = args.get(4).cloned();
                    WallpaperAction::Set { path, scheme, mode }
                }
                Some("get") => WallpaperAction::Get,
                Some("open") => WallpaperAction::Open,
                Some("close") => WallpaperAction::Close,
                _ => WallpaperAction::Toggle,
            };
            Ok(IpcCommand::Wallpaper(act))
        }
        "media" => {
            let act = match action_str {
                Some("play-pause") | Some("play") | Some("pause") | Some("toggle") => MediaAction::PlayPause,
                Some("next") => MediaAction::Next,
                Some("prev") | Some("previous") => MediaAction::Previous,
                Some("stop") => MediaAction::Stop,
                _ => MediaAction::Status,
            };
            Ok(IpcCommand::Media(act))
        }
        "theme" => {
            let act = match action_str {
                Some("palette") | Some("get") => ThemeAction::GetPalette,
                _ => ThemeAction::Reload,
            };
            Ok(IpcCommand::Theme(act))
        }
        "notif" => {
            let act = match action_str {
                Some("list") => NotifAction::List,
                Some("clear") => NotifAction::Clear,
                Some("dismiss") => {
                    let id = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    NotifAction::Dismiss(id)
                }
                Some("send") => {
                    let mut app_name = "Prism".to_string();
                    let mut positional = Vec::new();
                    let mut i = 2;
                    while i < args.len() {
                        if (args[i] == "-a" || args[i] == "--app") && i + 1 < args.len() {
                            app_name = args[i + 1].clone();
                            i += 2;
                        } else {
                            positional.push(args[i].clone());
                            i += 1;
                        }
                    }
                    let (summary, body) = match positional.len() {
                        0 => ("Test Notification".to_string(), String::new()),
                        1 => (positional[0].clone(), String::new()),
                        2 => (positional[0].clone(), positional[1].clone()),
                        _ => {
                            if app_name == "Prism" {
                                app_name = positional[0].clone();
                                (positional[1].clone(), positional[2].clone())
                            } else {
                                (positional[0].clone(), positional[1].clone())
                            }
                        }
                    };
                    NotifAction::Send {
                        app_name,
                        summary,
                        body,
                    }
                }
                Some("open") => NotifAction::Open,
                Some("close") => NotifAction::Close,
                Some("silent") => NotifAction::Silent(on_off(args.get(2))),
                Some("state") => NotifAction::State,
                _ => NotifAction::Toggle,
            };
            Ok(IpcCommand::Notif(act))
        }
        "caffeinate" => Ok(IpcCommand::Caffeinate(on_off(args.get(1)))),
        "timer" => {
            use prism_common::ipc::{parse_duration, TimerAction as T};
            let arg = args.get(2).map(String::as_str).unwrap_or("");
            let act = match args.get(1).map(String::as_str) {
                Some("start") => T::Start(parse_duration(arg).ok_or("usage: prism-ctl timer start <5m|90s|1h30m|2:30>")?),
                Some("add") => T::Add(parse_duration(arg.trim_start_matches('-')).ok_or("usage: prism-ctl timer add <1m>")? as i64 * if arg.starts_with('-') { -1 } else { 1 }),
                Some("pause") => T::Pause,
                Some("resume") => T::Resume,
                Some("cancel") | Some("stop") => T::Cancel,
                None | Some("status") => T::State,
                Some(d) => T::Start(parse_duration(d).ok_or_else(|| format!("unknown timer action '{d}'"))?),
            };
            Ok(IpcCommand::Timer(act))
        }
        "stopwatch" => {
            use prism_common::ipc::TimerAction as T;
            Ok(IpcCommand::Timer(match args.get(1).map(String::as_str) {
                Some("reset") => T::StopwatchReset,
                None | Some("toggle") | Some("start") | Some("pause") => T::StopwatchToggle,
                Some(other) => return Err(format!("unknown stopwatch action '{other}'")),
            }))
        }
        "setup" | "guide" => Ok(IpcCommand::Setup),
        "download" | "downloads" => parse_download(&args[1..]),
        "notes" => Ok(IpcCommand::Notes(args.get(1).cloned().unwrap_or_else(|| "toggle".into()))),
        "switcher" => Ok(IpcCommand::Switcher(args.get(1).cloned().unwrap_or_else(|| "next".into()))),
        "gamemode" | "game-mode" => Ok(IpcCommand::GameMode(on_off(args.get(1)))),
        "calendar" => match args.get(1).map(String::as_str) {
            Some("refresh") => Ok(IpcCommand::CalendarRefresh),
            None => Ok(IpcCommand::Calendar("toggle".into())),
            Some(a @ ("toggle" | "open" | "close")) => Ok(IpcCommand::Calendar(a.into())),
            Some(other) => Err(format!("calendar: unknown action '{other}' (toggle, open, close, refresh)")),
        },
        "clipboard" => {
            let act = match action_str {
                Some("open") => ClipboardAction::Open,
                Some("close") => ClipboardAction::Close,
                Some("list") => ClipboardAction::List,
                Some("clear") => ClipboardAction::Clear,
                Some("get") => {
                    let idx = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    ClipboardAction::Get(idx)
                }
                Some("add") => {
                    let text = args.get(2).cloned().unwrap_or_default();
                    ClipboardAction::Add(text)
                }
                Some("copy") => ClipboardAction::Copy(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0)),
                _ => ClipboardAction::Toggle,
            };
            Ok(IpcCommand::Clipboard(act))
        }
        "emoji" => {
            let act = match action_str {
                Some("search") => {
                    let q = args.get(2).cloned().unwrap_or_default();
                    EmojiAction::Search(q)
                }
                Some("copy") => {
                    let glyph = args.get(2).cloned().unwrap_or_default();
                    EmojiAction::Copy(glyph)
                }
                Some("list") => EmojiAction::List,
                Some("open") => EmojiAction::Open,
                Some("close") => EmojiAction::Close,
                _ => EmojiAction::Toggle,
            };
            Ok(IpcCommand::Emoji(act))
        }
        "power" => {
            let act = match action_str {
                Some("lock") => PowerAction::Lock,
                Some("logout") | Some("exit") => PowerAction::Logout,
                Some("suspend") => PowerAction::Suspend,
                Some("sleep") => PowerAction::Sleep,
                Some("hibernate") => PowerAction::Hibernate,
                Some("reboot") | Some("restart") => PowerAction::Reboot,
                Some("shutdown") | Some("poweroff") => PowerAction::Shutdown,
                Some("open") => PowerAction::Open,
                Some("close") => PowerAction::Close,
                _ => PowerAction::Toggle,
            };
            Ok(IpcCommand::Power(act))
        }
        "keybinds" => {
            let act = match action_str {
                Some("list") => KeybindsAction::List,
                Some("open") => KeybindsAction::Open,
                Some("close") => KeybindsAction::Close,
                Some("edit") => KeybindsAction::Edit(false),
                Some("rules") => KeybindsAction::Edit(true),
                _ => KeybindsAction::Toggle,
            };
            Ok(IpcCommand::Keybinds(act))
        }
        "network" => {
            let act = match action_str {
                Some("toggle-wifi") | Some("toggle") => NetworkAction::ToggleWifi,
                _ => NetworkAction::Status,
            };
            Ok(IpcCommand::Network(act))
        }
        "bluetooth" => {
            let act = match action_str {
                Some("toggle") => BluetoothAction::Toggle,
                _ => BluetoothAction::Status,
            };
            Ok(IpcCommand::Bluetooth(act))
        }

        "search" => {
            let act = match action_str {
                Some("open") => SearchAction::Open,
                Some("close") => SearchAction::Close,
                Some("query") => {
                    SearchAction::SetQuery(args[2..].join(" "))
                }
                Some("results") => SearchAction::GetResults(args[2..].join(" ")),
                _ => SearchAction::Toggle,
            };
            Ok(IpcCommand::Search(act))
        }

        "osd" => {
            let kind = action_str.unwrap_or("volume");
            let raw_val = args.get(2).map(|s| s.trim_end_matches('%')).unwrap_or("0");
            match kind {
                "volume" => {
                    if raw_val == "mute" {
                        Ok(IpcCommand::Osd(OsdAction::VolumeMute))
                    } else if let Some(v) = raw_val.strip_prefix('+') {
                        let delta = v.parse::<i32>().unwrap_or(5);
                        Ok(IpcCommand::Osd(OsdAction::VolumeDelta(delta)))
                    } else if let Some(v) = raw_val.strip_prefix('-') {
                        let delta = -v.parse::<i32>().unwrap_or(5);
                        Ok(IpcCommand::Osd(OsdAction::VolumeDelta(delta)))
                    } else {
                        Ok(IpcCommand::Osd(OsdAction::Volume(raw_val.to_string())))
                    }
                }
                "brightness" => {
                    if let Some(v) = raw_val.strip_prefix('+') {
                        let delta = v.parse::<i32>().unwrap_or(5);
                        Ok(IpcCommand::Osd(OsdAction::BrightnessDelta(delta)))
                    } else if let Some(v) = raw_val.strip_prefix('-') {
                        let delta = -v.parse::<i32>().unwrap_or(5);
                        Ok(IpcCommand::Osd(OsdAction::BrightnessDelta(delta)))
                    } else {
                        Ok(IpcCommand::Osd(OsdAction::Brightness(raw_val.to_string())))
                    }
                }
                _ => Ok(IpcCommand::Osd(OsdAction::Volume(raw_val.to_string()))),
            }
        }

        "colorpicker" => {
            let act = match action_str {
                Some("open") => ColorPickerAction::Open,
                Some("close") => ColorPickerAction::Close,
                Some("pick") => ColorPickerAction::Pick,
                _ => ColorPickerAction::Toggle,
            };
            Ok(IpcCommand::ColorPicker(act))
        }

        "settings" => {
            let act = match action_str {
                Some("open") => SettingsAction::Open,
                Some("close") => SettingsAction::Close,
                Some("tab") => {
                    let name = args[2..].join(" ");
                    let tab = prism_common::ipc::settings_tab(&name).ok_or_else(|| {
                        format!("unknown settings tab '{name}'; tabs: {}", prism_common::ipc::SETTINGS_TABS.join(", "))
                    })?;
                    SettingsAction::OpenTab(tab as u32)
                }
                _ => SettingsAction::Toggle,
            };
            Ok(IpcCommand::Settings(act))
        }

        "desktopedit" | "edit-mode" => {
            let act = match action_str {
                Some("open") => DesktopEditAction::Open,
                Some("close") => DesktopEditAction::Close,
                Some("save") => DesktopEditAction::Save,
                Some("discard") => DesktopEditAction::Discard,
                Some("status") => DesktopEditAction::Status,
                Some("reset") => DesktopEditAction::Reset,
                _ => DesktopEditAction::Toggle,
            };
            Ok(IpcCommand::DesktopEdit(act))
        }

        "desktop" => {
            let act = match action_str {
                Some("edit") | Some("edit-mode") => DesktopEditAction::Toggle,
                Some("open") => DesktopEditAction::Open,
                Some("close") => DesktopEditAction::Close,
                Some("save") => DesktopEditAction::Save,
                Some("discard") => DesktopEditAction::Discard,
                Some("status") => DesktopEditAction::Status,
                Some("reset") => DesktopEditAction::Reset,
                _ => DesktopEditAction::Toggle,
            };
            Ok(IpcCommand::DesktopEdit(act))
        }

        "lock" => {
            let act = match action_str {
                Some("status") => LockAction::Status,
                None | Some("lock") => LockAction::Lock,
                // Never lock on a mistyped action.
                Some(other) => return Err(format!("unknown lock action '{other}'")),
            };
            Ok(IpcCommand::Lock(act))
        }

        "bluelight" => {
            let act = match action_str {
                Some("manual_toggle") => BlueLightAction::ManualToggle,
                Some("dump") => BlueLightAction::Dump,
                _ => BlueLightAction::Toggle,
            };
            Ok(IpcCommand::BlueLight(act))
        }

        "depth" => {
            let act = match action_str {
                Some("generate") => DepthAction::Generate,
                Some("clear") => DepthAction::Clear,
                _ => DepthAction::Status,
            };
            Ok(IpcCommand::Depth(act))
        }

        "index" => {
            let act = match action_str {
                Some("rebuild") => IndexAction::Rebuild,
                _ => IndexAction::Status,
            };
            Ok(IpcCommand::Index(act))
        }

        "phone" => parse_phone(&args[1..]),
        "screenshot" | "capture" => {
            use prism_common::ipc::CaptureAction as A;
            let flag = |f: &str| args.iter().any(|a| a == f);
            let act = match action_str {
                None | Some("toggle") => A::Toggle,
                Some("open") => A::Open(args.get(2).cloned().unwrap_or_else(|| "region".into())),
                Some("close") => A::Close,
                Some("region") => A::Open("region".into()),
                Some("window") => A::Open("window".into()),
                Some("text") | Some("ocr") => A::Open("text".into()),
                Some("screen") | Some("output") => A::Screen { save: !flag("--no-save"), clipboard: !flag("--no-clipboard") },
                Some("record") => A::Record,
                Some("stop") => A::Stop,
                Some("status") => A::Status,
                Some("replay") => A::Replay(args.get(2).cloned().unwrap_or_else(|| "save".into())),
                Some(other) => return Err(format!("unknown screenshot action '{other}'")),
            };
            Ok(IpcCommand::Capture(act))
        }

        "ping" => Ok(IpcCommand::Ping),
        "help" => Ok(IpcCommand::Help),

        other => Err(format!("Unknown target '{}'", other)),
    }
}

/// `prism-ctl deps [--quiet]`: what is missing of the programs and services Prism uses.
/// Exit 0 when nothing needed is missing, 1 otherwise. `--quiet`: nothing when all are there.
fn print_deps(quiet: bool) -> i32 {
    use prism_common::deps;
    let missing = deps::missing();
    if missing.is_empty() {
        if !quiet {
            println!("Everything Prism uses is installed.");
        }
        return 0;
    }
    println!("Missing programs and services:");
    let width = missing.iter().map(|d| d.name.len()).max().unwrap_or(0);
    for d in &missing {
        println!("  {:width$}  {:11}  {} (Arch: {})", d.name, d.level.label(), d.what, d.arch);
    }
    if let Some(cmd) = deps::pacman_command(&missing).filter(|_| deps::in_path("pacman")) {
        println!("\nInstall with:\n  {cmd}");
    }
    if missing.iter().any(|d| d.arch.contains("AUR")) {
        println!("AUR packages need an AUR helper, for example: yay -S {}", missing.iter().filter(|d| d.arch.contains("AUR")).map(|d| d.arch.split_whitespace().next().unwrap_or(d.name)).collect::<Vec<_>>().join(" "));
    }
    i32::from(missing.iter().any(|d| d.level == deps::Level::Needed))
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print_usage();
        return;
    }
    // Needs no running Prism (install.sh runs it).
    if args[0] == "deps" {
        exit(print_deps(args.get(1).map(String::as_str) == Some("--quiet")));
    }

    let cmd = match parse_command(&args) {
        Ok(cmd) => cmd,
        Err(err) => {
            eprintln!("Error: {}", err);
            print_usage();
            exit(1);
        }
    };

    let socket_path = get_socket_path();
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Failed to connect to Prism daemon at '{}': {}",
                socket_path.display(),
                e
            );
            exit(2);
        }
    };

    let mut payload = match serde_json::to_string(&cmd) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to serialize command: {}", e);
            exit(3);
        }
    };
    payload.push('\n');

    if let Err(e) = stream.write_all(payload.as_bytes()) {
        eprintln!("Failed to send command: {}", e);
        exit(4);
    }

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    if let Err(e) = reader.read_line(&mut response_line) {
        eprintln!("Failed to read response: {}", e);
        exit(5);
    }

    match serde_json::from_str::<IpcResponse>(&response_line) {
        Ok(IpcResponse::Ok(data)) => {
            if let Some(msg) = data {
                if cmd == IpcCommand::Download(prism_common::ipc::DownloadAction::List) {
                    print_downloads(&msg);
                } else {
                    println!("{}", msg);
                }
            }
        }
        Ok(IpcResponse::Error(err)) => {
            eprintln!("Prism error: {}", err);
            exit(6);
        }
        Err(_) => {
            // Raw response fallback
            print!("{}", response_line);
        }
    }
}

/// `prism-ctl phone ...`
fn parse_phone(args: &[String]) -> Result<IpcCommand, String> {
    use prism_common::phone::{MediaCommand, PhoneCommand as P};
    let a = |i: usize| args.get(i).cloned();
    let dev = |i: usize| args.get(i).cloned();
    let cmd = match args.first().map(|s| s.as_str()).unwrap_or("toggle") {
        "toggle" | "open" | "close" => return Ok(IpcCommand::PhoneCard(args.first().cloned().unwrap_or_else(|| "toggle".into()))),
        "status" => P::Status,
        "refresh" => P::Refresh,
        "connect" => P::Connect { addr: a(1).ok_or("usage: phone connect <ip>[:port]")? },
        "pair" => P::Pair { device: dev(1) },
        "unpair" => P::Unpair { device: dev(1) },
        "accept" => P::Accept { device: dev(1) },
        "reject" => P::Reject { device: dev(1) },
        "ping" => P::Ping { device: dev(1) },
        "ring" => P::Ring { device: dev(1) },
        "clipboard" => P::SendClipboard { device: None, text: a(1).unwrap_or_default() },
        "media" => {
            let action = match a(1).as_deref() {
                Some("play") => MediaCommand::Play,
                Some("pause") => MediaCommand::Pause,
                Some("next") => MediaCommand::Next,
                Some("previous") | Some("prev") => MediaCommand::Previous,
                _ => MediaCommand::PlayPause,
            };
            P::Media { device: None, action }
        }
        "send" => {
            if args.get(1).map(|s| s.as_str()) == Some("--text") {
                P::SendText { device: None, text: args[2..].join(" ") }
            } else if args.get(1).map(|s| s.as_str()) == Some("--url") {
                P::SendUrl { device: None, url: a(2).ok_or("usage: phone send --url <url>")? }
            } else {
                let paths: Vec<String> = args[1..]
                    .iter()
                    .map(|p| std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| p.clone()))
                    .collect();
                if paths.is_empty() {
                    return Err("usage: phone send <file>... | --text <text> | --url <url>".into());
                }
                P::SendFiles { device: None, paths }
            }
        }
        other => return Err(format!("unknown phone command '{other}'")),
    };
    Ok(IpcCommand::Phone(cmd))
}

/// `prism-ctl download ...`
fn parse_download(args: &[String]) -> Result<IpcCommand, String> {
    use prism_common::ipc::{DownloadAction as D, DownloadRequest};
    let id = || -> Result<u64, String> { args.get(1).and_then(|s| s.parse().ok()).ok_or_else(|| format!("usage: prism-ctl download {} <id>", args[0])) };
    let act = match args.first().map(String::as_str).unwrap_or("list") {
        "list" | "ls" => D::List,
        "add" => {
            let mut req = DownloadRequest::default();
            let mut i = 1;
            while i < args.len() {
                let val = || args.get(i + 1).cloned().ok_or_else(|| format!("{} needs a value", args[i]));
                match args[i].as_str() {
                    "--name" | "-o" => req.name = Some(val()?),
                    "--dir" | "-d" => {
                        let d = val()?;
                        req.dir = Some(std::fs::canonicalize(&d).map(|p| p.to_string_lossy().into_owned()).unwrap_or(d));
                    }
                    "--referer" => req.referer = Some(val()?),
                    "--cookie" => req.cookie = Some(val()?),
                    "--user-agent" => req.user_agent = Some(val()?),
                    "--ask" => {
                        req.ask = true;
                        i += 1;
                        continue;
                    }
                    "--hls" => {
                        req.kind = "hls".into();
                        i += 1;
                        continue;
                    }
                    "--variant" => req.variant = Some(val()?),
                    url if req.url.is_empty() && !url.starts_with('-') => {
                        req.url = url.to_string();
                        i += 1;
                        continue;
                    }
                    other => return Err(format!("unexpected '{other}'")),
                }
                i += 2;
            }
            if req.url.is_empty() {
                return Err("usage: prism-ctl download add <url> [--name <file>] [--dir <folder>]".into());
            }
            // HLS playlists become one MP4.
            if req.url.split(['?', '#']).next().is_some_and(|p| p.ends_with(".m3u8")) {
                req.kind = "hls".into();
            }
            D::Add(req)
        }
        "start" => {
            let (mut name, mut dir) = (None, None);
            let mut i = 2;
            while i + 1 < args.len() {
                match args[i].as_str() {
                    "--name" | "-o" => name = Some(args[i + 1].clone()),
                    "--dir" | "-d" => dir = Some(args[i + 1].clone()),
                    other => return Err(format!("unexpected '{other}'")),
                }
                i += 2;
            }
            D::Start { id: id()?, name, dir }
        }
        "decline" => D::Decline(id()?),
        "pause" => D::Pause(id()?),
        "resume" | "retry" => D::Resume(id()?),
        "cancel" => D::Cancel(id()?),
        "remove" | "rm" => D::Remove(id()?),
        "clear" => D::Clear,
        // Without an id: the Downloads card.
        "open" if args.len() < 2 => D::Card("open".into()),
        "toggle" => D::Card("toggle".into()),
        "close" => D::Card("close".into()),
        "open" => D::Open(id()?),
        "show" => D::Show(id()?),
        other => return Err(format!("unknown download action '{other}'")),
    };
    Ok(IpcCommand::Download(act))
}

/// `download list` as a table.
fn print_downloads(json: &str) {
    let Ok(list) = serde_json::from_str::<Vec<prism_common::ipc::DownloadInfo>>(json) else {
        println!("{json}");
        return;
    };
    if list.is_empty() {
        println!("No downloads");
        return;
    }
    let mb = |b: u64| format!("{:.1} MB", b as f64 / 1e6);
    for d in list {
        let size = match d.size {
            Some(s) if s > 0 => format!("{} of {} ({:.0}%)", mb(d.done), mb(s), d.done as f64 * 100.0 / s as f64),
            _ => mb(d.done),
        };
        let rate = if d.rate > 0 { format!("  {}/s, {} conn", mb(d.rate), d.connections) } else { String::new() };
        let err = d.error.map(|e| format!("  ({e})")).unwrap_or_default();
        println!("{:>4}  {:<9} {}  {}{}{}", d.id, d.state, d.name, size, rate, err);
    }
}

/// "on" / "off" to Some(bool); anything else (or nothing) means toggle.
fn on_off(arg: Option<&String>) -> Option<bool> {
    match arg.map(|s| s.as_str()) {
        Some("on") | Some("true") | Some("1") => Some(true),
        Some("off") | Some("false") | Some("0") => Some(false),
        _ => None,
    }
}
