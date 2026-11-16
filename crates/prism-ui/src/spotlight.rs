//! Spotlight search (port of QML SpotlightSearch.qml + SearchProviderApps/Files).
//!
//! A 680 px card 140 px from the top. Empty it is just the 60 px search bar; with a query it
//! grows (Theme.animFast) to fit the results: up to 5 apps, then folders and files from
//! plocate in the configured section order, with section headers when two or more sections
//! have results. File search runs on a worker thread, debounced 150 ms; while it runs, the
//! previous file results stay (dimmed, not activatable) and the card does not shrink.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use prism_common::apps::{search_apps, DesktopApp};
use prism_common::files::{icon_for_path, search_paths, FileResult};
use prism_common::{Config, Theme};

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{draw_icon, draw_query, search_icon, Effect, Query, DOWN, ENTER, ESC, MOUSE_READY, PGDN, PGUP, UP};
use crate::ui::{out_cubic, Style};
use crate::waker::Waker;

pub const CARD_W: f64 = 680.0;
pub const TOP: f64 = 140.0;
const HEADER_H: f64 = 60.0;
const ROW_H: f64 = 50.0;
const ROW_GAP: f64 = 2.0;
const SECTION_H: f64 = 26.0;
const MAX_H: f64 = 480.0;
const MAX_APPS: usize = 5;
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Theme.animFast: the card's height Behavior.
const HEIGHT_MS: f64 = 53.0;

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    App(DesktopApp),
    Folder(String),
    File(String),
}

#[derive(Clone, Debug)]
struct Item {
    id: String,
    section: &'static str,
    title: String,
    subtitle: String,
    icon: String,
    kind: Kind,
}

/// Rows as laid out: a section header or a result.
enum Line {
    Header(&'static str),
    Item(usize),
}

struct Job {
    generation: u64,
    query: String,
    include_hidden: bool,
}

struct Found {
    generation: u64,
    folders: Vec<FileResult>,
    files: Vec<FileResult>,
}

pub struct SpotlightView {
    apps: Vec<DesktopApp>,
    config: Config,
    query: Query,
    app_items: Vec<Item>,
    folders: Vec<FileResult>,
    files: Vec<FileResult>,
    results: Vec<Item>,
    selected: usize,
    scroll: f64,
    generation: u64,
    searching: bool,
    jobs: Sender<Job>,
    found: Receiver<Found>,
    opened: Instant,
    mouse_active: bool,
    pointer: (f64, f64),
    rows: Vec<(usize, Rect)>,
    list: Rect,
    /// Animated card height.
    h_from: f64,
    h_to: f64,
    h_start: Instant,
    /// Height while file search last started (the card does not shrink during a search).
    last_h: f64,
    shift: bool,
}

impl SpotlightView {
    pub fn new(apps: Vec<DesktopApp>, config: Config, waker: Waker) -> Self {
        let (jobs, job_rx) = std::sync::mpsc::channel::<Job>();
        let (found_tx, found) = std::sync::mpsc::channel::<Found>();
        std::thread::Builder::new()
            .name("prism-spotlight".into())
            .spawn(move || file_worker(job_rx, found_tx, waker))
            .expect("spawn spotlight worker");
        Self {
            apps,
            config,
            query: Query::default(),
            app_items: Vec::new(),
            folders: Vec::new(),
            files: Vec::new(),
            results: Vec::new(),
            selected: 0,
            scroll: 0.0,
            generation: 0,
            searching: false,
            jobs,
            found,
            opened: Instant::now(),
            mouse_active: false,
            pointer: (-1.0, -1.0),
            rows: Vec::new(),
            list: Rect::default(),
            h_from: HEADER_H,
            h_to: HEADER_H,
            h_start: Instant::now(),
            last_h: HEADER_H,
            shift: false,
        }
    }

    /// Current (animated) card height.
    pub fn height(&self) -> f64 {
        let t = (self.h_start.elapsed().as_secs_f64() * 1000.0 / HEIGHT_MS).clamp(0.0, 1.0);
        (self.h_from + (self.h_to - self.h_from) * out_cubic(t)).round()
    }

    pub fn animating(&self) -> bool {
        self.h_start.elapsed().as_secs_f64() * 1000.0 < HEIGHT_MS
    }

    fn set_height(&mut self, h: f64) {
        if (h - self.h_to).abs() < 0.5 {
            return;
        }
        self.h_from = self.height();
        self.h_to = h;
        self.h_start = Instant::now();
    }

    /// Take file-search results. True when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(f) = self.found.try_recv() {
            if f.generation != self.generation {
                continue;
            }
            self.folders = f.folders;
            self.files = f.files;
            self.searching = false;
            changed = true;
        }
        if changed {
            self.rebuild(false);
        }
        changed
    }

    fn q(&self) -> String {
        self.query.text.trim().to_string()
    }

    fn apply_filter(&mut self) {
        let q = self.q();
        self.generation += 1;
        if q.is_empty() {
            self.app_items.clear();
            self.folders.clear();
            self.files.clear();
            self.searching = false;
            self.results.clear();
            self.selected = 0;
            self.update_height();
            return;
        }
        let c = &self.config;
        self.app_items = if c.search_enable_apps {
            search_apps(&self.apps, &q).into_iter().take(MAX_APPS).map(app_item).collect()
        } else {
            Vec::new()
        };
        if (c.search_enable_files || c.search_enable_folders) && q.chars().count() >= 2 {
            if !self.searching {
                self.last_h = self.h_to;
            }
            self.searching = true;
            let _ = self.jobs.send(Job { generation: self.generation, query: q, include_hidden: c.search_include_hidden });
        } else {
            self.searching = false;
            self.folders.clear();
            self.files.clear();
        }
        self.rebuild(true);
    }

    fn rebuild(&mut self, reset: bool) {
        let c = &self.config;
        let folders: Vec<Item> = if c.search_enable_folders {
            self.folders.iter().take(c.search_max_folder_results.max(1) as usize).map(|f| path_item(f, true)).collect()
        } else {
            Vec::new()
        };
        let files: Vec<Item> = if c.search_enable_files {
            self.files.iter().take(c.search_max_file_results.max(1) as usize).map(|f| path_item(f, false)).collect()
        } else {
            Vec::new()
        };
        let sections = [!self.app_items.is_empty(), !folders.is_empty(), !files.is_empty()].iter().filter(|b| **b).count();
        let show_headers = sections >= 2;
        let mut merged = Vec::new();
        for sec in section_order(&c.search_section_order) {
            let src = match sec {
                "apps" => &self.app_items,
                "folders" => &folders,
                _ => &files,
            };
            for it in src {
                let mut it = it.clone();
                if !show_headers {
                    it.section = "";
                }
                merged.push(it);
            }
        }
        let keep = (!reset).then(|| self.results.get(self.selected).map(|r| r.id.clone())).flatten();
        self.results = merged;
        self.selected = match keep {
            Some(id) => self.results.iter().position(|r| r.id == id).unwrap_or(self.selected),
            None => 0,
        }
        .min(self.results.len().saturating_sub(1));
        if reset {
            self.scroll = 0.0;
        }
        self.update_height();
    }

    fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        let mut last = "";
        for (i, r) in self.results.iter().enumerate() {
            if !r.section.is_empty() && r.section != last {
                out.push(Line::Header(r.section));
            }
            last = r.section;
            out.push(Line::Item(i));
        }
        out
    }

    fn content_height(&self) -> f64 {
        let lines = self.lines();
        let n = lines.len() as f64;
        lines.iter().map(|l| if matches!(l, Line::Header(_)) { SECTION_H } else { ROW_H }).sum::<f64>() + (n - 1.0).max(0.0) * ROW_GAP
    }

    fn update_height(&mut self) {
        let h = if self.q().is_empty() {
            HEADER_H
        } else if self.results.is_empty() {
            HEADER_H + 1.0 + 50.0
        } else {
            (HEADER_H + 1.0 + self.content_height() + 12.0).min(MAX_H)
        };
        let h = if self.searching && h < self.last_h && self.last_h > HEADER_H { self.last_h } else { h };
        self.set_height(h);
    }

    fn stale(&self, i: usize) -> bool {
        self.searching && matches!(self.results.get(i).map(|r| &r.kind), Some(Kind::Folder(_) | Kind::File(_)))
    }

    fn activate(&self, i: usize, shift: bool) -> Effect {
        let Some(item) = self.results.get(i) else { return Effect::None };
        if self.stale(i) {
            return Effect::None;
        }
        match &item.kind {
            Kind::App(app) => match app.launch() {
                Ok(child) => crate::reap(child),
                Err(e) => eprintln!("[prism-ui] cannot launch {}: {}", app.name, e),
            },
            Kind::Folder(p) | Kind::File(p) => {
                let target = if shift { parent_dir(p) } else { p.clone() };
                if let Ok(child) = prism_common::apps::app_command("xdg-open", &[target]).spawn() {
                    crate::reap(child);
                }
            }
        }
        Effect::Close
    }

    fn move_selection(&mut self, delta: i32) {
        if self.results.is_empty() {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, self.results.len() as i32 - 1) as usize;
        // Position of the selected row in content coordinates.
        let mut y = 0.0;
        for l in self.lines() {
            match l {
                Line::Header(_) => y += SECTION_H + ROW_GAP,
                Line::Item(i) if i == self.selected => break,
                Line::Item(_) => y += ROW_H + ROW_GAP,
            }
        }
        if y < self.scroll {
            self.scroll = y;
        } else if y + ROW_H > self.scroll + self.list.h {
            self.scroll = y + ROW_H - self.list.h;
        }
    }

    pub fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        let fg = style.fg;
        let inv = cb::hex_to_rgba(&theme.inverse_primary, 1.0);
        let sv_text = cb::hex_to_rgba(&theme.on_surface_variant, 1.0);
        let primary = cb::hex_to_rgba(&theme.primary, 1.0);
        let r = style.radius;
        cb::fill_rounded_rect(cr, card.x, card.y, card.w, card.h, r, style.card_bg);
        cb::stroke_rounded_rect(cr, card.x + 0.5, card.y + 0.5, card.w - 1.0, card.h - 1.0, r, style.card_border, 1.0);
        unsafe {
            cb::cairo_save(cr);
            cb::rounded_rect_path(cr, card.x, card.y, card.w, card.h, r);
            cb::cairo_clip(cr);
        }

        // Search bar.
        let bx = card.x + 18.0;
        let bw = card.w - 36.0;
        search_icon(cr, bx, card.y + 20.0, 20.0, primary, 2.2);
        let mut right = bx + bw;
        // "esc" chip.
        right -= 28.0;
        let chip = Rect { x: right, y: card.y + 20.0, w: 28.0, h: 20.0 };
        cb::fill_rounded_rect(cr, chip.x, chip.y, chip.w, chip.h, 4.0, (fg.0, fg.1, fg.2, 0.12));
        let ov = cb::hex_to_rgba(&theme.outline_variant, 0.3);
        cb::stroke_rounded_rect(cr, chip.x + 0.5, chip.y + 0.5, chip.w - 1.0, chip.h - 1.0, 4.0, ov, 1.0);
        let cf = "Inter, Sans Semi-Bold 10px";
        let (cw, ch) = cb::measure_pango_text(cr, "esc", cf, 100, Some(1));
        cb::draw_pango_text(cr, chip.x + ((chip.w - cw as f64) / 2.0).round(), chip.y + ((chip.h - ch as f64) / 2.0).round(), "esc", cf, sv_text, None);
        if self.searching && !self.results.is_empty() && !self.q().is_empty() {
            let sf = "Inter, Sans Italic 12px";
            let (sw, sh) = cb::measure_pango_text(cr, "searching…", sf, 400, Some(1));
            right -= 12.0 + sw as f64;
            cb::draw_pango_text(cr, right, card.y + ((HEADER_H - sh as f64) / 2.0).round(), "searching…", sf, primary, None);
        }
        let tx = bx + 20.0 + 12.0;
        draw_query(cr, &self.query, "Search apps, folders, and files…", tx, card.y + 18.0, right - 12.0 - tx, 24.0, "Inter, Sans 18px", cb::hex_to_rgba(&theme.on_surface, 1.0), sv_text);

        self.rows.clear();
        if !self.q().is_empty() {
            let dy = card.y + HEADER_H;
            cb::fill_rounded_rect(cr, card.x, dy, card.w, 1.0, 0.0, cb::hex_to_rgba(&theme.outline_variant, 0.25));
            let area = Rect { x: card.x, y: dy + 1.0, w: card.w, h: (card.y + card.h - dy - 1.0).max(0.0) };
            if self.results.is_empty() {
                let (msg, font, a) = if self.searching {
                    ("Searching files and folders…", "Inter, Sans Italic 14px", 0.8)
                } else {
                    ("No matches found", "Inter, Sans 14px", 0.7)
                };
                let (mw, mh) = cb::measure_pango_text(cr, msg, font, 2000, Some(1));
                cb::draw_pango_text(cr, area.x + ((area.w - mw as f64) / 2.0).round(), area.y + ((area.h - mh as f64) / 2.0).round(), msg, font, (sv_text.0, sv_text.1, sv_text.2, a), None);
            } else {
                self.list = Rect { x: area.x + 6.0, y: area.y + 6.0, w: area.w - 12.0, h: area.h - 12.0 };
                let max = (self.content_height() - self.list.h).max(0.0);
                self.scroll = self.scroll.clamp(0.0, max);
                unsafe {
                    cb::cairo_save(cr);
                    cb::cairo_rectangle(cr, self.list.x, self.list.y, self.list.w, self.list.h);
                    cb::cairo_clip(cr);
                }
                let hovered = self.hovered();
                let mut y = self.list.y - self.scroll;
                for line in self.lines() {
                    if y > self.list.y + self.list.h {
                        break;
                    }
                    match line {
                        Line::Header(s) => {
                            let f = "Inter, Sans Semi-Bold 11px";
                            let (_, th) = cb::measure_pango_text(cr, s, f, 1000, Some(1));
                            cb::draw_pango_text(cr, self.list.x + 12.0, y + ((SECTION_H - th as f64) / 2.0).round(), &s.to_uppercase(), f, (sv_text.0, sv_text.1, sv_text.2, 0.75), None);
                            y += SECTION_H + ROW_GAP;
                        }
                        Line::Item(i) => {
                            let row = Rect { x: self.list.x, y, w: self.list.w, h: ROW_H };
                            if row.y + row.h >= self.list.y {
                                self.rows.push((i, row));
                                self.draw_row(cr, row, i, hovered == Some(i), fg, inv, sv_text);
                            }
                            y += ROW_H + ROW_GAP;
                        }
                    }
                }
                unsafe { cb::cairo_restore(cr) };
            }
        }
        unsafe { cb::cairo_restore(cr) };
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(&self, cr: *mut Cairo, row: Rect, i: usize, hovered: bool, fg: crate::ui::Rgba, inv: crate::ui::Rgba, sv_text: crate::ui::Rgba) {
        let item = &self.results[i];
        let stale = self.stale(i);
        let on = i == self.selected || hovered;
        unsafe { cb::cairo_push_group(cr) };
        if on {
            cb::fill_rounded_rect(cr, row.x + 1.0, row.y + 1.0, row.w - 2.0, row.h - 2.0, 12.0, (fg.0, fg.1, fg.2, 0.85));
        }
        let fallback = match item.kind {
            Kind::Folder(_) => "inode-directory",
            Kind::File(_) => "text-x-generic",
            Kind::App(_) => "application-x-executable",
        };
        draw_icon(cr, &item.icon, fallback, row.x + 12.0, row.y + 11.0, 28.0);
        let tx = row.x + 12.0 + 28.0 + 12.0;
        let mut right = row.x + row.w - 12.0;
        if i == self.selected {
            let hint = if stale {
                "Searching…"
            } else {
                match item.kind {
                    Kind::Folder(_) => "Shift+↵ Parent  ↵ Open",
                    Kind::File(_) => "Shift+↵ Folder  ↵ Open",
                    Kind::App(_) => "↵ Open",
                }
            };
            let hf = "Inter, Sans 10px";
            let (hw, hh) = cb::measure_pango_text(cr, hint, hf, 1000, Some(1));
            right -= hw as f64;
            let c = if on { (inv.0, inv.1, inv.2, 0.6) } else { sv_text };
            cb::draw_pango_text(cr, right, row.y + ((ROW_H - hh as f64) / 2.0).round(), hint, hf, c, None);
            right -= 12.0;
        }
        let w = (right - tx).max(10.0);
        let tf = "Inter, Sans Medium 14px";
        let sf = "Inter, Sans 11px";
        let (_, th) = cb::measure_pango_text(cr, "Ag", tf, 1000, Some(1));
        let (_, sh) = if item.subtitle.is_empty() { (0, 0) } else { cb::measure_pango_text(cr, "Ag", sf, 1000, Some(1)) };
        let total = th as f64 + if sh > 0 { 1.0 + sh as f64 } else { 0.0 };
        let ty = row.y + ((ROW_H - total) / 2.0).round();
        cb::draw_pango_text(cr, tx, ty, &item.title, tf, if on { inv } else { fg }, Some(w as i32));
        if sh > 0 {
            let c = if on { (inv.0, inv.1, inv.2, 0.7) } else { sv_text };
            cb::draw_pango_text(cr, tx, ty + th as f64 + 1.0, &item.subtitle, sf, c, Some(w as i32));
        }
        unsafe {
            cb::cairo_pop_group_to_source(cr);
            cb::cairo_paint_with_alpha(cr, if stale { 0.45 } else { 1.0 });
        }
    }

    fn hovered(&self) -> Option<usize> {
        if !self.mouse_active {
            return None;
        }
        self.row_at(self.pointer.0, self.pointer.1).filter(|&i| !self.stale(i))
    }

    fn row_at(&self, x: f64, y: f64) -> Option<usize> {
        if !self.list.contains(x, y) {
            return None;
        }
        self.rows.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }

    pub fn over_row(&self) -> bool {
        self.row_at(self.pointer.0, self.pointer.1).map(|i| !self.stale(i)).unwrap_or(false)
    }

    pub fn motion(&mut self, x: f64, y: f64) -> bool {
        let moved = (x, y) != self.pointer;
        self.pointer = (x, y);
        if moved && !self.mouse_active && self.opened.elapsed() >= MOUSE_READY {
            self.mouse_active = true;
        }
        if let Some(i) = self.hovered() {
            if i != self.selected {
                self.selected = i;
                return true;
            }
        }
        false
    }

    pub fn click(&mut self, x: f64, y: f64) -> Effect {
        self.mouse_active = true;
        match self.row_at(x, y) {
            Some(i) => {
                self.selected = i;
                self.activate(i, self.shift)
            }
            None => Effect::None,
        }
    }

    pub fn scroll(&mut self, delta: f64) -> bool {
        self.scroll = (self.scroll + delta).max(0.0);
        true
    }

    pub fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        self.shift = shift;
        match special {
            ESC => return Effect::Close,
            ENTER => return self.activate(self.selected, shift),
            DOWN => self.move_selection(1),
            UP => self.move_selection(-1),
            PGDN => self.move_selection(((self.list.h / (ROW_H + ROW_GAP)) as i32).max(1)),
            PGUP => self.move_selection(-((self.list.h / (ROW_H + ROW_GAP)) as i32).max(1)),
            _ => match self.query.key(special, typed, ctrl) {
                Some(true) => self.apply_filter(),
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }
}

/// Config `search_section_order` ("apps,folders,files") sanitised: known names once each, in the
/// given order, then any missing ones.
fn section_order(s: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for part in s.split(',').map(|p| p.trim().to_lowercase()) {
        let name = match part.as_str() {
            "apps" => "apps",
            "folders" => "folders",
            "files" => "files",
            _ => continue,
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    for n in ["apps", "folders", "files"] {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

fn app_item(app: DesktopApp) -> Item {
    Item {
        id: format!("app:{}", app.id),
        section: "Applications",
        title: app.name.clone(),
        subtitle: app.generic_name.clone().unwrap_or_default(),
        icon: app.icon.clone().unwrap_or_else(|| "application-x-executable".into()),
        kind: Kind::App(app),
    }
}

fn path_item(f: &FileResult, folder: bool) -> Item {
    Item {
        id: format!("{}:{}", if folder { "folder" } else { "file" }, f.path),
        section: if folder { "Folders" } else { "Files" },
        title: f.name.clone(),
        subtitle: f.path.clone(),
        icon: if folder { "folder".into() } else { icon_for_path(&f.path).into() },
        kind: if folder { Kind::Folder(f.path.clone()) } else { Kind::File(f.path.clone()) },
    }
}

fn parent_dir(p: &str) -> String {
    match p.rfind('/') {
        Some(i) if i > 0 => p[..i].to_string(),
        _ => "/".into(),
    }
}

/// Debounced plocate searches; only the newest job of a burst runs.
fn file_worker(jobs: Receiver<Job>, out: Sender<Found>, waker: Waker) {
    while let Ok(mut job) = jobs.recv() {
        loop {
            match jobs.recv_timeout(DEBOUNCE) {
                Ok(newer) => job = newer,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let (folders, files) = search_paths(&job.query, job.include_hidden);
        if out.send(Found { generation: job.generation, folders, files }).is_err() {
            return;
        }
        waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_order_is_sanitised() {
        assert_eq!(section_order("files, apps,files,bogus"), ["files", "apps", "folders"]);
        assert_eq!(section_order(""), ["apps", "folders", "files"]);
        assert_eq!(parent_dir("/home/u/a.txt"), "/home/u");
        assert_eq!(parent_dir("/a"), "/");
    }
}

impl crate::view::CardView for SpotlightView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        SpotlightView::draw(self, cr, card, style, theme)
    }
    fn motion(&mut self, x: f64, y: f64) -> bool {
        SpotlightView::motion(self, x, y)
    }
    fn click(&mut self, x: f64, y: f64, shift: bool) -> Effect {
        self.shift = shift;
        SpotlightView::click(self, x, y)
    }
    fn scroll(&mut self, delta: f64) -> bool {
        SpotlightView::scroll(self, delta)
    }
    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        SpotlightView::key(self, special, typed, shift, ctrl)
    }
    fn poll(&mut self) -> bool {
        SpotlightView::poll(self)
    }
    fn animating(&self) -> bool {
        SpotlightView::animating(self)
    }
    fn clickable(&self) -> bool {
        self.over_row()
    }
    fn height(&self) -> Option<f64> {
        Some(SpotlightView::height(self))
    }
}
