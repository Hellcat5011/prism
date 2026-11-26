<p align="center">
  <picture>
    <source media="(prefers-color-scheme: light)" srcset="assets/logo/prism-lockup-light.png">
    <img src="assets/logo/prism-lockup.png" alt="Prism" width="420">
  </picture>
</p>

# Prism

> A native Rust Wayland desktop shell for **MangoWM**, with config snippets for **Niri** and **Hyprland**.

Prism is a desktop shell (launcher, search, notification center, desktop widgets, lock screen, greeter and more)
written in Rust. It started as a Quickshell/QML shell and was ported to Rust to cut GPU and CPU use: it draws with
cairo/pango straight into Wayland layer surfaces, runs one resident UI process and sits at 0% CPU when idle.

> [!WARNING]
> **Vibe Coded Project:** This is a fully vibe-coded project. If you are uncomfortable with this, please feel free to ignore this repository.

---

## ✨ Features

- **App launcher and Spotlight search**: fuzzy app search plus files and folders via `plocate`. The launcher comes
  in four designs: **Classic**, **Slices** (the wallpaper cut into one strip per app), **Spectrum** (a mosaic sorted
  by icon colour) and **Deck** (a floating hand of cards). Apps without an icon get their initials on their own colour.
- **Notification center**: notification history grouped by app, quick toggles, Wi-Fi and Bluetooth pages
  (NetworkManager and BlueZ over D-Bus, with pairing), volume and brightness sliders, audio output switching.
- **Notification server**: `org.freedesktop.Notifications` built into `prismd` (actions, urgency, images,
  do-not-disturb); popups can sit in any corner or edge. Screenshot notifications show a large preview.
- **Screenshots and screen recording**: a frozen-screen overlay to capture a region (move and resize handles), a
  window (searchable list) or the whole screen. Screenshots are saved as PNG and copied to the clipboard, with
  Open, Show in folder and Delete in the notification. Recordings use FFmpeg (NVENC, else x264) into MKV or MP4,
  with optional desktop audio and microphone (PipeWire); a small indicator shows the time and a Stop button.
- **Phone sync**: pairs with the official [KDE Connect](https://kdeconnect.kde.org/) Android app (no
  `kdeconnectd` needed). Phone battery and status, its media with controls and album art, the PC's players on the
  phone (multimedia control), clipboard sync both ways, phone notifications (reply right in the popup, dismiss),
  find my phone, files both ways, and the phone as a remote mouse and keyboard. Shown in a Phone card, a desktop widget and a Settings tab.
- **Desktop portal**: screenshots, colour picking and screen sharing for apps (browsers, OBS, Discord) through
  `xdg-desktop-portal`, with a picker for the screen or window to share. Replaces `xdg-desktop-portal-wlr`.
- **System prompts**: polkit and NetworkManager password prompts, and gnome-keyring's unlock prompts (replaces
  `gcr-prompter`), as Prism cards.
- **Removable drives**: USB drives and SD cards are mounted when plugged in and announced with Open and Eject.
- **Updates**: a notification and a bar module when package updates are available (`checkupdates`, `yay`/`paru`).
- **Game mode**: one toggle (notification center, `prism-ctl gamemode`) for silent notifications, the performance
  power profile and no animations or blur.
- **Quick answers**: calculator, unit conversion, `> command` and web search in the launcher and search.
- **Per-app volume** in the Control Centre's output panel, and a **privacy indicator** while the microphone or
  camera is in use or the screen is shared.
- **Window switcher** (Alt+Tab) with live thumbnails; Settings checks the compositor's own Alt+Tab binding and
  says what to change.
- **Copy text** from the screen (OCR with tesseract) in the screenshot overlay.
- **Download manager**: downloads over several connections (added on the fly, fewer when a server limits them),
  pause and resume even across restarts, a queue and a speed limit (`prism-ctl download`, Settings > Downloads).
  A pill at the top of the screen shows the real progress in percent; a failed download says why, with Retry
  in its notification. The **Downloads card** (`prism-ctl download toggle`, or click the pill) lists Prism's
  downloads with their controls. See [Firefox extension](#firefox-extension-prism-downloads).
- **Prism Downloads for Firefox** (`extension/`): hands browser downloads to Prism (a card confirms the name and
  folder first), adds "Download with Prism" to the link menu and lists downloads in its popup. It finds the videos a
  page plays (HLS streams with their qualities, whole video files) and puts a "Download video" button over them,
  like Brisk; streams are saved as MP4, with the headers the page used so stream hosts do not refuse them. The
  switch in its popup turns the whole extension off. It talks to Prism through native messaging
  (`prism-native-host`), not an open port.
- **First-time setup guide**: a short tour on first start with the main options (look, wallpaper, desktop, lock,
  notifications, night light, capture, downloads, phone), ending with any programs Prism uses that are not
  installed yet and the command to install them (`prism-ctl setup` runs it again).
- **Timer and stopwatch** in the Control Centre (`prism-ctl timer`), **weather** and **notes** desktop widgets,
  **low battery warnings** and **silent mode on a schedule**.
- **Motion**: cards open and close on springs (and shrink back to the button they were opened from), rows fade in
  one after another, selection highlights glide between rows, lists scroll smoothly (touchpad scrolling follows
  the fingers and keeps its momentum), and switches and hover states animate. Cards cast a soft drop shadow
  (can be turned off). Animations can be slowed down, sped up or turned off.
- **Video wallpapers**: the built-in wallpaper can loop a video, also on the lock screen and greeter.
- **Bar**: a panel on any screen edge in three styles (full width, floating, islands), with workspaces, clock, system
  monitor, tray, media player and launcher, notification and power buttons. Drag modules between left, center and right
  in Settings.
- **Dock**: pinned and running apps on the bottom, left or right edge. Right-click an app to pin or remove it. It can
  stay up, auto-hide, or hide only while windows are on screen, and hovering the screen edge brings it back.
- **Desktop widgets**: clock, analog clock, calendar, media player, phone, device batteries, an animated cat, system tray and shortcuts on the bottom layer,
  arranged in an on-screen edit mode. Widgets can be "sent to the background" so the wallpaper's foreground
  (a neural depth mask) passes in front of them.
- **System tray**: StatusNotifierItem host with tooltips and dbusmenu context menus (scrollable when long).
- **Lock screen and greeter**: `ext-session-lock-v1` lock with PAM, and a greetd greeter under `cage`. Nine designs:
  **Classic**, **Stencil** (the wallpaper seen through a giant clock), **Sentence** (the time and your
  notifications as a sentence), **Aperture** (a circular window that opens as you type), **Tide** (colour splashes
  that fill the greyscale wallpaper on unlock), **Ink** (ink blots bleeding into a paper-white wallpaper),
  **Fireflies** (a night-time wallpaper lit by a firefly per key; over a video each firefly opens a window onto it), **Halftone** (a dot screen whose dots merge into
  the picture as you type and on unlock and **Rings** (the wallpaper cut into concentric rings that turn as you
  type and line up on unlock). The greeter mirrors the lock
  screen or uses its own design, and clicking the user or session cycles through them.
- **Power menu**: five large designs (**Glide Bar**, **Split Panel**, **Shutters**, **Orbit** and **Floating
  Cards**). Choose which actions appear: Lock, Logout, Suspend, Sleep, Hibernate, Reboot and Shutdown (Sleep and
  Hibernate only where the computer supports them). Logout, Reboot and Shutdown can require a one-second hold.
- **Theming**: Material You palette from [matugen](https://github.com/InioX/matugen), applied live everywhere.
- **Wallpaper filters**: pick Color Pop, Pop Art, Synthwave, Noir or Vivid with small previews when applying a
  wallpaper. The filter shows on the desktop, lock screen and greeter alike, and can be turned off.
- **Blue light filter**: colour temperature through `wlr-gamma-control`, on a schedule or sunrise/sunset.
- **Idle handling**: screens dim 30 s before auto-lock; caffeinate (idle inhibitor) from the notification center.
- **Clipboard history**: text and images, stored with `0600` permissions; entries from password managers and
  password-like text are never saved.
- **Colour picker**: frozen-screen magnifier; copies the hex value to the clipboard.
- **Emoji picker, wallpaper selector, power menu, keybind viewer** (parsed from the MangoWM, Niri or Hyprland config)
  and a **settings** window (searchable, grouped under section headings) with live apply and undo.
- **On-screen display** for volume and brightness (`wpctl`, `brightnessctl` or DDC/CI via `ddcutil`).

---

## 📸 Screenshots

### Desktop with Widgets & Wallpaper Depth
![Desktop with Widgets & Wallpaper Depth](assets/screenshots/desktop-with-widgets.png)

### Spotlight Search
![Spotlight Search](assets/screenshots/spotlight-search.png)

### App Launcher

Choose a design in Settings > Launcher.

<table>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/app-launcher.png" alt="Classic launcher"><br><b>Classic</b></td>
    <td width="50%" align="center"><img src="assets/screenshots/launcher-slices.png" alt="Slices launcher"><br><b>Slices</b></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/launcher-spectrum.png" alt="Spectrum launcher"><br><b>Spectrum</b></td>
    <td width="50%" align="center"><img src="assets/screenshots/launcher-deck.png" alt="Deck launcher"><br><b>Deck</b></td>
  </tr>
</table>

### Wallpaper Selector
![Wallpaper Selector](assets/screenshots/wallpaper-selector.png)

### Notification Center
![Notification Center](assets/screenshots/notification-center.png)

### Clipboard Manager
![Clipboard Manager](assets/screenshots/clipboard.png)

### Keybindings Cheatsheet
![Keybindings Cheatsheet](assets/screenshots/keybinds.png)

### Lock Screen

Choose a design in Settings > Lock Screen. The greeter can mirror it or use its own (Settings > Greeter).

<table>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen.png" alt="Classic lock screen"><br><b>Classic</b></td>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-stencil.png" alt="Stencil lock screen"><br><b>Stencil</b></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-sentence.png" alt="Sentence lock screen"><br><b>Sentence</b></td>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-aperture.png" alt="Aperture lock screen"><br><b>Aperture</b></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-tide.png" alt="Tide lock screen"><br><b>Tide</b></td>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-ink.png" alt="Ink lock screen"><br><b>Ink</b></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-fireflies.png" alt="Fireflies lock screen"><br><b>Fireflies</b></td>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-halftone.png" alt="Halftone lock screen"><br><b>Halftone</b></td>
  </tr>
  <tr>
    <td width="50%" align="center"><img src="assets/screenshots/lockscreen-rings.png" alt="Rings lock screen"><br><b>Rings</b></td>
  </tr>
</table>

---

## 🏛️ Architecture

```
  prism-ctl ──(IPC: $XDG_RUNTIME_DIR/prism.sock)──► prismd ──(IPC: prism-ui.sock)──► prism-ui daemon
  (keybinds)                                        │                                 │
                                                    │ systemd user service            │ one Wayland connection,
                                                    │ notifications (D-Bus)           │ every overlay and the
                                                    │ idle, gamma, depth, clipboard   │ desktop widgets
                                                    ▼                                 ▼
                         prism-lock (session lock / greetd greeter)
                         prism-phoned (phone sync, while enabled)
                         prism-rec (screen recorder, while recording)
```

| Crate | Role |
|---|---|
| `prismd` | Background daemon (systemd user unit). IPC socket, notification server, idle monitor and dimming, blue light (gamma), clipboard watchers, depth mask worker, lock supervision. Starts and supervises one resident `prism-ui daemon`. |
| `prism-ui` | All UI in one process: modal cards (launcher, search, settings, ...), notification popups, OSD, colour picker, capture overlay and desktop widgets. Draws with cairo/pango, paced by `wl_surface.frame` callbacks. |
| `prism-ctl` | Command-line client; compositor keybinds call it. Every command: `man prism-ctl`. |
| `prism-lock` | Lock screen (`ext-session-lock-v1`) and greetd greeter (`--greeter`, under `cage`). |
| `prism-phone` (`prism-phoned`) | Phone sync over the KDE Connect protocol (v8), run by `prismd` while phone sync is on. |
| `prism-capture` | Screen capture core (`ext-image-copy-capture-v1`, screencopy fallback) shared by `prism-ui` and `prism-rec`. |
| `prism-rec` | Screen recorder (FFmpeg, PipeWire audio), run by `prismd` only while recording. |
| `prism-download` | Download engine: segmented HTTP downloads that add connections while they help, with pause and resume. Used by `prismd`'s download manager. |
| `prism-common` | Shared library: config, theme, IPC types, app/file search, clipboard store, hardware helpers. |

Apps launched from Prism run in their own systemd scope (`app.slice`), so restarting `prismd` never closes them.

---

## 📦 Building & Installation

### Requirements

- **Rust** 1.88 or newer.
- **Libraries**: wayland-client, xkbcommon, cairo, pango/pangocairo, gdk-pixbuf, GTK 3 (`gdk`), PAM, FFmpeg
  (libavcodec, libavformat, libavutil, libswscale, libswresample) and PipeWire for the recorder, and libclang at
  build time (bindings for FFmpeg and PipeWire).
- **Runtime programs and services**. Each feature needs only its own. `prism-ctl deps` lists what is missing on
  this computer (the installer runs it at the end and offers to install it, and the setup guide's last page shows
  it). Packages are named as on Arch Linux:

| Program / service | Package | Used for |
|---|---|---|
| **Needed** | | |
| `wpctl` | `wireplumber` | Volume and microphone, the sound OSD |
| `pw-dump` | `pipewire` | Per-app mixer, Sound page, privacy indicator |
| `xdg-open` | `xdg-utils` | Opening files, folders and links |
| `gsettings` | `glib2` | Following the icon theme |
| `fc-list` | `fontconfig` | Font lists in Settings |
| xdg-desktop-portal | `xdg-desktop-portal` | Screen sharing, screenshots for apps, folder pickers |
| **Recommended** | | |
| `matugen` | `matugen-bin` (AUR) | Palette from the wallpaper |
| NetworkManager | `networkmanager` | Wi-Fi, Ethernet and VPN pages |
| BlueZ | `bluez bluez-utils` | Bluetooth page (when the computer has Bluetooth) |
| UPower | `upower` | Battery levels and low battery warnings |
| power-profiles-daemon | `power-profiles-daemon` | Power profiles, game mode |
| UDisks2 | `udisks2` | Mounting and ejecting USB drives |
| Secret Service | `gnome-keyring` | Calendar addresses in the keyring (and app passwords) |
| `pkexec` | `polkit` | Changes that need your password (Prism is the polkit agent) |
| `ffmpeg` | `ffmpeg` | Trimming recordings, GIFs, joining downloaded videos |
| `curl` | `curl` | Weather, online calendars, sunset times |
| `pactl` | `libpulse` | Choosing sound devices and ports |
| `brightnessctl` | `brightnessctl` | Screen brightness (laptops) |
| **Optional** | | |
| `ddcutil` | `ddcutil` | Brightness of external monitors |
| `tesseract` | `tesseract tesseract-data-eng` | Copy text from the screen |
| `wtype` | `wtype` | Typing an emoji from the emoji picker |
| `plocate` | `plocate` | Finding files in Search (`pkexec updatedb` rebuilds the index) |
| `checkupdates` | `pacman-contrib` | Counting package updates |
| obexd | `bluez-obex` | Receiving files over Bluetooth |
| `python3` | `python` | Keybind viewer, wallpaper depth (installs its own venv) |
| `greetd`, `cage` | `greetd cage` | The login screen (greeter; the installer checks them when you set it up) |
| `awww` or `swww` | | Only when chosen instead of the built-in wallpaper daemon |
| KDE Connect app on the phone | | Phone sync (open UDP 1716 and TCP 1714-1764 in the firewall) |

Clipboard (ext-data-control), media control (MPRIS, `prism-ctl media play-pause|next|previous|stop` for media
keys), notifications and wallpaper resizing are built in; wl-clipboard, playerctl, libnotify and ImageMagick
are not needed. Also built in, each switched on in Settings: the wallpaper daemon (Wallpaper tab, replaces
awww/swww), monitor resolution, scale, rotation and adaptive sync (Display tab, replaces wlr-randr), and under
the System and Connections tabs an MPRIS bridge for MPD (mpd-mpris), the polkit agent (polkit-gnome), Wi-Fi/VPN password prompts
(nm-applet; with optional saving of VPN user names and passwords), Bluetooth file receiving (blueman) and
Network / Bluetooth tray icons (VPN, device and send-file menus). The Control Centre has a VPN page (connect,
filter, bulk import of .ovpn / WireGuard files through `nmcli`), Bluetooth devices have a Send button, and its
tiles are chosen in Settings > Notifications. Stop the program a service replaces before turning it on.

There are three ways to install. On Arch Linux and Arch-based systems (CachyOS, EndeavourOS, Manjaro once it
has caught up with Arch's FFmpeg and PipeWire), the pacman package is the simplest: nothing to compile, and
pacman brings in the libraries and keeps track of the files.

### Arch Linux: pacman package

Each [release](https://github.com/Hellcat5011/prism/releases) has `prism-shell-<version>-1-x86_64.pkg.tar.zst`,
built on an up-to-date Arch Linux by `.github/workflows/release.yml` from `deploy/arch/PKGBUILD`.

```bash
VER=0.1.0   # the release you want
curl -LO https://github.com/Hellcat5011/prism/releases/download/v$VER/prism-shell-$VER-1-x86_64.pkg.tar.zst
curl -LO https://github.com/Hellcat5011/prism/releases/download/v$VER/prism-shell-$VER-1-x86_64.pkg.tar.zst.sha256
sha256sum -c prism-shell-$VER-1-x86_64.pkg.tar.zst.sha256
sudo pacman -U prism-shell-$VER-1-x86_64.pkg.tar.zst
prism-setup          # once, as your user (not root)
```

The package puts the programs in `/usr/bin`, the rest in `/usr/share/prism`, and installs the user service,
the screen-sharing portal, the app icon, the Prism Settings launcher entry, the man page, the Pixel launcher
font and the lock screen's PAM service (`/etc/pam.d/prism`) system-wide. Recommended and optional programs
from the table above are its optional dependencies (`pacman -Qi prism-shell` lists them).

`prism-setup` is the installer in package mode: it enables `prismd`, chooses the Prism portal for your desktop,
offers the battery charge limit rule and the login screen (greeter), and checks the run-time programs. It also
finds an earlier manual install in your home folder (which would run instead) and offers to remove it; your
settings stay. Compositor snippets are in `/usr/share/prism/deploy/`.

To update, install the new release's package the same way, then `systemctl --user restart prismd`.

To build the package yourself: copy `deploy/arch/PKGBUILD` and `deploy/arch/prism-shell.install` into an empty
directory and run `makepkg -si` there (it downloads the tagged sources; compiling takes a while, see below).

### Arch Linux: prebuilt archive (no root)

The same binaries for an install in your home folder (`~/.local`), like a source build: each release also has
`prism-<version>-x86_64.tar.gz` and its `.sha256`.

```bash
VER=0.1.0   # the release you want
curl -LO https://github.com/Hellcat5011/prism/releases/download/v$VER/prism-$VER-x86_64.tar.gz
curl -LO https://github.com/Hellcat5011/prism/releases/download/v$VER/prism-$VER-x86_64.tar.gz.sha256
sha256sum -c prism-$VER-x86_64.tar.gz.sha256
tar xzf prism-$VER-x86_64.tar.gz
cd prism-$VER-x86_64
./deploy/install.sh
systemctl --user enable --now prismd
```

To update, do the same with the new release.

The installer sees the prebuilt binaries and installs them without compiling; everything else is as below. When
a library they were built against is missing or of another version, it says which; update the system
(`sudo pacman -Syu`) or build from source. `deploy/package-release.sh` makes the same archive locally.

### Any distribution: build from source

```bash
git clone https://github.com/Hellcat5011/prism.git
cd prism
./deploy/install.sh
systemctl --user enable --now prismd
```

> **The build takes a while, and `prism-ui` longest.** Release builds use link-time optimisation and a single
> code unit per crate (`[profile.release]` in `Cargo.toml`) for smaller, faster binaries. Cargo can sit on
> `prism-ui` for several minutes with one CPU core busy: that is expected, it has not frozen. Later builds
> recompile only what changed. The prebuilt release above skips this.

The installer checks the build dependencies, builds the workspace and installs:
- the binaries (`prismd`, `prism-ui`, `prism-ctl`, `prism-native-host`, `prism-lock`, `prism-phoned`, `prism-rec`,
  `prism-video`) to `~/.local/bin/`;
- helper scripts to `~/.local/share/prism/scripts/`, and Prism's own wallpaper (the logo on #1A1B26) to
  `~/.local/share/prism/wallpapers/prism.png`, shown on first start with the built-in wallpaper engine until you
  pick one;
- the `prism-ctl(1)` man page to `~/.local/share/man/man1/`;
- `prismd.service` to `~/.config/systemd/user/`.

At the end it checks the programs and services Prism uses at run time (the table above), lists what is missing
and offers to install it with `pacman`. The first time Prism starts, the setup guide opens by itself; its last
page lists anything still missing.

Run from a terminal, it asks whether to add `~/.local/bin` to your shell's PATH when it is missing (so `prism-ctl`
works from a terminal and from compositor keybinds), whether to install the lock screen's PAM service, and, on the
first install, whether to set up the login screen (greeter, below). Nothing outside your home directory changes unless you say yes there.
`--greeter`, `--no-greeter` and `--yes` answer without asking; `--uninstall` and `--purge` remove Prism
(below).

Run it again to update (after `git pull`): it restarts `prismd` when it is running (apps you opened keep running) and offers to update
the greeter when it is installed. `prismd` starts `prism-ui` itself. Do not also start `prismd` from the compositor
config.

### Uninstall

Uninstalling stops `prismd` and removes what the installer put in place. It asks before each step that needs
`sudo`, and before deleting your settings and data.

**Installed from source or the prebuilt archive:** from the same folder you installed from,

```bash
./deploy/install.sh --uninstall     # keeps your settings and data unless you say otherwise
./deploy/install.sh --purge         # also deletes them, without asking
```

**Installed with the pacman package:**

```bash
prism-setup --uninstall             # or: prism-setup --purge
```

It removes your user's setup, then asks before removing the package itself (`sudo pacman -Rn prism-shell`).
Other users of the computer run `prism-setup --uninstall` before that. Avoid `pacman -Rns prism-shell`: `-s`
would also remove libraries such as FFmpeg and GTK when no other package depends on them, even if you use
them yourself.

What is removed:
- the programs in `~/.local/bin`, the helper scripts and default wallpaper in `~/.local/share/prism`, the
  `prismd` service, the portal files, the app icon, the launcher entry, the man page and the Pixel launcher
  font (a manual install; pacman removes the package's own);
- the portal choices file the installer wrote (`~/.config/xdg-desktop-portal/<desktop>-portals.conf`, only
  when unchanged) and the Firefox / LibreWolf native messaging files for Prism Downloads;
- with `sudo`, each after asking: `/etc/pam.d/prism`, the battery charge limit udev rule, the keyring lines
  Prism added to `/etc/pam.d/greetd`, and the greeter: your earlier greetd config is put back from its backup,
  then `/usr/local/bin/prism-lock`, `/usr/local/bin/prism-video` and `/var/lib/greetd/prism` are removed. With no
  backup to put back, the greeter stays (you could not log in without it); set another greeter in
  `/etc/greetd/config.toml` or switch display manager, then run the uninstaller again;
- the line the installer added to your shell's PATH, only if you say yes (other programs in `~/.local/bin`
  may need it).

**Packages are your choice.** Prism uses programs that are useful on their own (matugen, ddcutil, tesseract,
NetworkManager...), so the uninstaller never removes one by itself. On Arch-based systems it lists those that
are installed, numbered, and you type the numbers of the ones to remove separated by spaces, `all` for every
one, or just Enter to keep them all:

```
  1) matugen                      colours taken from the wallpaper (recommended)
  ...
 12) ddcutil                      brightness of external monitors (optional)
 13) tesseract tesseract-data-eng Copy text from the screen (optional)
  Numbers to remove, separated by spaces, "all", or Enter to keep them all: 1 13
```

Only those are removed, with `sudo pacman -Rns` (pacman shows the full list and asks once more), and the rest
stay. A chosen package that another installed package still needs is kept and the uninstaller says which
package needs it. Sound (PipeWire, WirePlumber), the desktop portal, xdg-utils, GLib and fontconfig are not
offered: the desktop needs them. Without a terminal, and with `--yes` or `--purge`, no package is removed.

Your settings and data are kept unless you say yes, or pass `--purge`: `~/.config/prism`,
`~/.local/share/prism`, `~/.local/state/prism`, `~/.cache/prism` and the wallpaper copies `~/.wa.jpg` and
`~/.wa-thumb.jpg`.

Left for you: the Prism lines in your compositor config and the matugen template. Calendar addresses stay in
your keyring, and a keyring renamed to "login" keeps that name. Remove the Prism Downloads extension in
Firefox's Add-ons page.

### Compositor

Include the snippet for your compositor. It sets up the keybinds and, for MangoWM, the layer rules (blur only behind cards).
`/path/to/prism` is the folder you installed from, or `/usr/share/prism` with the pacman package:

| Compositor | Add to config |
|---|---|
| MangoWM (`~/.config/mango/config.conf`) | `source=/path/to/prism/deploy/prism-mango.conf` |
| Niri (`~/.config/niri/config.kdl`) | `include "/path/to/prism/deploy/prism-niri.kdl"` |
| Hyprland 0.55+ (`~/.config/hypr/hyprland.lua`) | `require("prism")`, after `ln -s /path/to/prism/deploy/prism-hyprland.lua ~/.config/hypr/prism.lua` |

`prismd` starts with `graphical-session.target`, so start Niri with `niri-session`. Older Hyprland releases may need
UWSM to reach that target.

`prismd.service` also starts `xdg-desktop-autostart.target`, which runs the XDG autostart entries
(`~/.config/autostart`, `/etc/xdg/autostart`) once per session. MangoWM does not run them otherwise. Do not also
launch those apps from the compositor config (`exec-once`, `dex -a`), or they run twice.

### Theme

Add the template in `deploy/matugen/prism-colors.json` to `~/.config/matugen/config.toml` as
`[templates.prism-rust]`, writing to `~/.config/prism/colors.json`.

### Firefox extension (Prism Downloads)

Install **Prism Downloads** from addons.mozilla.org, or build it yourself: `deploy/package-extension.sh` writes
`target/extension/prism-downloads-<version>.xpi` (signing steps in the script), and `about:debugging` > This
Firefox > Load Temporary Add-on (`extension/manifest.json`) loads it until Firefox restarts. Nothing else to set
up: `prismd` registers `prism-native-host` with Firefox and LibreWolf when they are installed
(`~/.mozilla/native-messaging-hosts/prism_downloads.json`). The extension shows "Connected to Prism" in its
settings once Prism runs.

### Lock screen and greeter (optional, needs root)

```bash
# PAM service for the lock screen (otherwise "login" is used)
sudo install -Dm644 deploy/pam.d/prism /etc/pam.d/prism

# Check your password works before relying on the lock screen
prism-lock --check-auth

# Greeter (needs greetd and cage): answer yes when the installer asks, or pass --greeter.
# Through sudo it installs prism-lock to /usr/local/bin, /var/lib/greetd/prism
# and deploy/greetd/config.toml as /etc/greetd/config.toml (the old one backed up)
./deploy/install.sh --greeter
```

Test the greeter from a TTY (`sudo systemctl restart greetd`) before rebooting. The greeter reads the wallpaper,
palette and settings that Prism copies into `/var/lib/greetd/quickshell-greeter` (a directory your user must be able
to write to, e.g. through a shared group).

---

## ⚙️ Configuration

- `~/.config/prism/config.json`: every setting; edited by the Settings window (`prism-ctl settings toggle`) and
  reloaded live when the file changes.
- `~/.config/prism/colors.json`: matugen palette.
- Calendar feeds (Settings > Calendar > Calendars) are kept in your keyring (gnome-keyring or another
  Secret Service), not in `config.json`; your password shows and changes the list. Settings, their
  snapshots and backups are readable only by you.
- `~/.cache/prism/`: clipboard history, notifications, current wallpaper (private, `0700`).
- Screenshots go to `~/Pictures/Screenshots` and recordings to `~/Videos/Recordings` (both set in Settings > Capture).

Logs: `journalctl --user -u prismd -f` (covers `prism-ui` too). `PRISM_PERF=1` logs frame times.

---

## 💻 CLI Reference (`prism-ctl`)

Run `prism-ctl --help` for the full list. The common ones:

| Command | Action |
|---|---|
| `prism-ctl launcher toggle` | App launcher |
| `prism-ctl search toggle` | Spotlight search |
| `prism-ctl notif toggle` | Notification center (`notif silent on\|off` for do-not-disturb) |
| `prism-ctl settings toggle` | Settings |
| `prism-ctl wallpaper toggle` | Wallpaper selector (`wallpaper set <path>` to set one) |
| `prism-ctl clipboard toggle` | Clipboard history |
| `prism-ctl emoji toggle` | Emoji picker |
| `prism-ctl colorpicker toggle` | Colour picker |
| `prism-ctl screenshot toggle` | Capture overlay (`screenshot screen` for the whole screen at once) |
| `prism-ctl screenshot record` | Start or stop recording (`screenshot stop` to stop) |
| `prism-ctl phone toggle` | Phone card (`phone pair`, `phone ring`, `phone send <file>...`) |
| `prism-ctl power toggle` | Power menu (`power suspend`, `sleep`, `hibernate`, `reboot`, `shutdown` run one directly) |
| `prism-ctl keybinds toggle` | Keybind viewer |
| `prism-ctl lock lock` | Lock the session |
| `prism-ctl osd volume +5` / `-5` / `mute` | Volume with OSD |
| `prism-ctl osd brightness +5` / `-5` | Brightness with OSD |
| `prism-ctl media play-pause` | Media control (`next`, `prev`, `stop`) |
| `prism-ctl caffeinate on\|off` | Hold or release the idle inhibitor |
| `prism-ctl bluelight toggle` | Blue light filter on/off |
| `prism-ctl desktop edit` | Desktop widget edit mode |
| `prism-ctl depth generate` | Generate the depth mask for the current wallpaper |
| `prism-ctl index status` | File index state (`index rebuild` to rebuild) |
| `prism-ctl download toggle` | Downloads card (`download add <url>`, `list`, `pause`, `resume`, `cancel <id>`) |
| `prism-ctl setup` | The first-time setup guide |
| `prism-ctl deps` | Programs and services Prism uses that are missing (works without `prismd`) |
| `prism-ctl ping` | Check that `prismd` responds |

---

## 📄 License

Prism is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, either version 3 of
the License, or (at your option) any later version (`GPL-3.0-or-later`). See [LICENSE](LICENSE).

The bundled Pixelify Sans font (`deploy/fonts`) is under the SIL Open Font License
(`deploy/fonts/PixelifySans-OFL.txt`).
