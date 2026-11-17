//! Notification center / control centre (port of QML NotificationCenter.qml and its Nc* pages).
//!
//!   ┌ Control Centre ─────────── 🔋 ⚙ ┐
//!   │ [Network] [BT] [Notifications] [☕]│  tiles: the selected one expands, the others are icons
//!   │                                  │
//!   │  page: notifications / network / bluetooth (cross-fade)
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

use prism_common::ipc::{IpcCommand, NotifAction};
use prism_common::notifications::Notification;
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::status::{self, StatusHandle, StatusRequest, StatusSnapshot};
use crate::svg;
use crate::ui::{Style, ANIM_MED_MS};
use crate::waker::Waker;
use model::{BtBusy, DetailRow, Net};
use widgets::{alpha, font, text_size, text_v, Pal, Tween, RADIUS};

pub const CARD_W: f64 = 400.0;
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
const BACKSPACE: u32 = 14;
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
fn entry_actions(cr: *mut Cairo, tx: f64, top: f64, tw: f64, n: &Notification) -> (Rect, Rect, Vec<Rect>, f64) {
    const H: f64 = 28.0;
    let gap = 6.0;
    let acts: Vec<&str> = n.actions.iter().take(3).map(|(_, l)| l.as_str()).collect();
    if acts.is_empty() {
        let bw = (tw - gap) / 2.0;
        return (Rect { x: tx, y: top, w: bw, h: H }, Rect { x: tx + bw + gap, y: top, w: bw, h: H }, Vec::new(), H);
    }
    let small = 40.0;
    let natural: Vec<f64> = acts.iter().map(|l| widgets::button_size(cr, l, "", true).1).collect();
    let beside = tw - 2.0 * (small + gap);
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
    let cx0 = tx + tw - 2.0 * small - gap;
    (Rect { x: cx0, y: icons_y, w: small, h: H }, Rect { x: cx0 + small + gap, y: icons_y, w: small, h: H }, rects, rows_h)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Hit {
    Gear,
    Tile(usize),
    Silent,
    ClearAll,
    GroupHead(usize),
    GroupClear(usize),
    EntryCopy(u32),
    EntryClose(u32),
    /// (notification id, action index)
    EntryAction(u32, usize),
    Entry(u32),
    Volume,
    Brightness,
    SinkToggle,
    Sink(usize),
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
    Unnamed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Volume,
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
    page: [Tween; 3],
    tile_w: [Tween; 4],
    hits: Vec<(Hit, Rect)>,
    hover: Option<Hit>,
    /// Hover fades of the hit targets (see `hv!`).
    hover_fade: crate::ui::Fades<Hit>,
    pointer: (f64, f64),
    pressed: Option<Hit>,
    drag: Option<Drag>,
    swipe_dx: f64,
    scroll: [crate::ui::Scroller; 3],
    /// Animation time scale from the last draw (None: animations off).
    time_scale: Option<f64>,
    expanded: Option<String>,
    expand: HashMap<String, Tween>,
    /// Entries sliding out after a dismiss: nid -> start.
    dismissing: Vec<(u32, Instant)>,
    sink_open: bool,
    sink_t: Tween,
    eth_open: bool,
    eth_t: Tween,
    expanded_ssid: Option<String>,
    /// Network list frozen while a row is expanded (QML freezeList).
    frozen: Option<Vec<Net>>,
    pw: String,
    show_unnamed: bool,
    vol_local: Option<(u32, Instant)>,
    bri_local: Option<(u32, Instant)>,
    last_wifi_scan: Option<Instant>,
    knobs: [Tween; 3],
    detail_values: Vec<String>,
    spin_start: Instant,
}

impl NcView {
    pub fn new(waker: Waker) -> Self {
        let status = status::spawn(waker);
        let mut v = NcView {
            status,
            snap: StatusSnapshot::default(),
            tab: 0,
            page: [Tween::new(1.0), Tween::new(0.0), Tween::new(0.0)],
            tile_w: [Tween::new(44.0), Tween::new(44.0), Tween::new(0.0), Tween::new(44.0)],
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
            dismissing: Vec::new(),
            sink_open: false,
            sink_t: Tween::new(0.0),
            eth_open: true,
            eth_t: Tween::new(1.0),
            expanded_ssid: None,
            frozen: None,
            pw: String::new(),
            show_unnamed: false,
            vol_local: None,
            bri_local: None,
            last_wifi_scan: None,
            knobs: [Tween::new(0.0), Tween::new(0.0), Tween::new(0.0)],
            detail_values: Vec::new(),
            spin_start: Instant::now(),
        };
        v.tile_w[2] = Tween::new(f64::NAN); // set to the wide width on first draw
        v
    }

    fn send(&self, r: StatusRequest) {
        let _ = self.status.tx.send(r);
    }

    fn daemon(&self, c: IpcCommand) {
        self.send(StatusRequest::Daemon(c));
    }

    /// Take new snapshots and run timers. Returns true when a redraw is needed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(s) = self.status.rx.try_iter().last() {
            self.snap = s;
            changed = true;
        }
        let now = Instant::now();
        // Finished slide-outs: remove from the store.
        let done: Vec<u32> = self.dismissing.iter().filter(|(_, t)| now.duration_since(*t).as_millis() >= 390).map(|(n, _)| *n).collect();
        if !done.is_empty() {
            self.dismissing.retain(|(n, _)| !done.contains(n));
            for nid in done {
                self.snap.notifications.retain(|n| n.id != nid);
                self.daemon(IpcCommand::Notif(NotifAction::Dismiss(nid)));
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
            || self.expand.values().any(|t| t.running())
            || self.sink_t.running()
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
        for n in &self.snap.notifications {
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
        text_v(cr, x, y, 32.0, "Control Centre", &font(18.0, "Bold"), p.fg, Some(w - 120.0));
        let gear = Rect { x: x + w - 32.0, y, w: 32.0, h: 32.0 };
        let gh = self.hover == Some(Hit::Gear);
        if gh {
            cb::fill_circle(cr, gear.x + 16.0, gear.y + 16.0, 16.0, p.fg);
        }
        cb::stroke_circle(cr, gear.x + 16.0, gear.y + 16.0, 15.5, p.fg, 1.0);
        svg::icon(cr, "gear", gear.x + 7.0, gear.y + 7.0, 18.0, if gh { p.inv } else { p.fg }, false);
        self.hits.push((Hit::Gear, gear));
        if let Some(b) = &self.snap.battery {
            widgets::battery(cr, gear.x - 8.0, y, b.percent, b.charging, &p);
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

        for tab in 0..3 {
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
                0 => self.draw_notifications(cr, page, &p),
                1 => self.draw_network(cr, page, &p),
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
        let wide = (w - 3.0 * 44.0 - 3.0 * gap).max(44.0);
        for i in 0..4 {
            let target = if i == self.tab && i < 3 { wide } else { 44.0 };
            if self.tile_w[i].target().is_nan() {
                self.tile_w[i] = Tween::new(target);
            }
            self.tile_w[i].set(target, TILE_MS);
        }
        let net = &self.snap.net;
        let bt = &self.snap.bt;
        let bt_dev = bt.devices.iter().find(|d| d.connected);
        let flags = self.snap.flags;
        let count = self.snap.notifications.len();
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
        let specs = [
            widgets::TileSpec {
                icon: if net.ethernet_connected { "ethernet" } else { "wifi" },
                title: &net_title,
                subtitle: &net_sub,
                count: "",
                selected: self.tab == 1,
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
                selected: self.tab == 2,
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
                selected: self.tab == 0,
                on: false,
                dimmed: false,
                slashed: flags.silent,
                badge: count,
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
        ];
        // QML order: network, bluetooth, notifications, caffeinate.
        let mut tx = x;
        let mut hits = Vec::new();
        for (slot, spec) in specs.iter().enumerate() {
            let tab = [1, 2, 0, 3][slot];
            let r = Rect { x: tx, y, w: self.tile_w[tab].value(), h: 44.0 };
            widgets::tile(cr, r, spec, hv!(self, Hit::Tile(tab)), p);
            hits.push((Hit::Tile(tab), r));
            tx += r.w + gap;
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
        let (_, sw) = widgets::button_size(cr, "Silent", "bell", false);
        let (_, clw) = widgets::button_size(cr, "Clear", "close", false);
        let clear = Rect { x: r.x + r.w - clw, y: row_y, w: clw, h: row_h };
        let sil = Rect { x: clear.x - 8.0 - sw, y: row_y, w: sw, h: row_h };
        text_v(cr, r.x, row_y, row_h, &label, &font(14.0, ""), p.fg, Some(sil.x - r.x - 8.0));
        widgets::button(cr, sil, "Silent", "bell", false, silent, true, hv!(self, Hit::Silent), p);
        widgets::button(cr, clear, "Clear", "close", false, false, count > 0, hv!(self, Hit::ClearAll), p);
        self.hits.push((Hit::Silent, sil));
        if count > 0 {
            self.hits.push((Hit::ClearAll, clear));
        }

        let list = Rect { x: r.x, y: r.y, w: r.w, h: (row_y - 10.0 - r.y).max(0.0) };
        if count == 0 {
            self.empty_state(cr, list, "bell", silent, if silent { "Silent mode is on" } else { "No notifications" }, p);
            return;
        }
        // Collect the group layout first (immutable borrow of the snapshot).
        let groups: Vec<(String, Vec<Notification>)> = self.groups().into_iter().map(|g| (g.key, g.items.into_iter().cloned().collect())).collect();
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

    fn expand_t(&mut self, key: &str) -> f64 {
        let open = self.expanded.as_deref() == Some(key);
        let t = self.expand.entry(key.to_string()).or_insert_with(|| Tween::new(if open { 1.0 } else { 0.0 }));
        t.set(if open { 1.0 } else { 0.0 }, ANIM_MED_MS);
        t.value()
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_group(&mut self, cr: *mut Cairo, r: Rect, gi: usize, key: &str, items: &[Notification], p: &Pal) -> f64 {
        let e = self.expand_t(key);
        let latest = &items[0];
        let total = items.len();
        let critical = latest.urgency >= 2;
        let tx = r.x + 12.0 + 46.0 + 12.0;
        let tw = r.w - 24.0 - 46.0 - 12.0;

        // Head: pill row (20) + 3 + preview (collapses while expanded).
        let line_h = 19.0;
        let second = if total > 1 { true } else { !latest.body.is_empty() };
        let preview_full = line_h + if second { 2.0 + line_h } else { 0.0 };
        let preview_h = preview_full * (1.0 - e);
        let text_h = 20.0 + 3.0 + preview_h;
        let head_h = text_h.max(46.0);

        // Details (entries) height.
        let details_inner = self.entries_height(cr, r.w - 20.0, items);
        let details_h = (details_inner + 10.0) * e;
        let card_h = 12.0 + head_h + details_h + 12.0;

        let card = Rect { x: r.x, y: r.y, w: r.w, h: card_h };
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
        crate::toasts::draw_notif_icon(cr, head.x, head.y, 46.0, &latest.app_name, latest.icon.as_deref(), head_image, &latest.desktop_entry, p.fg, 1.0);

        // Pill row: app name · time · [count ⌄]
        let small = font(11.0, "");
        let time = time_label(latest.timestamp);
        let pill_label = if total > 1 { total.to_string() } else { String::new() };
        let pl_w = if pill_label.is_empty() { 0.0 } else { text_size(cr, &pill_label, &small).0 + 4.0 };
        let pill_w = pl_w + 12.0 + 14.0;
        let pill = Rect { x: tx + tw - pill_w, y: head.y, w: pill_w, h: 20.0 };
        let (time_w, _) = text_size(cr, &time, &small);
        let time_x = pill.x - 8.0 - time_w;
        text_v(cr, tx, head.y, 20.0, &latest.app_name, &small, alpha(p.fg, 0.65), Some(time_x - tx - 8.0));
        text_v(cr, time_x, head.y, 20.0, &time, &small, alpha(p.fg, 0.65), None);
        let ph = self.hover == Some(Hit::GroupHead(gi)) && pill.contains(self.pointer.0, self.pointer.1);
        cb::fill_rounded_rect(cr, pill.x, pill.y, pill.w, pill.h, 10.0, alpha(p.fg, if ph { 0.32 } else { 0.18 }));
        let mut px = pill.x + 7.0;
        if !pill_label.is_empty() {
            text_v(cr, px, pill.y, 20.0, &pill_label, &small, p.fg, None);
            px += pl_w;
        }
        svg::icon(cr, if self.expanded.as_deref() == Some(key) { "chevron-up" } else { "chevron-down" }, px, pill.y + 4.0, 12.0, p.fg, false);

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
            self.draw_entries(cr, card.x + 10.0, dy + 10.0, card.w - 20.0, gi, items, p);
            for h in self.hits[before..].iter_mut() {
                h.1 = clip_rect(h.1, clip);
            }
            self.hits.retain(|(_, r)| r.h > 0.0);
            unsafe { cb::cairo_restore(cr) };
        }
        card_h
    }

    fn entry_height(&self, cr: *mut Cairo, w: f64, n: &Notification) -> f64 {
        let cw = w - 20.0;
        let (_, sh) = text_size(cr, &n.summary, &font(14.0, "Bold"));
        let body = plain(&n.body);
        let bh = if body.is_empty() { 0.0 } else { cb::measure_pango_text(cr, &body, &font(13.0, ""), cw as i32, Some(8)).1 as f64 };
        let text = sh + if bh > 0.0 { 3.0 + bh } else { 0.0 };
        let ph = crate::toasts::preview_height(entry_preview(n), cw);
        let pic = if ph > 0.0 { 8.0 + ph } else { 0.0 };
        let (_, _, _, rows_h) = entry_actions(cr, 0.0, 0.0, cw, n);
        10.0 + text + pic + 8.0 + rows_h + 10.0
    }

    fn entries_height(&self, cr: *mut Cairo, w: f64, items: &[Notification]) -> f64 {
        let mut h = 0.0;
        for n in items {
            h += self.entry_height(cr, w, n) * self.collapse_factor(n.id) + 8.0;
        }
        if items.len() > 1 {
            h += 28.0;
        } else {
            h -= 8.0;
        }
        h
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

    #[allow(clippy::too_many_arguments)]
    fn draw_entries(&mut self, cr: *mut Cairo, x: f64, mut y: f64, w: f64, gi: usize, items: &[Notification], p: &Pal) {
        for n in items {
            let full_h = self.entry_height(cr, w, n);
            let k = self.collapse_factor(n.id);
            let h = full_h * k;
            // Slide right while dismissing or swiping.
            let (dx, a) = match self.dismissing.iter().find(|(id, _)| *id == n.id) {
                Some((_, t)) => {
                    let s = (t.elapsed().as_secs_f64() * 1000.0 / 180.0).clamp(0.0, 1.0);
                    let from = if matches!(self.drag, Some(Drag::Entry(id, _)) if id == n.id) { self.swipe_dx } else { 0.0 };
                    (from + (w - from) * crate::ui::in_cubic(s), 1.0 - s)
                }
                None if matches!(self.drag, Some(Drag::Entry(id, _)) if id == n.id) => (self.swipe_dx, 1.0),
                None => (0.0, 1.0),
            };
            if h > 0.5 {
                unsafe {
                    cb::cairo_save(cr);
                    cb::cairo_rectangle(cr, x, y, w, h);
                    cb::cairo_clip(cr);
                    cb::cairo_push_group(cr);
                }
                let cx = x + dx;
                cb::fill_rounded_rect(cr, cx, y, w, full_h, RADIUS, alpha(p.inv, 0.55));
                if n.urgency >= 2 {
                    cb::stroke_rounded_rect(cr, cx + 0.75, y + 0.75, w - 1.5, full_h - 1.5, RADIUS, p.error, 1.5);
                }
                let tx = cx + 10.0;
                let tw = w - 20.0;
                let small = font(11.0, "");
                let time = time_label(n.timestamp);
                let (tmw, _) = text_size(cr, &time, &small);
                let fs = font(14.0, "Bold");
                let (_, sh) = text_size(cr, &n.summary, &fs);
                text_v(cr, tx, y + 10.0, sh, &n.summary, &fs, p.fg, Some(tw - tmw - 8.0));
                text_v(cr, tx + tw - tmw, y + 10.0, sh, &time, &small, alpha(p.fg, 0.55), None);
                let body = plain(&n.body);
                let mut by = y + 10.0 + sh;
                if !body.is_empty() {
                    let (_, bh) = cb::draw_pango_text_wrapped(cr, tx, by + 3.0, &body, &font(13.0, ""), alpha(p.fg, 0.85), tw as i32, Some(8));
                    by += 3.0 + bh as f64;
                }
                // Screenshot-style preview picture across the entry.
                if let Some(path) = entry_preview(n) {
                    let ph = crate::toasts::preview_height(Some(path), tw);
                    cb::draw_image_cover(cr, path, tx, by + 8.0, tw, ph, 10.0);
                    cb::stroke_rounded_rect(cr, tx + 0.5, by + 8.5, tw - 1.0, ph - 1.0, 10.0, alpha(p.fg, 0.12), 1.0);
                    by += 8.0 + ph;
                }
                let acts: Vec<(String, String)> = n.actions.iter().take(3).cloned().collect();
                let (copy, close, action_rects, _) = entry_actions(cr, tx, by + 8.0, tw, n);
                for (i, (r, (_, label))) in action_rects.iter().zip(acts.iter()).enumerate() {
                    widgets::button(cr, *r, label, "", true, false, true, hv!(self, Hit::EntryAction(n.id, i)), p);
                }
                widgets::button(cr, copy, "", "copy", true, false, true, hv!(self, Hit::EntryCopy(n.id)), p);
                widgets::button(cr, close, "", "close", true, false, true, hv!(self, Hit::EntryClose(n.id)), p);
                unsafe {
                    cb::cairo_pop_group_to_source(cr);
                    cb::cairo_paint_with_alpha(cr, a);
                    cb::cairo_restore(cr);
                }
                if dx == 0.0 && k >= 1.0 {
                    self.hits.push((Hit::Entry(n.id), Rect { x, y, w, h }));
                    self.hits.push((Hit::EntryCopy(n.id), copy));
                    self.hits.push((Hit::EntryClose(n.id), close));
                    for (i, r) in action_rects.iter().enumerate() {
                        self.hits.push((Hit::EntryAction(n.id, i), *r));
                    }
                }
            }
            y += h + 8.0;
        }
        if items.len() > 1 {
            let label = format!("Clear {}", items.len());
            let (bh, bw) = widgets::button_size(cr, &label, "", true);
            let r = Rect { x: x + w - bw, y, w: bw, h: bh };
            widgets::button(cr, r, &label, "", true, false, true, hv!(self, Hit::GroupClear(gi)), p);
            self.hits.push((Hit::GroupClear(gi), r));
        }
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
                let f = font(14.0, "");
                let ix = field.x + 8.0;
                let iw = join.x - 8.0 - ix;
                if self.pw.is_empty() {
                    text_v(cr, ix, field.y, 44.0, "Password", &f, p.on_surface_variant, None);
                    cb::fill_rounded_rect(cr, ix, field.y + 13.0, 1.5, 18.0, 0.0, p.on_surface);
                } else {
                    let dots: String = "•".repeat(self.pw.chars().count());
                    let w = text_v(cr, ix, field.y, 44.0, &dots, &f, p.on_surface, Some(iw));
                    cb::fill_rounded_rect(cr, ix + w + 1.0, field.y + 13.0, 1.5, 18.0, 0.0, p.on_surface);
                }
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
        (n * 36.0 + (n - 1.0).max(0.0) * 4.0).min(170.0)
    }

    fn draw_footer(&mut self, cr: *mut Cairo, r: Rect, sink_h: f64, p: &Pal) {
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, RADIUS, alpha(p.fg, 0.08));
        let x = r.x + 12.0;
        let w = r.w - 24.0;
        let mut y = r.y + 12.0;
        if sink_h > 0.5 {
            unsafe {
                cb::cairo_save(cr);
                cb::cairo_rectangle(cr, x, y, w, sink_h);
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
                if sy + 36.0 <= y + sink_h + 0.5 {
                    self.hits.push((Hit::Sink(i), row));
                }
                sy += 40.0;
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
            Some(Drag::Volume) | Some(Drag::Brightness) => return Effect::Redraw,
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

    fn dismiss(&mut self, nid: u32) {
        if !self.dismissing.iter().any(|(n, _)| *n == nid) {
            self.dismissing.push((nid, Instant::now()));
        }
    }

    fn activate(&mut self, h: Hit) -> Effect {
        let nets = self.networks();
        match h {
            Hit::Gear => return Effect::OpenSettings,
            Hit::Tile(3) => self.daemon(IpcCommand::Caffeinate(Some(!self.snap.flags.caffeinate))),
            Hit::Tile(t) => self.set_tab(t),
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
                self.expanded = if self.expanded == key { None } else { key };
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
            Hit::EntryAction(nid, i) => {
                if let Some(key) = self.snap.notifications.iter().find(|n| n.id == nid).and_then(|n| n.actions.get(i)).map(|a| a.0.clone()) {
                    // prismd tells the app and removes the notification.
                    crate::toasts::invoke_action(nid, key);
                    self.snap.notifications.retain(|n| n.id != nid);
                    return Effect::Close;
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
        if self.pw.is_empty() {
            return;
        }
        let pw = std::mem::take(&mut self.pw);
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

    pub fn key(&mut self, special: u32, typed: Option<char>) -> Effect {
        if special == ESC {
            if self.sink_open {
                self.sink_open = false;
                self.sink_t.set(0.0, ANIM_MED_MS);
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
        if self.typing() {
            match (special, typed) {
                (ENTER, _) => self.submit_password(),
                (BACKSPACE, _) => {
                    self.pw.pop();
                }
                (_, Some(c)) if !c.is_control() => self.pw.push(c),
                _ => return Effect::None,
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
