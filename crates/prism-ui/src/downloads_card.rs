//! The Downloads card (`prism-ctl download toggle`, or a click on the downloads pill): Prism's
//! downloads with their progress, speed and connections, Pause / Resume / Cancel / Open / Show
//! in Folder / Remove, a field to paste a link into, and Clear finished. A worker thread asks
//! prismd for the list once a second (`IpcCommand::Download(List)` over prism.sock) and runs
//! the actions.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use prism_common::ipc::{get_socket_path, DownloadAction, DownloadInfo, DownloadRequest, IpcCommand, IpcResponse};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{Query, ENTER, ESC};
use crate::ui::{Scroller, Style};
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 640.0;
pub const CARD_H: f64 = 540.0;
const PAD: f64 = 22.0;
const ROW_H: f64 = 70.0;
const ROW_GAP: f64 = 8.0;

const PAUSE: &str = "M8 5v14M16 5v14";
const RESUME: &str = "M7 4.5v15L19.5 12z";
const CANCEL: &str = "M6 6l12 12M18 6 6 18";
const OPEN: &str = "M14 3h7v7M21 3l-9 9M19 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h5";
const FOLDER: &str = "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z";
const REMOVE: &str = "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3";
const ICON: &str = "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3";

fn ask(cmd: &IpcCommand) -> Result<Option<String>, String> {
    let mut s = UnixStream::connect(get_socket_path()).map_err(|_| "Prism's daemon is not running".to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    BufReader::new(s).read_line(&mut answer).map_err(|e| e.to_string())?;
    match serde_json::from_str::<IpcResponse>(&answer) {
        Ok(IpcResponse::Ok(d)) => Ok(d),
        Ok(IpcResponse::Error(e)) => Err(e),
        Err(_) => Err("unexpected answer".into()),
    }
}

enum Msg {
    List(Vec<DownloadInfo>),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Act(u64, &'static str),
    Add,
    Clear,
    Field,
}

pub struct DownloadsView {
    list: Vec<DownloadInfo>,
    loaded: bool,
    error: Option<String>,
    rx: Receiver<Msg>,
    tx: Sender<DownloadAction>,
    link: Query,
    scroll: Scroller,
    hits: Vec<(Hit, Rect)>,
    pointer: (f64, f64),
    list_rect: Rect,
}

fn human(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

fn eta(d: &DownloadInfo) -> Option<String> {
    let (size, rate) = (d.size?, d.rate);
    if rate == 0 || size <= d.done {
        return None;
    }
    let s = (size - d.done) / rate;
    Some(if s < 60 {
        format!("{s} s left")
    } else if s < 3600 {
        format!("{} min left", s / 60)
    } else {
        format!("{:.1} h left", s as f64 / 3600.0)
    })
}

fn detail(d: &DownloadInfo) -> String {
    let amount = match d.size {
        Some(s) if s > 0 => format!("{} of {}", human(d.done), human(s)),
        _ => human(d.done),
    };
    match d.state.as_str() {
        "running" => {
            let mut parts = vec![amount];
            if d.rate > 0 {
                parts.push(format!("{}/s", human(d.rate)));
            }
            parts.extend(eta(d));
            if d.connections > 1 {
                parts.push(format!("{} connections", d.connections));
            }
            parts.join("  \u{b7}  ")
        }
        "probing" => "Starting\u{2026}".into(),
        "queued" => "Waiting for other downloads".into(),
        "confirm" => "Waiting for you to confirm".into(),
        "paused" => format!("Paused  \u{b7}  {amount}"),
        "done" => d.size.map(human).unwrap_or_else(|| "Done".into()),
        "failed" => d.error.clone().unwrap_or_else(|| "Failed".into()),
        "cancelled" => "Cancelled".into(),
        s => s.to_string(),
    }
}

fn actions(state: &str) -> &'static [&'static str] {
    match state {
        "running" | "queued" => &["pause", "cancel"],
        "probing" => &["cancel"],
        "paused" | "failed" => &["resume", "cancel", "remove"],
        "done" => &["open", "show", "remove"],
        _ => &["remove"],
    }
}

fn icon_of(a: &str) -> (&'static str, bool) {
    match a {
        "pause" => (PAUSE, true),
        "resume" => (RESUME, false),
        "cancel" => (CANCEL, true),
        "open" => (OPEN, true),
        "show" => (FOLDER, true),
        _ => (REMOVE, true),
    }
}

impl DownloadsView {
    pub fn new(waker: Waker) -> Self {
        let (msg_tx, rx) = channel();
        let (tx, act_rx) = channel::<DownloadAction>();
        std::thread::Builder::new()
            .name("prism-downloads-card".into())
            .spawn(move || loop {
                // The card is gone when its end of the channel is.
                match act_rx.recv_timeout(Duration::from_millis(1000)) {
                    Ok(a) => {
                        if let Err(e) = ask(&IpcCommand::Download(a)) {
                            let _ = msg_tx.send(Msg::Error(e));
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                let m = match ask(&IpcCommand::Download(DownloadAction::List)) {
                    Ok(Some(j)) => Msg::List(serde_json::from_str(&j).unwrap_or_default()),
                    Ok(None) => Msg::List(Vec::new()),
                    Err(e) => Msg::Error(e),
                };
                if msg_tx.send(m).is_err() {
                    return;
                }
                waker.wake();
            })
            .ok();
        // The first list without waiting a second.
        let _ = tx.send(DownloadAction::List);
        DownloadsView { list: Vec::new(), loaded: false, error: None, rx, tx, link: Query::default(), scroll: Scroller::default(), hits: Vec::new(), pointer: (-1.0, -1.0), list_rect: Rect::default() }
    }

    fn palette(style: &Style, theme: &Theme) -> Palette {
        Palette {
            fg: style.fg,
            accent: style.accent,
            card: style.card_bg,
            on_accent: cb::hex_to_rgba(&theme.on_primary, 1.0),
            outline: cb::hex_to_rgba(&theme.outline_variant, 1.0),
            error: cb::hex_to_rgba(&theme.error, 1.0),
            family: style.font_family.clone(),
        }
    }

    fn add_link(&mut self) -> Effect {
        let url = self.link.text.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.error = Some("Paste an http or https link".into());
            return Effect::Redraw;
        }
        let kind = if url.split(['?', '#']).next().is_some_and(|p| p.ends_with(".m3u8")) { "hls" } else { "" };
        let _ = self.tx.send(DownloadAction::Add(DownloadRequest { url, kind: kind.into(), ..Default::default() }));
        self.link = Query::default();
        self.error = None;
        Effect::Redraw
    }

    fn hover(&self, h: Hit) -> f64 {
        let on = self.hits.iter().any(|(k, r)| *k == h && r.contains(self.pointer.0, self.pointer.1));
        if on {
            1.0
        } else {
            0.0
        }
    }
}

impl CardView for DownloadsView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let dim = (fg.0, fg.1, fg.2, 0.62);
        let font = |px: u32, w: &str| format!("{} {w} {px}px", p.family);
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;
        let mut hits = Vec::new();

        // Header: title, what is going on, Clear finished.
        cb::fill_circle(cr, x + 18.0, y + 18.0, 18.0, (p.accent.0, p.accent.1, p.accent.2, 0.18));
        crate::svg::draw_path(cr, ICON, x + 8.0, y + 8.0, 20.0, p.accent, Some(2.0));
        cb::draw_pango_text(cr, x + 48.0, y - 1.0, "Downloads", &font(18, "Bold"), fg, None);
        let running: Vec<&DownloadInfo> = self.list.iter().filter(|d| d.state == "running").collect();
        let sub = if running.is_empty() {
            format!("{} in the list", self.list.len())
        } else {
            format!("{} running  \u{b7}  {}/s", running.len(), human(running.iter().map(|d| d.rate).sum()))
        };
        cb::draw_pango_text(cr, x + 48.0, y + 23.0, &sub, &font(12, ""), dim, None);
        let clear = Rect { x: x + w - 130.0, y: y + 2.0, w: 130.0, h: 32.0 };
        let finished = self.list.iter().any(|d| matches!(d.state.as_str(), "done" | "failed" | "cancelled"));
        uikit::button(cr, clear, "Clear finished", ButtonKind::Outline, self.hover(Hit::Clear), finished, &p);
        hits.push((Hit::Clear, clear));
        y += 52.0;

        // Paste a link.
        let field = Rect { x, y, w: w - 120.0, h: 38.0 };
        let add = Rect { x: x + w - 110.0, y, w: 110.0, h: 38.0 };
        uikit::text_field(cr, field, &self.link.text, "Paste a link to download", (!self.link.text.is_empty()).then(|| self.link.caret()), self.hover(Hit::Field), self.error.is_some(), &p);
        uikit::button(cr, add, "Download", ButtonKind::Primary, self.hover(Hit::Add), !self.link.text.trim().is_empty(), &p);
        hits.push((Hit::Field, field));
        hits.push((Hit::Add, add));
        y += 38.0 + 8.0;
        if let Some(e) = &self.error {
            cb::draw_pango_text(cr, x + 6.0, y, e, &font(12, ""), p.error, Some(w as i32));
        }
        y += 22.0;

        // The list.
        let list = Rect { x, y, w, h: card.y + card.h - PAD - y };
        self.list_rect = list;
        if self.list.is_empty() {
            let msg = if !self.loaded { "Loading\u{2026}" } else { "No downloads. Links you paste here, prism-ctl download add, and downloads the Prism Downloads Firefox extension hands over appear here." };
            cb::draw_pango_text(cr, x + 20.0, y + 30.0, msg, &font(13, ""), dim, Some((w - 40.0) as i32));
        }
        let content = self.list.len() as f64 * (ROW_H + ROW_GAP);
        self.scroll.set_max(content - list.h, style);
        let off = self.scroll.pos();
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, list.x - 2.0, list.y, list.w + 4.0, list.h);
            cb::cairo_clip(cr);
        }
        let mut ry = list.y - off;
        for d in &self.list {
            if ry + ROW_H < list.y || ry > list.y + list.h {
                ry += ROW_H + ROW_GAP;
                continue;
            }
            let row = Rect { x: list.x, y: ry, w: list.w, h: ROW_H };
            cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 12.0, (fg.0, fg.1, fg.2, 0.06));
            // Actions on the right.
            let acts = actions(&d.state);
            let mut bx = row.x + row.w - 12.0 - acts.len() as f64 * 32.0;
            let names_w = bx - row.x - 24.0;
            for a in acts {
                let b = Rect { x: bx, y: row.y + 10.0, w: 28.0, h: 28.0 };
                let h = Hit::Act(d.id, a);
                let on = b.contains(self.pointer.0, self.pointer.1) && b.y >= list.y && b.y + b.h <= list.y + list.h;
                if on {
                    cb::fill_circle(cr, b.x + 14.0, b.y + 14.0, 14.0, (fg.0, fg.1, fg.2, 0.14));
                }
                let (path, stroked) = icon_of(a);
                let c = if *a == "cancel" && on { p.error } else { (fg.0, fg.1, fg.2, if on { 1.0 } else { 0.7 }) };
                crate::svg::draw_path(cr, path, b.x + 6.0, b.y + 6.0, 16.0, c, stroked.then_some(2.0));
                if b.y >= list.y && b.y + b.h <= list.y + list.h {
                    hits.push((h, b));
                }
                bx += 32.0;
            }
            cb::draw_pango_text(cr, row.x + 14.0, row.y + 10.0, &d.name, &font(14, "Semi-Bold"), fg, Some(names_w as i32));
            // Progress.
            let show_bar = matches!(d.state.as_str(), "running" | "paused" | "probing" | "queued");
            let by = row.y + 38.0;
            if show_bar {
                let bw = row.w - 28.0;
                cb::fill_rounded_rect(cr, row.x + 14.0, by, bw, 4.0, 2.0, (fg.0, fg.1, fg.2, 0.12));
                let frac = d.size.filter(|s| *s > 0).map(|s| (d.done as f64 / s as f64).clamp(0.0, 1.0));
                let color = if d.state == "paused" { dim } else { p.accent };
                match frac {
                    Some(f) => cb::fill_rounded_rect(cr, row.x + 14.0, by, (bw * f).max(4.0), 4.0, 2.0, color),
                    None if d.state == "running" => {
                        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                        let seg = bw * 0.25;
                        let pos = (t / 1.4).fract() * (bw + seg) - seg;
                        let (a, b) = (pos.max(0.0), (pos + seg).min(bw));
                        if b > a {
                            cb::fill_rounded_rect(cr, row.x + 14.0 + a, by, b - a, 4.0, 2.0, color);
                        }
                        crate::ui::keep_animating();
                    }
                    None => {}
                }
            }
            let dc = if d.state == "failed" { p.error } else { dim };
            cb::draw_pango_text(cr, row.x + 14.0, if show_bar { by + 9.0 } else { row.y + 36.0 }, &detail(d), &font(12, ""), dc, Some((row.w - 28.0) as i32));
            ry += ROW_H + ROW_GAP;
        }
        unsafe { cb::cairo_restore(cr) };
        if content > list.h {
            let th = (list.h * list.h / content).max(30.0);
            let ty = list.y + (list.h - th) * (off / (content - list.h)).clamp(0.0, 1.0);
            cb::fill_rounded_rect(cr, card.x + card.w - 9.0, ty, 3.0, th, 1.5, (fg.0, fg.1, fg.2, 0.3));
        }
        self.hits = hits;
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let before = self.hits.iter().position(|(_, r)| r.contains(self.pointer.0, self.pointer.1));
        self.pointer = (x, y);
        before != self.hits.iter().position(|(_, r)| r.contains(x, y))
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        let Some((h, _)) = self.hits.iter().find(|(_, r)| r.contains(x, y)).copied() else { return Effect::None };
        match h {
            Hit::Add => return self.add_link(),
            Hit::Clear => {
                let _ = self.tx.send(DownloadAction::Clear);
            }
            Hit::Field => {}
            Hit::Act(id, a) => {
                let act = match a {
                    "pause" => DownloadAction::Pause(id),
                    "resume" => DownloadAction::Resume(id),
                    "cancel" => DownloadAction::Cancel(id),
                    "open" => DownloadAction::Open(id),
                    "show" => DownloadAction::Show(id),
                    _ => DownloadAction::Remove(id),
                };
                let _ = self.tx.send(act);
                if matches!(a, "open" | "show") {
                    return Effect::Close;
                }
            }
        }
        Effect::Redraw
    }

    fn scroll(&mut self, delta: f64) -> bool {
        self.scroll.by(delta)
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        match special {
            ESC => Effect::Close,
            ENTER => self.add_link(),
            _ => match self.link.key(special, typed, ctrl) {
                Some(_) => {
                    self.error = None;
                    Effect::Redraw
                }
                None => Effect::None,
            },
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::List(l) => {
                    if !self.loaded || l != self.list {
                        changed = true;
                    }
                    self.list = l;
                    self.loaded = true;
                }
                Msg::Error(e) => {
                    self.error = Some(e);
                    changed = true;
                }
            }
        }
        changed
    }

    fn clickable(&self) -> bool {
        self.hits.iter().any(|(h, r)| *h != Hit::Field && r.contains(self.pointer.0, self.pointer.1))
    }
}
