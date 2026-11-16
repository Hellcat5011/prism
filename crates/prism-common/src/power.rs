use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Lock,
    Logout,
    Suspend,
    Reboot,
    Shutdown,
    Toggle,
    Open,
    Close,
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
    fn test_detect_compositor_logout_fallback() {
        let (cmd, _) = detect_compositor_logout_cmd();
        assert!(!cmd.is_empty());
    }
}
