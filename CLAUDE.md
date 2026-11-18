# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Prism is a Wayland desktop shell written in Rust, ported from a Quickshell/QML shell. The QML original lives at
`/mnt/hdd/config/quickshell/prism (bak)` and is the reference for look and behaviour (sizes, colours, timings).
The primary target compositor is MangoWM (layer-shell); Niri and Hyprland snippets exist in `deploy/`.

## Commands

```
cargo build --release                       # whole workspace
cargo test --release --workspace            # all tests
cargo test --release -p prism-ui settings   # one crate / tests matching a name
cargo clippy --release --workspace

# Install for live testing (the user runs these binaries from ~/.local/bin)
install -m755 target/release/{prismd,prism-ui,prism-ctl,prism-lock,prism-phoned,prism-rec} ~/.local/bin/
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
  matugen; nmcli/bluetoothctl only for prism-ctl's network/bluetooth commands). `wlclip.rs` is the clipboard
  (ext-data-control on its own thread and Wayland connection: copy, paste, watch; Prism's copies that must stay
  out of the history offer `application/x-prism-self`), `media.rs` is MPRIS over zbus, `pixbuf.rs` resizes
  images (gdk-pixbuf), `outputs.rs` is wlr-output-management (list outputs, apply `Config::outputs`).

* `prismd`: background daemon. IPC socket `$XDG_RUNTIME_DIR/prism.sock`, idle monitor, gamma/blue light, clipboard
  watchers, notification store, and the notification server (`notifd.rs`: org.freedesktop.Notifications via
  zbus; actions, urgency, image hints; signals back to apps). The old Python bridge is gone (killed if running).
  It supervises one resident `prism-ui daemon` and forwards UI requests to it via `send_ui()` over
  `$XDG_RUNTIME_DIR/prism-ui.sock`; it never spawns per-window processes. Built-in replacements for helper
  daemons, each behind a config switch: `mpd.rs` (MPRIS bridge for MPD, `org.mpris.MediaPlayer2.mpd`), `obex.rs`
  (org.bluez.obex.Agent1: Bluetooth file receiving, asks through a notification), `displays.rs` (applies
  `Config::outputs` at login; a later change reverts after 15 s unless kept).

* `prism-ctl`: CLI that sends `IpcCommand`s to prismd (used by compositor keybinds).

* `prism-ui`: all UI. Raw FFI to cairo/pango/gdk-pixbuf (`cairo_backend.rs`), wayland-client, xkbcommon, zbus.

* `prism-phone` (binary `prism-phoned`): phone sync with the official KDE Connect Android app (KDE Connect
  protocol v8, see `PHONE_SYNC_PLAN.md`). prismd runs it while `phone_enabled` is on and forwards
  `IpcCommand::Phone`; it serves `$XDG_RUNTIME_DIR/prism-phone.sock` (`prism_common::phone`: commands, and a state
  stream after `Subscribe` that prism-ui's `phone_client.rs` reads for the Phone card `phone.rs` and the desktop
  Phone widget). Link: UDP 1716 discovery, TCP 1716-1764, TLS with roles reversed (the TCP initiator is the TLS
  server and re-sends its identity inside TLS), self-signed EC certs pinned at pairing, verification key =
  SHA-256(larger SPKI, smaller SPKI, timestamp)[..8]. Payloads (files, album art, icons) on their own TLS socket
  (ports 1739-1764, listener = TLS server). Remote input goes through `zwlr_virtual_pointer_v1` and
  `zwp_virtual_keyboard_v1` (keymap built on the fly, like wtype); refused while locked unless configured. Phone
  notifications, pair requests and received files are posted over org.freedesktop.Notifications. Local
  `kdeconnectd` works as an interop peer for testing (desktop to desktop), but never leave it running next to
  prism-phoned (both want UDP 1716). `PRISM_PHONE_DEBUG=1` logs every packet.

* `prism-capture`: screen capture core (SCREEN_CAPTURE_PLAN.md) shared by prism-ui and prism-rec: outputs
  (xdg-output geometry) and windows (`ext_foreign_toplevel_list_v1`), one-shot and streaming capture with
  `ext_image_copy_capture_v1` (screencopy fallback for outputs), pixels as cairo ARGB32 at buffer size.
  prism-ui's `capture.rs` is the overlay (freeze, region/window/screen, Screenshot/Record), PNG via cairo,
  clipboard via `prism_common::wlclip`; `rec_indicator.rs` is the recording pill (a
  compact dot when it would be in the picture). The colour picker uses it too (no grim).

* `prism-rec`: recorder, started by prismd (`recording.rs`) only while recording, with a JSON `RecordJob`;
  SIGINT stops it. System FFmpeg via ffmpeg-next (NVENC, else x264), MKV or fragmented MP4 written as it goes,
  variable frame rate with presentation timestamps; optional PipeWire audio (desktop monitor and/or mic, mixed
  on the recording clock, Opus/AAC).

* `prism-lock`: lockscreen (ext-session-lock) and greetd greeter (`--greeter`, run under `cage -s --`), QML
  Lockscreen.qml look. PAM runs on a worker thread with one service (`/etc/pam.d/prism`, else `login`); lock
  surfaces are created right after `lock()`. prismd supervises it: a lock client that exits non-zero is
  restarted so the locked session always has a prompt (exit 3 = compositor cannot lock). Test locking only in
  a nested Mango (`mango -c <empty conf>`, then `WAYLAND_DISPLAY=wayland-1 prism-lock`), never on the live
  session. `prism-lock --check-auth` checks PAM from a terminal. Designs other than Classic are in `themes.rs` (Stencil, Horizon, Sentence, Aperture, Tide; `lockscreen_theme`, and
  `greeter_theme` whose "mirror" is resolved into the greeter snapshot). They draw with cairo on wl_shm, so they run under cage too:
  `PRISM_LOCK_THEME=tide cage -s -- prism-lock --greeter` is a preview (typing `unlock` plays the unlock animation), and
  `PRISM_LOCK_RENDER_DIR=<dir> cargo test --release -p prism-lock every_design -- --nocapture` writes each state as PNG with frame times. A nested Mango rewrites `WAYLAND_DISPLAY` in the systemd user environment and,
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
`nc/model.rs` holds the network/Bluetooth/VPN state, `nc/dbus.rs` fills it from NetworkManager and BlueZ over the
system bus, with a pairing agent and signal-driven refreshes and a cache of connection profiles; tabs are ids
`TAB_*`, the shown tiles come from `nc_tiles`) have their own fields. `launcher.rs` also has
the shared `Query` text input. Add new screens as views, not as `UiMode` match arms.

Agents that need a password (`polkit_agent.rs`: polkit AuthenticationAgent, checks via
`/run/polkit/agent-helper.socket`; `nm_agent.rs`: NetworkManager SecretAgent) queue requests in `prompt.rs`; the run
loop opens them one at a time as `UiMode::Prompt` cards (answers and verdicts over channels, Retry keeps the card
open). `displays.rs` caches the output list for Settings > Display. `tray.rs` serves the Network and Bluetooth
StatusNotifierItems with com.canonical.dbusmenu menus built on open.

Not overlays: `background.rs` (built-in wallpaper daemon: a `prism-background` Background-layer surface per output,
images decoded off-thread at the output's pixel size, spring transitions; between transitions only the shown
buffer is kept), `osd.rs` (UiState view whose layer surface slides by margin), `toasts.rs` (notification popups:
own event queue, one surface per popup; action buttons sized by `ui::fit_widths`; notifications with
`preview` (hint `x-prism-preview`, sent by screenshots) show their image large above the actions, here and in the
NC), `caffeinate.rs` (idle inhibitor), `svg.rs` (SVG path icons, the QML
NcIcon set).

### Rendering and motion

* Modal cards use a card-sized layer surface plus a separate full-screen `prism-scrim` surface (1x1 single-pixel
  buffer scaled by wp_viewporter) for dim and click-outside. Compositor blur covers the whole layer surface on
  Mango, so never draw cards on full-screen blurred surfaces.

* Drawing is paced by `wl_surface.frame` callbacks: `render_and_commit` only schedules; `draw_frame` draws.
  Animations use wall-clock time, never per-frame steps (`ui::Transition`, `lerp_factor`).

* Card content renders into an offscreen cache and is composited with the enter/exit `Pose`; transition-only frames
  must not re-render content (`content_dirty` vs `animating`).

* Motion is springs (`ui.rs`): `Spring`/`SpringValue` (closed form, wall clock, keep velocity when retargeted) under
  card `Transition`s and every in-card animation. Shared primitives: `Scroller` (every scrollable list: eased wheel
  steps and keyboard paging, touchpad 1:1 from `wl_pointer.axis_source` with rubber-banded edges and momentum),
  `Glide` (selection highlights and focus rings that move between rows; text mixes by `ui::coverage`), `Fades`
  (per-key hover levels and switch knobs), `stagger`/`staggered` (row entrance). Anything drawn while still moving
  calls `ui::keep_animating()` so the frame loop keeps re-rendering content (`take_anim_pending`); views need no
  extra `animating()` for these. `uikit` and NC widgets take hover/knob levels (f64), not bools. No fixed-duration
  tweens in new code.

* `ui::card_surface` draws every modal card's background (tint, top sheen, lit hairline border; `card_edge` alone
  for cards that paint over their background). `cb::drop_shadow` is for floating elements inside a card
  (dropdowns, tooltips).

* Card drop shadows (`shadow.rs`, Settings > UI > Window Shadows, `shadows_enabled`): a subsurface of the scrim,
  so it is unblurred (the `^prism-scrim$` layer rule), sits between the dim and the card and takes no input. Drawn
  once per card size at 1/4 resolution with the card cut out; `UiState::update_shadow` moves, scales and fades it
  with the card's `Pose` (subsurface position, viewport, alpha modifier) and the scrim commit applies it. It must
  use the card rectangle as drawn (the NC surface is wider than the card while sliding). Never grow the card
  surface to hold a shadow: Mango would blur the whole enlarged surface.

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
  twins), prism-scrim, prism-desktop, prism-desktop-edit, prism-wallpaper, prism-background (not blurred).

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