# Screen Capture for Prism: screenshots and screen recording

Status (2026-11-17): steps 1-5 and 7 implemented. Verified live on Mango: full-output capture
pixel-identical to grim (30 ms), window capture (20 ms), the overlay with region / window / screen and
Screenshot / Record, PNG + clipboard (ext-data-control) + notification, NVENC recording (1440p60 at about
24 % of one core on the SHM path, 400 MB RSS mostly CUDA), region and window recording, MKV and fragmented
MP4, desktop audio and mic (audio and video end within 2 ms), the recording indicator (a compact dot when
it would be in the picture) and stop. Step 6 (zero-copy dmabuf to NVENC) is deferred: FFmpeg cannot map
DRM PRIME frames to CUDA directly; it needs a DRM PRIME to Vulkan to CUDA chain that is fragile on the
NVIDIA driver, and the SHM path is already cheap enough. Written 2026-11-17.

Goal: built-in screenshots and screen recording that replace grim and slurp (and the old xeon-shell
`ScreenCapture.qml`). They should be as small and efficient as possible and run no external
programs. Recording links the system FFmpeg libraries (libavcodec/libavformat), which are libraries
already on the system, not programs.

## Decisions (2026-11-17)

1. **Encoder:** system libavcodec/libavformat (option A). NVENC on the RTX 3060 (`h264_nvenc`,
   `hevc_nvenc`, `av1_nvenc`), with a fallback to `libx264` (or VAAPI on other GPUs).
2. **Audio:** off by default. It is turned on from the capture overlay itself (desktop audio and/or
   microphone), and the choice is remembered.
3. **Window capture:** picked from a list of open windows (title and app icon). The window itself is
   captured, so it works even when covered.
4. This plan comes before any code.

---

## 1. What the old implementation did (xeon-shell), and what changes

### What it did

* A full-screen overlay with a frozen `ScreencopyView`, a dim mask with a cut-out, a top mode bar
  (Screenshot / Recording) and a bottom bar (Region / Window / Output, fps, folder).
* **Screenshot:** `grabToImage` of the frozen view (or a crop), then `saveToFile`. `wl-copy` copied
  it to the clipboard and `notify-send` sent the notification.
* **Recording:** a timer grabbed the view every 1/fps seconds into numbered JPEG files. On stop, a
  script ran `ffmpeg` over the folder to make an MP4.

### What to keep

The freeze-then-select overlay, the mode bar, region / window / output, and configurable folders.

### What changes

| Old | Problem | New |
|---|---|---|
| `grabToImage` of a Qt item | Captured at item size, so it can be resampled or blurry on scaled outputs | Copy the compositor's buffer at its real pixel size |
| `hyprctl -j clients` for windows | Hyprland only; broken on Mango | `ext_foreign_toplevel_list_v1` + toplevel capture source (any compositor that has it) |
| 150 ms "settle" timers | Guesswork | Wait for the capture protocol's `ready` event |
| Recording via per-frame JPEG files, then ffmpeg | CPU readback, JPEG and disk write for every frame, then a second full encode. Skipped frames were not timestamped, so the length drifted. No audio. | Frames go straight to NVENC with their real timestamps, muxed as they arrive (no temp files, crash-safe), optional audio |
| `mkdir`, `wl-copy`, `notify-send`, `sh` | External processes | `std::fs`, `ext_data_control_v1` in prism-ui, org.freedesktop.Notifications over zbus |

---

## 2. Protocols (all advertised by Mango, checked with `wayland-info`)

* **Capture:** `ext_image_copy_capture_manager_v1`.
  * Sources: `ext_output_image_capture_source_manager_v1` (outputs) and
    `ext_foreign_toplevel_image_capture_source_manager_v1` (windows from
    `ext_foreign_toplevel_list_v1`).
  * Sessions report buffer size and formats, deliver frames on damage with a presentation time, and
    have optional cursor capture.
  * **Fallback:** `zwlr_screencopy_manager_v1` (outputs and regions only) for compositors without
    ext-image-copy-capture. Hyprland and Niri have one or both.
* **Buffers:** `wl_shm` for screenshots (CPU access needed for PNG and the frozen backdrop), and
  `zwp_linux_dmabuf_v1` for recording (see section 5).
* **Outputs:** `zxdg_output_manager_v1` for logical position, size and scale, to map a region drawn
  in logical coordinates onto buffer pixels.
* **Clipboard:** `ext_data_control_manager_v1`. The resident prism-ui daemon offers `image/png` and
  answers paste requests, so no `wl-copy` is needed.

---

## 3. Processes

```
prism-ctl screenshot ... ──► prismd ──► prism-ui daemon (resident)
                                         ├─ capture overlay view (freeze, select, mode bar)
                                         ├─ screenshot: copy frame → crop → PNG (cairo) → file + clipboard
                                         └─ starts recording: prismd spawns prism-rec
                                 prismd ──► prism-rec (only while recording; exits when done)
                                         ├─ own Wayland connection: capture session (dmabuf/shm)
                                         ├─ libavcodec: NVENC video (+ Opus/AAC audio)
                                         ├─ PipeWire: desktop and/or mic audio (only when enabled)
                                         └─ libavformat: MKV/MP4 written as it goes
```

* **Screenshots stay in prism-ui.** It already has the Wayland connection, cairo (PNG writer
  included) and the overlay machinery. Screenshots add no dependencies.
* **Recording is a separate small binary, `prism-rec`.**
  * The FFmpeg and PipeWire libraries are only loaded while recording; the resident UI never maps
    them, so idle cost is zero.
  * prismd supervises it like prism-phoned and prism-lock. On stop it sends SIGINT; prism-rec
    flushes the encoder and closes the file.
  * A crash cannot take the UI down.
* **Recording indicator:** prism-ui shows a small pill (red dot, elapsed time, Stop) on the OSD
  layer while prism-rec runs. prism-rec reports its state on `$XDG_RUNTIME_DIR/prism-rec.sock`.

---

## 4. Screenshots

### Flow

1. `prism-ctl screenshot` (or a keybind) opens the overlay. prism-ui captures every output once into
   SHM buffers before drawing anything, so the overlay never appears in the shot.
2. The overlay shows the frozen frame, dimmed, with the selection cut out. Overlay parts:
   * **Top bar:** Screenshot / Recording.
   * **Bottom bar:** Region / Window / Screen, plus the audio toggles when Recording is chosen.
   * **Region:** drag a rectangle. It can be moved and resized with handles, and shows its size
     ("1280 × 720"). Enter confirms, Escape cancels, a right click resets.
   * **Window:** a list of open windows from `ext_foreign_toplevel_list_v1` (app icon through
     `icons.rs`, title, app id). Picking one captures that window's own buffer, which works even
     when it is covered or on another workspace (where the compositor allows).
   * **Screen:** click the output to take, or Enter for the one under the pointer.
3. **Crop** in buffer pixels (logical rectangle × output scale), with no resampling.
4. **Save** as PNG through `cairo_surface_write_to_png` to the screenshot folder
   (default `~/Pictures/Screenshots`, created when missing). The name is `Screenshot_YYYY-MM-DD_HH-MM-SS.png`.
5. **Copy** to the clipboard as `image/png` (ext-data-control, served by prism-ui), and add it to
   Prism's clipboard history the way the clipboard watcher does.
6. **Notify:** a notification with the thumbnail as its image hint and the actions Open, Show in
   folder and Delete.

### Instant modes, no overlay

* `prism-ctl screenshot screen`: the output under the pointer, immediately.
* `prism-ctl screenshot window`: the focused window, through `ext_foreign_toplevel_list_v1` plus a
  focus check. If focus can't be told, fall back to the picker.
* `prism-ctl screenshot region`: opens straight into region selection.
* `--no-clipboard` and `--no-save` flags.

### Cursor

Off by default for screenshots. A config switch turns it on (the capture session's cursor option).

---

## 5. Recording

### Start

* Choose Recording in the overlay, select the region, window or screen, then press Start (or Enter).
* The overlay closes and prismd starts `prism-rec` with a JSON job: target (output name,
  toplevel identifier or region), fps cap, audio sources, quality, container and folder.

### prism-rec pipeline

1. **Capture session** on the target, preferring **dmabuf** buffers.
   * Frames come only when the screen changes, each with the compositor's presentation time.
   * Frames are capped at the chosen fps (30 or 60). A still screen costs nothing.
2. **Video encode** (libavcodec):
   * **NVENC:** `h264_nvenc` by default (`hevc_nvenc` / `av1_nvenc` selectable), preset p4, VBR,
     quality setting mapped to `cq`. Input is a hardware frame:
     * **Step 1 (simpler):** SHM BGRA frames are uploaded as `AV_PIX_FMT_BGR0`; NVENC converts
       colour on the GPU. It costs one GPU→CPU→GPU copy per frame, which is fine at 1440p60 on this
       machine.
     * **Step 2 (zero copy):** dmabuf frames are imported as `AV_PIX_FMT_DRM_PRIME` and mapped to
       CUDA (`av_hwframe_map`), so the frame never touches the CPU. This is done after step 1
       works.
   * **Fallback** when NVENC is missing: `h264_vaapi` (AMD/Intel), else `libx264` (preset
     veryfast, CRF).
   * **Timestamps:** the presentation time relative to the first frame, in a 1/1000 s (or
     1/90000 s) time base. Variable frame rate is fine for MKV/MP4 and keeps the length exact.
   * **Region:** the region is cropped before encoding (the filter graph or a sub-rectangle copy);
     its size is rounded to even numbers.
3. **Audio (only when turned on):** PipeWire streams.
   * **Desktop audio:** a capture stream on the default sink's monitor.
   * **Microphone:** the default source.
   * When both are on, they are mixed into one track (simple sample add with clipping, or
     `amix`).
   * Encoded with `libopus` in MKV, or `aac` in MP4, using audio timestamps from the same clock.
4. **Mux** with libavformat into **MKV by default**. MKV is playable even if recording is killed, and
   holds any codec.
   * MP4 is an option, written as fragmented MP4 (`movflags=frag_keyframe+empty_moov`), so it is
     also crash-safe.
   * The file is written straight to the folder (default `~/Videos/Recordings`) as
     `Recording_YYYY-MM-DD_HH-MM-SS.mkv`.
5. **Stop** comes from SIGINT, `prism-ctl screenshot stop`, the indicator's Stop button, or the
   target going away (window closed, output unplugged). prism-rec then flushes the encoder, writes
   the trailer and exits.
   * Prism posts a notification with Open and Show in folder.

### Indicator

A small pill on the OSD layer, top-centre by default:

* A pulsing red dot, the elapsed time, a mic/speaker glyph when audio is on, and a Stop button.
* Whether it appears in an output recording depends on the compositor (output capture usually
  includes every layer). Section 11 has the check and the fallback.

---

## 6. UI (prism-ui)

**Capture overlay view** (`capture/` module), built as a view per the CLAUDE.md rule:

* **Surfaces:** one full-screen layer surface per output (Overlay layer, keyboard exclusive), showing
  that output's frozen frame. A selection can span outputs only in Screen mode.
* **Top mode bar:** a pill with Screenshot and Recording.
* **Bottom bar:**
  * Region / Window / Screen.
  * In Recording mode also: fps (30 / 60), and **audio pills "Desktop audio" and "Microphone"
    (both off by default, any combination; the choice is saved)**.
  * Cursor on/off, and a folder button that opens the portal folder picker.
* **Window picker:** a card listing the windows (icon, title, app id) with search-as-you-type,
  like the launcher.
* **Recording indicator:** a small view with its own layer surface, like the OSD.

**Settings, new "Capture" tab:**

* Screenshot folder, copy to clipboard, show cursor, notification.
* Recording folder, container (MKV / MP4), codec (Auto / H.264 / HEVC / AV1), quality
  (Low / Medium / High / Lossless-ish), fps cap, default audio sources.

**Keybind examples** (`deploy/prism-mango.conf`, and the Niri and Hyprland snippets):

```
Print           prism-ctl screenshot          # overlay
Shift+Print     prism-ctl screenshot screen   # instant full output
Alt+Print       prism-ctl screenshot window   # focused window
Super+Shift+R   prism-ctl screenshot record   # overlay in Recording mode; again = stop
```

---

## 7. IPC and configuration

**prism-ctl:**

```
prism-ctl screenshot [toggle|open|close]
prism-ctl screenshot region|window|screen [--no-clipboard] [--no-save]
prism-ctl screenshot record            # overlay in Recording mode, or stop when recording
prism-ctl screenshot stop
prism-ctl screenshot status            # JSON: recording?, elapsed, file
```

`IpcCommand::Capture(CaptureAction)` goes to prismd. prismd forwards UI actions to prism-ui
(`UiCommand::Open("capture")`) and starts or stops prism-rec.

**Config fields** (per-field serde defaults):

* `capture_screenshot_dir` (`~/Pictures/Screenshots`)
* `capture_copy_to_clipboard` (true)
* `capture_cursor` (false)
* `capture_recording_dir` (`~/Videos/Recordings`)
* `capture_container` ("mkv")
* `capture_codec` ("auto")
* `capture_quality` ("medium")
* `capture_fps` (60)
* `capture_audio_desktop` (false), `capture_audio_mic` (false)
* `capture_last_mode` ("region")

---

## 8. Dependencies

* **prism-ui:** none new. It uses the `wayland-protocols` staging features already in the workspace
  (`ext_image_copy_capture`, `ext_foreign_toplevel_list`, `ext_data_control`), plus cairo.
* **prism-rec (new crate):**
  * `ffmpeg-next` (or `ffmpeg-sys-next` plus a thin wrapper) against the system FFmpeg 8 (libav* 63;
    clang is installed for its bindgen step).
  * `pipewire` (libpipewire 1.6), only for audio.
  * Plus the workspace's `wayland-client` and `wayland-protocols`.
  * Size: dynamic linking, so the binary stays small; the libraries are only mapped while
    recording.
* **Runtime:** nothing new for screenshots. Recording needs the FFmpeg libraries (installed), and
  for NVENC the NVIDIA driver (620.70, installed).

---

## 9. Plan of action

### Step 1: capture core in prism-ui

* Bind the ext-image-copy-capture globals (with the screencopy fallback) and xdg-output.
* One-shot capture of an output into SHM, with format handling (XRGB/ARGB 8888, 2101010 → 8 bit).
* `prism-ctl screenshot screen` saves a PNG.
* **Done when:** the PNG matches the screen pixel for pixel at the output's native resolution, with
  no overlay in it; this replaces `grim` for full-screen shots.

### Step 2: overlay with region selection

* Frozen backdrop per output, dim mask with the cut-out, a drag-to-select rectangle with handles and
  a size label, and keyboard control.
* Crop in buffer pixels, then save, copy to the clipboard (ext-data-control in prism-ui) and notify
  with the thumbnail and actions.
* **Done when:** region shots replace `grim -g "$(slurp)"`, the clipboard paste works in a browser
  and an image editor, and nothing is resampled.

### Step 3: windows

* `ext_foreign_toplevel_list_v1` window list with icons and the picker card.
* Capture through a toplevel image capture source, plus `prism-ctl screenshot window`.
* **Done when:** any listed window can be captured, also when covered.

### Step 4: prism-rec, video only (SHM path)

* New crate and prismd supervision (start with a job, SIGINT to stop, status socket).
* Capture session at the fps cap; NVENC H.264 from BGR0 SHM frames with real timestamps; MKV mux;
  libx264 / VAAPI fallback.
* Recording mode in the overlay, the indicator, `prism-ctl screenshot record|stop|status`, and the
  finished notification.
* **Done when:**
  * A 2-minute 1440p60 recording is exactly 2:00 long and plays in mpv and a browser.
  * CPU use stays low with NVENC.
  * Killing prism-rec mid-way still leaves a playable file.

### Step 5: audio

* PipeWire desktop-monitor and mic streams, a simple mixer, and Opus (MKV) or AAC (MP4) on the
  same clock.
* The audio pills in the overlay (off by default, remembered).
* **Done when:** audio stays in sync (under 40 ms drift over 10 minutes) with either or both
  sources on.

### Step 6: zero-copy dmabuf path

* dmabuf capture buffers imported as DRM PRIME and mapped to CUDA frames for NVENC; fall back to
  SHM when import fails.
* **Done when:** CPU use while recording 1440p60 drops clearly below the SHM path, with identical
  output.

### Step 7: settings and polish

* The Settings Capture tab, keybind snippets in `deploy/`, and the cursor option.
* Recording a region that spans a scaled output.
* Docs in CLAUDE.md.

---

## 10. Testing

* **Unit:** crop maths (logical to buffer pixels at scales 1, 1.25, 1.5, 2); file naming; format
  conversion of 2101010 frames; timestamp conversion; audio mixing.
* **Live, screenshots:**
  * Compare against `grim` output for the same output (pixel diff 0).
  * Check that the overlay is never in the shot.
  * Paste into a browser.
* **Live, recording:** length versus wall clock, A/V sync (clap test), CPU and GPU load
  (`nvidia-smi dmon`), playable after `kill -9`, and window capture while the window is covered.
* Never send synthetic input to anything but Prism's own overlays (the CLAUDE.md rule about wtype).

---

## 11. Risks and open points

* **Overlay-layer exclusion:** whether Mango's output capture includes the recording indicator. If
  it does, the indicator goes on a surface outside the captured output, or is hidden during
  recording with Stop kept on a keybind and the tray.
* **Toplevel capture on other workspaces:** depends on the compositor; Mango's behaviour needs a
  test.
* **DRM PRIME to CUDA import on the NVIDIA proprietary driver:** may need `nvidia-drm.modeset=1`
  and matching modifiers; the SHM path stays as the fallback.
* **HDR / 10-bit outputs:** convert to 8-bit for PNG and H.264; HEVC 10-bit later if wanted.
