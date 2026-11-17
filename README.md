# Prism 💎

> A native Rust Wayland desktop shell for **MangoWM**, with config snippets for **Niri** and **Hyprland**.

Prism is a desktop shell (launcher, search, notification center, desktop widgets, lock screen, greeter and more)
written in Rust. It started as a Quickshell/QML shell and was ported to Rust to cut GPU and CPU use: it draws with
cairo/pango straight into Wayland layer surfaces, runs one resident UI process and sits at 0% CPU when idle.

> [!WARNING]
> **Vibe Coded Project:** This is a fully vibe-coded project. If you are uncomfortable with this, please feel free to ignore this repository.

---

## ✨ Features

- **App launcher and Spotlight search**: fuzzy app search plus files and folders via `plocate`.
- **Notification center**: notification history grouped by app, quick toggles, Wi-Fi and Bluetooth pages
  (NetworkManager and BlueZ over D-Bus, with pairing), volume and brightness sliders, audio output switching.
- **Notification server**: `org.freedesktop.Notifications` built into `prismd` (actions, urgency, images,
  do-not-disturb); popups can sit in any corner or edge.
- **Desktop widgets**: clock, analog clock, calendar, media player, system tray and shortcuts on the bottom layer,
  arranged in an on-screen edit mode. Widgets can be "sent to the background" so the wallpaper's foreground
  (a neural depth mask) passes in front of them.
- **Lock screen and greeter**: `ext-session-lock-v1` lock with PAM, and the same look as a greetd greeter under `cage`.
- **Theming**: Material You palette from [matugen](https://github.com/InioX/matugen), applied live everywhere.
- **Blue light filter**: colour temperature through `wlr-gamma-control`, on a schedule or sunrise/sunset.
- **Idle handling**: screens dim 30 s before auto-lock; caffeinate (idle inhibitor) from the notification center.
- **Clipboard history**: text and images, stored with `0600` permissions; entries from password managers and
  password-like text are never saved.
- **Colour picker**: frozen-screen magnifier; copies the hex value to the clipboard.
- **Emoji picker, wallpaper selector, power menu, keybind viewer** (parsed from the MangoWM, Niri or Hyprland config)
  and a **settings** window with live apply and undo.
- **On-screen display** for volume and brightness (`wpctl`, `brightnessctl` or DDC/CI via `ddcutil`).

---

## 📸 Screenshots

### Desktop with Widgets & Wallpaper Depth
![Desktop with Widgets & Wallpaper Depth](assets/screenshots/desktop-with-widgets.png)

### Spotlight Search
![Spotlight Search](assets/screenshots/spotlight-search.png)

### App Launcher
![App Launcher](assets/screenshots/app-launcher.png)

### Wallpaper Selector
![Wallpaper Selector](assets/screenshots/wallpaper-selector.png)

### Notification Center
![Notification Center](assets/screenshots/notification-center.png)

### Clipboard Manager
![Clipboard Manager](assets/screenshots/clipboard.png)

### Keybindings Cheatsheet
![Keybindings Cheatsheet](assets/screenshots/keybinds.png)

### Lock Screen
![Lock Screen](assets/screenshots/lockscreen.png)

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
```

| Crate | Role |
|---|---|
| `prismd` | Background daemon (systemd user unit). IPC socket, notification server, idle monitor and dimming, blue light (gamma), clipboard watchers, depth mask worker, lock supervision. Starts and supervises one resident `prism-ui daemon`. |
| `prism-ui` | All UI in one process: modal cards (launcher, search, settings, ...), notification popups, OSD, colour picker and desktop widgets. Draws with cairo/pango, paced by `wl_surface.frame` callbacks. |
| `prism-ctl` | Command-line client; compositor keybinds call it. |
| `prism-lock` | Lock screen (`ext-session-lock-v1`) and greetd greeter (`--greeter`, under `cage`). |
| `prism-common` | Shared library: config, theme, IPC types, app/file search, clipboard store, hardware helpers. |

Apps launched from Prism run in their own systemd scope (`app.slice`), so restarting `prismd` never closes them.

---

## 📦 Building & Installation

### Requirements

- **Rust** 1.77 or newer.
- **Libraries**: wayland-client, xkbcommon, cairo, pango/pangocairo, gdk-pixbuf, GTK 3 (`gdk`), PAM.
- **Runtime tools** (each feature needs only its own):

| Tool | Used for |
|---|---|
| `wl-clipboard` (`wl-copy`, `wl-paste`) | Clipboard history, colour picker, emoji copy |
| `wpctl` / `pactl`, `playerctl` | Volume, audio outputs, media keys |
| `brightnessctl` or `ddcutil` | Brightness (laptop backlight or external monitor) |
| `matugen` | Palette from the wallpaper |
| `awww` or `swww` | Wallpaper daemon |
| `plocate` | File search (`pkexec updatedb` to rebuild the index) |
| `grim` | Colour picker |
| `wtype` | Pasting emoji into the focused app |
| `NetworkManager`, `BlueZ` | Network and Bluetooth pages |
| `python3` | Keybind parsers; wallpaper depth (installs its own venv) |
| `greetd`, `cage` | Greeter (optional) |

### Build and install

```bash
./deploy/install.sh
systemctl --user enable --now prismd
```

The installer builds the workspace and installs:
- the binaries to `~/.local/bin/`;
- helper scripts to `~/.local/share/prism/scripts/`;
- `prismd.service` to `~/.config/systemd/user/`.

`prismd` starts `prism-ui` itself. Do not also start `prismd` from the compositor config.

### Compositor

Include the snippet for your compositor. It sets up the keybinds and, for MangoWM, the layer rules (blur only behind cards):

| Compositor | Add to config |
|---|---|
| MangoWM (`~/.config/mango/config.conf`) | `source=/path/to/prism/deploy/prism-mango.conf` |
| Niri (`~/.config/niri/config.kdl`) | `include "/path/to/prism/deploy/prism-niri.kdl"` |
| Hyprland (`~/.config/hypr/hyprland.conf`) | `source = /path/to/prism/deploy/prism-hyprland.conf` |

### Theme

Add the template in `deploy/matugen/prism-colors.json` to `~/.config/matugen/config.toml` as
`[templates.prism-rust]`, writing to `~/.config/prism/colors.json`.

### Lock screen and greeter (optional, needs root)

```bash
# PAM service for the lock screen (otherwise "login" is used)
sudo install -Dm644 deploy/pam.d/prism /etc/pam.d/prism

# Check your password works before relying on the lock screen
prism-lock --check-auth

# Greeter: greetd runs prism-lock under cage
sudo install -m755 target/release/prism-lock /usr/local/bin/prism-lock
sudo cp /etc/greetd/config.toml /etc/greetd/config.toml.bak
sudo install -m644 deploy/greetd/config.toml /etc/greetd/config.toml
```

Test the greeter from a TTY (`sudo systemctl restart greetd`) before rebooting. The greeter reads the wallpaper,
palette and settings that Prism copies into `/var/lib/greetd/quickshell-greeter` (a directory your user must be able
to write to, e.g. through a shared group).

---

## ⚙️ Configuration

- `~/.config/prism/config.json`: every setting; edited by the Settings window (`prism-ctl settings toggle`) and
  reloaded live when the file changes.
- `~/.config/prism/colors.json`: matugen palette.
- `~/.cache/prism/`: clipboard history, notifications, current wallpaper (private, `0700`).

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
| `prism-ctl power toggle` | Power menu |
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
| `prism-ctl ping` | Check that `prismd` responds |

---

## 📄 License

GPL-3.0 License.
