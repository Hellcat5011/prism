//! The confirmation card for a browser download (prismd `dl.rs` asks with
//! `UiCommand::DownloadConfirm`, one at a time): the file name (editable), its size and site,
//! the folder (Change… picks another through the portal) and Download / Cancel. Download runs
//! `prism-ctl download start <id> --name --dir`; Cancel, Esc or closing the card any other way
//! declines it (`prism-ctl download decline <id>`).

use std::sync::mpsc::Receiver;
use std::sync::Mutex;

use prism_common::ipc::DownloadAsk;
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{Query, ENTER, ESC};
use crate::ui::Style;
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};
use crate::waker::Waker;

pub const CARD_W: f64 = 560.0;
pub const CARD_H: f64 = 318.0;
const PAD: f64 = 24.0;
const ICON: &str = "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3";

static PENDING: Mutex<Option<DownloadAsk>> = Mutex::new(None);

pub fn set_pending(ask: DownloadAsk) {
    *PENDING.lock().unwrap() = Some(ask);
}

pub struct DownloadConfirmView {
    ask: DownloadAsk,
    name: Query,
    dir: String,
    /// Answered (Download or Cancel): nothing to do when the card goes.
    answered: bool,
    done: bool,
    picking: Option<Receiver<Result<Option<std::path::PathBuf>, String>>>,
    layer_request: Option<bool>,
    waker: Waker,
    pointer: (f64, f64),
    name_rect: Rect,
    change_btn: Rect,
    ok_btn: Rect,
    cancel_btn: Rect,
    error: Option<String>,
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

fn host(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split(['/', '?', '#']).next()).map(|h| h.rsplit('@').next().unwrap_or(h).to_string()).unwrap_or_default()
}

/// `/home/u/Downloads` as `~/Downloads`.
fn tilde(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn ctl(args: &[&str]) {
    if let Ok(c) = std::process::Command::new("prism-ctl").args(args).spawn() {
        crate::reap(c);
    }
}

impl DownloadConfirmView {
    pub fn new(waker: Waker) -> Self {
        let ask = PENDING.lock().unwrap().take().unwrap_or_default();
        DownloadConfirmView {
            name: Query::with_text(&ask.name),
            dir: ask.dir.clone(),
            ask,
            answered: false,
            done: false,
            picking: None,
            layer_request: None,
            waker,
            pointer: (-1.0, -1.0),
            name_rect: Rect::default(),
            change_btn: Rect::default(),
            ok_btn: Rect::default(),
            cancel_btn: Rect::default(),
            error: None,
        }
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

    fn accept(&mut self) -> Effect {
        let name = self.name.text.trim().to_string();
        if name.is_empty() {
            self.error = Some("The file needs a name".into());
            return Effect::Redraw;
        }
        let id = self.ask.id.to_string();
        let dir = prism_common::config::expand_home(self.dir.trim()).to_string_lossy().into_owned();
        ctl(&["download", "start", &id, "--name", &name, "--dir", &dir]);
        self.answered = true;
        self.done = true;
        Effect::Close
    }

    fn change_folder(&mut self) {
        if self.picking.is_some() {
            return;
        }
        let start = prism_common::config::expand_home(self.dir.trim());
        let start = if start.is_dir() { start } else { prism_common::config::expand_home("~") };
        let (tx, rx) = std::sync::mpsc::channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::portal::pick_folder("Save the download in", Some(&start)));
            waker.wake();
        });
        self.picking = Some(rx);
        self.layer_request = Some(true);
    }

    fn over(&self, r: Rect) -> f64 {
        if r.contains(self.pointer.0, self.pointer.1) {
            1.0
        } else {
            0.0
        }
    }
}

impl Drop for DownloadConfirmView {
    fn drop(&mut self) {
        // Closed without an answer (Esc, a click outside): declined.
        if !self.answered && self.ask.id != 0 {
            ctl(&["download", "decline", &self.ask.id.to_string()]);
        }
    }
}

impl CardView for DownloadConfirmView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let dim = (fg.0, fg.1, fg.2, 0.62);
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;

        // Header: the download icon, the title and where it comes from.
        cb::fill_circle(cr, x + 20.0, y + 20.0, 20.0, (p.accent.0, p.accent.1, p.accent.2, 0.18));
        crate::svg::draw_path(cr, ICON, x + 9.0, y + 9.0, 22.0, p.accent, Some(2.0));
        cb::draw_pango_text(cr, x + 54.0, y, "Download File", &format!("{} Bold 18px", p.family), fg, None);
        let mut sub = host(&self.ask.url);
        if let Some(s) = self.ask.size {
            sub = format!("{}  \u{b7}  {sub}", human(s));
        }
        cb::draw_pango_text(cr, x + 54.0, y + 25.0, &sub, &format!("{} 12px", p.family), dim, Some((w - 54.0) as i32));
        y += 62.0;

        // The name.
        cb::draw_pango_text(cr, x, y, "Name", &format!("{} Semi-Bold 12px", p.family), dim, None);
        y += 20.0;
        self.name_rect = Rect { x, y, w, h: 38.0 };
        let caret = (self.picking.is_none()).then(|| self.name.caret());
        uikit::text_field(cr, self.name_rect, &self.name.text, "File name", caret, self.over(self.name_rect), self.error.is_some(), &p);
        y += 38.0 + 14.0;

        // The folder.
        cb::draw_pango_text(cr, x, y, "Save to", &format!("{} Semi-Bold 12px", p.family), dim, None);
        y += 20.0;
        self.change_btn = Rect { x: x + w - 110.0, y, w: 110.0, h: 38.0 };
        let folder = Rect { x, y, w: w - 110.0 - 10.0, h: 38.0 };
        uikit::text_field(cr, folder, &tilde(&self.dir), "Downloads", None, 0.0, false, &p);
        let label = if self.picking.is_some() { "Choosing\u{2026}" } else { "Change\u{2026}" };
        uikit::button(cr, self.change_btn, label, ButtonKind::Outline, self.over(self.change_btn), self.picking.is_none(), &p);

        // Buttons, with a note on the left (an error, or that the server could not be asked).
        let by = card.y + card.h - PAD - 40.0;
        self.ok_btn = Rect { x: x + w - 130.0, y: by, w: 130.0, h: 40.0 };
        self.cancel_btn = Rect { x: self.ok_btn.x - 10.0 - 100.0, y: by, w: 100.0, h: 40.0 };
        uikit::button(cr, self.cancel_btn, "Cancel", ButtonKind::Outline, self.over(self.cancel_btn), true, &p);
        uikit::button(cr, self.ok_btn, "Download", ButtonKind::Primary, self.over(self.ok_btn), self.picking.is_none(), &p);
        let note = self.error.clone().or_else(|| self.ask.error.as_ref().map(|e| format!("Could not check the file: {e}")));
        if let Some(n) = note {
            cb::draw_pango_text(cr, x, by + 2.0, &n, &format!("{} 12px", p.family), p.error, Some((self.cancel_btn.x - x - 12.0) as i32));
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        let was = self.pointer;
        self.pointer = (x, y);
        [self.name_rect, self.change_btn, self.ok_btn, self.cancel_btn].iter().any(|r| r.contains(was.0, was.1) != r.contains(x, y))
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        if self.picking.is_some() {
            return Effect::None;
        }
        if self.ok_btn.contains(x, y) {
            return self.accept();
        }
        if self.cancel_btn.contains(x, y) {
            return Effect::Close;
        }
        if self.change_btn.contains(x, y) {
            self.change_folder();
            return Effect::Redraw;
        }
        Effect::None
    }

    fn key(&mut self, special: u32, typed: Option<char>, _shift: bool, ctrl: bool) -> Effect {
        if self.picking.is_some() {
            return Effect::None;
        }
        match special {
            ESC => Effect::Close,
            ENTER => self.accept(),
            _ => match self.name.key(special, typed, ctrl) {
                Some(_) => {
                    self.error = None;
                    Effect::Redraw
                }
                None => Effect::None,
            },
        }
    }

    fn poll(&mut self) -> bool {
        let Some(rx) = self.picking.as_ref() else { return false };
        let Ok(r) = rx.try_recv() else { return false };
        match r {
            Ok(Some(dir)) => self.dir = dir.to_string_lossy().into_owned(),
            Ok(None) => {}
            Err(e) => self.error = Some(format!("Folder dialog unavailable: {e}")),
        }
        self.picking = None;
        self.layer_request = Some(false);
        true
    }

    fn take_layer_request(&mut self) -> Option<bool> {
        self.layer_request.take()
    }

    fn wants_close(&self) -> bool {
        self.done
    }

    fn clickable(&self) -> bool {
        let (x, y) = self.pointer;
        [self.change_btn, self.ok_btn, self.cancel_btn].iter().any(|r| r.contains(x, y))
    }
}
