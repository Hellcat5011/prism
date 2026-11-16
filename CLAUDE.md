# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Prism is a Wayland desktop shell written in Rust, ported from a Quickshell/QML shell. The QML original lives at
`/mnt/hdd/config/quickshell/prism (bak)` and is the reference for look and behaviour (sizes, colours, timings).
The primary target compositor is MangoWM (layer-shell); Niri and Hyprland snippets exist in `deploy/`.
`implementation`, `implementation plan2` and `to-do` were written by an earlier agent and are not reliable; use
them only as a feature inventory. `README.md` describes an older multi-process design and is out of date.

## Commands

```
cargo build --release                       # whole workspace
cargo test --release --workspace            # all tests
cargo test --release -p prism-ui settings   # one crate / tests matching a name
cargo clippy --release --workspace

# Install for live testing (the user runs these binaries from ~/.local/bin)
install -m755 target/release/{prismd,prism-ui,prism-ctl,prism-lock} ~/.local/bin/
systemctl --user restart prismd             # restarts prismd and the prism-ui daemon
journalctl --user -u prismd -f              # logs of prismd and prism-ui
```

`prismd` runs as the systemd user unit `prismd.service` (`deploy/prismd.service`); never also start it from the
compositor config. Apps are launched through `prism_common::apps::app_command` (systemd-run scope in app.slice) so
restarting prismd never kills them. `prism-ui <mode>` (launcher, search, settings, wallpaper, notif-center, ...) runs a single overlay
standalone for debugging; `PRISM_PERF=1` logs per-frame draw times.

## Architecture

Workspace crates (`crates/`):

* `prism-common`: shared library. `Config` (`~/.config/prism/config.json`, serde with per-field defaults), `Theme`
  (matugen palette, `~/.config/prism/colors.json` with fallback to the QML path), IPC types (`IpcCommand` for
  prism-ctl to prismd, `UiCommand` for prismd to prism-ui), and helpers that shell out (wpctl, brightnessctl/ddcutil,
  playerctl, matugen; nmcli/bluetoothctl only for prism-ctl's network/bluetooth commands).

* `prismd`: background daemon. IPC socket `$XDG_RUNTIME_DIR/prism.sock`, idle monitor, gamma/blue light, clipboard
  watchers, notification store, and the notification server (`notifd.rs`: org.freedesktop.Notifications via
  zbus; actions, urgency, image hints; signals back to apps). The old Python bridge is gone (killed if running).
  It supervises one resident `prism-ui daemon` and forwards UI requests to it via `send_ui()` over
  `$XDG_RUNTIME_DIR/prism-ui.sock`; it never spawns per-window processes.

* `prism-ctl`: CLI that sends `IpcCommand`s to prismd (used by compositor keybinds).

* `prism-ui`: all UI. Raw FFI to cairo/pango/gdk-pixbuf (`cairo_backend.rs`), wayland-client, xkbcommon, zbus.

* `prism-lock`: lockscreen (ext-session-lock) and greetd greeter (`--greeter`, run under `cage -s --`), QML
  Lockscreen.qml look. PAM runs on a worker thread with one service (`/etc/pam.d/prism`, else `login`); lock
  surfaces are created right after `lock()`. prismd supervises it: a lock client that exits non-zero is
  restarted so the locked session always has a prompt (exit 3 = compositor cannot lock). Test locking only in
  a nested Mango (`mango -c <empty conf>`, then `WAYLAND_DISPLAY=wayland-1 prism-lock`), never on the live
  session. `prism-lock --check-auth` checks PAM from a terminal. A nested Mango rewrites `WAYLAND_DISPLAY` in the systemd user environment and,
  when it exits, stops `mango-session.target` (and with it prismd). Afterwards run
  `systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP && systemctl --user start mango-session.target prismd`.

### prism-ui process model (`main.rs`)

One process, one Wayland connection. `Globals` binds compositor globals once. Each overlay (card, toast, OSD) is
an `Overlay`: its own `EventQueue<UiState>` + `UiState`, created by `Overlay::open` and destroyed explicitly by
`teardown()` (destroy every protocol object; the process keeps running). `run()` is a single poll loop over the
Wayland fd, a `Waker` pipe (worker threads wake the loop) and the desktop's inotify fd; `handle_command` applies
`UiCommand`s (toggle/open/close modal, OSD, notifications, edit mode). EAGAIN on read is not a disconnect.

`UiState` owns the surfaces, transitions and input plumbing; every screen is a self-contained view.
The modal cards implement `view::CardView` (`launcher.rs`, `spotlight.rs`, `power.rs`, `clipboard.rs`,
`emoji.rs`, `keybinds.rs`) and are held as `UiState::view`; `settings.rs` (`SettingsView`, rows bound to
`Config` via fn getters/setters, live apply + undo), `wallpaper.rs` and `nc/` (notification center;
`nc/model.rs` holds the network/Bluetooth state, `nc/dbus.rs` fills it from NetworkManager and BlueZ over the
system bus, with a pairing agent and signal-driven refreshes) have their own fields. `launcher.rs` also has
the shared `Query` text input. Add new screens as views, not as `UiMode` match arms.

Not overlays: `osd.rs` (UiState view whose layer surface slides by margin), `toasts.rs` (notification popups:
own event queue, one surface per popup), `caffeinate.rs` (idle inhibitor), `svg.rs` (SVG path icons, the QML
NcIcon set).

### Rendering and motion

* Modal cards use a card-sized layer surface plus a separate full-screen `prism-scrim` surface (1x1 single-pixel
  buffer scaled by wp_viewporter) for dim and click-outside. Compositor blur covers the whole layer surface on
  Mango, so never draw cards on full-screen blurred surfaces.

* Drawing is paced by `wl_surface.frame` callbacks: `render_and_commit` only schedules; `draw_frame` draws.
  Animations use wall-clock time, never per-frame steps (`ui::Transition`, `lerp_factor`).

* Card content renders into an offscreen cache and is composited with the enter/exit `Pose`; transition-only frames
  must not re-render content (`content_dirty` vs `animating`).

* `ui.rs` `Style` is the single source of colours, radius, opacity, font, blur namespace (`-solid` twin when blur is
  off), animation on/off/speed and dim. Cards use inversePrimary @ background_opacity and onPrimaryContainer text,
  as in QML. Font specs starting with "Inter, Sans" are rewritten to the global font.

* No I/O on the render path: system state comes from `status.rs` (worker thread, coalesced requests); images go
  through the decoded-surface cache (`cached_image`, `cached_image_cover`); icons through `icons.rs` (gsettings
  icon theme, inherits chain, live updates).

* `uikit.rs` has the shared controls (switch, slider, text field, button, searchable `Dropdown`, `font_families`).

### Desktop widgets (`desktop/`)

`Desktop` runs inside the daemon loop; each widget (clock, analog clock, calendar, media, tray, shortcuts) is its own
Bottom-layer surface with its own buffers and frame callbacks. `layout.rs` ports QML `DesktopLayout` geometry
(fractions of the screen; legacy px / negative-from-edge values still resolve). `widgets.rs` draws in local
coordinates and returns hit regions (shared with edit mode). `dbus.rs` is one worker thread for MPRIS and the
system tray (acts as `org.kde.StatusNotifierWatcher`, dbusmenu context menus). `edit.rs` is edit mode; saving writes
`config.json` and the desktop reloads it through inotify.

## Environment notes

* Mango layer rules live in the user's `~/.config/mango/user/wrules.conf` (template in `deploy/prism-mango.conf`).
  Every matching `layer_rule` overwrites all options, `layer_name` is an unanchored regex, and options need explicit
  values (`no_blur:1`). Prism namespaces: prism-modal, prism-notification, prism-osd (blurred, with `-solid`
  twins), prism-scrim, prism-desktop, prism-desktop-edit, prism-wallpaper (not blurred).

* Matugen writes the palette via `[templates.prism-rust]` in `~/.config/matugen/config.toml`
  (template copy: `deploy/matugen/prism-colors.json`).

* `wtype` may be used to test input only while a Prism overlay has keyboard focus; check it is still open before
  sending more keys, otherwise keystrokes land in the user's terminal.

## Token Management and Tool Guidelines

To conserve context tokens and prevent context overflow, adhere to the following rules when using tools:

* **Limit Bash Output:** Never output raw, large logs or full file contents into the terminal. Always use small, filtered wrappers. Pipe your outputs through filters like `grep`, `head -50`, or `tail -50` so you consume summaries rather than terminal noise.

### File Reading and Output Limits
* **Avoid large file reads:** If a file exceeds 500 lines, you MUST use the `offset` and `limit` parameters to read it in sequential chunks. Do not read the entire file into context at once.
* **Tool enforcement:** A PreToolUse hook is active in this environment. If you attempt to use the `Read` tool on large files without specifying limits, or if you run raw `grep`/`rg` commands without piping through `head -50` or `tail`, the system will reject your command. If this happens, immediately retry your command with the appropriate limits applied.