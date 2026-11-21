//! Notification center / control centre (port of QML NotificationCenter.qml and its Nc* pages).
//!
//!   ┌ Control Centre ─────────── 🔋 ⚙ ┐
//!   │ [Network] [BT] [Notifications] [☕]│  tiles: the selected one expands, the others are icons
//!   │                                  │
//!   │  page: notifications / network / bluetooth / VPN / timer / sound (cross-fade)
//!   │                                  │
//!   │ [sink list ▲] 🔊 ───────●──── ⌃ │  footer
//!   │               ☀ ─────●────────   │
//!   └──────────────────────────────────┘
//!
//! System state comes from the status worker (status.rs, nc/model.rs); this view only reads
//! its snapshots and sends requests. Drawing records hit rectangles that pointer events use.

pub mod dbus;
pub mod model;
mod widgets;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use prism_common::ipc::{IpcCommand, NotifAction, TimerAction};

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
use prism_common::notifications::Notification;
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::launcher::{draw_text_caret, Query};
use crate::desktop::layout::Rect;
use crate::status::{self, StatusHandle, StatusRequest, StatusSnapshot};
use crate::svg;
use crate::ui::{Style, ANIM_MED_MS};
use crate::waker::Waker;
use model::{BtBusy, DetailRow, Net};
use widgets::{alpha, font, text_size, text_v, Pal, Tween, RADIUS};

pub const CARD_W: f64 = 400.0;
/// Height of the "Apps" heading over the per-app volume rows.
const APPS_HEAD: f64 = 26.0;
/// Height of the "Sound settings" row under the output panel.
const MORE_ROW: f64 = 36.0;
/// Fonts of the reply and password fields, and of the VPN filter.
const FIELD_FONT: &str = "Inter, Sans 14px";
const FILTER_FONT: &str = "Inter, Sans 13px";
/// Height of the inline reply field.
const REPLY_H: f64 = 40.0;
/// Notification groups: the open header's height, entries shown before "Show N more", that
/// row's height, the thread's text inset (timeline on its left), entry padding and gap, the
/// edges a closed stack shows under it, and the open/close spring.
const HEAD_OPEN: f64 = 26.0;
const GROUP_LIMIT: usize = 4;
const MORE_H: f64 = 26.0;
const THREAD_X: f64 = 30.0;
const ENTRY_PAD: f64 = 8.0;
const ENTRY_GAP: f64 = 4.0;
const PEEK: f64 = 5.0;
const SPRING_GROUP: (f64, f64) = (0.34, 0.86);

/// The notification a hit belongs to (its row, buttons or reply field).
/// "Snooze" hides a notification for an hour.
const SNOOZE_SECS: u64 = 3600;

fn hit_nid(h: Hit) -> Option<u32> {
    match h {
        Hit::Entry(n) | Hit::EntryAction(n, _) | Hit::EntryCopy(n) | Hit::EntrySnooze(n) | Hit::EntryClose(n) | Hit::ReplyField(n) | Hit::ReplySend(n) => Some(n),
        _ => None,
    }
}

/// Tab / tile ids. Pages: notifications, network, bluetooth, VPN; caffeine is a toggle tile.
pub const TAB_NOTIFS: usize = 0;
pub const TAB_NET: usize = 1;
pub const TAB_BT: usize = 2;
const TILE_CAFFEINE: usize = 3;
pub const TAB_VPN: usize = 4;
const TILE_GAME: usize = 5;
pub const TAB_TIMER: usize = 6;
/// Sound page (pavucontrol's features, `audio.rs`); also opened from the footer's output panel.
pub const TAB_AUDIO: usize = 7;
const TILES: usize = 8;
/// Tile order on screen (QML order, VPN after Bluetooth) and their config names.
const TILE_ORDER: [(usize, &str); TILES] = [(TAB_NET, "network"), (TAB_BT, "bluetooth"), (TAB_VPN, "vpn"), (TAB_NOTIFS, "notifications"), (TILE_CAFFEINE, "caffeine"), (TILE_GAME, "game"), (TAB_TIMER, "timer"), (TAB_AUDIO, "audio")];
const PAGES: [usize; 6] = [TAB_NOTIFS, TAB_NET, TAB_BT, TAB_VPN, TAB_TIMER, TAB_AUDIO];

/// Tab the next Control Centre opens on (tray icons), e.g. TAB_NET.
static OPEN_TAB: std::sync::Mutex<Option<usize>> = std::sync::Mutex::new(None);

pub fn open_on_tab(tab: usize) {
    *OPEN_TAB.lock().unwrap() = Some(tab);
}

/// Action key of an inline reply (freedesktop notification spec).
pub const INLINE_REPLY: &str = "inline-reply";

/// Notification whose reply field the next Control Centre opens with.
static OPEN_REPLY: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

/// Open the Control Centre on the notifications page with the reply field of `id` (from a
/// popup's Reply button).
pub fn open_reply(id: u32) {
    *OPEN_REPLY.lock().unwrap() = Some(id);
    open_on_tab(TAB_NOTIFS);
    std::thread::spawn(|| {
        if let Err(e) = prism_common::ipc::request(&prism_common::IpcCommand::Notif(prism_common::ipc::NotifAction::Open)) {
            eprintln!("[prism-ui] {e}");
        }
    });
}
/// Card inset from the screen edges (OverlayWindow anchors.margins 20 inside a window with
/// 10 px top/bottom margins, panelHeight = window height - 20).
pub const CARD_MARGIN: f64 = 20.0;
const PAD: f64 = 20.0;

/// Screen rectangle of the card for a `notification_center_position` (left, center, right).
pub fn card_rect(position: &str, sw: f64, sh: f64) -> Rect {
    let w = CARD_W.min(sw - 40.0);
    let h = (sh - 2.0 * CARD_MARGIN).max(400.0);
    let x = match position {
        "center" => ((sw - w) / 2.0).round(),
        "right" => sw - CARD_MARGIN - w,
        _ => CARD_MARGIN,
    };
    Rect { x, y: CARD_MARGIN, w, h }
}
const ANIM_FAST_MS: f64 = 53.0;
const TILE_MS: f64 = 220.0;
/// QML NcNetwork: re-scan every 8 s while the network page is showing.
const WIFI_POLL: Duration = Duration::from_secs(8);
/// Local slider values win over snapshots this long after the last change.
const SLIDER_HOLD: Duration = Duration::from_millis(1500);

const ESC: u32 = 1;
const ENTER: u32 = 28;

pub enum Effect {
    None,
    Redraw,
    Close,
    OpenSettings,
}

/// The picture an entry shows as a large preview (screenshots), if any.
fn entry_preview(n: &Notification) -> Option<&str> {
    n.image.as_deref().filter(|p| wants_preview(n) && std::path::Path::new(p).exists())
}

/// Screenshots stored before the preview flag existed are recognised by their sender.
fn wants_preview(n: &Notification) -> bool {
    n.preview || n.desktop_entry == "prism-capture"
}

/// Action row(s) of an entry whose text column starts at `tx` (width `tw`), from `top`:
/// (copy, close, app action rects, height of the rows). App actions are as wide as their
/// labels need plus a share of the spare room, followed by copy and close (40 px each); if
/// the labels do not fit next to those, the actions get a row of their own. Without app
/// actions, copy and close share the row (QML).
/// Entry buttons: (copy, snooze, close, the app's actions, height of the button rows).
fn entry_actions(cr: *mut Cairo, tx: f64, top: f64, tw: f64, n: &Notification) -> (Rect, Rect, Rect, Vec<Rect>, f64) {
    const H: f64 = 28.0;
    let gap = 6.0;
    let acts: Vec<&str> = n.actions.iter().take(4).map(|(_, l)| l.as_str()).collect();
    if acts.is_empty() {
        let bw = (tw - 2.0 * gap) / 3.0;
        let at = |i: f64| Rect { x: tx + i * (bw + gap), y: top, w: bw, h: H };
        return (at(0.0), at(1.0), at(2.0), Vec::new(), H);
    }
    let small = 40.0;
    let natural: Vec<f64> = acts.iter().map(|l| widgets::button_size(cr, l, "", true).1).collect();
    let beside = tw - 3.0 * (small + gap);
    let (widths, icons_y, rows_h) = if crate::ui::widths_fit(&natural, beside, gap) {
        (crate::ui::fit_widths(&natural, beside, gap), top, H)
    } else {
        (crate::ui::fit_widths(&natural, tw, gap), top + H + gap, 2.0 * H + gap)
    };
    let mut x = tx;
    let rects = widths
        .into_iter()
        .map(|w| {
            let r = Rect { x, y: top, w, h: H };
            x += w + gap;
            r
        })
        .collect();
    let cx0 = tx + tw - 3.0 * small - 2.0 * gap;
    let at = |i: f64| Rect { x: cx0 + i * (small + gap), y: icons_y, w: small, h: H };
    (at(0.0), at(1.0), at(2.0), rects, rows_h)
}

/// Hover level 0..1 of `hit` (fades in and out), for widgets drawn with a hover state.
macro_rules! hv {
    ($v:expr, $hit:expr) => {{
        let hit = $hit;
        let on = $v.hover == Some(hit);
        let ts = $v.time_scale;
        $v.hover_fade.level_scaled(hit, on, ts)
    }};
}

mod audio;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Hit {
    Gear,
    /// Cycles the power-profiles-daemon profile.
    Profile,
    Tile(usize),
    Silent,
    ClearAll,
    GroupHead(usize),
    GroupClear(usize),
    /// Timer page: start a preset (seconds), add seconds, pause / resume, cancel; stopwatch.
    TimerPreset(u32),
    TimerAdd(i32),
    TimerPause,
    TimerCancel,
    SwToggle,
    SwReset,
    /// "Show N more" under a long group.
    GroupMore(usize),
    /// Fade key of an entry's actions (never a hit target).
    EntryReveal(u32),
    EntryCopy(u32),
    EntryClose(u32),
    /// Snooze the entry for an hour.
    EntrySnooze(u32),
    /// (notification id, action index)
    EntryAction(u32, usize),
    ReplyField(u32),
    ReplySend(u32),
    Entry(u32),
    Volume,
    Brightness,
    SinkToggle,
    Sink(usize),
    /// Per-app volume slider and mute button (index into `mixer`).
    AppVolume(usize),
    AppMute(usize),
    /// Footer output panel: open the Sound page.
    AudioPage,
    /// Sound page: section tab, make default, volume / balance slider (node id), mute, open a
    /// dropdown, an option of the open dropdown.
    AudioSec(usize),
    AudioDefault(u32),
    AudioVol(u32),
    AudioBal(u32),
    AudioMute(u32),
    AudioPick(audio::Pick),
    AudioChoice(usize),
    EthHeader,
    Detail(usize),
    WifiSwitch,
    WifiRefresh,
    NetRow(usize),
    NetForget(usize),
    NetConnect(usize),
    PwField,
    PwJoin,
    BtSwitch,
    Discoverable,
    BtRefresh,
    BtForget(usize),
    BtAction(usize),
    BtSend(usize),
    VpnRow(usize),
    VpnImport,
    VpnFilter,
    /// The search field of the notifications page.
    NotifFilter,
    /// Focus: the button that opens its options, the two switches, an app of the allow-list.
    Focus,
    FocusShare,
    FocusFull,
    FocusApp(usize),
    /// Animation key of a Focus switch's knob (0 screen share, 1 fullscreen).
    FocusKnob(u8),
    Unnamed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Volume,
    AppVolume(usize),
    /// A Sound page slider (Hit::AudioVol / Hit::AudioBal).
    Audio(Hit),
    Brightness,
    /// Swiping a notification entry: (nid, start x).
    Entry(u32, f64),
}

struct Group<'a> {
    key: String,
    items: Vec<&'a Notification>,
}

pub struct NcView {
    status: StatusHandle,
    snap: StatusSnapshot,
    tab: usize,
    page: [Tween; TILES],
    tile_w: [Tween; TILES],
    /// Shown tiles (ids), from `nc_tiles`.
    tiles: Vec<usize>,
    /// VPN list filter (typed while the VPN page is open).
    vpn_filter: Query,
    /// Search text of the notifications page (app, summary or body).
    notif_filter: Query,
    /// The Focus options replace the notification list.
    focus_open: bool,
    /// Focus settings while the options are open: (on screen share, on fullscreen, allowed apps).
    focus_cfg: (bool, bool, Vec<String>),
    /// Apps shown as chips (index = `Hit::FocusApp`).
    focus_apps: Vec<String>,
    hits: Vec<(Hit, Rect)>,
    hover: Option<Hit>,
    /// Hover fades of the hit targets (see `hv!`).
    hover_fade: crate::ui::Fades<Hit>,
    pointer: (f64, f64),
    pressed: Option<Hit>,
    drag: Option<Drag>,
    swipe_dx: f64,
    scroll: [crate::ui::Scroller; TILES],
    /// Animation time scale from the last draw (None: animations off).
    time_scale: Option<f64>,
    expanded: Option<String>,
    /// Group expansion springs (0 collapsed, 1 open).
    expand: HashMap<String, crate::ui::SpringValue>,
    /// Groups showing all their entries (past GROUP_LIMIT).
    show_all: std::collections::HashSet<String>,
    /// Entries sliding out after a dismiss: nid -> start.
    dismissing: Vec<(u32, Instant)>,
    /// Entries in `dismissing` that are snoozed, not dismissed.
    snoozing: Vec<u32>,
    sink_open: bool,
    sink_t: Tween,
    eth_open: bool,
    eth_t: Tween,
    expanded_ssid: Option<String>,
    /// Network list frozen while a row is expanded (QML freezeList).
    frozen: Option<Vec<Net>>,
    pw: Query,
    show_unnamed: bool,
    vol_local: Option<(u32, Instant)>,
    bri_local: Option<(u32, Instant)>,
    last_wifi_scan: Option<Instant>,
    knobs: [Tween; 3],
    detail_values: Vec<String>,
    spin_start: Instant,
    waker: Waker,
    /// After a notification action: tells when the app opened or raised a window.
    action_watch: Option<std::sync::mpsc::Receiver<()>>,
    close: bool,
    /// Apps playing sound, as last drawn: (name, icon, stream ids, volume, muted).
    mixer: Vec<(String, String, Vec<u32>, f64, bool)>,
    /// PipeWire state generation last drawn.
    pw_seen: u64,
    /// The second the timer was last drawn at (it redraws once a second while counting).
    timer_sec: i64,
    /// A dragged app volume, shown until PipeWire reports it back.
    app_local: Option<(String, f64, Instant)>,
    /// Inline reply being typed: notification id and text.
    reply: Option<(u32, Query)>,
    /// Text fields as drawn: hit, text x and width, font (clicks place the caret).
    text_geo: Vec<(Hit, f64, f64, &'static str)>,
    /// Sound page: section, its pill spring, the open dropdown and its options as drawn, a
    /// dragged slider value, and the PipeWire state as drawn.
    audio_sec: usize,
    audio_seg: Tween,
    audio_pick: Option<audio::Pick>,
    audio_choices: Vec<audio::Choice>,
    audio_local: Option<(Hit, f64, Instant)>,
    audio: crate::pw_watch::Audio,
    streams: Vec<crate::pw_watch::Stream>,
}

impl NcView {
    pub fn new(waker: Waker) -> Self {
        let status = status::spawn(waker.clone());
        crate::pw_watch::start(waker.clone());
        let names = prism_common::Config::load().nc_tiles;
        // Shown tiles in the configured order (Settings lets the user drag them).
        let mut tiles: Vec<usize> = Vec::new();
        for id in names.iter().filter_map(|n| TILE_ORDER.iter().find(|(_, t)| t == n).map(|(id, _)| *id)) {
            if !tiles.contains(&id) {
                tiles.push(id);
            }
        }
        let pages: Vec<usize> = tiles.iter().copied().filter(|t| PAGES.contains(t)).collect();
        let wanted = OPEN_TAB.lock().unwrap().take();
        let tab = wanted.filter(|t| PAGES.contains(t)).or(pages.contains(&TAB_NOTIFS).then_some(TAB_NOTIFS)).or(pages.first().copied()).unwrap_or(TAB_NOTIFS);
        let page: [Tween; TILES] = std::array::from_fn(|i| Tween::new(if i == tab { 1.0 } else { 0.0 }));
        let tile_w: [Tween; TILES] = std::array::from_fn(|i| Tween::new(if i == tab && tiles.contains(&i) { f64::NAN } else if tiles.contains(&i) { 44.0 } else { 0.0 }));
        NcView {
            waker,
            action_watch: None,
            close: false,
            mixer: Vec::new(),
            pw_seen: 0,
            timer_sec: 0,
            app_local: None,
            reply: OPEN_REPLY.lock().unwrap().take().map(|id| (id, Query::default())),
            text_geo: Vec::new(),
            audio_sec: audio::SEC_OUTPUT,
            audio_seg: Tween::new(audio::SEC_OUTPUT as f64),
            audio_pick: None,
            audio_choices: Vec::new(),
            audio_local: None,
            audio: Default::default(),
            streams: Vec::new(),
            status,
            snap: StatusSnapshot::default(),
            tab,
            page,
            tile_w,
            tiles,
            vpn_filter: Query::default(),
            notif_filter: Query::default(),
            focus_open: false,
            focus_cfg: (false, false, Vec::new()),
            focus_apps: Vec::new(),
            hits: Vec::new(),
            hover: None,
            hover_fade: crate::ui::Fades::default(),
            pointer: (-1.0, -1.0),
            pressed: None,
            drag: None,
            swipe_dx: 0.0,
            scroll: Default::default(),
            time_scale: Some(1.0),
            expanded: None,
            expand: HashMap::new(),
            show_all: Default::default(),
            dismissing: Vec::new(),
            snoozing: Vec::new(),
            sink_open: false,
            sink_t: Tween::new(0.0),
            eth_open: true,
            eth_t: Tween::new(1.0),
            expanded_ssid: None,
            frozen: None,
            pw: Query::default(),
            show_unnamed: false,
            vol_local: None,
            bri_local: None,
            last_wifi_scan: None,
            knobs: [Tween::new(0.0), Tween::new(0.0), Tween::new(0.0)],
            detail_values: Vec::new(),
            spin_start: Instant::now(),
        }
    }

    fn send(&self, r: StatusRequest) {
        let _ = self.status.tx.send(r);
    }

    fn daemon(&self, c: IpcCommand) {
        self.send(StatusRequest::Daemon(c));
    }

    /// Take new snapshots and run timers. Returns true when a redraw is needed.
    /// The centre should close (an action opened a window).
    pub fn wants_close(&self) -> bool {
        self.close
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if self.snap.flags.timer.ticking() {
            let sec = unix_ms() / 1000;
            if sec != self.timer_sec {
                self.timer_sec = sec;
                changed = true;
            }
        }
        // Apps started or stopped playing: the open output panel shows them.
        // Level meters move while a shown device makes sound.
        changed |= self.meters_shown() && crate::meters::moving();
        let pw = crate::pw_watch::generation();
        if pw != self.pw_seen {
            self.pw_seen = pw;
            changed |= self.sink_open || self.tab == TAB_AUDIO;
        }
        if let Some(rx) = &self.action_watch {
            match rx.try_recv() {
                Ok(()) => {
                    self.action_watch = None;
                    self.close = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.action_watch = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(s) = self.status.rx.try_iter().last() {
            self.snap = s;
            changed = true;
            // The notification being answered went away (withdrawn by the app).
            if self.reply.as_ref().is_some_and(|(id, _)| !self.snap.notifications.iter().any(|n| n.id == *id)) {
                self.reply = None;
            }
        }
        let now = Instant::now();
        // Finished slide-outs: remove from the store.
        let done: Vec<u32> = self.dismissing.iter().filter(|(_, t)| now.duration_since(*t).as_millis() >= 390).map(|(n, _)| *n).collect();
        if !done.is_empty() {
            self.dismissing.retain(|(n, _)| !done.contains(n));
            for nid in done {
                self.snap.notifications.retain(|n| n.id != nid);
                if let Some(i) = self.snoozing.iter().position(|n| *n == nid) {
                    self.snoozing.remove(i);
                    self.daemon(IpcCommand::Notif(NotifAction::Snooze { id: nid, secs: SNOOZE_SECS }));
                } else {
                    self.daemon(IpcCommand::Notif(NotifAction::Dismiss(nid)));
                }
            }
            changed = true;
        }
        if self.tab == 1 && self.snap.net.wifi_enabled && self.snap.net.wifi_present() {
            let due = self.last_wifi_scan.map(|t| now.duration_since(t) >= WIFI_POLL).unwrap_or(true);
            if due && self.snap.loaded {
                self.last_wifi_scan = Some(now);
                self.send(StatusRequest::ScanWifi);
            }
        }
        changed
    }

    /// An animation is running (another frame is wanted).
    pub fn animating(&self) -> bool {
        self.page.iter().chain(self.tile_w.iter()).chain(self.knobs.iter()).any(|t| t.running())
            || self.sink_t.running()
            || self.audio_seg.running()
            || self.eth_t.running()
            || !self.dismissing.is_empty()
            || (self.tab == 1 && self.snap.net.scanning)
            || (self.tab == 2 && self.snap.bt.discovering)
    }

    fn set_tab(&mut self, tab: usize) {
        if tab == self.tab {
            return;
        }
        self.tab = tab;
        for (i, p) in self.page.iter_mut().enumerate() {
            p.set(if i == tab { 1.0 } else { 0.0 }, ANIM_FAST_MS);
        }
        if tab != 1 {
            self.expanded_ssid = None;
            self.frozen = None;
        }
        if tab == 1 {
            self.last_wifi_scan = None;
        }
        if tab == 2 && self.snap.bt.powered {
            self.send(StatusRequest::ScanBluetooth);
        }
    }

    fn groups(&self) -> Vec<Group<'_>> {
        let mut out: Vec<Group> = Vec::new();
        let q = self.notif_filter.text.trim().to_lowercase();
        let matches = |n: &Notification| q.is_empty() || [&n.app_name, &n.summary, &n.body].iter().any(|t| plain(t).to_lowercase().contains(&q));
        for n in self.snap.notifications.iter().filter(|n| matches(n)) {
            let key = if n.app_name.is_empty() { "unknown".to_string() } else { n.app_name.to_lowercase() };
            match out.iter_mut().find(|g| g.key == key) {
                Some(g) => g.items.push(n),
                None => out.push(Group { key, items: vec![n] }),
            }
        }
        out
    }

    fn volume(&self) -> u32 {
        match self.vol_local {
            Some((v, t)) if t.elapsed() < SLIDER_HOLD => v,
            _ => self.snap.volume,
        }
    }

    fn brightness(&self) -> u32 {
        match self.bri_local {
            Some((v, t)) if t.elapsed() < SLIDER_HOLD => v,
            _ => self.snap.brightness.unwrap_or(50),
        }
    }

    fn networks(&self) -> Vec<Net> {
        self.frozen.clone().unwrap_or_else(|| self.snap.net.networks.clone())
    }

    // ── Drawing ──────────────────────────────────────────────────────────

    pub fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        self.time_scale = style.time_scale();
        self.hits.clear();
        self.text_geo.clear();
        self.detail_values.clear();
        let p = Pal {
            fg: style.fg,
            inv: cb::hex_to_rgba(&theme.inverse_primary, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            primary: cb::hex_to_rgba(&theme.primary, 1.0),
            surface_variant: cb::hex_to_rgba(&theme.surface_variant, 1.0),
            on_surface: cb::hex_to_rgba(&theme.on_surface, 1.0),
            on_surface_variant: cb::hex_to_rgba(&theme.on_surface_variant, 1.0),
        };
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);

        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;

        // Header.
        text_v(cr, x, y, 32.0, "Control Centre", &font(18.0, "Bold"), p.fg, Some(w - 160.0));
        let gear = Rect { x: x + w - 32.0, y, w: 32.0, h: 32.0 };
        let gh = self.hover == Some(Hit::Gear);
        if gh {
            cb::fill_circle(cr, gear.x + 16.0, gear.y + 16.0, 16.0, p.fg);
        }
        cb::stroke_circle(cr, gear.x + 16.0, gear.y + 16.0, 15.5, p.fg, 1.0);
        svg::icon(cr, "gear", gear.x + 7.0, gear.y + 7.0, 18.0, if gh { p.inv } else { p.fg }, false);
        self.hits.push((Hit::Gear, gear));
        let mut right = gear.x - 8.0;
        if let Some((active, _)) = &self.snap.power_profile {
            let r = Rect { x: right - 32.0, y, w: 32.0, h: 32.0 };
            let ph = self.hover == Some(Hit::Profile);
            if ph {
                cb::fill_circle(cr, r.x + 16.0, r.y + 16.0, 16.0, p.fg);
            }
            cb::stroke_circle(cr, r.x + 16.0, r.y + 16.0, 15.5, p.fg, 1.0);
            let icon = match active.as_str() {
                "power-saver" => "power-saver",
                "performance" => "performance",
                _ => "balanced",
            };
            svg::icon(cr, icon, r.x + 8.0, r.y + 8.0, 16.0, if ph { p.inv } else { p.fg }, false);
            self.hits.push((Hit::Profile, r));
            right = r.x - 8.0;
        }
        if let Some(b) = &self.snap.battery {
            widgets::battery(cr, right, y, b.percent, b.charging, &p);
        }
        y += 32.0 + 16.0;

        // Tiles. Sections enter one after another (ui::staggered): tiles, groups, footer.
        crate::ui::staggered(cr, 0, Rect { x, y, w, h: 44.0 }, || self.draw_tiles(cr, x, y, w, &p));
        y += 44.0 + 16.0;

        // Footer (bottom-up so the page gets the remaining height).
        let sink_h = self.sink_list_height() * self.sink_t.value();
        let footer_h = sink_h + 108.0;
        let footer = Rect { x, y: card.y + card.h - PAD - footer_h, w, h: footer_h };
        let page = Rect { x, y, w, h: (footer.y - 16.0 - y).max(0.0) };

        for tab in PAGES {
            let a = self.page[tab].value();
            if a <= 0.001 {
                continue;
            }
            let interactive = tab == self.tab;
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, page.x - 6.0, page.y - 6.0, page.w + 12.0, page.h + 12.0);
                cb::cairo_clip(cr);
                cb::cairo_push_group(cr);
            }
            let hits_before = self.hits.len();
            match tab {
                TAB_NOTIFS => self.draw_notifications(cr, page, &p),
                TAB_NET => self.draw_network(cr, page, &p),
                TAB_VPN => self.draw_vpn(cr, page, &p),
                TAB_TIMER => self.draw_timer(cr, page, &p),
                TAB_AUDIO => self.draw_audio(cr, page, &p),
                _ => self.draw_bluetooth(cr, page, &p),
            }
            if !interactive {
                self.hits.truncate(hits_before);
            }
            unsafe {
                cb::cairo_pop_group_to_source(cr);
                cb::cairo_paint_with_alpha(cr, a);
                cb::cairo_restore(cr);
            }
        }

        crate::ui::staggered(cr, 4, footer, || self.draw_footer(cr, footer, sink_h, &p));
    }

    fn draw_tiles(&mut self, cr: *mut Cairo, x: f64, y: f64, w: f64, p: &Pal) {
        let gap = 8.0;
        let n = self.tiles.len().max(1) as f64;
        let has_wide = self.tiles.contains(&self.tab);
        // One tile (the open page) is wide; the rest are 44 px icons. Hidden tiles shrink to 0.
        let wide = if has_wide { (w - (n - 1.0) * (44.0 + gap)).max(44.0) } else { 44.0 };
        for i in 0..TILES {
            let target = if !self.tiles.contains(&i) {
                0.0
            } else if i == self.tab && i != TILE_CAFFEINE && i != TILE_GAME {
                wide
            } else {
                44.0
            };
            if self.tile_w[i].target().is_nan() {
                self.tile_w[i] = Tween::new(target);
            }
            self.tile_w[i].set(target, TILE_MS);
        }
        let vpn_on = self.snap.net.vpns.iter().filter(|v| v.state == model::VpnState::On).count();
        let vpn_title = match self.snap.net.vpns.iter().find(|v| v.state != model::VpnState::Off) {
            Some(v) => v.name.clone(),
            None => "VPN".into(),
        };
        let vpn_sub = if vpn_on > 0 { "Connected".to_string() } else if self.snap.net.vpns.iter().any(|v| v.state == model::VpnState::Connecting) { "Connecting\u{2026}".into() } else { "Off".into() };
        let net = &self.snap.net;
        let bt = &self.snap.bt;
        let bt_dev = bt.devices.iter().find(|d| d.connected);
        let flags = self.snap.flags;
        let now = unix_ms();
        let timer_sub = match (flags.timer.remaining(now), flags.timer.sw_start_ms) {
            (Some(left), _) if flags.timer.end_ms.is_some() => prism_common::ipc::clock_text(left),
            (Some(left), _) => format!("Paused {}", prism_common::ipc::clock_text(left)),
            (None, Some(_)) => format!("Stopwatch {}", prism_common::ipc::clock_text(flags.timer.stopwatch(now))),
            _ => "Off".into(),
        };
        let count = self.snap.notifications.len();
        let sound = crate::pw_watch::audio();
        let out = sound.sinks.iter().find(|d| d.is_default);
        let sound_muted = out.is_some_and(|d| d.muted);
        let sound_sub = out.map(|d| d.description.clone()).unwrap_or_else(|| "No output".into());
        let net_title = net.status_text();
        let net_sub = net.status_detail();
        let bt_title = if !bt.adapter {
            "No adapter".to_string()
        } else if !bt.powered {
            "Bluetooth".into()
        } else {
            bt_dev.map(|d| d.name.clone()).unwrap_or_else(|| "Bluetooth".into())
        };
        let bt_sub = if !bt.adapter {
            String::new()
        } else if !bt.powered {
            "Off".into()
        } else if let Some(d) = bt_dev {
            format!("Connected{}", d.battery.map(|b| format!("  ·  {b}%")).unwrap_or_default())
        } else {
            "On".into()
        };
        let count_s = if count > 0 { count.to_string() } else { String::new() };
        let mut specs = [
            widgets::TileSpec {
                icon: if net.ethernet_connected { "ethernet" } else { "wifi" },
                title: &net_title,
                subtitle: &net_sub,
                count: "",
                selected: self.tab == TAB_NET,
                on: false,
                dimmed: !net.ethernet_connected && (!net.wifi_present() || !net.wifi_enabled),
                slashed: !net.ethernet_connected && net.wifi_present() && !net.wifi_enabled,
                badge: 0,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "bluetooth",
                title: &bt_title,
                subtitle: &bt_sub,
                count: "",
                selected: self.tab == TAB_BT,
                on: false,
                dimmed: !bt.powered,
                slashed: false,
                badge: 0,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "bell",
                title: "Notifications",
                subtitle: if flags.silent { "Silent" } else { "" },
                count: &count_s,
                selected: self.tab == TAB_NOTIFS,
                on: false,
                dimmed: false,
                slashed: flags.silent,
                badge: count,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "lock",
                title: &vpn_title,
                subtitle: &vpn_sub,
                count: "",
                selected: self.tab == TAB_VPN,
                on: false,
                dimmed: vpn_on == 0,
                slashed: false,
                badge: 0,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "coffee",
                title: "",
                subtitle: "",
                count: "",
                selected: false,
                on: flags.caffeinate,
                dimmed: !flags.caffeinate,
                slashed: !flags.caffeinate,
                badge: 0,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "gamepad",
                title: "",
                subtitle: "",
                count: "",
                selected: false,
                on: flags.game_mode,
                dimmed: !flags.game_mode,
                slashed: false,
                badge: 0,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "timer",
                title: "Timer",
                subtitle: &timer_sub,
                count: "",
                selected: self.tab == TAB_TIMER,
                on: flags.timer.ticking(),
                dimmed: !flags.timer.ticking() && flags.timer.paused_ms.is_none(),
                slashed: false,
                badge: 0,
                expanded_w: wide,
            },
            widgets::TileSpec {
                icon: "volume",
                title: "Sound",
                subtitle: &sound_sub,
                count: "",
                selected: self.tab == TAB_AUDIO,
                on: false,
                dimmed: sound_muted,
                slashed: sound_muted,
                badge: 0,
                expanded_w: wide,
            },
        ];
        // Specs are listed network, bluetooth, notifications, VPN, caffeine: put them in TILE_ORDER.
        specs.swap(2, 3);
        // Network, bluetooth, VPN, notifications, caffeinate; hidden tiles close up the gap.
        let mut tx = x;
        let mut hits = Vec::new();
        // Shown tiles in their configured order, then hidden ones (shrinking to nothing).
        let order: Vec<usize> = self.tiles.iter().copied().chain(TILE_ORDER.iter().map(|(id, _)| *id).filter(|id| !self.tiles.contains(id))).collect();
        for tab in order {
            let Some(slot) = TILE_ORDER.iter().position(|(id, _)| *id == tab) else { continue };
            let spec = &specs[slot];
            let tw = self.tile_w[tab].value();
            if tw < 0.5 {
                continue;
            }
            let r = Rect { x: tx, y, w: tw, h: 44.0 };
            if tw >= 20.0 {
                widgets::tile(cr, r, spec, hv!(self, Hit::Tile(tab)), p);
                if self.tiles.contains(&tab) {
                    hits.push((Hit::Tile(tab), r));
                }
            }
            tx += tw + gap * (tw / 44.0).min(1.0);
        }
        self.hits.extend(hits);
    }

    fn empty_state(&self, cr: *mut Cairo, r: Rect, icon: &str, slashed: bool, text: &str, p: &Pal) {
        let f = font(14.0, "");
        let (tw, th) = text_size(cr, text, &f);
        let total = 34.0 + 10.0 + th;
        let y = r.y + ((r.h - total) / 2.0).round();
        let c = alpha(p.fg, 0.6);
        svg::icon(cr, icon, r.x + (r.w - 34.0) / 2.0, y, 34.0, c, slashed);
        cb::draw_pango_text(cr, r.x + ((r.w - tw) / 2.0).round(), y + 44.0, text, &f, c, None);
    }

    /// Clip to `r`, run `f` with the scrolled y origin, record the content height, and clamp
    /// the page's scroll offset.
    fn scrolled(&mut self, cr: *mut Cairo, tab: usize, r: Rect, f: impl FnOnce(&mut Self, f64) -> f64) {
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, r.x - 6.0, r.y, r.w + 12.0, r.h);
            cb::cairo_clip(cr);
        }
        let before = self.hits.len();
        let scroll = self.scroll[tab].pos();
        let content_h = f(self, r.y - scroll);
        unsafe { cb::cairo_restore(cr) };
        // Hits outside the viewport are not clickable.
        let mut i = before;
        while i < self.hits.len() {
            let h = self.hits[i].1;
            if h.y + h.h <= r.y || h.y >= r.y + r.h {
                self.hits.remove(i);
            } else {
                self.hits[i].1 = clip_rect(h, r);
                i += 1;
            }
        }
        let ts = self.time_scale;
        self.scroll[tab].set_max_scaled(content_h - r.h, ts);
    }

    // ── Notifications page ───────────────────────────────────────────────

    /// Up to three calendar events starting in the next 24 hours (or going on now); returns
    /// the height used (0 without any).
    fn draw_upcoming(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) -> f64 {
        let now = unix_ms() / 1000;
        let events = crate::cal_events::snapshot();
        let next: Vec<_> = events.iter().filter(|o| o.end > now && o.start < now + 86400 && !(o.all_day && o.start > now + 43200)).take(3).collect();
        if next.is_empty() {
            return 0.0;
        }
        let row_h = 26.0;
        let h = 12.0 + next.len() as f64 * row_h + 10.0;
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, h, RADIUS, alpha(p.fg, 0.08));
        let today = crate::cal_events::local(now).2;
        let mut y = r.y + 10.0;
        let tf = font(12.0, "Medium");
        for o in next {
            let when = if o.all_day {
                "All day".to_string()
            } else if o.start <= now {
                "Now".to_string()
            } else if crate::cal_events::local(o.start).2 != today {
                format!("Tmrw {}", crate::cal_events::clock(o.start))
            } else {
                crate::cal_events::clock(o.start)
            };
            cb::fill_rounded_rect(cr, r.x + 12.0, y + 6.0, 3.0, row_h - 12.0, 1.5, p.primary);
            text_v(cr, r.x + 22.0, y, row_h, &when, &tf, alpha(p.fg, 0.75), Some(80.0));
            let title = if o.location.is_empty() { o.summary.clone() } else { format!("{}  \u{b7}  {}", o.summary, o.location) };
            text_v(cr, r.x + 106.0, y, row_h, &title, &font(13.0, ""), p.fg, Some(r.w - 118.0));
            y += row_h;
        }
        h + 10.0
    }

    /// Focus options: when silent mode turns on by itself, and which apps still pop up.
    fn draw_focus(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        let mut y = r.y;
        cb::draw_pango_text(cr, r.x, y, "Focus", &font(16.0, "Bold"), p.fg, None);
        let sub = "While silent, only critical notifications and the apps below pop up. The rest still arrive here.";
        let (_, sh) = cb::draw_pango_text_wrapped(cr, r.x, y + 22.0, sub, &font(12.0, ""), alpha(p.fg, 0.7), r.w as i32, Some(3));
        y += 22.0 + sh as f64 + 12.0;
        let ts = self.time_scale;
        let (share, full) = (self.focus_cfg.0, self.focus_cfg.1);
        for (hit, label, on) in [(Hit::FocusShare, "Silent while sharing the screen", share), (Hit::FocusFull, "Silent while a window is fullscreen", full)] {
            let row = Rect { x: r.x, y, w: r.w, h: 40.0 };
            let hv = hv!(self, hit);
            cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 12.0, alpha(p.fg, 0.08 + 0.06 * hv));
            text_v(cr, row.x + 12.0, row.y, row.h, label, &font(13.0, ""), p.fg, Some(row.w - 70.0));
            let knob = self.hover_fade.level_scaled(Hit::FocusKnob(if hit == Hit::FocusShare { 0 } else { 1 }), on, ts);
            widgets::switch(cr, row.x + row.w - 54.0, row.y + 8.0, knob, true, p);
            self.hits.push((hit, row));
            y += row.h + 6.0;
        }
        y += 8.0;
        cb::draw_pango_text(cr, r.x, y, "Allowed while silent", &font(12.0, "Bold"), alpha(p.fg, 0.7), None);
        y += 22.0;
        // Chips: the allowed apps, and the apps that sent the notifications shown.
        let mut apps: Vec<String> = self.focus_cfg.2.clone();
        for n in &self.snap.notifications {
            if !n.app_name.is_empty() && !apps.iter().any(|a| a.eq_ignore_ascii_case(&n.app_name)) {
                apps.push(n.app_name.clone());
            }
        }
        if apps.is_empty() {
            cb::draw_pango_text_wrapped(cr, r.x, y, "Apps appear here once they have sent a notification.", &font(12.0, ""), alpha(p.fg, 0.6), r.w as i32, Some(2));
        }
        let f = font(12.0, "Medium");
        let mut x = r.x;
        for (i, app) in apps.iter().enumerate() {
            let on = self.focus_cfg.2.iter().any(|a| a.eq_ignore_ascii_case(app));
            let (tw, _) = text_size(cr, app, &f);
            let w = (tw + 24.0 + if on { 18.0 } else { 0.0 }).min(r.w);
            if x + w > r.x + r.w {
                x = r.x;
                y += 36.0;
            }
            if y + 30.0 > r.y + r.h {
                break;
            }
            let chip = Rect { x, y, w, h: 30.0 };
            let hv = hv!(self, Hit::FocusApp(i));
            cb::fill_rounded_rect(cr, chip.x, chip.y, chip.w, chip.h, 15.0, if on { alpha(p.fg, 0.85 + 0.1 * hv) } else { alpha(p.fg, 0.12 + 0.1 * hv) });
            let c = if on { p.inv } else { p.fg };
            let mut tx = chip.x + 12.0;
            if on {
                svg::icon(cr, "check", tx - 2.0, chip.y + 7.0, 16.0, c, false);
                tx += 18.0;
            }
            text_v(cr, tx, chip.y, chip.h, app, &f, c, Some(chip.w - (tx - chip.x) - 10.0));
            self.hits.push((Hit::FocusApp(i), chip));
            x += w + 8.0;
        }
        self.focus_apps = apps;
    }

    /// A Focus option was clicked: change it and save the config (prismd reloads it).
    fn focus_change(&mut self, h: Hit) {
        match h {
            Hit::FocusShare => self.focus_cfg.0 = !self.focus_cfg.0,
            Hit::FocusFull => self.focus_cfg.1 = !self.focus_cfg.1,
            Hit::FocusApp(i) => {
                let Some(app) = self.focus_apps.get(i).cloned() else { return };
                let list = &mut self.focus_cfg.2;
                match list.iter().position(|a| a.eq_ignore_ascii_case(&app)) {
                    Some(k) => {
                        list.remove(k);
                    }
                    None => list.push(app),
                }
            }
            _ => return,
        }
        let mut c = prism_common::Config::load();
        c.focus_on_screen_share = self.focus_cfg.0;
        c.focus_on_fullscreen = self.focus_cfg.1;
        c.focus_allowed_apps = self.focus_cfg.2.clone();
        if let Err(e) = c.save() {
            eprintln!("[prism-ui/nc] focus: {e}");
        }
    }

    fn draw_notifications(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        let count = self.snap.notifications.len();
        let silent = self.snap.flags.silent;
        // Bottom row.
        let row_h = 34.0;
        let row_y = r.y + r.h - row_h;
        let label = match count {
            0 => "No notifications".to_string(),
            1 => "1 notification".to_string(),
            n => format!("{n} notifications"),
        };
        let label = if self.notif_filter.text.trim().is_empty() {
            label
        } else {
            let shown: usize = self.groups().iter().map(|g| g.items.len()).sum();
            format!("{shown} of {count}")
        };
        let (_, sw) = widgets::button_size(cr, "Silent", "bell", false);
        let (_, clw) = widgets::button_size(cr, "Clear", "close", false);
        let (_, fw) = widgets::button_size(cr, "Focus", "moon", false);
        let clear = Rect { x: r.x + r.w - clw, y: row_y, w: clw, h: row_h };
        let sil = Rect { x: clear.x - 8.0 - sw, y: row_y, w: sw, h: row_h };
        let foc = Rect { x: sil.x - 8.0 - fw, y: row_y, w: fw, h: row_h };
        let label = if self.snap.flags.auto_silent { "Focus is on".to_string() } else { label };
        // Too long next to the buttons: just the number.
        let avail = foc.x - r.x - 8.0;
        let label = if text_size(cr, &label, &font(14.0, "")).0 > avail && !self.snap.flags.auto_silent && self.notif_filter.text.trim().is_empty() { count.to_string() } else { label };
        text_v(cr, r.x, row_y, row_h, &label, &font(14.0, ""), p.fg, Some(avail));
        widgets::button(cr, foc, "Focus", "moon", false, self.focus_open || self.snap.flags.auto_silent, true, hv!(self, Hit::Focus), p);
        self.hits.push((Hit::Focus, foc));
        widgets::button(cr, sil, "Silent", "bell", false, silent, true, hv!(self, Hit::Silent), p);
        widgets::button(cr, clear, "Clear", "close", false, false, count > 0, hv!(self, Hit::ClearAll), p);
        self.hits.push((Hit::Silent, sil));
        if count > 0 {
            self.hits.push((Hit::ClearAll, clear));
        }

        let mut list = Rect { x: r.x, y: r.y, w: r.w, h: (row_y - 10.0 - r.y).max(0.0) };
        if self.focus_open {
            self.draw_focus(cr, list, p);
            return;
        }
        if count == 0 {
            self.notif_filter.clear();
            let used = self.draw_upcoming(cr, list, p);
            list.y += used;
            list.h = (list.h - used).max(0.0);
            self.empty_state(cr, list, "bell", silent, if silent { "Silent mode is on" } else { "No notifications" }, p);
            return;
        }
        // Calendar: the next events of the coming day.
        let used = self.draw_upcoming(cr, list, p);
        list.y += used;
        list.h = (list.h - used).max(0.0);
        // Search: typing on this page goes here.
        let fr = Rect { x: list.x, y: list.y, w: list.w, h: 36.0 };
        cb::fill_rounded_rect(cr, fr.x, fr.y, fr.w, fr.h, 18.0, alpha(p.fg, 0.1));
        draw_text_caret(cr, &self.notif_filter.text, Some(self.notif_filter.caret()), "Type to search notifications", fr.x + 16.0, fr.y, fr.w - 32.0, fr.h, FILTER_FONT, p.fg, alpha(p.fg, 0.5));
        self.text_geo.push((Hit::NotifFilter, fr.x + 16.0, fr.w - 32.0, FILTER_FONT));
        self.hits.push((Hit::NotifFilter, fr));
        list.y += fr.h + 10.0;
        list.h = (list.h - fr.h - 10.0).max(0.0);
        // Collect the group layout first (immutable borrow of the snapshot).
        let groups: Vec<(String, Vec<Notification>)> = self.groups().into_iter().map(|g| (g.key, g.items.into_iter().cloned().collect())).collect();
        if groups.is_empty() {
            let msg = format!("No notifications match \u{201C}{}\u{201D}", self.notif_filter.text.trim());
            self.empty_state(cr, list, "bell", false, &msg, p);
            return;
        }
        let p = *p;
        self.scrolled(cr, 0, list, |v, mut y| {
            let start = y;
            for (gi, (key, items)) in groups.iter().enumerate() {
                let mut h = 0.0;
                crate::ui::staggered(cr, gi + 1, Rect { x: list.x, y, w: list.w, h: list.h }, || {
                    h = v.draw_group(cr, Rect { x: list.x, y, w: list.w, h: 0.0 }, gi, key, items, &p);
                });
                y += h + 8.0;
            }
            y - start - 8.0
        });
    }

    /// Send a timer action; the state comes back with the next status refresh, so apply it
    /// here too for an immediate redraw.
    fn timer(&mut self, a: TimerAction) {
        if let Ok(Some(json)) = prism_common::ipc::request(&IpcCommand::Timer(a)) {
            if let Ok(t) = serde_json::from_str(&json) {
                self.snap.flags.timer = t;
            }
        }
    }

    /// Wake the loop each second while the timer or stopwatch counts (the page and tile show it).
    pub fn wake_in(&self) -> Option<std::time::Duration> {
        if self.meters_shown() {
            // Look for sound at about the frame rate.
            return Some(std::time::Duration::from_millis(16));
        }
        self.snap.flags.timer.ticking().then(|| std::time::Duration::from_millis(1000 - (unix_ms() % 1000) as u64 + 5))
    }

    fn draw_timer(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        let t = self.snap.flags.timer;
        let now = unix_ms();
        let left = t.remaining(now);
        // Countdown: a ring that empties, the time in the middle.
        let ring_r = 62.0;
        let (cx, cy) = (r.x + r.w / 2.0, r.y + 16.0 + ring_r);
        let frac = match left {
            Some(l) if t.total_ms > 0 => (l as f64 / t.total_ms as f64).clamp(0.0, 1.0),
            _ => 0.0,
        };
        cb::stroke_circle(cr, cx, cy, ring_r, alpha(p.fg, 0.15), 6.0);
        if frac > 0.0 {
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_set_line_width(cr, 6.0);
                let c = p.primary;
                cb::cairo_set_source_rgba(cr, c.0, c.1, c.2, c.3);
                let a0 = -std::f64::consts::FRAC_PI_2;
                cb::cairo_new_path(cr);
                cb::cairo_arc(cr, cx, cy, ring_r, a0, a0 + frac * std::f64::consts::TAU);
                cb::cairo_stroke(cr);
                cb::cairo_restore(cr);
            }
        }
        let big = font(30.0, "Bold");
        let text = left.map(prism_common::ipc::clock_text).unwrap_or_else(|| "00:00".into());
        let (tw, th) = text_size(cr, &text, &big);
        cb::draw_pango_text(cr, cx - tw / 2.0, cy - th / 2.0 - 6.0, &text, &big, if left.is_some() { p.fg } else { alpha(p.fg, 0.45) }, None);
        let state = if t.end_ms.is_some() { "Running" } else if t.paused_ms.is_some() { "Paused" } else { "Pick a time" };
        let small = font(11.0, "");
        let (sw, _) = text_size(cr, state, &small);
        cb::draw_pango_text(cr, cx - sw / 2.0, cy + th / 2.0 - 4.0, state, &small, alpha(p.fg, 0.6), None);
        let mut y = cy + ring_r + 16.0;
        // Controls for a set timer, presets otherwise.
        let row = |labels: &[(String, Hit)], y: f64, v: &mut Self| {
            let gap = 8.0;
            let bw = (r.w - gap * (labels.len() as f64 - 1.0)) / labels.len() as f64;
            for (i, (l, h)) in labels.iter().enumerate() {
                let b = Rect { x: r.x + i as f64 * (bw + gap), y, w: bw, h: 32.0 };
                widgets::button(cr, b, l, "", true, false, true, hv!(v, *h), p);
                v.hits.push((*h, b));
            }
        };
        if left.is_some() {
            let pause = if t.end_ms.is_some() { "Pause" } else { "Resume" };
            row(&[(pause.into(), Hit::TimerPause), ("+1 min".into(), Hit::TimerAdd(60)), ("Cancel".into(), Hit::TimerCancel)], y, self);
            y += 40.0;
        } else {
            let presets: Vec<(String, Hit)> = [1u32, 5, 10].iter().map(|m| (format!("{m} min"), Hit::TimerPreset(m * 60))).collect();
            row(&presets, y, self);
            y += 40.0;
            let presets: Vec<(String, Hit)> = [15u32, 25, 60].iter().map(|m| (format!("{m} min"), Hit::TimerPreset(m * 60))).collect();
            row(&presets, y, self);
            y += 40.0;
        }
        // Stopwatch.
        y += 10.0;
        cb::fill_rounded_rect(cr, r.x, y, r.w, 1.0, 0.0, alpha(p.fg, 0.12));
        y += 12.0;
        text_v(cr, r.x + 2.0, y, 20.0, "Stopwatch", &font(12.0, "Bold"), alpha(p.fg, 0.65), None);
        let sw_text = prism_common::ipc::clock_text(t.stopwatch(now));
        let (stw, _) = text_size(cr, &sw_text, &font(20.0, "Bold"));
        text_v(cr, r.x + r.w - stw, y - 4.0, 28.0, &sw_text, &font(20.0, "Bold"), if t.stopwatch(now) > 0 { p.fg } else { alpha(p.fg, 0.45) }, None);
        y += 30.0;
        let go = if t.sw_start_ms.is_some() { "Pause" } else if t.sw_acc_ms > 0 { "Resume" } else { "Start" };
        row(&[(go.into(), Hit::SwToggle), ("Reset".into(), Hit::SwReset)], y, self);
    }

    /// Expansion of group `key` (0 collapsed, 1 open), on a spring.
    fn expand_t(&mut self, key: &str) -> f64 {
        let to = if self.expanded.as_deref() == Some(key) { 1.0 } else { 0.0 };
        let ts = self.time_scale;
        let s = self.expand.entry(key.to_string()).or_insert_with(|| crate::ui::SpringValue::new(to));
        if (s.target() - to).abs() > 1e-6 {
            s.set_scaled(to, SPRING_GROUP, ts);
        }
        s.animated(0.002).clamp(0.0, 1.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_group(&mut self, cr: *mut Cairo, r: Rect, gi: usize, key: &str, items: &[Notification], p: &Pal) -> f64 {
        let e = self.expand_t(key);
        let latest = &items[0];
        let total = items.len();
        let critical = latest.urgency >= 2;
        // The app icon shrinks into the header row as the group opens.
        let icon = 46.0 + (HEAD_OPEN - 46.0) * e;
        let tx = r.x + 12.0 + icon + 12.0;
        let tw = r.w - 24.0 - icon - 12.0;

        // Head: pill row (20) + 3 + preview (collapses while expanded).
        let line_h = 19.0;
        let second = if total > 1 { true } else { !latest.body.is_empty() };
        let preview_full = line_h + if second { 2.0 + line_h } else { 0.0 };
        let preview_h = preview_full * (1.0 - e);
        let closed_h = (20.0 + 3.0 + preview_full).max(46.0);
        let head_h = closed_h + (HEAD_OPEN - closed_h) * e;

        // The thread: the latest GROUP_LIMIT entries unless all were asked for.
        let shown = if self.show_all.contains(key) { total } else { total.min(GROUP_LIMIT) };
        let more = total - shown;
        let layout = if e > 0.0 { self.entry_layout(cr, r.w - 20.0, &items[..shown]) } else { Vec::new() };
        let mut list_h: f64 = layout.iter().zip(&items[..shown]).map(|(l, n)| l.0 * self.collapse_factor(n.id)).sum::<f64>() + ENTRY_GAP * shown.saturating_sub(1) as f64;
        if more > 0 {
            list_h += 6.0 + MORE_H;
        }
        let details_h = (list_h + 10.0) * e;
        let card_h = 12.0 + head_h + details_h + 12.0;
        // A closed group of several shows the edges of the cards under it.
        let layers = (total.saturating_sub(1)).min(2);
        let peek = layers as f64 * PEEK * (1.0 - e);

        let card = Rect { x: r.x, y: r.y, w: r.w, h: card_h };
        if peek > 0.1 {
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, card.x, card.y + card_h, card.w, peek + 1.0);
                cb::cairo_clip(cr);
            }
            for k in 1..=layers {
                let inset = 12.0 * k as f64;
                let a = (0.10 - 0.025 * k as f64) * (1.0 - e);
                cb::fill_rounded_rect(cr, card.x + inset, card.y + PEEK * k as f64 * (1.0 - e), card.w - 2.0 * inset, card_h, RADIUS, alpha(p.fg, a));
            }
            unsafe { cb::cairo_restore(cr) };
        }
        cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, RADIUS, alpha(p.fg, 0.12));
        if e > 0.0 {
            cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, RADIUS, alpha(p.fg, 0.05 * e));
        }
        if critical {
            cb::stroke_rounded_rect(cr, card.x + 0.75, card.y + 0.75, card.w - 1.5, card.h - 1.5, RADIUS, p.error, 1.5);
        }
        let head = Rect { x: r.x + 12.0, y: r.y + 12.0, w: r.w - 24.0, h: head_h };
        self.hits.push((Hit::GroupHead(gi), head));
        // Preview pictures show inside the entries; the group shows the app's icon.
        let head_image = if wants_preview(latest) { None } else { latest.image.as_deref() };
        crate::toasts::draw_notif_icon(cr, head.x, head.y, icon, &latest.app_name, latest.icon.as_deref(), head_image, &latest.desktop_entry, p.fg, 1.0);

        // Pill row: app name · time · [count ⌄] closed; app name · Clear all · [Show less ⌃] open.
        let row_y = head.y + (HEAD_OPEN - 20.0) / 2.0 * e;
        let small = font(11.0, "");
        let open = e > 0.5;
        let pill_label = if open { "Show less".to_string() } else if total > 1 { total.to_string() } else { String::new() };
        let pl_w = if pill_label.is_empty() { 0.0 } else { text_size(cr, &pill_label, &small).0 + 4.0 };
        let pill_w = pl_w + 12.0 + 14.0;
        let pill = Rect { x: tx + tw - pill_w, y: row_y, w: pill_w, h: 20.0 };
        let time = time_label(latest.timestamp);
        let (time_w, _) = text_size(cr, &time, &small);
        let time_x = pill.x - 8.0 - time_w;
        // "Clear all" takes the time's place as the group opens.
        let clear_label = "Clear all";
        let (clear_w, _) = text_size(cr, clear_label, &small);
        let clear = Rect { x: pill.x - 10.0 - clear_w - 12.0, y: row_y, w: clear_w + 12.0, h: 20.0 };
        let name_end = if open { clear.x } else { time_x } - 8.0;
        text_v(cr, tx, row_y, 20.0, &latest.app_name, &font(11.0 + 1.5 * e, if open { "Bold" } else { "" }), alpha(p.fg, 0.65 + 0.3 * e), Some(name_end - tx));
        if e < 1.0 {
            text_v(cr, time_x, row_y, 20.0, &time, &small, alpha(p.fg, 0.65 * (1.0 - e)), None);
        }
        if e > 0.0 {
            let ch = hv!(self, Hit::GroupClear(gi));
            cb::fill_rounded_rect(cr, clear.x, clear.y, clear.w, clear.h, 10.0, alpha(p.fg, 0.14 * ch * e));
            text_v(cr, clear.x + 6.0, row_y, 20.0, clear_label, &small, alpha(p.fg, 0.8 * e), None);
            if open {
                self.hits.push((Hit::GroupClear(gi), clear));
            }
        }
        let ph = self.hover == Some(Hit::GroupHead(gi)) && pill.contains(self.pointer.0, self.pointer.1);
        cb::fill_rounded_rect(cr, pill.x, pill.y, pill.w, pill.h, 10.0, alpha(p.fg, if ph { 0.32 } else { 0.18 }));
        let mut px = pill.x + 7.0;
        if !pill_label.is_empty() {
            text_v(cr, px, pill.y, 20.0, &pill_label, &small, p.fg, None);
            px += pl_w;
        }
        svg::icon(cr, if open { "chevron-up" } else { "chevron-down" }, px, pill.y + 4.0, 12.0, p.fg, false);

        // Preview lines.
        if preview_h > 0.5 {
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, tx, head.y + 23.0, tw, preview_h);
                cb::cairo_clip(cr);
                cb::cairo_push_group(cr);
            }
            let y1 = head.y + 23.0;
            let body_of = |n: &Notification| plain_line(&n.body);
            if total > 1 {
                preview_line(cr, tx, y1, tw, &latest.summary, &body_of(latest), 1.0, true, p);
                let prev = &items[1];
                preview_line(cr, tx, y1 + line_h + 2.0, tw, &prev.summary, &body_of(prev), 0.7, false, p);
            } else {
                preview_line(cr, tx, y1, tw, &latest.summary, "", 1.0, true, p);
                if second {
                    preview_line(cr, tx, y1 + line_h + 2.0, tw, "", &body_of(latest), 1.0, false, p);
                }
            }
            unsafe {
                cb::cairo_pop_group_to_source(cr);
                cb::cairo_paint_with_alpha(cr, 1.0 - e);
                cb::cairo_restore(cr);
            }
        }

        // Expanded entries.
        if details_h > 0.5 {
            let dy = head.y + head_h;
            let clip = Rect { x: card.x, y: dy, w: card.w, h: details_h };
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, clip.x, clip.y, clip.w, clip.h);
                cb::cairo_clip(cr);
            }
            let before = self.hits.len();
            let end = self.draw_entries(cr, card.x + 10.0, dy + 10.0, card.w - 20.0, &items[..shown], &layout, e, p);
            if more > 0 {
                let label = format!("Show {more} more");
                let (lw, _) = text_size(cr, &label, &small);
                let mr = Rect { x: card.x + 10.0 + THREAD_X - 6.0, y: end + 6.0, w: lw + 24.0, h: MORE_H };
                let mh = hv!(self, Hit::GroupMore(gi));
                cb::fill_rounded_rect(cr, mr.x, mr.y, mr.w, mr.h, mr.h / 2.0, alpha(p.fg, (0.10 + 0.10 * mh) * e));
                text_v(cr, mr.x + 12.0, mr.y, mr.h, &label, &small, alpha(p.fg, 0.85 * e), None);
                self.hits.push((Hit::GroupMore(gi), mr));
            }
            for h in self.hits[before..].iter_mut() {
                h.1 = clip_rect(h.1, clip);
            }
            self.hits.retain(|(_, r)| r.h > 0.0);
            unsafe { cb::cairo_restore(cr) };
        }
        card_h + peek
    }

    /// How far an entry's buttons are shown: while the pointer is on it, or its reply is open.
    fn reveal(&mut self, n: &Notification) -> f64 {
        let on = self.hover.and_then(hit_nid) == Some(n.id) || self.reply.as_ref().is_some_and(|(id, _)| *id == n.id);
        let ts = self.time_scale;
        self.hover_fade.level_scaled(Hit::EntryReveal(n.id), on, ts)
    }

    /// Per entry: (height, buttons shown 0..1, summary shown). A summary repeating the one
    /// before it (a sender writing again) is left out, like a chat thread.
    fn entry_layout(&mut self, cr: *mut Cairo, w: f64, items: &[Notification]) -> Vec<(f64, f64, bool)> {
        let tw = w - THREAD_X - 4.0;
        items
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let body = plain(&n.body);
                let show_summary = i == 0 || items[i - 1].summary != n.summary || body.is_empty();
                let reveal = self.reveal(n);
                let (_, sh) = text_size(cr, &n.summary, &font(14.0, "Bold"));
                let (tmw, _) = text_size(cr, &time_label(n.timestamp), &font(11.0, ""));
                let body_w = if show_summary { tw } else { tw - tmw - 8.0 };
                let bh = if body.is_empty() { 0.0 } else { cb::measure_pango_text(cr, &body, &font(13.0, ""), body_w as i32, Some(8)).1 as f64 };
                let text = match (show_summary, bh > 0.0) {
                    (true, true) => sh + 2.0 + bh,
                    (true, false) => sh,
                    (false, _) => bh,
                };
                let ph = crate::toasts::preview_height(entry_preview(n), tw);
                let pic = if ph > 0.0 { 8.0 + ph } else { 0.0 };
                let (_, _, _, _, rows_h) = entry_actions(cr, 0.0, 0.0, tw, n);
                let reply = if self.reply.as_ref().is_some_and(|(id, _)| *id == n.id) { 8.0 + REPLY_H } else { 0.0 };
                (ENTRY_PAD + text + pic + (8.0 + rows_h) * reveal + reply + ENTRY_PAD, reveal, show_summary)
            })
            .collect()
    }

    /// 1 while shown, shrinking to 0 during the collapse after a dismiss slide.
    fn collapse_factor(&self, nid: u32) -> f64 {
        match self.dismissing.iter().find(|(n, _)| *n == nid) {
            Some((_, t)) => {
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                1.0 - crate::ui::out_cubic(((ms - 180.0) / 200.0).clamp(0.0, 1.0))
            }
            None => 1.0,
        }
    }

    /// The group's entries as a thread along a timeline: a dot per entry joined by a thin
    /// line, no boxes; buttons fade in on the entry under the pointer. Entries cascade in as
    /// the group opens (`e`). Returns where the list ends.
    #[allow(clippy::too_many_arguments)]
    fn draw_entries(&mut self, cr: *mut Cairo, x: f64, mut y: f64, w: f64, items: &[Notification], layout: &[(f64, f64, bool)], e: f64, p: &Pal) -> f64 {
        let count = items.len().max(1) as f64;
        let step = (0.5 / count).min(0.12);
        let line_x = x + 13.0;
        let mut last_dot: Option<f64> = None;
        for (i, (n, &(full_h, reveal, show_summary))) in items.iter().zip(layout).enumerate() {
            let k = self.collapse_factor(n.id);
            let h = full_h * k;
            // Cascade: each entry follows the one above a little later.
            let s = ((e - i as f64 * step) / (1.0 - (count - 1.0) * step)).clamp(0.0, 1.0);
            let s = crate::ui::out_cubic(s);
            let drop = -10.0 * (1.0 - s);
            // Slide right while dismissing or swiping.
            let (dx, a) = match self.dismissing.iter().find(|(id, _)| *id == n.id) {
                Some((_, t)) => {
                    let f = (t.elapsed().as_secs_f64() * 1000.0 / 180.0).clamp(0.0, 1.0);
                    let from = if matches!(self.drag, Some(Drag::Entry(id, _)) if id == n.id) { self.swipe_dx } else { 0.0 };
                    (from + (w - from) * crate::ui::in_cubic(f), 1.0 - f)
                }
                None if matches!(self.drag, Some(Drag::Entry(id, _)) if id == n.id) => (self.swipe_dx, 1.0),
                None => (0.0, 1.0),
            };
            let a = a * s;
            let fs = font(14.0, "Bold");
            let (_, sh) = text_size(cr, &n.summary, &fs);
            let dot_y = y + drop + ENTRY_PAD + if show_summary { sh / 2.0 } else { 9.0 };
            if h > 0.5 {
                // Timeline segment from the previous dot.
                if let Some(prev) = last_dot {
                    cb::fill_rounded_rect(cr, line_x - 0.75, prev + 7.0, 1.5, (dot_y - prev - 14.0).max(0.0), 0.75, alpha(p.fg, 0.20 * s));
                }
                unsafe {
                    cb::cairo_save(cr);
                    cb::cairo_rectangle(cr, x, y, w, h);
                    cb::cairo_clip(cr);
                    cb::cairo_push_group(cr);
                }
                let cx = x + dx;
                let top = y + drop;
                // Soft highlight under the entry being pointed at.
                if reveal > 0.0 {
                    cb::fill_rounded_rect(cr, cx + THREAD_X - 8.0, top, w - THREAD_X + 8.0, full_h, RADIUS, alpha(p.fg, 0.06 * reveal));
                }
                let dot = if i == 0 { p.primary } else { alpha(p.fg, 0.55) };
                cb::fill_rounded_rect(cr, line_x + dx - 4.0, dot_y - 4.0, 8.0, 8.0, 4.0, dot);
                if n.urgency >= 2 {
                    cb::stroke_rounded_rect(cr, line_x + dx - 6.0, dot_y - 6.0, 12.0, 12.0, 6.0, p.error, 1.5);
                }
                let tx = cx + THREAD_X;
                let tw = w - THREAD_X - 4.0;
                let small = font(11.0, "");
                let time = time_label(n.timestamp);
                let (tmw, _) = text_size(cr, &time, &small);
                let mut by = top + ENTRY_PAD;
                if show_summary {
                    text_v(cr, tx, by, sh, &n.summary, &fs, p.fg, Some(tw - tmw - 8.0));
                    text_v(cr, tx + tw - tmw, by, sh, &time, &small, alpha(p.fg, 0.5), None);
                    by += sh;
                } else {
                    text_v(cr, tx + tw - tmw, by, 18.0, &time, &small, alpha(p.fg, 0.5), None);
                }
                let body = plain(&n.body);
                if !body.is_empty() {
                    let (bw, gap) = if show_summary { (tw, 2.0) } else { (tw - tmw - 8.0, 0.0) };
                    let (_, bh) = cb::draw_pango_text_wrapped(cr, tx, by + gap, &body, &font(13.0, ""), alpha(p.fg, 0.85), bw as i32, Some(8));
                    by += gap + bh as f64;
                }
                // Screenshot-style preview picture across the entry.
                if let Some(path) = entry_preview(n) {
                    let ph = crate::toasts::preview_height(Some(path), tw);
                    cb::draw_image_cover(cr, path, tx, by + 8.0, tw, ph, 10.0);
                    cb::stroke_rounded_rect(cr, tx + 0.5, by + 8.5, tw - 1.0, ph - 1.0, 10.0, alpha(p.fg, 0.12), 1.0);
                    by += 8.0 + ph;
                }
                // Buttons, faded and folded in while the pointer is elsewhere.
                let acts: Vec<(String, String)> = n.actions.iter().take(4).cloned().collect();
                let (copy, snooze, close, action_rects, rows_h) = entry_actions(cr, tx, by + 8.0, tw, n);
                if reveal > 0.01 {
                    unsafe {
                        cb::cairo_save(cr);
                        cb::cairo_rectangle(cr, tx - 2.0, by, tw + 4.0, (8.0 + rows_h) * reveal);
                        cb::cairo_clip(cr);
                        cb::cairo_push_group(cr);
                    }
                    for (j, (r, (_, label))) in action_rects.iter().zip(acts.iter()).enumerate() {
                        widgets::button(cr, *r, label, "", true, false, true, hv!(self, Hit::EntryAction(n.id, j)), p);
                    }
                    widgets::button(cr, copy, "", "copy", true, false, true, hv!(self, Hit::EntryCopy(n.id)), p);
                    widgets::button(cr, snooze, "", "timer", true, false, true, hv!(self, Hit::EntrySnooze(n.id)), p);
                    widgets::button(cr, close, "", "close", true, false, true, hv!(self, Hit::EntryClose(n.id)), p);
                    unsafe {
                        cb::cairo_pop_group_to_source(cr);
                        cb::cairo_paint_with_alpha(cr, reveal);
                        cb::cairo_restore(cr);
                    }
                }
                by += (8.0 + rows_h) * reveal;
                let mut reply_hits = Vec::new();
                if let Some((_, q)) = self.reply.as_ref().filter(|(id, _)| *id == n.id) {
                    let (text, caret) = (q.text.clone(), q.caret());
                    let field = Rect { x: tx, y: by + 8.0, w: tw, h: REPLY_H };
                    cb::fill_rounded_rect(cr, field.x, field.y, field.w, field.h, RADIUS, p.surface_variant);
                    cb::stroke_rounded_rect(cr, field.x + 1.0, field.y + 1.0, field.w - 2.0, field.h - 2.0, RADIUS, p.primary, 2.0);
                    let (bh, bw) = widgets::button_size(cr, "Send", "", true);
                    let send = Rect { x: field.x + field.w - 6.0 - bw, y: field.y + (REPLY_H - bh) / 2.0, w: bw, h: bh };
                    widgets::button(cr, send, "Send", "", true, false, !text.trim().is_empty(), hv!(self, Hit::ReplySend(n.id)), p);
                    let ix = field.x + 10.0;
                    let iw = send.x - 8.0 - ix;
                    let hint = if n.reply_placeholder.is_empty() { "Reply" } else { n.reply_placeholder.as_str() };
                    draw_text_caret(cr, &text, Some(caret), hint, ix, field.y, iw, REPLY_H, FIELD_FONT, p.on_surface, p.on_surface_variant);
                    self.text_geo.push((Hit::ReplyField(n.id), ix, iw, FIELD_FONT));
                    reply_hits.push((Hit::ReplyField(n.id), field));
                    reply_hits.push((Hit::ReplySend(n.id), send));
                }
                unsafe {
                    cb::cairo_pop_group_to_source(cr);
                    cb::cairo_paint_with_alpha(cr, a);
                    cb::cairo_restore(cr);
                }
                if dx == 0.0 && k >= 1.0 && s > 0.99 {
                    self.hits.push((Hit::Entry(n.id), Rect { x, y, w, h }));
                    if reveal > 0.5 {
                        self.hits.push((Hit::EntryCopy(n.id), copy));
                        self.hits.push((Hit::EntrySnooze(n.id), snooze));
                        self.hits.push((Hit::EntryClose(n.id), close));
                        for (j, r) in action_rects.iter().enumerate() {
                            self.hits.push((Hit::EntryAction(n.id, j), *r));
                        }
                    }
                    // The send button sits on the field: listed after it so it wins.
                    self.hits.extend(reply_hits);
                }
                last_dot = Some(dot_y);
            }
            y += h + ENTRY_GAP;
        }
        y - ENTRY_GAP
    }

    // ── Network page ─────────────────────────────────────────────────────

    fn draw_network(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        let net = self.snap.net.clone();
        let wired = net.ethernet_connected;
        let has_wifi = net.wifi_present();
        let mut y = r.y;
        if !wired && !has_wifi {
            self.empty_state(cr, r, "ethernet", false, "No network connection", p);
            return;
        }
        if wired {
            let head = Rect { x: r.x, y, w: r.w, h: 40.0 };
            self.hits.push((Hit::EthHeader, head));
            svg::icon(cr, "ethernet", r.x, y + 7.0, 26.0, p.fg, false);
            let fx = r.x + 26.0 + 12.0;
            cb::draw_pango_text(cr, fx, y + 1.0, "Ethernet", &font(16.0, "Bold"), p.fg, None);
            let sub = if net.eth.connection.is_empty() {
                net.ethernet_device.clone()
            } else {
                format!("{}  ·  {}", net.eth.connection, net.ethernet_device)
            };
            cb::draw_pango_text(cr, fx, y + 22.0, &sub, &font(12.0, ""), alpha(p.fg, 0.65), Some((r.w - 26.0 - 12.0 - 28.0) as i32));
            let deg = 180.0 * self.eth_t.value();
            svg::icon_rotated(cr, "chevron-down", r.x + r.w - 16.0, y + 12.0, 16.0, alpha(p.fg, 0.7), deg);
            y += 40.0 + 8.0;
            let rows = net.eth_rows();
            let inner = details_height(&rows).max(48.0) + 12.0;
            let shown = inner * self.eth_t.value();
            if shown > 0.5 {
                unsafe {
                    cb::cairo_save(cr);
                    cb::cairo_rectangle(cr, r.x, y, r.w, shown);
                    cb::cairo_clip(cr);
                }
                cb::fill_rounded_rect(cr, r.x, y, r.w, inner, RADIUS, alpha(p.fg, 0.1));
                if rows.is_empty() {
                    let f = font(12.0, "");
                    let t = "Reading connection details…";
                    let (tw, _) = text_size(cr, t, &f);
                    text_v(cr, r.x + (r.w - tw) / 2.0, y, inner, t, &f, alpha(p.fg, 0.6), None);
                } else {
                    let before = self.hits.len();
                    self.draw_details(cr, r.x, y + 6.0, r.w, &rows, p);
                    let clip = Rect { x: r.x, y, w: r.w, h: shown };
                    for h in self.hits[before..].iter_mut() {
                        h.1 = clip_rect(h.1, clip);
                    }
                    self.hits.retain(|(_, r)| r.h > 0.0);
                }
                unsafe { cb::cairo_restore(cr) };
            }
            y += shown;
            if has_wifi {
                y += 12.0;
                cb::fill_rounded_rect(cr, r.x, y, r.w, 1.0, 0.0, alpha(p.fg, 0.15));
                y += 1.0 + 12.0;
            }
        }
        if !has_wifi {
            return;
        }
        // Wi-Fi header: icon, title/status, switch.
        svg::icon(cr, "wifi", r.x, y + 6.0, 24.0, p.fg, !net.wifi_enabled);
        let fx = r.x + 34.0;
        cb::draw_pango_text(cr, fx, y, "Wi-Fi", &font(16.0, "Bold"), p.fg, None);
        let status = if !net.wifi_enabled {
            "Turned off".to_string()
        } else if let Some(a) = net.active() {
            format!("Connected to {}", a.ssid)
        } else {
            "Not connected".into()
        };
        cb::draw_pango_text(cr, fx, y + 21.0, &status, &font(12.0, ""), alpha(p.fg, 0.65), Some(220));
        self.knobs[0].set_spring(if net.wifi_enabled { 1.0 } else { 0.0 }, crate::ui::SPRING_KNOB, self.time_scale);
        let sw = Rect { x: r.x + r.w - 42.0, y: y + 8.0, w: 42.0, h: 24.0 };
        widgets::switch(cr, sw.x, sw.y, self.knobs[0].value(), true, p);
        self.hits.push((Hit::WifiSwitch, sw));
        y += 40.0 + 10.0;

        let list_top = if net.wifi_enabled {
            text_v(cr, r.x, y, 30.0, "Networks", &font(13.0, "Bold"), alpha(p.fg, 0.8), None);
            let rb = Rect { x: r.x + r.w - 30.0, y, w: 30.0, h: 30.0 };
            let angle = if net.scanning { self.spin_start.elapsed().as_secs_f64() * 400.0 % 360.0 } else { 0.0 };
            widgets::refresh_button(cr, rb.x, rb.y, angle, hv!(self, Hit::WifiRefresh), p);
            self.hits.push((Hit::WifiRefresh, rb));
            y + 30.0 + 10.0
        } else {
            y
        };
        let list = Rect { x: r.x, y: list_top, w: r.w, h: (r.y + r.h - list_top).max(0.0) };
        let nets = self.networks();
        if !net.wifi_enabled || nets.is_empty() {
            let msg = if !net.wifi_enabled {
                "Wi-Fi is turned off"
            } else if net.scanning {
                "Searching for networks…"
            } else {
                "No networks found"
            };
            self.empty_state(cr, list, "wifi", !net.wifi_enabled, msg, p);
            return;
        }
        let p = *p;
        self.scrolled(cr, 1, list, |v, mut y| {
            let start = y;
            let mut group = u8::MAX;
            for (i, n) in nets.iter().enumerate() {
                if n.group() != group {
                    group = n.group();
                    let label = ["CONNECTED", "SAVED NETWORKS", "AVAILABLE NETWORKS"][group as usize];
                    y += 6.0;
                    cb::draw_pango_text(cr, list.x + 4.0, y, label, &font(11.0, "Bold"), alpha(p.fg, 0.6), None);
                    y += 16.0 + 4.0;
                }
                y += v.draw_net_row(cr, Rect { x: list.x, y, w: list.w, h: 0.0 }, i, n, &net, &p) + 4.0;
            }
            y - start
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_net_row(&mut self, cr: *mut Cairo, r: Rect, i: usize, n: &Net, net: &model::NetState, p: &Pal) -> f64 {
        let expanded = self.expanded_ssid.as_deref() == Some(n.ssid.as_str());
        let connecting = net.connecting.as_deref() == Some(n.ssid.as_str());
        let error = net.error.as_ref().filter(|(s, _)| *s == n.ssid).map(|(_, m)| m.clone());
        let rows = if expanded && n.active { net.wifi_rows() } else { Vec::new() };
        let extra_inner = if expanded { (if n.active { details_height(&rows) } else { 44.0 }) + 8.0 } else { 0.0 };
        let err_h = if error.is_some() { 6.0 + 16.0 } else { 0.0 };
        let h = 52.0 + extra_inner + err_h + 6.0;
        let row = Rect { x: r.x, y: r.y, w: r.w, h };
        let hovered = matches!(self.hover, Some(Hit::NetRow(j) | Hit::NetForget(j) | Hit::NetConnect(j)) if j == i);
        if n.active {
            cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, RADIUS, alpha(p.fg, 0.16));
        } else if hovered {
            cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, RADIUS, alpha(p.fg, 0.1));
        }
        let main = Rect { x: r.x, y: r.y, w: r.w, h: 52.0 };
        self.hits.push((Hit::NetRow(i), main));
        let level = (n.signal as f64 / 25.0).round();
        svg::icon(cr, "wifi", r.x + 12.0, r.y + 15.0, 22.0, alpha(p.fg, 0.35 + 0.65 * (level / 3.0).min(1.0)), false);
        let tx = r.x + 12.0 + 22.0 + 12.0;
        // Buttons from the right.
        let mut bx = r.x + r.w - 10.0;
        let label = if n.active { "Disconnect" } else { "Connect" };
        let (bh, bw) = widgets::button_size(cr, label, "", true);
        bx -= bw;
        let cb_r = Rect { x: bx, y: r.y + (52.0 - bh) / 2.0, w: bw, h: bh };
        widgets::button(cr, cb_r, label, "", true, n.active, net.connecting.is_none(), hv!(self, Hit::NetConnect(i)), p);
        if net.connecting.is_none() {
            self.hits.push((Hit::NetConnect(i), cb_r));
        }
        if n.known && !n.active {
            let (fh, fw) = widgets::button_size(cr, "Forget", "", true);
            bx -= 12.0 + fw;
            let fr = Rect { x: bx, y: r.y + (52.0 - fh) / 2.0, w: fw, h: fh };
            widgets::button(cr, fr, "Forget", "", true, false, true, hv!(self, Hit::NetForget(i)), p);
            self.hits.push((Hit::NetForget(i), fr));
        }
        if n.active {
            bx -= 12.0 + 14.0;
            svg::icon_rotated(cr, "chevron-down", bx, r.y + 19.0, 14.0, alpha(p.fg, 0.7), if expanded { 180.0 } else { 0.0 });
        }
        let name_max = (bx - 12.0 - tx).min(150.0);
        let nf = font(14.0, if n.active { "Bold" } else { "" });
        let nw = text_v(cr, tx, r.y + 8.0, 20.0, &n.ssid, &nf, p.fg, Some(name_max));
        if n.secure {
            svg::icon(cr, "lock", tx + nw + 6.0, r.y + 12.0, 12.0, alpha(p.fg, 0.7), false);
        }
        let sub = if connecting {
            "Connecting…".to_string()
        } else if n.active {
            format!("Connected  ·  {}%", n.signal)
        } else if n.known {
            "Saved".into()
        } else if n.secure {
            "Secured".into()
        } else {
            "Open".into()
        };
        cb::draw_pango_text(cr, tx, r.y + 29.0, &sub, &font(11.0, ""), alpha(p.fg, 0.65), None);

        let mut y = r.y + 52.0;
        if expanded {
            if n.active {
                self.draw_details(cr, r.x, y, r.w, &rows, p);
                y += details_height(&rows);
            } else {
                let field = Rect { x: r.x + 12.0, y, w: r.w - 24.0, h: 44.0 };
                cb::fill_rounded_rect(cr, field.x, field.y, field.w, field.h, RADIUS, p.surface_variant);
                cb::stroke_rounded_rect(cr, field.x + 1.0, field.y + 1.0, field.w - 2.0, field.h - 2.0, RADIUS, p.primary, 2.0);
                let (jh, jw) = widgets::button_size(cr, "Join", "", true);
                let join = Rect { x: field.x + field.w - 8.0 - jw, y: field.y + (44.0 - jh) / 2.0, w: jw, h: jh };
                widgets::button(cr, join, "Join", "", true, false, true, hv!(self, Hit::PwJoin), p);
                self.hits.push((Hit::PwField, field));
                self.hits.push((Hit::PwJoin, join));
                let ix = field.x + 8.0;
                let iw = join.x - 8.0 - ix;
                let dots: String = "\u{2022}".repeat(self.pw.text.chars().count());
                draw_text_caret(cr, &dots, Some(self.pw.caret()), "Password", ix, field.y, iw, 44.0, FIELD_FONT, p.on_surface, p.on_surface_variant);
                self.text_geo.push((Hit::PwField, ix, iw, FIELD_FONT));
                y += 44.0;
            }
            y += 8.0;
        }
        if let Some(e) = error {
            cb::draw_pango_text(cr, r.x + 12.0, y + 6.0, &e, &font(11.0, ""), p.error, Some((r.w - 24.0) as i32));
        }
        h
    }

    fn draw_details(&mut self, cr: *mut Cairo, x: f64, mut y: f64, w: f64, rows: &[DetailRow], p: &Pal) {
        for row in rows {
            match row {
                DetailRow::Header(t) => {
                    let f = font(13.0, "Bold");
                    let (tw, th) = text_size(cr, t, &f);
                    cb::draw_pango_text(cr, x + (w - tw) / 2.0, y + 30.0 - 3.0 - th, t, &f, p.fg, None);
                    y += 30.0 + 2.0;
                }
                DetailRow::Item(label, value) => {
                    let idx = self.detail_values.len();
                    self.detail_values.push(value.clone());
                    let line = Rect { x: x + 4.0, y, w: w - 8.0, h: 26.0 };
                    let hovered = self.hover == Some(Hit::Detail(idx));
                    if hovered {
                        cb::fill_rounded_rect(cr, line.x, line.y, line.w, line.h, RADIUS, alpha(p.fg, 0.1));
                    }
                    let lf = font(12.0, "");
                    let lw = text_v(cr, x + 12.0, y, 26.0, label, &lf, alpha(p.fg, 0.6), None);
                    let vf = font(13.0, "");
                    let right = x + w - 12.0 - 13.0 - 10.0;
                    let max = right - (x + 12.0 + lw + 10.0);
                    let (vw, _) = text_size(cr, value, &vf);
                    let vw = vw.min(max);
                    text_v(cr, right - vw, y, 26.0, value, &vf, p.fg, Some(max));
                    if hovered {
                        svg::icon(cr, "copy", x + w - 12.0 - 13.0, y + 6.5, 13.0, alpha(p.fg, 0.8), false);
                    }
                    self.hits.push((Hit::Detail(idx), line));
                    y += 26.0 + 2.0;
                }
            }
        }
    }

    // ── Bluetooth page ───────────────────────────────────────────────────

    fn draw_bluetooth(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        let bt = self.snap.bt.clone();
        let mut y = r.y;
        cb::draw_pango_text(cr, r.x, y, "Bluetooth", &font(16.0, "Bold"), p.fg, None);
        let status = if !bt.adapter {
            "No adapter found"
        } else if bt.powered {
            "On"
        } else {
            "Turned off"
        };
        cb::draw_pango_text(cr, r.x, y + 21.0, status, &font(12.0, ""), alpha(p.fg, 0.65), None);
        self.knobs[1].set_spring(if bt.powered { 1.0 } else { 0.0 }, crate::ui::SPRING_KNOB, self.time_scale);
        let sw = Rect { x: r.x + r.w - 42.0, y: y + 8.0, w: 42.0, h: 24.0 };
        widgets::switch(cr, sw.x, sw.y, self.knobs[1].value(), bt.adapter, p);
        if bt.adapter {
            self.hits.push((Hit::BtSwitch, sw));
        }
        y += 40.0 + 10.0;

        if bt.powered {
            cb::fill_rounded_rect(cr, r.x, y, r.w, 52.0, RADIUS, alpha(p.fg, 0.1));
            cb::draw_pango_text(cr, r.x + 12.0, y + 8.0, "Discoverable", &font(14.0, ""), p.fg, None);
            let sub = if bt.discoverable { format!("Visible as “{}”", bt.name) } else { "Hidden from other devices".into() };
            cb::draw_pango_text(cr, r.x + 12.0, y + 28.0, &sub, &font(11.0, ""), alpha(p.fg, 0.65), Some((r.w - 24.0 - 52.0) as i32));
            self.knobs[2].set_spring(if bt.discoverable { 1.0 } else { 0.0 }, crate::ui::SPRING_KNOB, self.time_scale);
            let ds = Rect { x: r.x + r.w - 12.0 - 42.0, y: y + 14.0, w: 42.0, h: 24.0 };
            widgets::switch(cr, ds.x, ds.y, self.knobs[2].value(), true, p);
            self.hits.push((Hit::Discoverable, ds));
            y += 52.0 + 10.0;

            let title = if bt.discovering { "Devices  ·  searching…" } else { "Devices" };
            text_v(cr, r.x, y, 30.0, title, &font(13.0, "Bold"), alpha(p.fg, 0.8), None);
            let rb = Rect { x: r.x + r.w - 30.0, y, w: 30.0, h: 30.0 };
            let angle = if bt.discovering { self.spin_start.elapsed().as_secs_f64() * 400.0 % 360.0 } else { 0.0 };
            widgets::refresh_button(cr, rb.x, rb.y, angle, hv!(self, Hit::BtRefresh), p);
            self.hits.push((Hit::BtRefresh, rb));
            y += 30.0 + 10.0;
        }
        let list = Rect { x: r.x, y, w: r.w, h: (r.y + r.h - y).max(0.0) };
        let connected: Vec<usize> = (0..bt.devices.len()).filter(|&i| bt.devices[i].connected).collect();
        let paired: Vec<usize> = (0..bt.devices.len()).filter(|&i| bt.devices[i].paired && !bt.devices[i].connected).collect();
        let available: Vec<usize> = (0..bt.devices.len()).filter(|&i| !bt.devices[i].paired && !bt.devices[i].connected && bt.devices[i].named()).collect();
        let unnamed: Vec<usize> = (0..bt.devices.len()).filter(|&i| !bt.devices[i].paired && !bt.devices[i].connected && !bt.devices[i].named()).collect();
        if !bt.powered || connected.len() + paired.len() + available.len() + unnamed.len() == 0 {
            let msg = if !bt.powered {
                "Bluetooth is turned off"
            } else if bt.discovering {
                "Searching for devices…"
            } else {
                "No devices found"
            };
            self.empty_state(cr, list, "bluetooth", false, msg, p);
            return;
        }
        let p = *p;
        let show_unnamed = self.show_unnamed;
        self.scrolled(cr, 2, list, |v, mut y| {
            let start = y;
            let section = |y: &mut f64, label: &str| {
                *y += 6.0;
                cb::draw_pango_text(cr, list.x + 4.0, *y, label, &font(11.0, "Bold"), alpha(p.fg, 0.6), None);
                *y += 16.0 + 4.0;
            };
            for (label, idx) in [("CONNECTED", &connected), ("PAIRED", &paired), ("AVAILABLE", &available)] {
                if idx.is_empty() {
                    continue;
                }
                section(&mut y, label);
                for &i in idx {
                    v.draw_bt_row(cr, Rect { x: list.x, y, w: list.w, h: 54.0 }, i, &bt, &p);
                    y += 54.0 + 4.0;
                }
            }
            if !unnamed.is_empty() {
                let n = unnamed.len();
                let label = format!("{n} unnamed device{}", if n == 1 { "" } else { "s" });
                text_v(cr, list.x + 4.0, y, 40.0, &label, &font(12.0, ""), alpha(p.fg, 0.6), None);
                let bl = if show_unnamed { "Hide" } else { "Show" };
                let (bh, bw) = widgets::button_size(cr, bl, "", true);
                let br = Rect { x: list.x + list.w - bw, y: y + (40.0 - bh) / 2.0, w: bw, h: bh };
                widgets::button(cr, br, bl, "", true, false, true, hv!(v, Hit::Unnamed), &p);
                v.hits.push((Hit::Unnamed, br));
                y += 40.0 + 4.0;
                if show_unnamed {
                    for &i in &unnamed {
                        v.draw_bt_row(cr, Rect { x: list.x, y, w: list.w, h: 54.0 }, i, &bt, &p);
                        y += 54.0 + 4.0;
                    }
                }
            }
            y - start
        });
    }

    /// VPNs matching the filter, in display order.
    fn vpn_list(&self) -> Vec<model::Vpn> {
        let f = self.vpn_filter.text.to_lowercase();
        self.snap.net.vpns.iter().filter(|v| f.is_empty() || v.name.to_lowercase().contains(&f)).cloned().collect()
    }

    fn draw_vpn(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        let net = self.snap.net.clone();
        let mut y = r.y;
        cb::draw_pango_text(cr, r.x, y, "VPN", &font(16.0, "Bold"), p.fg, None);
        let on: Vec<&model::Vpn> = net.vpns.iter().filter(|v| v.state == model::VpnState::On).collect();
        let status = match on.as_slice() {
            [] if net.vpns.is_empty() => "No profiles".to_string(),
            [] => format!("{} profiles  \u{00B7}  not connected", net.vpns.len()),
            [v] => format!("Connected to {}", v.name),
            more => format!("{} connected", more.len()),
        };
        let (ih, iw) = widgets::button_size(cr, "Import\u{2026}", "", true);
        let ir = Rect { x: r.x + r.w - iw, y: y + (40.0 - ih) / 2.0, w: iw, h: ih };
        cb::draw_pango_text(cr, r.x, y + 21.0, &status, &font(12.0, ""), alpha(p.fg, 0.65), Some((ir.x - r.x - 8.0) as i32));
        widgets::button(cr, ir, "Import\u{2026}", "", true, false, true, hv!(self, Hit::VpnImport), p);
        self.hits.push((Hit::VpnImport, ir));
        y += 40.0 + 10.0;

        // Type-to-filter field.
        let fr = Rect { x: r.x, y, w: r.w, h: 36.0 };
        cb::fill_rounded_rect(cr, fr.x, fr.y, fr.w, fr.h, 18.0, alpha(p.fg, 0.1));
        // Typing on this page always goes here, so the caret shows.
        draw_text_caret(cr, &self.vpn_filter.text, Some(self.vpn_filter.caret()), "Type to filter", fr.x + 16.0, fr.y, fr.w - 32.0, fr.h, FILTER_FONT, p.fg, alpha(p.fg, 0.5));
        self.text_geo.push((Hit::VpnFilter, fr.x + 16.0, fr.w - 32.0, FILTER_FONT));
        self.hits.push((Hit::VpnFilter, fr));
        y += 36.0 + 10.0;
        if let Some((m, err)) = &net.vpn_msg {
            cb::draw_pango_text(cr, r.x + 4.0, y, m, &font(12.0, ""), if *err { p.error } else { alpha(p.fg, 0.75) }, Some(r.w as i32 - 8));
            y += 22.0;
        }
        let list = Rect { x: r.x, y, w: r.w, h: (r.y + r.h - y).max(0.0) };
        let vpns = self.vpn_list();
        if vpns.is_empty() {
            let msg = if net.vpns.is_empty() { "No VPN profiles yet. Import .ovpn files to add them.".to_string() } else { format!("No VPN matches \u{201C}{}\u{201D}", self.vpn_filter.text) };
            self.empty_state(cr, list, "lock", false, &msg, p);
            return;
        }
        let p = *p;
        const ROW: f64 = 48.0;
        self.scrolled(cr, TAB_VPN, list, |v, mut y| {
            let start = y;
            for (i, vpn) in vpns.iter().enumerate() {
                let row = Rect { x: list.x, y, w: list.w, h: ROW };
                y += ROW + 4.0;
                // Only rows in view are drawn (there may be hundreds).
                if row.y + row.h < list.y || row.y > list.y + list.h {
                    continue;
                }
                let busy = net.vpn_busy.contains(&vpn.path) || matches!(vpn.state, model::VpnState::Connecting | model::VpnState::Disconnecting);
                let on = vpn.state == model::VpnState::On;
                let hovered = row.contains(v.pointer.0, v.pointer.1) && list.contains(v.pointer.0, v.pointer.1);
                if on {
                    cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, RADIUS, alpha(p.fg, 0.16));
                } else if hovered {
                    cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, RADIUS, alpha(p.fg, 0.1));
                }
                svg::icon(cr, "lock", row.x + 12.0, row.y + 14.0, 20.0, p.fg, false);
                let label = if on || vpn.state == model::VpnState::Connecting { "Disconnect" } else { "Connect" };
                let (bh, bw) = widgets::button_size(cr, label, "", true);
                let br = Rect { x: row.x + row.w - 10.0 - bw, y: row.y + (ROW - bh) / 2.0, w: bw, h: bh };
                widgets::button(cr, br, label, "", true, on, !busy || vpn.state == model::VpnState::Connecting, hv!(v, Hit::VpnRow(i)), &p);
                let tx = row.x + 12.0 + 20.0 + 12.0;
                text_v(cr, tx, row.y + 6.0, 20.0, &vpn.name, &font(14.0, if on { "Bold" } else { "" }), p.fg, Some(br.x - 10.0 - tx));
                let sub = match vpn.state {
                    model::VpnState::On => "Connected".to_string(),
                    model::VpnState::Connecting => "Connecting\u{2026}".to_string(),
                    model::VpnState::Disconnecting => "Disconnecting\u{2026}".to_string(),
                    model::VpnState::Off => match vpn.kind.as_str() {
                        "openvpn" => "OpenVPN".to_string(),
                        "wireguard" => "WireGuard".to_string(),
                        k => k.to_string(),
                    },
                };
                text_v(cr, tx, row.y + 26.0, 16.0, &sub, &font(11.0, ""), alpha(p.fg, 0.65), Some(br.x - 10.0 - tx));
                if !busy || vpn.state == model::VpnState::Connecting {
                    v.hits.push((Hit::VpnRow(i), row));
                }
            }
            y - start
        });
    }

    fn draw_bt_row(&mut self, cr: *mut Cairo, r: Rect, i: usize, bt: &model::BtState, p: &Pal) {
        let d = &bt.devices[i];
        let busy = bt.busy(&d.mac);
        let failed = bt.failed.contains(&d.mac);
        let hovered = matches!(self.hover, Some(Hit::BtForget(j) | Hit::BtAction(j)) if j == i) || (r.contains(self.pointer.0, self.pointer.1) && self.hover.is_some());
        if d.connected {
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, 0.16));
        } else if hovered {
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, 0.1));
        }
        svg::icon(cr, d.glyph(), r.x + 12.0, r.y + 16.0, 22.0, p.fg, false);
        let tx = r.x + 12.0 + 22.0 + 12.0;
        let connecting = busy == Some(BtBusy::Connecting);
        let pairing = busy == Some(BtBusy::Pairing);
        let label = if d.connected {
            "Disconnect"
        } else if pairing {
            "Cancel"
        } else if d.paired {
            "Connect"
        } else {
            "Pair"
        };
        let mut bx = r.x + r.w - 10.0;
        let (bh, bw) = widgets::button_size(cr, label, "", true);
        bx -= bw;
        let ar = Rect { x: bx, y: r.y + (r.h - bh) / 2.0, w: bw, h: bh };
        widgets::button(cr, ar, label, "", true, d.connected, !connecting, hv!(self, Hit::BtAction(i)), p);
        if !connecting {
            self.hits.push((Hit::BtAction(i), ar));
        }
        if d.paired && !connecting && !pairing {
            let (sh, sw) = widgets::button_size(cr, "Send", "", true);
            bx -= 8.0 + sw;
            let sr = Rect { x: bx, y: r.y + (r.h - sh) / 2.0, w: sw, h: sh };
            widgets::button(cr, sr, "Send", "", true, false, true, hv!(self, Hit::BtSend(i)), p);
            self.hits.push((Hit::BtSend(i), sr));
        }
        if d.paired && !d.connected && !connecting {
            let (fh, fw) = widgets::button_size(cr, "Forget", "", true);
            bx -= 12.0 + fw;
            let fr = Rect { x: bx, y: r.y + (r.h - fh) / 2.0, w: fw, h: fh };
            widgets::button(cr, fr, "Forget", "", true, false, true, hv!(self, Hit::BtForget(i)), p);
            self.hits.push((Hit::BtForget(i), fr));
        }
        let nf = font(14.0, if d.connected { "Bold" } else { "" });
        text_v(cr, tx, r.y + 9.0, 20.0, &d.name, &nf, p.fg, Some(bx - 12.0 - tx));
        let status = if failed {
            "Pairing failed".to_string()
        } else if connecting {
            "Connecting…".into()
        } else if pairing {
            "Pairing…".into()
        } else if d.connected {
            format!("Connected{}", d.battery.map(|b| format!("  ·  {b}%")).unwrap_or_default())
        } else if d.paired {
            "Paired".into()
        } else {
            "Available".into()
        };
        cb::draw_pango_text(cr, tx, r.y + 30.0, &status, &font(11.0, ""), if failed { p.error } else { alpha(p.fg, 0.65) }, None);
    }

    // ── Footer ───────────────────────────────────────────────────────────

    fn sink_list_height(&self) -> f64 {
        let n = self.snap.sinks.len() as f64;
        let apps = crate::pw_watch::mixer().len() as f64;
        let mix = if apps > 0.0 { APPS_HEAD + apps * 40.0 } else { 0.0 };
        // The "Sound settings" row stays below the (capped) lists.
        (n * 36.0 + (n - 1.0).max(0.0) * 4.0 + mix).min(260.0) + 4.0 + MORE_ROW
    }

    fn draw_footer(&mut self, cr: *mut Cairo, r: Rect, sink_h: f64, p: &Pal) {
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, 0.08));
        let x = r.x + 12.0;
        let w = r.w - 24.0;
        let mut y = r.y + 12.0;
        if sink_h > 0.5 {
            // Sinks and apps above, the "Sound settings" row at the bottom of the panel.
            let lists_h = (sink_h - 4.0 - MORE_ROW).max(0.0);
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, x, y, w, lists_h);
                cb::cairo_clip(cr);
            }
            let mut sy = y;
            let sinks = self.snap.sinks.clone();
            for (i, s) in sinks.iter().enumerate() {
                let row = Rect { x, y: sy, w, h: 36.0 };
                let hov = self.hover == Some(Hit::Sink(i));
                if hov {
                    cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, RADIUS, p.fg);
                }
                let c = if hov { p.inv } else { p.fg };
                cb::stroke_circle(cr, row.x + 8.0 + 7.0, row.y + 18.0, 6.5, c, 1.0);
                if s.is_default {
                    cb::fill_circle(cr, row.x + 15.0, row.y + 18.0, 4.0, c);
                }
                svg::icon(cr, model::sink_glyph(&s.name, &s.description), row.x + 30.0, row.y + 10.0, 16.0, c, false);
                text_v(cr, row.x + 54.0, row.y, 36.0, &s.description, &font(13.0, ""), c, Some(row.w - 62.0));
                if sy + 36.0 <= y + lists_h + 0.5 {
                    self.hits.push((Hit::Sink(i), row));
                }
                sy += 40.0;
            }
            // Apps playing sound: a slider and a mute button each.
            self.mixer = crate::pw_watch::mixer();
            if !self.mixer.is_empty() {
                text_v(cr, x + 4.0, sy + 2.0, 18.0, "Apps", &font(11.0, "Bold"), alpha(p.fg, 0.6), None);
                sy += APPS_HEAD;
                let mixer = self.mixer.clone();
                for (i, (name, icon, _, vol, muted)) in mixer.iter().enumerate() {
                    let row = Rect { x, y: sy, w, h: 36.0 };
                    crate::toasts::draw_notif_icon(cr, row.x + 2.0, row.y + 6.0, 24.0, name, (!icon.is_empty()).then_some(icon.as_str()), None, icon, p.fg, 1.0);
                    text_v(cr, row.x + 34.0, row.y, 36.0, name, &font(12.0, ""), p.fg, Some(86.0));
                    let v = match &self.app_local {
                        Some((a, v, t)) if a == name && t.elapsed() < SLIDER_HOLD => *v,
                        _ => *vol,
                    };
                    let slider = Rect { x: row.x + 126.0, y: row.y + 2.0, w: row.w - 126.0 - 40.0, h: 32.0 };
                    widgets::slider(cr, slider, (v / 1.0).min(1.0), p);
                    let mute = Rect { x: row.x + row.w - 32.0, y: row.y + 2.0, w: 32.0, h: 32.0 };
                    if self.hover == Some(Hit::AppMute(i)) {
                        cb::fill_circle(cr, mute.x + 16.0, mute.y + 16.0, 16.0, alpha(p.fg, 0.2));
                    }
                    svg::icon(cr, "speaker", mute.x + 7.0, mute.y + 7.0, 18.0, alpha(p.fg, if *muted { 0.5 } else { 1.0 }), *muted);
                    if sy + 36.0 <= y + lists_h + 0.5 {
                        self.hits.push((Hit::AppVolume(i), slider));
                        self.hits.push((Hit::AppMute(i), mute));
                    }
                    sy += 40.0;
                }
            }
            unsafe { cb::cairo_restore(cr) };
            // Everything else (inputs, ports, balance, profiles, moving apps) on the Sound page.
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, x, y, w, sink_h);
                cb::cairo_clip(cr);
            }
            let row = Rect { x, y: y + sink_h - MORE_ROW, w, h: MORE_ROW };
            let hov = hv!(self, Hit::AudioPage);
            if hov > 0.0 {
                cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, RADIUS, alpha(p.fg, 0.14 * hov));
            }
            svg::icon(cr, "gear", row.x + 8.0, row.y + 10.0, 16.0, p.fg, false);
            text_v(cr, row.x + 54.0, row.y, row.h, "Sound settings", &font(13.0, ""), p.fg, Some(row.w - 62.0));
            if row.y >= y - 0.5 {
                self.hits.push((Hit::AudioPage, row));
            }
            unsafe { cb::cairo_restore(cr) };
            y += sink_h;
        }
        y += 10.0;
        // Volume row.
        svg::icon(cr, "volume", x, y + 5.0, 22.0, p.fg, false);
        let vol = Rect { x: x + 34.0, y, w: w - 34.0 - 12.0 - 32.0, h: 32.0 };
        widgets::slider(cr, vol, self.volume() as f64 / 100.0, p);
        self.hits.push((Hit::Volume, vol));
        let chev = Rect { x: x + w - 32.0, y, w: 32.0, h: 32.0 };
        if self.hover == Some(Hit::SinkToggle) {
            cb::fill_circle(cr, chev.x + 16.0, chev.y + 16.0, 16.0, alpha(p.fg, 0.2));
        }
        // Menu opens upward: ^ when closed, v when open.
        svg::icon_rotated(cr, "chevron-down", chev.x + 7.0, chev.y + 7.0, 18.0, p.fg, 180.0 * (1.0 - self.sink_t.value()));
        self.hits.push((Hit::SinkToggle, chev));
        y += 32.0 + 10.0;
        // Brightness row.
        svg::icon(cr, "sun", x, y + 5.0, 22.0, p.fg, false);
        let bri = Rect { x: x + 34.0, y, w: w - 34.0 - 12.0 - 32.0, h: 32.0 };
        widgets::slider(cr, bri, self.brightness() as f64 / 100.0, p);
        self.hits.push((Hit::Brightness, bri));
    }

    // ── Input ────────────────────────────────────────────────────────────

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        self.hits.iter().rev().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)
    }

    fn hit_rect(&self, h: Hit) -> Option<Rect> {
        self.hits.iter().find(|(k, _)| *k == h).map(|(_, r)| *r)
    }

    /// True when the pointer is over something clickable (for the cursor shape).
    pub fn over_clickable(&self) -> bool {
        self.hover.is_some()
    }

    pub fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        match self.drag {
            Some(Drag::Volume) => {
                if let Some(r) = self.hit_rect(Hit::Volume) {
                    let v = (widgets::slider_value(r, x) * 100.0).round() as u32;
                    self.vol_local = Some((v, Instant::now()));
                    self.send(StatusRequest::SetVolume(v));
                }
                return true;
            }
            Some(Drag::AppVolume(i)) => {
                if let (Some(r), Some((name, _, ids, _, _))) = (self.hit_rect(Hit::AppVolume(i)), self.mixer.get(i).cloned()) {
                    let v = widgets::slider_value(r, x);
                    self.app_local = Some((name, v, Instant::now()));
                    crate::pw_watch::set_volume(&ids, v);
                }
                return true;
            }
            Some(Drag::Audio(h)) => {
                self.audio_drag(h, x);
                return true;
            }
            Some(Drag::Brightness) => {
                if let Some(r) = self.hit_rect(Hit::Brightness) {
                    let v = ((widgets::slider_value(r, x) * 100.0).round() as u32).max(1);
                    self.bri_local = Some((v, Instant::now()));
                    self.send(StatusRequest::SetBrightness(v));
                }
                return true;
            }
            Some(Drag::Entry(_, sx)) => {
                self.swipe_dx = (x - sx).max(0.0);
                return true;
            }
            None => {}
        }
        let h = self.hit_at(x, y);
        // Pill hover depends on the exact position inside the group head.
        let changed = h != self.hover || matches!(h, Some(Hit::GroupHead(_)));
        self.hover = h;
        changed
    }

    pub fn leave(&mut self) -> bool {
        self.pointer = (-1.0, -1.0);
        self.hover.take().is_some()
    }

    pub fn press(&mut self, x: f64, y: f64) -> Effect {
        self.pointer = (x, y);
        let h = self.hit_at(x, y);
        self.pressed = h;
        match h {
            Some(Hit::Volume) => {
                self.drag = Some(Drag::Volume);
                self.motion(x, y);
                Effect::Redraw
            }
            Some(Hit::Brightness) => {
                self.drag = Some(Drag::Brightness);
                self.motion(x, y);
                Effect::Redraw
            }
            Some(Hit::AppVolume(i)) => {
                self.drag = Some(Drag::AppVolume(i));
                self.motion(x, y);
                Effect::Redraw
            }
            Some(h @ (Hit::AudioVol(_) | Hit::AudioBal(_))) => {
                self.drag = Some(Drag::Audio(h));
                self.motion(x, y);
                Effect::Redraw
            }
            Some(h @ (Hit::ReplyField(_) | Hit::PwField | Hit::VpnFilter | Hit::NotifFilter)) => {
                self.place_caret(h, x);
                Effect::Redraw
            }
            Some(Hit::Entry(nid)) => {
                self.drag = Some(Drag::Entry(nid, x));
                self.swipe_dx = 0.0;
                Effect::None
            }
            _ => Effect::None,
        }
    }

    pub fn release(&mut self) -> Effect {
        let pressed = self.pressed.take();
        match self.drag.take() {
            Some(Drag::Volume) | Some(Drag::Brightness) | Some(Drag::AppVolume(_)) | Some(Drag::Audio(_)) => return Effect::Redraw,
            Some(Drag::Entry(nid, _)) => {
                let width = self.hit_rect(Hit::Entry(nid)).map(|r| r.w).unwrap_or(340.0);
                if self.swipe_dx > width * 0.35 {
                    self.dismiss(nid);
                    // Keep the swipe offset as the slide-out start.
                    self.drag = None;
                    return Effect::Redraw;
                }
                if self.swipe_dx > 2.0 {
                    self.swipe_dx = 0.0;
                    return Effect::Redraw;
                }
            }
            None => {}
        }
        let here = self.hit_at(self.pointer.0, self.pointer.1);
        let Some(h) = pressed.filter(|p| Some(*p) == here || matches!(p, Hit::Entry(_))) else { return Effect::None };
        self.activate(h)
    }

    /// A click in a text field puts the caret there.
    fn place_caret(&mut self, h: Hit, px: f64) {
        let Some(&(_, x, w, font)) = self.text_geo.iter().find(|(k, ..)| *k == h) else { return };
        let q = match h {
            Hit::ReplyField(id) => match self.reply.as_mut() {
                Some((r, q)) if *r == id => q,
                _ => return,
            },
            Hit::PwField => &mut self.pw,
            Hit::NotifFilter => &mut self.notif_filter,
            _ => &mut self.vpn_filter,
        };
        // The password shows one dot per character.
        let shown = if h == Hit::PwField { "\u{2022}".repeat(q.text.chars().count()) } else { q.text.clone() };
        let ci = crate::launcher::text_index_at(&shown, q.caret(), font, x, w, px);
        q.set_caret(ci);
    }

    fn dismiss(&mut self, nid: u32) {
        if !self.dismissing.iter().any(|(n, _)| *n == nid) {
            self.dismissing.push((nid, Instant::now()));
        }
    }

    fn activate(&mut self, h: Hit) -> Effect {
        let nets = self.networks();
        match h {
            Hit::Gear => return Effect::OpenSettings,
            Hit::Profile => {
                if let Some((active, list)) = &mut self.snap.power_profile {
                    if let Some(next) = list.iter().position(|x| x == active).map(|i| list[(i + 1) % list.len()].clone()).or_else(|| list.first().cloned()) {
                        *active = next.clone();
                        self.send(StatusRequest::SetPowerProfile(next));
                    }
                }
            }
            Hit::Tile(TILE_CAFFEINE) => self.daemon(IpcCommand::Caffeinate(Some(!self.snap.flags.caffeinate))),
            Hit::Tile(TILE_GAME) => self.daemon(IpcCommand::GameMode(Some(!self.snap.flags.game_mode))),
            Hit::Tile(t) => self.set_tab(t),
            Hit::Focus => {
                self.focus_open = !self.focus_open;
                if self.focus_open {
                    let c = prism_common::Config::load();
                    self.focus_cfg = (c.focus_on_screen_share, c.focus_on_fullscreen, c.focus_allowed_apps);
                }
            }
            Hit::FocusShare | Hit::FocusFull | Hit::FocusApp(_) => self.focus_change(h),
            Hit::Silent => {
                self.snap.flags.silent = !self.snap.flags.silent;
                self.daemon(IpcCommand::Notif(NotifAction::Silent(Some(self.snap.flags.silent))));
            }
            Hit::ClearAll => {
                self.snap.notifications.clear();
                self.expanded = None;
                self.daemon(IpcCommand::Notif(NotifAction::Clear));
            }
            Hit::GroupHead(gi) => {
                let key = self.groups().get(gi).map(|g| g.key.clone());
                // Opening one group closes the other; a click on the open one closes it.
                let was = self.expanded.take();
                if let Some(old) = &was {
                    self.show_all.remove(old);
                }
                if key != was {
                    self.expanded = key;
                }
            }
            Hit::TimerPreset(secs) => self.timer(TimerAction::Start(secs as u64)),
            Hit::TimerAdd(secs) => self.timer(TimerAction::Add(secs as i64)),
            Hit::TimerPause => {
                let a = if self.snap.flags.timer.end_ms.is_some() { TimerAction::Pause } else { TimerAction::Resume };
                self.timer(a);
            }
            Hit::TimerCancel => self.timer(TimerAction::Cancel),
            Hit::SwToggle => self.timer(TimerAction::StopwatchToggle),
            Hit::SwReset => self.timer(TimerAction::StopwatchReset),
            Hit::GroupMore(gi) => {
                if let Some(g) = self.groups().get(gi) {
                    self.show_all.insert(g.key.clone());
                }
            }
            Hit::GroupClear(gi) => {
                if let Some(g) = self.groups().get(gi) {
                    let app = g.items[0].app_name.clone();
                    let key = g.key.clone();
                    self.snap.notifications.retain(|n| n.app_name.to_lowercase() != key);
                    self.daemon(IpcCommand::Notif(NotifAction::ClearApp(app)));
                }
            }
            Hit::EntryCopy(nid) => {
                if let Some(n) = self.snap.notifications.iter().find(|n| n.id == nid) {
                    let body = plain(&n.body);
                    let text = if body.is_empty() { n.summary.clone() } else { format!("{}\n{}", n.summary, body) };
                    self.send(StatusRequest::Copy(text));
                }
            }
            Hit::EntryClose(nid) => self.dismiss(nid),
            Hit::EntrySnooze(nid) => {
                self.snoozing.push(nid);
                self.dismiss(nid);
            }
            Hit::ReplyField(_) | Hit::EntryReveal(_) | Hit::VpnFilter | Hit::NotifFilter | Hit::FocusKnob(_) => {}
            Hit::ReplySend(nid) => return self.send_reply(nid),
            Hit::EntryAction(nid, i) => {
                if let Some(key) = self.snap.notifications.iter().find(|n| n.id == nid).and_then(|n| n.actions.get(i)).map(|a| a.0.clone()) {
                    if key == INLINE_REPLY {
                        // Type the answer here; the field toggles.
                        self.reply = match self.reply.take() {
                            Some((id, _)) if id == nid => None,
                            _ => Some((nid, Query::default())),
                        };
                        return Effect::Redraw;
                    }
                    // prismd tells the app and removes the notification. The centre closes only
                    // if the app answers with a window (opens or raises one), not for actions
                    // like "Mark as read".
                    self.action_watch = Some(crate::window_watch::watch(std::time::Duration::from_secs(5), Some(self.waker.clone())));
                    crate::toasts::invoke_action(nid, key);
                    self.snap.notifications.retain(|n| n.id != nid);
                    return Effect::Redraw;
                }
            }
            Hit::Entry(_) => return Effect::None,
            Hit::Volume | Hit::Brightness => {}
            Hit::SinkToggle => {
                self.sink_open = !self.sink_open;
                self.sink_t.set(if self.sink_open { 1.0 } else { 0.0 }, ANIM_MED_MS);
                if self.sink_open {
                    self.send(StatusRequest::RefreshAll);
                }
            }
            Hit::AppVolume(_) => {}
            Hit::AudioPage => {
                self.sink_open = false;
                self.sink_t.set(0.0, ANIM_MED_MS);
                self.set_tab(TAB_AUDIO);
            }
            Hit::AudioSec(_) | Hit::AudioDefault(_) | Hit::AudioVol(_) | Hit::AudioBal(_) | Hit::AudioMute(_) | Hit::AudioPick(_) | Hit::AudioChoice(_) => self.audio_activate(h),
            Hit::AppMute(i) => {
                if let Some((_, _, ids, _, muted)) = self.mixer.get(i) {
                    crate::pw_watch::set_mute(ids, !muted);
                }
            }
            Hit::Sink(i) => {
                if let Some(s) = self.snap.sinks.get(i) {
                    self.send(StatusRequest::SetDefaultSink(s.id.clone()));
                }
                self.sink_open = false;
                self.sink_t.set(0.0, ANIM_MED_MS);
            }
            Hit::EthHeader => {
                self.eth_open = !self.eth_open;
                self.eth_t.set(if self.eth_open { 1.0 } else { 0.0 }, ANIM_MED_MS);
            }
            Hit::Detail(i) => {
                if let Some(v) = self.detail_values.get(i) {
                    self.send(StatusRequest::Copy(v.clone()));
                }
            }
            Hit::WifiSwitch => self.send(StatusRequest::SetWifi(!self.snap.net.wifi_enabled)),
            Hit::WifiRefresh => {
                self.last_wifi_scan = Some(Instant::now());
                self.send(StatusRequest::ScanWifi);
            }
            Hit::NetRow(i) => {
                if let Some(n) = nets.get(i) {
                    if n.active {
                        self.toggle_expanded(&n.ssid);
                    } else {
                        self.request_connect(n);
                    }
                }
            }
            Hit::NetConnect(i) => {
                if let Some(n) = nets.get(i) {
                    if n.active {
                        self.send(StatusRequest::DisconnectWifi);
                    } else {
                        self.request_connect(n);
                    }
                }
            }
            Hit::NetForget(i) => {
                if let Some(n) = nets.get(i) {
                    self.send(StatusRequest::ForgetWifi(n.ssid.clone()));
                }
            }
            Hit::PwField => {}
            Hit::PwJoin => self.submit_password(),
            Hit::BtSwitch => self.send(StatusRequest::SetBluetooth(!self.snap.bt.powered)),
            Hit::Discoverable => self.send(StatusRequest::SetDiscoverable(!self.snap.bt.discoverable)),
            Hit::BtRefresh => self.send(StatusRequest::ScanBluetooth),
            Hit::BtForget(i) => {
                if let Some(d) = self.snap.bt.devices.get(i) {
                    self.send(StatusRequest::ForgetBluetooth(d.mac.clone()));
                }
            }
            Hit::BtSend(i) => {
                if let Some(d) = self.snap.bt.devices.get(i).cloned() {
                    send_files_bluetooth(d.mac, d.name);
                    return Effect::Close;
                }
            }
            Hit::VpnImport => {
                let tx = self.status.tx.clone();
                std::thread::spawn(move || match crate::portal::pick_files("Import VPN profiles (.ovpn, WireGuard .conf)") {
                    Ok(files) if !files.is_empty() => {
                        let _ = tx.send(StatusRequest::ImportVpn(files));
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("[prism-ui/vpn] file dialog: {e}"),
                });
            }
            Hit::VpnRow(i) => {
                if let Some(v) = self.vpn_list().get(i).cloned() {
                    match (v.state, v.active) {
                        (model::VpnState::Off, _) => self.send(StatusRequest::VpnUp(v.path)),
                        (_, Some(a)) => self.send(StatusRequest::VpnDown(v.path, a)),
                        _ => {}
                    }
                }
            }
            Hit::BtAction(i) => {
                if let Some(d) = self.snap.bt.devices.get(i).cloned() {
                    let req = match (d.connected, self.snap.bt.busy(&d.mac), d.paired) {
                        (true, _, _) => StatusRequest::DisconnectBluetooth(d.mac),
                        (false, Some(BtBusy::Pairing), _) => StatusRequest::CancelPair(d.mac),
                        (false, _, true) => StatusRequest::ConnectBluetooth(d.mac),
                        (false, _, false) => StatusRequest::PairBluetooth(d.mac),
                    };
                    self.send(req);
                }
            }
            Hit::Unnamed => self.show_unnamed = !self.show_unnamed,
        }
        Effect::Redraw
    }

    fn toggle_expanded(&mut self, ssid: &str) {
        if self.expanded_ssid.as_deref() == Some(ssid) {
            self.expanded_ssid = None;
            self.frozen = None;
        } else {
            self.expanded_ssid = Some(ssid.to_string());
            self.frozen = Some(self.snap.net.networks.clone());
            self.pw.clear();
        }
    }

    fn request_connect(&mut self, n: &Net) {
        if self.snap.net.connecting.is_some() {
            return;
        }
        if n.known || !n.secure {
            self.send(StatusRequest::ConnectWifi { ssid: n.ssid.clone(), password: None });
        } else if self.expanded_ssid.as_deref() != Some(n.ssid.as_str()) {
            self.toggle_expanded(&n.ssid);
        }
    }

    fn submit_password(&mut self) {
        let Some(ssid) = self.expanded_ssid.clone() else { return };
        if self.pw.text.is_empty() {
            return;
        }
        let pw = std::mem::take(&mut self.pw).text;
        self.send(StatusRequest::ConnectWifi { ssid, password: Some(pw) });
        self.expanded_ssid = None;
        self.frozen = None;
    }

    /// Password entry is active: a non-connected network row is expanded on the network page.
    fn typing(&self) -> bool {
        self.tab == 1
            && self.expanded_ssid.as_ref().map(|s| !self.networks().iter().any(|n| &n.ssid == s && n.active)).unwrap_or(false)
    }

    pub fn scroll(&mut self, x: f64, y: f64, delta: f64) -> bool {
        // Scroll the page under the pointer (the footer does not scroll).
        let _ = (x, y);
        self.scroll[self.tab].by(delta)
    }

    fn send_reply(&mut self, nid: u32) -> Effect {
        let Some((id, q)) = self.reply.take_if(|(id, q)| *id == nid && !q.text.trim().is_empty()) else { return Effect::None };
        crate::toasts::send_reply(id, q.text);
        self.snap.notifications.retain(|n| n.id != id);
        Effect::Redraw
    }

    pub fn key(&mut self, special: u32, typed: Option<char>, ctrl: bool) -> Effect {
        // An open reply field takes the keyboard; Esc closes the field, not the centre.
        if let Some((nid, q)) = self.reply.as_mut() {
            let nid = *nid;
            match special {
                ESC => self.reply = None,
                ENTER => return self.send_reply(nid),
                _ => {
                    if q.key(special, typed, ctrl).is_none() {
                        return Effect::None;
                    }
                }
            }
            return Effect::Redraw;
        }
        if special == ESC {
            if self.sink_open {
                self.sink_open = false;
                self.sink_t.set(0.0, ANIM_MED_MS);
                return Effect::Redraw;
            }
            if self.tab == TAB_AUDIO && self.audio_pick.is_some() {
                self.audio_pick = None;
                return Effect::Redraw;
            }
            if self.tab == TAB_VPN && !self.vpn_filter.text.is_empty() {
                self.vpn_filter.clear();
                return Effect::Redraw;
            }
            if self.tab == TAB_NOTIFS && self.focus_open {
                self.focus_open = false;
                return Effect::Redraw;
            }
            if self.tab == TAB_NOTIFS && !self.notif_filter.text.is_empty() {
                self.notif_filter.clear();
                self.scroll[TAB_NOTIFS] = Default::default();
                return Effect::Redraw;
            }
            if self.tab == 1 && self.expanded_ssid.is_some() {
                self.expanded_ssid = None;
                self.frozen = None;
                self.pw.clear();
                return Effect::Redraw;
            }
            return Effect::Close;
        }
        if self.tab == TAB_NOTIFS && !self.snap.notifications.is_empty() {
            if let Some(changed) = self.notif_filter.key(special, typed, ctrl) {
                if changed {
                    self.scroll[TAB_NOTIFS] = Default::default();
                }
                return Effect::Redraw;
            }
        }
        if self.tab == TAB_VPN {
            match self.vpn_filter.key(special, typed, ctrl) {
                Some(true) => self.scroll[TAB_VPN] = Default::default(),
                Some(false) => {}
                None => return Effect::None,
            }
            return Effect::Redraw;
        }
        if self.typing() {
            match special {
                ENTER => self.submit_password(),
                _ => {
                    if self.pw.key(special, typed, ctrl).is_none() {
                        return Effect::None;
                    }
                }
            }
            return Effect::Redraw;
        }
        Effect::None
    }
}

fn clip_rect(r: Rect, c: Rect) -> Rect {
    let y0 = r.y.max(c.y);
    let y1 = (r.y + r.h).min(c.y + c.h);
    Rect { x: r.x, y: y0, w: r.w, h: (y1 - y0).max(0.0) }
}

fn details_height(rows: &[DetailRow]) -> f64 {
    rows.iter().map(|r| if matches!(r, DetailRow::Header(_)) { 32.0 } else { 28.0 }).sum::<f64>()
}

/// Body text without markup.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&").trim().to_string()
}

/// One-line preview of a body.
fn plain_line(s: &str) -> String {
    plain(s).split_whitespace().collect::<Vec<_>>().join(" ")
}

#[allow(clippy::too_many_arguments)]
fn preview_line(cr: *mut Cairo, x: f64, y: f64, w: f64, title: &str, text: &str, dim: f64, bold: bool, p: &Pal) {
    let mut tx = x;
    if !title.is_empty() {
        let f = font(14.0, if bold { "Bold" } else { "" });
        let tw = text_v(cr, x, y, 19.0, title, &f, alpha(p.fg, dim), Some(w * 0.62));
        tx += tw + 6.0;
    }
    if !text.is_empty() && tx < x + w {
        text_v(cr, tx, y, 19.0, text, &font(13.0, ""), alpha(p.fg, 0.6 * dim), Some(x + w - tx));
    }
}


/// QML NotificationManager.timeLabel for a unix timestamp (seconds).
fn time_label(stamp: u64) -> String {
    if stamp == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let diff = now.saturating_sub(stamp);
    if diff < 60 {
        return "Now".into();
    }
    if diff < 3600 {
        return format!("{}m", diff / 60);
    }
    let day = |t: u64| -> (i32, i32, i32, i32) {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let tt = t as libc::time_t;
        unsafe { libc::localtime_r(&tt, &mut tm) };
        (tm.tm_year, tm.tm_yday, tm.tm_mday, tm.tm_mon)
    };
    let (ny, nd, _, _) = day(now);
    let (sy, sd, smday, smon) = day(stamp);
    if (ny, nd) == (sy, sd) {
        return format!("{}h", diff / 3600);
    }
    let (yy, yd, _, _) = day(now.saturating_sub(86400));
    if (yy, yd) == (sy, sd) {
        return "Yesterday".into();
    }
    if diff < 7 * 86400 {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let tt = stamp as libc::time_t;
        unsafe { libc::localtime_r(&tt, &mut tm) };
        return ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][tm.tm_wday.clamp(0, 6) as usize].into();
    }
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {}", smday, months[smon.clamp(0, 11) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_labels() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(time_label(now - 5), "Now");
        assert_eq!(time_label(now - 600), "10m");
        assert_eq!(time_label(0), "");
        assert!(!time_label(now - 40 * 86400).is_empty());
    }

    #[test]
    fn plain_strips_markup() {
        assert_eq!(plain_line("<b>a</b>\n  b &amp; c"), "a b & c");
    }
}

/// Pick files with the system dialog and send them to a Bluetooth device (prismd, OBEX push).
pub fn send_files_bluetooth(mac: String, name: String) {
    std::thread::spawn(move || match crate::portal::pick_files(&format!("Send to {name}")) {
        Ok(files) if !files.is_empty() => {
            if let Err(e) = prism_common::ipc::request(&IpcCommand::BluetoothSend { address: mac, files }) {
                eprintln!("[prism-ui] bluetooth send: {e}");
            }
        }
        Ok(_) => {}
        Err(e) => eprintln!("[prism-ui] file dialog: {e}"),
    });
}
