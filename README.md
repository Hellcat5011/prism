# Prism 💎

> High-performance, modular native Rust Wayland desktop shell for **MangoWM**, **Niri**, and **Hyprland**.

Prism is a fast, crash-resilient desktop shell environment written entirely in Rust. By utilizing a multi-process architecture communicating over Unix domain sockets, Prism completely eliminates Qt/QML runtime segfaults, memory bloat, and single-point-of-failure cascades while providing hardware-accelerated layer-shell surfaces, quintic ease-out animations, and deep compositor integration.

> [!WARNING]
> **Vibe Coded Project:** This is a fully vibe-coded project. If you are uncomfortable with this, please feel free to ignore this repository.

---

## 🏛️ Architecture

Prism decouples its desktop responsibilities into dedicated Rust binaries communicating over a high-speed Unix domain socket (`/tmp/prism-<UID>.sock`):

```
                            ┌────────────────────────┐
                            │   Wayland Compositor   │
                            │ (MangoWM/Niri/Hyprland)│
                            └───┬────────┬───────┬───┘
                                │        │       │
       ext-session-lock-v1      │        │       │  zwlr-layer-shell-v1
  ┌─────────────────────────────┘        │       └──────────────────────────┐
  ▼                                      ▼                                  ▼
┌──────────────────┐           ┌──────────────────┐               ┌────────────────────┐
│   prism-lock     │           │      prismd      │  Unix Socket  │      prism-ui      │
│  (Rust Binary)   │           │  (Rust Daemon)   │◄═════════════►│  (Layer Surfaces)  │
├──────────────────┤           ├──────────────────┤      IPC      ├────────────────────┤
│ • Zero UI bloat  │           │ • Notification D.│               │ • Launcher & Search│
│ • PAM Auth       │           │ • Idle Monitor   │               │ • Desktop Widgets  │
│ • Never crashes  │           │ • BlueLight/Gamma│               │ • Smooth OSD       │
│ • Wayland shm    │           │ • MPRIS & State  │               │ • Toast Popups     │
└──────────────────┘           └──────────────────┘               └────────────────────┘
                                         ▲
                                         │ IPC
                               ┌─────────┴────────┐
                               │     prism-ctl    │
                               │  (CLI Controller)│
                               └──────────────────┘
```

### Binaries & Crates
- **`crates/prism-common`**: Shared library containing IPC protocol serialization, 19-token Matugen Material You theme palette parsing, FreeDesktop `.desktop` indexing, `plocate` file search scoring, credential-filtered clipboard history, MPRIS media controllers, WirePlumber/brightnessctl hardware bridges, and depth estimation hooks.
- **`crates/prism-ctl`**: Fast command-line controller for keybindings, scripts, and status inspection.
- **`crates/prism-lock`**: Standalone Wayland session lockscreen utilizing `ext-session-lock-v1` and Linux PAM authentication.
- **`crates/prismd`**: Lightweight background daemon maintaining the Unix IPC socket, Wayland `ext-idle-notify-v1` monitor, Tanner Helland gamma blue light filter, and system state stores.
- **`crates/prism-ui`**: Hardware-accelerated Wayland double-buffered layer-shell surface renderer (`zwlr_layer_shell_v1`) supporting OSD, App Launcher, Desktop Widgets (`Layer::Bottom`), toast notification popups, Power menu, and Keybinds cheatsheet overlay with smooth frame-callback animations ($1 - (1 - t)^5$).

---

## ✨ Features

- **Hardware-Smooth Animations**: Frame-callback vsync-synchronized animations with Quintic Ease-Out curves for fluid notifications and OSD overlays.
- **Spotlight & App Launcher**: High-speed unified application and file search indexed via `.desktop` cache and `plocate`.
- **Desktop Widgets & Wallpaper Depth**: Clock, date, system load, and media widgets positioned on the background layer with parallax neural depth mask integration.
- **Rock-Solid Lockscreen**: Zero-dependency session locking via `ext-session-lock-v1` with PAM authentication (`/etc/pam.d/prism` / `system-local-login`).
- **Dynamic Theming**: Instant Material Design 3 / Matugen palette synchronization across 19 color tokens.
- **Smart Blue Light Filter**: Smooth Kelvin color temperature adjustments via Tanner Helland algorithm and Wayland gamma protocols.
- **Clipboard History**: Keyboard-navigable clipboard manager with automatic 24-hour expiration and regex-based credential filtering (API keys, SSH keys, passwords).
- **Unicode Emoji Picker**: 1,900+ Unicode glyph dataset with category browsing and fuzzy search.
- **Hardware Volume & Brightness OSD**: Smooth sliders integrated with `wpctl` and `brightnessctl`.
- **Multi-WM Power Menu & Keybinds**: Instant overlay showing compositor keybinds parsed from MangoWM, Niri, or Hyprland.

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

## 📦 Building & Installation

### Requirements
- **Rust Toolchain**: 1.75+ (`cargo`, `rustc`)
- **System Libraries**:
  - `libwayland-client`
  - `libpam0g-dev` (or `pam-devel`)
  - `libxkbcommon-dev`
- **Optional Tools**:
  - `plocate` (for high-speed file search)
  - `wpctl` (WirePlumber, for audio controls)
  - `brightnessctl` (for backlight controls)
  - `matugen` (for dynamic wallpaper palette generation)

### Build from Source
```bash
# Build release binaries (produces ~4.8MB across all 4 binaries in ~3s)
cargo build --release
```

### Install Binaries & Configs
Run the included deployment installer:
```bash
./deploy/install.sh
```
This installs:
- Binaries (`prism-ctl`, `prism-lock`, `prismd`, `prism-ui`) to `~/.local/bin/`
- Systemd user service `prismd.service` to `~/.config/systemd/user/`

---

## 🚀 Compositor Integration

Include the appropriate configuration file in your compositor configuration:

### MangoWM (`~/.config/mango/config.conf`)
```ini
source = /path/to/prism/deploy/prism-mango.conf
```

### Niri (`~/.config/niri/config.kdl`)
```kdl
include "/path/to/prism/deploy/prism-niri.kdl"
```

### Hyprland (`~/.config/hypr/hyprland.conf`)
```ini
source = /path/to/prism/deploy/prism-hyprland.conf
```

---

## 💻 CLI Reference (`prism-ctl`)

| Target | Command | Description |
|---|---|---|
| **launcher** | `prism-ctl launcher [toggle\|open\|close]` | Toggle or show application launcher |
| **search** | `prism-ctl search [toggle\|query <text>]` | Query desktop apps and indexed files |
| **wallpaper**| `prism-ctl wallpaper set <path>` | Change wallpaper & regenerate Matugen theme |
| **media** | `prism-ctl media [play-pause\|next\|prev]` | Control MPRIS active audio player |
| **theme** | `prism-ctl theme palette` | Dump current 19-token color palette JSON |
| **notif** | `prism-ctl notif [list\|clear\|send <title>]` | Query or post desktop notifications |
| **osd** | `prism-ctl osd volume [+5%\|-5%\|mute]` | Step audio volume and show smooth OSD |
| **osd** | `prism-ctl osd brightness [+5%\|-5%]` | Step backlight and show smooth OSD |
| **clipboard**| `prism-ctl clipboard [toggle\|list]` | Manage secure clipboard history |
| **emoji** | `prism-ctl emoji search <query>` | Search Unicode emojis |
| **colorpicker**| `prism-ctl colorpicker pick` | Sample screen color with loupe |
| **power** | `prism-ctl power [lock\|logout\|reboot]` | Execute system power action |
| **keybinds** | `prism-ctl keybinds [list\|toggle]` | View compositor keybindings |
| **bluelight**| `prism-ctl bluelight [toggle\|manual_toggle]`| Toggle warm gamma night light |
| **depth** | `prism-ctl depth [status\|generate]` | Generate or query wallpaper depth mask |
| **ping** | `prism-ctl ping` | Check `prismd` daemon responsiveness |

---

## 📄 License

GPL-3.0 License.
