use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Lock,
    Logout,
    Suspend,
    /// Hybrid sleep: suspends with a copy of memory on disk.
    Sleep,
    Hibernate,
    Reboot,
    Shutdown,
    Toggle,
    Open,
    Close,
}

/// The sleep states this computer can enter, as logind reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSupport {
    pub suspend: bool,
    pub sleep: bool,
    pub hibernate: bool,
}

/// logind's answer to `CanSuspend` and friends: "challenge" means allowed after asking.
fn logind_allows(answer: &str) -> bool {
    matches!(answer, "yes" | "challenge")
}

fn ask_logind() -> Option<PowerSupport> {
    let conn = zbus::blocking::Connection::system().ok()?;
    let can = |method: &str| -> Option<bool> {
        let reply = conn.call_method(Some("org.freedesktop.login1"), "/org/freedesktop/login1", Some("org.freedesktop.login1.Manager"), method, &()).ok()?;
        Some(logind_allows(&reply.body().deserialize::<String>().ok()?))
    };
    Some(PowerSupport { suspend: can("CanSuspend")?, sleep: can("CanHybridSleep")?, hibernate: can("CanHibernate")? })
}

/// What this computer supports. Asked once; without an answer only suspend is offered.
pub fn support() -> PowerSupport {
    static SUPPORT: std::sync::OnceLock<PowerSupport> = std::sync::OnceLock::new();
    *SUPPORT.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(ask_logind());
        });
        let unknown = PowerSupport { suspend: true, sleep: false, hibernate: false };
        rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten().unwrap_or(unknown)
    })
}

/// Detect the active Wayland compositor and return its graceful quit command
pub fn detect_compositor_logout_cmd() -> (&'static str, &'static [&'static str]) {
    if env::var("MANGO_INSTANCE_SIGNATURE").is_ok() {
        ("mmsg", &["quit"])
    } else if env::var("NIRI_SOCKET").is_ok() {
        ("niri", &["msg", "action", "quit", "--skip-confirmation"])
    } else if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        if env::var("UWSM_INSTANCE_ID").is_ok() || env::var("UWSM_SESSION_ID").is_ok() {
            ("uwsm", &["stop"])
        } else {
            ("hyprctl", &["dispatch", "exit"])
        }
    } else if env::var("SWAYSOCK").is_ok() {
        ("swaymsg", &["exit"])
    } else {
        ("loginctl", &["terminate-session", "self"])
    }
}

/// Execute a system power or session management action
pub fn execute_power(action: PowerAction) -> Result<String, String> {
    match action {
        PowerAction::Lock => {
            let status = Command::new("prism-lock")
                .spawn()
                .map_err(|e| format!("Failed to spawn prism-lock: {}", e))?;
            Ok(format!("Lockscreen spawned (PID {})", status.id()))
        }
        PowerAction::Logout => {
            // Specialized resilient cascade for Hyprland
            if env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
                // 1. Try UWSM if managed under systemd UWSM
                if env::var("UWSM_INSTANCE_ID").is_ok() || env::var("UWSM_SESSION_ID").is_ok() {
                    if let Ok(status) = Command::new("uwsm").arg("stop").status() {
                        if status.success() {
                            return Ok("Session logout initiated via UWSM".to_string());
                        }
                    }
                }
                // 2. Try hyprshutdown (graceful application saving) if available
                if let Ok(status) = Command::new("hyprshutdown").status() {
                    if status.success() {
                        return Ok("Session logout initiated via hyprshutdown".to_string());
                    }
                }
                // 3. Try standard hyprctl dispatch exit
                if let Ok(status) = Command::new("hyprctl").args(["dispatch", "exit"]).status() {
                    if status.success() {
                        return Ok("Session logout initiated via hyprctl".to_string());
                    }
                }
                // 4. Universal systemd logout fallback
                if let Ok(status) = Command::new("loginctl").args(["terminate-session", "self"]).status() {
                    if status.success() {
                        return Ok("Session logout initiated via loginctl fallback".to_string());
                    }
                }
                return Err("Failed to terminate Hyprland session via all dispatchers".to_string());
            }

            // Standard logout path for MangoWM, Niri, Sway, and fallback
            let (cmd, args) = detect_compositor_logout_cmd();
            let status = Command::new(cmd)
                .args(args)
                .status()
                .map_err(|e| format!("Failed to execute logout command '{}': {}", cmd, e))?;

            if status.success() {
                Ok("Session logout initiated".to_string())
            } else {
                // Best-effort fallback to loginctl
                if let Ok(fallback_status) = Command::new("loginctl").args(["terminate-session", "self"]).status() {
                    if fallback_status.success() {
                        return Ok("Session logout initiated via loginctl fallback".to_string());
                    }
                }
                Err(format!("Compositor logout '{}' returned exit code {:?}", cmd, status.code()))
            }
        }
        PowerAction::Suspend => {
            let status = Command::new("systemctl")
                .arg("suspend")
                .status()
                .map_err(|e| format!("Failed to run systemctl suspend: {}", e))?;

            if status.success() {
                Ok("Suspending system...".to_string())
            } else {
                Err("systemctl suspend failed".to_string())
            }
        }
        PowerAction::Sleep => {
            let status = Command::new("systemctl")
                .arg("hybrid-sleep")
                .status()
                .map_err(|e| format!("Failed to run systemctl hybrid-sleep: {}", e))?;

            if status.success() {
                Ok("Going to sleep...".to_string())
            } else {
                Err("systemctl hybrid-sleep failed".to_string())
            }
        }
        PowerAction::Hibernate => {
            let status = Command::new("systemctl")
                .arg("hibernate")
                .status()
                .map_err(|e| format!("Failed to run systemctl hibernate: {}", e))?;

            if status.success() {
                Ok("Hibernating system...".to_string())
            } else {
                Err("systemctl hibernate failed".to_string())
            }
        }
        PowerAction::Reboot => {
            let status = Command::new("systemctl")
                .arg("reboot")
                .status()
                .map_err(|e| format!("Failed to run systemctl reboot: {}", e))?;

            if status.success() {
                Ok("Rebooting system...".to_string())
            } else {
                Err("systemctl reboot failed".to_string())
            }
        }
        PowerAction::Shutdown => {
            let status = Command::new("systemctl")
                .arg("poweroff")
                .status()
                .map_err(|e| format!("Failed to run systemctl poweroff: {}", e))?;

            if status.success() {
                Ok("Powering off system...".to_string())
            } else {
                Err("systemctl poweroff failed".to_string())
            }
        }
        PowerAction::Toggle | PowerAction::Open | PowerAction::Close => {
            Ok("Power menu window action dispatched".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logind_answers_are_understood() {
        assert!(logind_allows("yes") && logind_allows("challenge"));
        assert!(!logind_allows("na") && !logind_allows("no") && !logind_allows(""));
    }

    #[test]
    fn test_detect_compositor_logout_fallback() {
        let (cmd, _) = detect_compositor_logout_cmd();
        assert!(!cmd.is_empty());
    }
}
