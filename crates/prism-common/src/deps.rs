//! Programs and services Prism uses at run time, and which of them are missing.
//!
//! One list for `prism-ctl deps`, deploy/install.sh (which runs it) and the last page of the
//! setup guide. Build dependencies (libraries, headers) are checked by install.sh itself.
//! Entries that cannot matter on this machine (brightnessctl without a backlight, BlueZ
//! without a Bluetooth adapter, checkupdates off Arch) are not checked.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Core parts of the shell do not work without it.
    Needed,
    /// A feature most people use does not work without it.
    Recommended,
    /// One smaller feature.
    Optional,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Needed => "needed",
            Level::Recommended => "recommended",
            Level::Optional => "optional",
        }
    }
}

/// How to tell a dependency is there.
#[derive(Clone, Copy, Debug)]
pub enum Probe {
    /// A program in PATH.
    Cmd(&'static str),
    /// Any of these files (a D-Bus service description, a helper binary).
    AnyPath(&'static [&'static str]),
}

#[derive(Clone, Copy, Debug)]
pub struct Dep {
    pub name: &'static str,
    pub probe: Probe,
    /// What does not work without it.
    pub what: &'static str,
    /// The package(s) to install on Arch Linux.
    pub arch: &'static str,
    pub level: Level,
    /// Whether it matters on this machine.
    pub applies: fn() -> bool,
}

fn always() -> bool {
    true
}

fn has_entries(dir: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

fn has_backlight() -> bool {
    has_entries("/sys/class/backlight")
}

fn has_bluetooth() -> bool {
    has_entries("/sys/class/bluetooth")
}

fn is_arch() -> bool {
    in_path("pacman")
}

pub const DEPS: &[Dep] = &[
    Dep { name: "wireplumber", probe: Probe::Cmd("wpctl"), what: "volume and microphone controls, the sound OSD", arch: "wireplumber", level: Level::Needed, applies: always },
    Dep { name: "pipewire", probe: Probe::Cmd("pw-dump"), what: "the per-app mixer, the Sound page and the privacy indicator", arch: "pipewire", level: Level::Needed, applies: always },
    Dep { name: "xdg-utils", probe: Probe::Cmd("xdg-open"), what: "opening files, folders and links", arch: "xdg-utils", level: Level::Needed, applies: always },
    Dep { name: "gsettings", probe: Probe::Cmd("gsettings"), what: "following the icon theme", arch: "glib2", level: Level::Needed, applies: always },
    Dep { name: "fontconfig", probe: Probe::Cmd("fc-list"), what: "the font lists in Settings", arch: "fontconfig", level: Level::Needed, applies: always },
    Dep {
        name: "xdg-desktop-portal",
        probe: Probe::AnyPath(&["/usr/lib/xdg-desktop-portal", "/usr/libexec/xdg-desktop-portal"]),
        what: "screen sharing, screenshots for apps and folder pickers",
        arch: "xdg-desktop-portal",
        level: Level::Needed,
        applies: always,
    },
    Dep { name: "matugen", probe: Probe::Cmd("matugen"), what: "colours taken from the wallpaper", arch: "matugen-bin (AUR)", level: Level::Recommended, applies: always },
    Dep {
        name: "NetworkManager",
        probe: Probe::AnyPath(&["/usr/share/dbus-1/system-services/org.freedesktop.NetworkManager.service", "/usr/bin/NetworkManager"]),
        what: "Wi-Fi, Ethernet and VPN in the notification centre",
        arch: "networkmanager",
        level: Level::Recommended,
        applies: always,
    },
    Dep {
        name: "BlueZ",
        probe: Probe::AnyPath(&["/usr/share/dbus-1/system-services/org.bluez.service", "/usr/lib/bluetooth/bluetoothd"]),
        what: "Bluetooth in the notification centre",
        arch: "bluez bluez-utils",
        level: Level::Recommended,
        applies: has_bluetooth,
    },
    Dep {
        name: "UPower",
        probe: Probe::AnyPath(&["/usr/share/dbus-1/system-services/org.freedesktop.UPower.service"]),
        what: "battery levels and low battery warnings",
        arch: "upower",
        level: Level::Recommended,
        applies: always,
    },
    Dep {
        name: "power-profiles-daemon",
        probe: Probe::AnyPath(&[
            "/usr/share/dbus-1/system-services/net.hadess.PowerProfiles.service",
            "/usr/share/dbus-1/system-services/org.freedesktop.UPower.PowerProfiles.service",
        ]),
        what: "power profiles and game mode's performance profile",
        arch: "power-profiles-daemon",
        level: Level::Recommended,
        applies: always,
    },
    Dep {
        name: "UDisks2",
        probe: Probe::AnyPath(&["/usr/share/dbus-1/system-services/org.freedesktop.UDisks2.service"]),
        what: "mounting and ejecting USB drives",
        arch: "udisks2",
        level: Level::Recommended,
        applies: always,
    },
    Dep {
        name: "keyring",
        probe: Probe::AnyPath(&["/usr/share/dbus-1/services/org.freedesktop.secrets.service", "/usr/bin/gnome-keyring-daemon", "/usr/bin/keepassxc"]),
        what: "keeping calendar addresses (and app passwords) safe",
        arch: "gnome-keyring",
        level: Level::Recommended,
        applies: always,
    },
    Dep { name: "polkit", probe: Probe::Cmd("pkexec"), what: "changes that need your password (Prism is the polkit agent)", arch: "polkit", level: Level::Recommended, applies: always },
    Dep { name: "ffmpeg", probe: Probe::Cmd("ffmpeg"), what: "trimming recordings, GIFs and joining downloaded videos", arch: "ffmpeg", level: Level::Recommended, applies: always },
    Dep { name: "curl", probe: Probe::Cmd("curl"), what: "weather, online calendars and sunset times for the night light", arch: "curl", level: Level::Recommended, applies: always },
    Dep { name: "pactl", probe: Probe::Cmd("pactl"), what: "choosing sound devices and ports", arch: "libpulse", level: Level::Recommended, applies: always },
    Dep { name: "brightnessctl", probe: Probe::Cmd("brightnessctl"), what: "screen brightness keys and slider", arch: "brightnessctl", level: Level::Recommended, applies: has_backlight },
    Dep { name: "ddcutil", probe: Probe::Cmd("ddcutil"), what: "brightness of external monitors", arch: "ddcutil", level: Level::Optional, applies: always },
    Dep { name: "tesseract", probe: Probe::Cmd("tesseract"), what: "Copy text from the screen", arch: "tesseract tesseract-data-eng", level: Level::Optional, applies: always },
    Dep { name: "wtype", probe: Probe::Cmd("wtype"), what: "typing an emoji from the emoji picker", arch: "wtype", level: Level::Optional, applies: always },
    Dep { name: "plocate", probe: Probe::Cmd("plocate"), what: "finding files in Search", arch: "plocate", level: Level::Optional, applies: always },
    Dep { name: "checkupdates", probe: Probe::Cmd("checkupdates"), what: "counting package updates", arch: "pacman-contrib", level: Level::Optional, applies: is_arch },
    Dep {
        name: "obexd",
        probe: Probe::AnyPath(&["/usr/lib/bluetooth/obexd", "/usr/libexec/bluetooth/obexd"]),
        what: "receiving files over Bluetooth",
        arch: "bluez-obex",
        level: Level::Optional,
        applies: has_bluetooth,
    },
    Dep { name: "python3", probe: Probe::Cmd("python3"), what: "the keybinds viewer and depth wallpapers", arch: "python", level: Level::Optional, applies: always },
];

/// Whether `cmd` is an executable file in PATH.
pub fn in_path(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into());
    std::env::split_paths(&path).any(|d| std::fs::metadata(d.join(cmd)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

impl Dep {
    pub fn present(&self) -> bool {
        match self.probe {
            Probe::Cmd(c) => in_path(c),
            Probe::AnyPath(paths) => paths.iter().any(|p| Path::new(p).exists()),
        }
    }
}

/// Dependencies that matter here and are missing, most important first.
pub fn missing() -> Vec<&'static Dep> {
    let mut v: Vec<&Dep> = DEPS.iter().filter(|d| (d.applies)() && !d.present()).collect();
    v.sort_by_key(|d| d.level);
    v
}

/// The pacman command that installs these (AUR packages left out), if any.
pub fn pacman_command(deps: &[&Dep]) -> Option<String> {
    let pkgs: Vec<&str> = deps.iter().filter(|d| !d.arch.contains("AUR")).flat_map(|d| d.arch.split_whitespace()).collect();
    (!pkgs.is_empty()).then(|| format!("sudo pacman -S --needed {}", pkgs.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_described() {
        let mut names: Vec<&str> = DEPS.iter().map(|d| d.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), DEPS.len());
        assert!(DEPS.iter().all(|d| !d.what.is_empty() && !d.arch.is_empty()));
    }

    #[test]
    fn sh_is_in_path() {
        assert!(in_path("sh"));
        assert!(!in_path("prism-no-such-program"));
    }

    #[test]
    fn pacman_command_skips_aur() {
        let m = DEPS.iter().find(|d| d.name == "matugen").unwrap();
        let t = DEPS.iter().find(|d| d.name == "tesseract").unwrap();
        assert_eq!(pacman_command(&[m, t]).unwrap(), "sudo pacman -S --needed tesseract tesseract-data-eng");
        assert_eq!(pacman_command(&[m]), None);
    }
}
