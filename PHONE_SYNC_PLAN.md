# Phone Sync for Prism: overview and plan of action

Status (2026-11-17): steps 0 to 7 implemented (crate `crates/prism-phone`, binary `prism-phoned`).
Verified against a local kdeconnectd: discovery and links in both directions, pairing from either side
with matching verification keys, ping, remote input (pointer, Unicode text, special keys),
phone media, clipboard both ways, and file transfer both ways. Not yet verified with a real phone:
battery, network status, notifications with reply, find my phone, album art, scroll direction and presenter scaling. Still open:
step 8 (drag and drop onto the card) and exporting the phone's player as
`org.mpris.MediaPlayer2.prism_phone`. Written 2026-11-17, revised the same day (scope reduced; then
remote input confirmed as the top priority, files go to `~/Downloads`, ufw is active).

Goal: connect an Android phone to Prism over the local network, using the **KDE Connect
protocol** and the **official KDE Connect Android app** (pairing works the way KDE Connect
already does it: discovery on the LAN, a pair request, and a verification key compared on both
devices; no QR code). The look of the Phone card follows the Unison popover
(https://github.com/venu005/unison, reference screenshot
`~/Pictures/Screenshots/screenshot-17-11-26 183201`).

## Scope

In scope:

1. **Phone status:** connected or not, battery level and charging, mobile network type and signal.
2. **Media playing on the phone:** title, artist, app, album art and progress, with
   previous / play-pause / next and seek from the PC.
3. **Clipboard sync** in both directions.
4. **Phone notifications on the PC**, with action buttons, dismiss and **reply**.
5. **Find my phone** (make the phone ring).
6. **File sending** from the "Send Files" tile, in both directions.
7. **Remote input (top priority):** the phone as touchpad and keyboard for the PC (KDE Connect
   "Remote input"), plus the presenter pointer.
8. **A Phone card** opened from a new **Phone button in the desktop Shortcuts widget**.
9. **A desktop Phone widget.**

Not in scope:

* Screen mirroring and PC-to-phone control (no scrcpy, no ADB).
* QR pairing.
* The Unison app or protocol.
* A notification-centre tile.

Deferred:

* The phone controlling the PC's media players.

---

## 1. What KDE Connect gives us

* A desktop daemon (`kdeconnectd`) and an Android app (F-Droid, Google Play), both open source.
  They talk an open protocol (version 8). Prism implements the **desktop side** in Rust; the
  phone runs the unchanged KDE Connect app.
* The features are plugins, and each plugin is a set of JSON packet types. The ones used here:
  * Battery.
  * Connectivity report.
  * The MPRIS receiver, for the phone's media.
  * Clipboard.
  * Notifications, with their request, reply and action packets.
  * Find my phone.
  * Share, for files.
  * Ping.

## 2. The look (from the reference screenshot)

```
┌───────────────────────────────────────────────┐
│  ╭───╮                                        │
│  │ ▯ │  Nothing Phone (3)        🔔  ⧉   ⋯   │  find phone · send clipboard · more
│  │76%│  ● Connected · 76%  · 5G ▂▄▆           │
│  ╰───╯  (battery ring, phone glyph inside)    │
├──────────────────────┬────────────────────────┤
│ ⇪ Send Files         │ 🔔 Notifications        │  tiles
│   or drop here       │   3 new                 │
├──────────────────────┴────────────────────────┤
│ [art] Late Summer Static                      │  phone media
│       Moon Harbor · Spotify                   │
│  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━  │
│  1:24                                   3:31  │
│              ⏮       ⏸       ⏭               │
├───────────────────────────────────────────────┤
│ ⚙                                     ⏻       │  settings · disconnect
└───────────────────────────────────────────────┘
```

* **Header:**
  * A battery ring (accent colour, tweened fill) with a phone glyph and the percentage inside.
  * The phone name in bold, and the status line "● Connected · 76%". The dot is green when
    connected and greyed when not. The network type and signal follow when known.
  * Round icon buttons on the right: find my phone (bell), send clipboard to the phone, and a
    "more" menu (unpair, refresh).
* **Tiles:**
  * "Send Files / or drop here" opens the portal file picker. "Drop here" needs drag and drop
    (step 7).
  * "Notifications / N new" switches the card body to the phone notification list.
* **Media card:** album art with rounded corners, title, "artist · app", a progress bar, elapsed
  and total time, and previous / play-pause / next.
* **Footer:** a gear (opens the Settings Phone tab) and a power button (disconnects or turns
  phone sync off).
* **Notifications page:**
  * A list of phone notifications: app icon, app name, title, text and time.
  * Each has its action buttons and a **Reply** button. Reply opens a text field inline in the
    row, and Enter sends it.
  * Swipe or ✕ dismisses it on the phone too. A back arrow returns to the main page.
* **Not-paired state:** "Pair a Phone".
  * Lists the devices discovered on the LAN, each with a Pair button.
  * Explains how to get KDE Connect (F-Droid and Play, as text) and which firewall ports to open.
* **Pair request** (from either side): shows the verification key big ("Confirm **A1B2C3D4**
  matches your phone") with Accept and Reject. An incoming request also comes up as a toast, so
  the card does not have to be open.
* **Unreachable state:** the header is greyed with "Not connected · last seen 12 min ago", and
  the actions are disabled.

Colours, fonts, radius and opacity come from `ui.rs` `Style` (the matugen palette, the card
background at `background_opacity`), as on every other card, not from Unison's peach theme.
Motion matches the other cards: spring open and close, staggered rows, wall-clock tweens.

---

## 3. Architecture

```
 Android phone (KDE Connect app)
        │  UDP 1716 discovery           TCP 1714-1764 + TLS (packets, payload sockets)
        ▼
 prism-phoned  (new binary, crate crates/prism-phone)
   ├─ link/      discovery, TCP+TLS, identity, pairing, trust store, reconnect
   ├─ plugins/   battery, connectivity, mpris-receiver, clipboard, notifications,
   │             findmyphone, share, ping
   ├─ mpris.rs   zbus: exports the phone's player as org.mpris.MediaPlayer2.prism_phone
   └─ ctl socket $XDG_RUNTIME_DIR/prism-phone.sock (JSON lines: commands + state stream)
        ▲                               ▲
        │ supervised, IPC forward       │ subscribes to the state stream (worker thread)
     prismd ◄── prism-ctl phone …       prism-ui (Phone card, Phone widget, toasts, Settings tab)
```

### Why a separate process

Networking and TLS stay out of prismd and prism-ui, so a crash or a stuck socket cannot take
them down.

* prismd supervises `prism-phoned` like `prism-ui daemon`: it starts it when `phone_enabled` is
  on, restarts it if it exits non-zero, and stops it when the setting is turned off.
* It is not started from the compositor, and not as its own systemd unit.

### Crate layout (`crates/prism-phone`)

* `main.rs`: the `prism-phoned` entry point and a poll loop, using the codebase's style
  (std threads and a waker pipe, no async runtime).
* `identity.rs`: device id, name, type and the certificate and key. These live in
  `~/.local/share/prism/phone/` (`cert.pem`, `key.pem` with mode 0600).
* `trust.rs`: paired devices, one file each in `~/.local/share/prism/phone/trusted/<deviceId>.json`
  (name, type, pinned certificate, last-seen address).
* `discovery.rs`: UDP identity broadcast and listen on port 1716.
* `link.rs`: the TCP and TLS handshake, newline-delimited JSON packets, and reconnect.
* `pairing.rs`: the pair state machine, the 30 s timeout and the verification key.
* `payload.rs`: payload sockets for files, album art and notification icons.
* `plugins/*.rs`: one module per plugin. Each declares its packet types in
  `incomingCapabilities` and `outgoingCapabilities`.
* `mpris.rs`: exports the phone's media session on the session bus.
* `ctl.rs`: the control socket that prismd and prism-ui talk to.

### Shared types (`prism-common/src/phone.rs`)

* `PhoneState`:
  * Devices (paired, discovered, reachable), and pending pair requests with their
    verification key.
  * Battery, network.
  * The media session (with art cached as a file path).
  * The notifications list (with icon paths), clipboard sync status, and transfers.
* `PhoneCommand`:
  * `Pair`, `Unpair`, `Accept`, `Reject`.
  * `Ring`, `SendClipboard`.
  * `Media(action)`, `Seek(ms)`.
  * `NotifAction { id, action }`, `NotifReply { id, text }`, `NotifDismiss(id)`.
  * `SendFiles(paths)`, `CancelTransfer(id)`.
* `IpcCommand::Phone(PhoneAction)` for prism-ctl, and `UiCommand::Toggle("phone")` /
  `Open("phone")` for the card.

### Reuse of what Prism already has

* **Notifications:** phone notifications are also posted into prismd's notification store, as
  "<app> · <phone name>" with their actions. They then appear as Prism toasts and in the
  notification centre:
  * Dismissing one in Prism dismisses it on the phone.
  * A "Reply" action opens the Phone card on that notification with the reply field focused.
  * Pair requests and received files use the same path.
* **MPRIS:** the exported `org.mpris.MediaPlayer2.prism_phone` player means the existing desktop
  media widget and playerctl-driven media keys can control the phone's player for free.
* **Clipboard:** prismd's clipboard watcher sends PC copies to the phone. Text from the phone is
  set with the same mechanism the clipboard card uses, and lands in Prism's clipboard history.
* **File picker:** `prism-ui/src/portal.rs`.
* **Opening received files:** `prism_common::apps::app_command`.

---

## 4. KDE Connect protocol v8: what has to be implemented

Check every detail below against upstream before coding: kdeconnect-kde `core/backends/lan/`
and `plugins/*`, and kdeconnect-android. Packet captures from phase 0 become test fixtures.

### Packet format

Packets are newline-terminated JSON:
`{"id": <ms timestamp>, "type": "kdeconnect.<x>", "body": {...}}`, with optional `"payloadSize"`
and `"payloadTransferInfo": {"port": N}`.

### Identity

`kdeconnect.identity` with body:

* `deviceId`: 32–38 characters of `[a-zA-Z0-9_]`, so use a UUID with underscores.
* `deviceName`, and `deviceType: "desktop"`.
* `protocolVersion: 8`.
* `incomingCapabilities`, `outgoingCapabilities`.
* `tcpPort`.

### Discovery and connection

* Both sides broadcast their identity over UDP to port 1716 and listen on TCP 1714–1764.
* The side that **receives** a UDP identity opens TCP to the sender's `tcpPort` and sends its own
  identity in plain text.
* TLS then starts with roles reversed: the side that opened the TCP connection acts as the
  **TLS server**. Verify this in `LanLinkProvider`.
* In v8 both sides re-send their identity inside TLS, and only that identity is trusted.

### TLS and trust

* Self-signed certificates, with the `deviceId` as the CN.
* Any certificate is accepted while unpaired. Once paired, the certificate must byte-match the
  pinned one, or the connection is refused.

### Pairing (the KDE Connect method, no QR)

* `kdeconnect.pair {pair: true, timestamp}` asks to pair. The answer is `pair: true` (accept) or
  `pair: false` (reject). `pair: false` later means unpair. Requests time out after 30 s.
* Both devices show the same **verification key** (SHA-256 over both certificates' public keys
  and the timestamp, shown as 8 characters). The user compares the two before accepting.
* Either side can start pairing:
  * Prism lists discovered phones and sends the request. The user accepts on the phone.
  * Or the phone lists Prism and sends the request. The user accepts in Prism, from the toast or
    the card.

### Payloads

* The sender listens on a port in 1739–1764 and puts it in `payloadTransferInfo`. The receiver
  connects to it, and TLS runs with the same certificates.
* Check which side takes which TLS role here.

### Plugins

| Feature | Packets | Direction |
|---|---|---|
| Ping (keep-alive and test) | `kdeconnect.ping` | both |
| Battery | `kdeconnect.battery {currentCharge, isCharging, thresholdEvent}`; ask with `kdeconnect.battery.request` if the app still needs it | phone → PC |
| Network status | `kdeconnect.connectivity_report {signalStrengths: {<subId>: {networkType, signalStrength}}}` | phone → PC |
| Phone media | out: `kdeconnect.mpris.request {requestPlayerList}`, `{player, requestNowPlaying}`, `{player, action: Play/Pause/PlayPause/Next/Previous}`, `{player, SetPosition}`, `{player, albumArtUrl}`; in: `kdeconnect.mpris {playerList, supportAlbumArtPayload, player, title, artist, album, isPlaying, pos, length, canPause, canPlay, canGoNext, canGoPrevious, canSeek, albumArtUrl}`, and album art as a payload with `transferringAlbumArt` | PC controls phone |
| Clipboard | `kdeconnect.clipboard {content}` (both), `kdeconnect.clipboard.connect {content, timestamp}` on connect | both |
| Notifications | in: `kdeconnect.notification {id, appName, title, text, ticker, time, isClearable, isCancel, silent, onlyOnce, requestReplyId, actions[], payloadHash}` + icon payload; out: `kdeconnect.notification.request {request: true}` (fetch all), `{cancel: id}` (dismiss), `kdeconnect.notification.reply {requestReplyId, message}`, `kdeconnect.notification.action {key, action}` | phone → PC, with replies back |
| Find my phone | `kdeconnect.findmyphone.request` | PC → phone |
| Files | `kdeconnect.share.request {filename, creationTime, lastModified, numberOfFiles, totalPayloadSize}` + payload; `{text}`; `{url}`; `kdeconnect.share.request.update` | both |

### Phone-side limits to design around

* **Clipboard on Android 10 and later:** apps cannot read the clipboard in the background. PC to
  phone works automatically. Phone to PC only happens when the user taps "Send clipboard" in the
  KDE Connect app (its persistent notification or quick-settings tile).
  * The card and the Settings text say this, so it does not look broken.
* **Notifications:** the user must grant the KDE Connect app notification access on the phone.
  * Until then the phone sends nothing. The notifications page shows a hint instead of an empty
    list.
* **Reply:** only works for notifications whose app offers a reply action (a non-empty
  `requestReplyId`). Other notifications show no Reply button.

---

## 5. Prism integration points

### Shortcuts widget: the new Phone button

* `desktop/widgets.rs`:
  * Add `("phone", <svg phone path>, "Phone")` to `SHORTCUTS`, making it 5 entries.
  * Extend `shortcut_enabled`.
  * Clicking it sends `UiCommand::Toggle("phone")`, the same path as the other shortcuts.
* `config.rs`: add `ShortcutsConfig.phone`. The default is `true`, but the button only draws
  while `phone_enabled` is on.
* Edit mode and the Settings "Shortcuts" options pick up the new entry.

### Phone card (`prism-ui/src/phone/`)

* A new view, following the CLAUDE.md rule: views, not `UiMode` arms.
* The layout is in section 2. State arrives over the `prism-phone.sock` subscription, read on a
  worker thread that wakes the loop. No I/O on the render path.
* Album art and notification icons are files that prism-phoned writes under
  `$XDG_RUNTIME_DIR/prism-phone/`. They load through `cached_image`.
* Text input (the reply field) reuses the `launcher.rs` `Query` input and the `uikit.rs`
  `text_field`.

### Desktop Phone widget (`desktop/`)

* A new widget kind with its own Bottom-layer surface, like the others.
* It shows the battery ring with the name and status, the media title with play-pause and next,
  the notification count, and a find-my-phone button.
* Clicking the body opens the Phone card.
* Its geometry is a `WidgetPosition` (`desktop_layout.phone`). It is movable, resizable and
  removable in edit mode, and appears in the Settings Desktop widget list.
* `vertical` and `transparent_bg` are honoured like on the other widgets.
* When not paired or disconnected, it shows a compact "Phone not connected" state.

### Toasts and notification centre

Through prismd's store:

* Phone notifications, with the app's actions plus Reply. Reply opens the card on the reply field.
* Pair requests (Accept, Reject).
* File received (Open, Show in folder).
* Transfer failed.

### Settings: new "Phone" tab

* Enable phone sync, and this device's name.
* Paired devices, each with Unpair, and a "Pair a new phone" button that opens the card on its
  pair page.
* Clipboard sync on or off.
* Mirror phone notifications on or off, plus "Show phone notifications as popups".
* Download folder (Text with browse), and "Open files when received".
* Phone widget visibility, as in the Widgets section.

### prism-ctl

```
prism-ctl phone toggle|open|close
prism-ctl phone status                  # JSON: devices, battery, network, media
prism-ctl phone pair <id> | unpair <id> | accept | reject
prism-ctl phone ring
prism-ctl phone clipboard               # push the PC clipboard to the phone now
prism-ctl phone media play-pause|next|previous
prism-ctl phone send <file>...
```

---

## 6. Configuration and state

New `Config` fields, each with a per-field serde default:

* `phone_enabled` (false)
* `phone_device_name` (default: the hostname)
* `phone_clipboard_sync` (true)
* `phone_notifications` (true)
* `phone_notification_popups` (true)
* `phone_download_dir` (`~/Downloads`)
* `phone_allow_input` (true), `phone_input_when_locked` (false)
* `phone_open_received` (false)
* `desktop_layout.phone` (`WidgetPosition`, hidden by default)
* `ShortcutsConfig.phone` (true)

**Runtime state** lives in `~/.local/share/prism/phone/`: identity, trusted devices and
last-seen addresses. The address lets Prism reconnect at once instead of waiting for a broadcast.

**Caches** (album art, icons) live in `$XDG_RUNTIME_DIR/prism-phone/`.

**Received files:** partial files are written as `<name>.part` and renamed when complete. Names
are sanitised, and clashes get " (1)", " (2)".

---

## 7. Plan of action

Each step ends with something usable.

### Step 0: spike

* Generate an identity, do UDP discovery and the TCP and TLS handshake with a real phone running
  KDE Connect, and log every packet.
* **Done when:** the phone's app lists "Prism (<hostname>)" as an available device, the TLS roles
  and v8 identity exchange are confirmed against upstream code, and the captured packets are
  saved as fixtures.

### Step 1: link, pairing, status

* Write `crates/prism-phone` (identity, trust, discovery, link, pairing, reconnect) and the
  `prism-phoned` binary supervised by prismd.
* Add the ctl socket, the `prism-common::phone` types, `IpcCommand::Phone`, and the prism-ctl
  `status | pair | unpair | accept | reject`.
* Add the ping, battery and connectivity plugins.
* Build the UI:
  * The Shortcuts Phone button.
  * The Phone card's pair page and header (battery ring, status).
  * The pair-request toast.
  * The Settings Phone tab (enable, name, paired devices).
* **Done when:**
  * Pairing works from either side, with matching keys.
  * It survives prismd restarts, Wi-Fi drops and suspend.
  * Battery and network show live.
  * Unpair works from both sides.
  * Unit tests cover packet encoding, the id rules and the pair state machine.
  * An integration test pairs two in-process peers.

### Step 2: remote input (top priority)

* Mousepad plugin: relative pointer motion, clicks, double click, middle and right click, drag
  (`singlehold` / `singlerelease`), scroll, Unicode text (`key`), special keys (`specialKey`,
  table in section 4), modifiers, `sendAck` echo, and `keyboardstate` on connect.
* Presenter plugin: pointer motion (a spotlight overlay in prism-ui can follow later).
* Injection through `zwlr_virtual_pointer_manager_v1` and `zwp_virtual_keyboard_manager_v1`
  (both advertised by Mango), with a keymap built on the fly for Unicode text.
* Gate: paired devices only, `phone_allow_input` on, refused while locked unless
  `phone_input_when_locked`.
* **Done when:** the KDE Connect "Remote input" screen moves the pointer smoothly, clicks and
  scrolls, and its keyboard types (including non-ASCII) into the focused window.

### Step 3: find my phone and clipboard

* Add the ring button (header and widget) and `prism-ctl phone ring`.
* Clipboard:
  * PC to phone automatically on copy, through prismd's watcher, when sync is on.
  * Phone to PC whenever the app sends it. It lands in the clipboard and in Prism's history.
  * The send-clipboard header button.
* **Done when:** the phone rings from the PC, and copied text goes both ways (phone to PC through
  the app's Send clipboard action), with no echo loops.

### Step 4: phone media

* Add the MPRIS receiver (player list, now playing, actions, seek, album art as a payload).
* Build the card's media section.
* Export `org.mpris.MediaPlayer2.prism_phone`.
* **Done when:**
  * Spotify (or YouTube Music) on the phone shows in the card with art and live progress.
  * The buttons and seek work.
  * The existing desktop media widget shows the phone's player too.

### Step 5: notifications with reply

* Add the notification plugin (fetch all on connect, posted / updated / cancelled, icons as
  payloads).
* Feed phone notifications into prismd's store, with actions and dismiss sync.
* Build the card's notifications page with inline reply.
* Add the Settings toggles.
* **Done when:**
  * A WhatsApp or Messages notification shows as a Prism toast and in the card.
  * Reply from the card arrives in the chat.
  * Action buttons work.
  * Dismissing on either side removes it on the other.

### Step 6: desktop Phone widget

* Build the widget (layout, drawing, hit regions, edit mode, Settings entry).
* **Done when:** it can be placed and resized in edit mode, it updates live, and its buttons work.

### Step 7: file sending

* Phone to PC into the download folder, with a toast offering Open and Show in folder.
* PC to phone from the Send Files tile (portal picker) and `prism-ctl phone send`.
* Show progress in the card while transfers run.
* **Done when:** large files transfer both ways with correct progress, and cancel and full-disk
  errors are handled.

### Step 8: drag and drop onto the card

* Add `wl_data_device` support to prism-ui, so files dropped on the Send Files tile are sent.
* **Done when:** dragging files from a file manager onto the card sends them.

---

## 8. Security

* **Pinning:** TLS everywhere, with each paired device's certificate pinned. An unknown
  certificate claiming a paired `deviceId` is rejected and logged.
* **Pairing:** only with explicit confirmation in Prism (card or toast), with the verification
  key shown. There is no auto-accept.
* **Key storage:** the private key is a 0600 file.
* **Limits:** packet size is capped (1 MiB of JSON per line), and payload sizes are checked
  against `payloadSize`. Unpaired peers may send only identity and pair packets.
* **Clipboard:** content from the phone is never executed or opened automatically.
* **Firewall:** ufw is active on this machine. The kdeconnect package installs a ufw application
  profile, so `sudo ufw allow KDEConnect` opens UDP and TCP 1714–1764. Document this (and the
  firewalld equivalent) in README/deploy.
* **Running KDE Connect alongside:** `kdeconnectd` or GSConnect must **not** run at the same time
  (port 1716 conflict). prism-phoned detects the bound port and says so in the card.

---

## 9. Dependencies (new)

* `rustls` 0.23 (custom verifiers for pinning and accept-any-while-unpaired, client certificates)
  with `rustls-pki-types`, and `rcgen` for the self-signed identity.
* `sha2` (the verification key), and `uuid` (the device id).
* `zbus` and `serde_json` are already in the workspace.

---

## 10. Testing

* **Unit:** packet serde against the phase-0 fixtures; the device-id rules; the verification key;
  file-name sanitising; the notification-store mapping; the clipboard echo suppression.
* **Integration:** two link instances in one test (one acting as the phone) cover pairing,
  reconnect, a ping, a share with payload, and rejection of a wrong certificate.
* **Live:** a real Android phone with KDE Connect on the same Wi-Fi, logged through
  `journalctl --user -u prismd -f`.

---

## 11. Decisions (answered 2026-11-17)

1. File sending: kept.
2. Remote input: the most important feature; step 2, right after the link.
3. Received files go to `~/Downloads`.
4. ufw is active: open the ports with `sudo ufw allow KDEConnect`.

## 12. Local reference

KDE Connect 26.08 is installed on this machine (`kdeconnectd`, `kdeconnect-cli`, plugins in
`/usr/lib/qt6/plugins/kdeconnect/`). Packet types and body field names were checked against the
strings in those plugins. For interop tests, run `kdeconnectd` as a reference peer (desktop to
desktop pairing works), but never leave it running next to prism-phoned (both want UDP 1716).
