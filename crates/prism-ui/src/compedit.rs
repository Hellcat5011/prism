//! Shortcuts and Window Rules editor (`prism-ctl keybinds edit`, Settings): the compositor's
//! key bindings or window rules in a filterable list, and a form that changes, adds or deletes
//! one (Ctrl+Tab switches the tabs). Reading and writing is `prism_common::compconf` (MangoWM, niri, Hyprland); every change
//! reloads the compositor. Typing filters the list unless a form field is focused.

use prism_common::alt_tab::Compositor;
use prism_common::compconf::{self, Binding, Rule};
use prism_common::Theme;

use crate::cairo_backend::{self as cb, Cairo};
use crate::desktop::layout::Rect;
use crate::launcher::{Query, DOWN, ENTER, ESC, UP};
use crate::ui::Style;
use crate::uikit::{self, ButtonKind, Palette};
use crate::view::{CardView, Effect};

pub const CARD_W: f64 = 1100.0;
pub const CARD_H: f64 = 700.0;
const PAD: f64 = 22.0;
const ROW_H: f64 = 46.0;
const FIELD_H: f64 = 40.0;
const TAB: u32 = 15;

/// The tab the next editor opens on (true: window rules).
static OPEN_RULES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn open_on_rules(rules: bool) {
    OPEN_RULES.store(rules, std::sync::atomic::Ordering::SeqCst);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Keys,
    Action,
    App,
    Title,
    Width,
    Height,
    Workspace,
    Opacity,
}

const BIND_FIELDS: [Field; 2] = [Field::Keys, Field::Action];
const RULE_FIELDS: [Field; 6] = [Field::App, Field::Title, Field::Width, Field::Height, Field::Workspace, Field::Opacity];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Tab(bool),
    Row(usize),
    New,
    Field(Field),
    Float(Option<bool>),
    App(usize),
    Save,
    Delete,
}

pub struct CompEditView {
    comp: Option<Compositor>,
    rules_tab: bool,
    binds: Vec<Binding>,
    rules: Vec<Rule>,
    search: Query,
    /// Indices into binds/rules that match the search.
    shown: Vec<usize>,
    /// The entry in the form (None: a new one).
    selected: Option<usize>,
    scroll: f64,
    fields: Vec<(Field, Query)>,
    floating: Option<bool>,
    focus: Option<Field>,
    apps: Vec<String>,
    message: Option<(String, bool)>,
    hits: Vec<(Hit, Rect)>,
    pointer: (f64, f64),
    list: Rect,
}

fn compositor_label(c: Option<Compositor>) -> &'static str {
    c.map(|c| c.name()).unwrap_or("No supported compositor")
}

impl CompEditView {
    pub fn new() -> Self {
        let comp = compconf::current();
        let mut v = CompEditView {
            comp,
            rules_tab: OPEN_RULES.swap(false, std::sync::atomic::Ordering::SeqCst),
            binds: Vec::new(),
            rules: Vec::new(),
            search: Query::default(),
            shown: Vec::new(),
            selected: None,
            scroll: 0.0,
            fields: Vec::new(),
            floating: None,
            focus: None,
            apps: crate::wintrack::open_app_ids(),
            message: None,
            hits: Vec::new(),
            pointer: (0.0, 0.0),
            list: Rect::default(),
        };
        v.reload();
        v.load_form();
        v
    }

    fn reload(&mut self) {
        if let Some(c) = self.comp {
            self.binds = compconf::bindings(c);
            self.rules = compconf::rules(c);
        }
        self.filter();
    }

    fn filter(&mut self) {
        let q = self.search.text.trim().to_lowercase();
        let has = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        self.shown = if self.rules_tab {
            (0..self.rules.len()).filter(|&i| {
                let r = &self.rules[i];
                has(&format!("{} {} {}", r.app_id, r.title, r.other.join(" ")))
            }).collect()
        } else {
            (0..self.binds.len()).filter(|&i| {
                let b = &self.binds[i];
                has(&format!("{}+{} {}", b.mods, b.key, b.action))
            }).collect()
        };
        if self.selected.is_some_and(|s| !self.shown.contains(&s)) {
            self.selected = None;
            self.load_form();
        }
    }

    /// Fill the form from the selected entry (or empty for a new one).
    fn load_form(&mut self) {
        self.focus = None;
        if self.rules_tab {
            let r = self.selected.and_then(|i| self.rules.get(i)).cloned().unwrap_or_default();
            self.floating = r.floating;
            self.fields = vec![
                (Field::App, Query::with_text(&r.app_id)),
                (Field::Title, Query::with_text(&r.title)),
                (Field::Width, Query::with_text(&r.width)),
                (Field::Height, Query::with_text(&r.height)),
                (Field::Workspace, Query::with_text(&r.workspace)),
                (Field::Opacity, Query::with_text(&r.opacity)),
            ];
        } else {
            let b = self.selected.and_then(|i| self.binds.get(i));
            let keys = b.map(|b| if b.mods.is_empty() { b.key.clone() } else { format!("{}+{}", b.mods, b.key) }).unwrap_or_default();
            self.fields = vec![(Field::Keys, Query::with_text(&keys)), (Field::Action, Query::with_text(b.map(|b| b.action.as_str()).unwrap_or("")))];
        }
    }

    fn field(&self, f: Field) -> &str {
        self.fields.iter().find(|(k, _)| *k == f).map(|(_, q)| q.text.as_str()).unwrap_or("")
    }

    fn field_mut(&mut self, f: Field) -> Option<&mut Query> {
        self.fields.iter_mut().find(|(k, _)| *k == f).map(|(_, q)| q)
    }

    fn editable(&self) -> bool {
        match self.selected {
            None => self.comp.is_some(),
            Some(i) if self.rules_tab => self.rules.get(i).is_some_and(|r| r.editable),
            Some(i) => self.binds.get(i).is_some_and(|b| b.editable),
        }
    }

    fn action_hint(&self) -> &'static str {
        match self.comp {
            Some(Compositor::Mango) => "spawn,firefox   killclient   togglefloating   view,2",
            Some(Compositor::Niri) => "spawn \"firefox\"   close-window   toggle-window-floating",
            Some(Compositor::Hyprland) => "firefox   (a command to run)",
            None => "",
        }
    }

    fn save(&mut self, delete: bool) {
        let Some(c) = self.comp else { return };
        if !self.editable() {
            self.message = Some(("Prism cannot change this entry; edit it in its config file".into(), false));
            return;
        }
        let result = if self.rules_tab {
            let old = self.selected.and_then(|i| self.rules.get(i)).cloned();
            let new = (!delete).then(|| Rule {
                app_id: self.field(Field::App).trim().to_string(),
                title: self.field(Field::Title).trim().to_string(),
                floating: self.floating,
                width: self.field(Field::Width).trim().to_string(),
                height: self.field(Field::Height).trim().to_string(),
                workspace: self.field(Field::Workspace).trim().to_string(),
                opacity: self.field(Field::Opacity).trim().to_string(),
                // What the form does not show (other settings, name, indentation) stays.
                ..old.clone().unwrap_or_default()
            });
            if let Some(n) = &new {
                if n.app_id.is_empty() && n.title.is_empty() {
                    self.message = Some(("Give the app (or a title) the rule is for".into(), false));
                    return;
                }
            }
            compconf::save_rule(c, old.as_ref(), new.as_ref())
        } else {
            let old = self.selected.and_then(|i| self.binds.get(i)).cloned();
            let keys = self.field(Field::Keys).trim().to_string();
            let action = self.field(Field::Action).trim().to_string();
            if !delete && (keys.is_empty() || action.is_empty()) {
                self.message = Some(("Fill in the keys and the action".into(), false));
                return;
            }
            let (mods, key) = match keys.rsplit_once('+') {
                Some((m, k)) => (m.to_string(), k.trim().to_string()),
                None => (String::new(), keys.clone()),
            };
            compconf::save_binding(c, old.as_ref(), (!delete).then_some((mods.as_str(), key.as_str(), action.as_str())))
        };
        match result {
            Ok(()) => {
                std::thread::spawn(move || compconf::reload(c));
                self.message = Some((if delete { "Deleted".into() } else { "Saved; the old file is kept as *.prism-bak".into() }, true));
                let keep = self.selected;
                self.reload();
                self.selected = if delete { None } else { keep.or(Some(if self.rules_tab { self.rules.len().saturating_sub(1) } else { self.binds.len().saturating_sub(1) })) };
                if delete || self.selected.is_none() {
                    self.selected = None;
                }
                self.load_form();
            }
            Err(e) => self.message = Some((e, false)),
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

    fn hover(&self, h: Hit) -> f64 {
        if self.hits.iter().any(|(k, r)| *k == h && r.contains(self.pointer.0, self.pointer.1)) { 1.0 } else { 0.0 }
    }

    fn set_tab(&mut self, rules: bool) {
        if self.rules_tab != rules {
            self.rules_tab = rules;
            self.selected = None;
            self.scroll = 0.0;
            self.message = None;
            self.filter();
            self.load_form();
        }
    }
}

impl CardView for CompEditView {
    fn draw(&mut self, cr: *mut Cairo, card: Rect, style: &Style, theme: &Theme) {
        crate::ui::card_surface(cr, card.x, card.y, card.w, card.h, style);
        let p = Self::palette(style, theme);
        let fg = style.fg;
        let dim = (fg.0, fg.1, fg.2, 0.6);
        let f = |px: f64, w: &str| format!("{} {w} {px}px", p.family);
        self.hits.clear();
        let x = card.x + PAD;
        let w = card.w - 2.0 * PAD;
        let mut y = card.y + PAD;

        // Header: tabs and the compositor.
        let mut tx = x;
        for (rules, label) in [(false, "Shortcuts"), (true, "Window Rules")] {
            let (tw, _) = cb::measure_pango_text(cr, label, &f(15.0, "Semi-Bold"), 400, Some(1));
            let r = Rect { x: tx, y, w: tw as f64 + 28.0, h: 36.0 };
            let on = self.rules_tab == rules;
            let hv = self.hover(Hit::Tab(rules));
            cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 18.0, if on { style.accent } else { (fg.0, fg.1, fg.2, 0.08 + 0.06 * hv) });
            cb::draw_pango_text(cr, r.x + 14.0, r.y + 8.0, label, &f(15.0, "Semi-Bold"), if on { p.on_accent } else { fg }, None);
            self.hits.push((Hit::Tab(rules), r));
            tx += r.w + 8.0;
        }
        let comp = compositor_label(self.comp);
        let (cw, _) = cb::measure_pango_text(cr, comp, &f(12.0, ""), 400, Some(1));
        cb::draw_pango_text(cr, x + w - cw as f64, y + 10.0, comp, &f(12.0, ""), dim, None);
        y += 36.0 + 14.0;

        // Left: search, New, list.
        let lw = (w * 0.52).round();
        let search = Rect { x, y, w: lw - 110.0, h: FIELD_H };
        uikit::text_field(cr, search, &self.search.text, if self.rules_tab { "Type to filter rules" } else { "Type to filter shortcuts" }, self.focus.is_none().then(|| self.search.caret()), 0.0, false, &p);
        let new = Rect { x: x + lw - 100.0, y, w: 100.0, h: FIELD_H };
        uikit::button(cr, new, "New", ButtonKind::Primary, self.hover(Hit::New), self.comp.is_some(), &p);
        self.hits.push((Hit::New, new));
        y += FIELD_H + 10.0;
        self.list = Rect { x, y, w: lw, h: card.y + card.h - PAD - y };
        let list = self.list;
        let max_scroll = (self.shown.len() as f64 * ROW_H - list.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        unsafe {
            cb::cairo_save(cr);
            cb::cairo_rectangle(cr, list.x, list.y, list.w, list.h);
            cb::cairo_clip(cr);
        }
        let first = (self.scroll / ROW_H) as usize;
        for (k, &i) in self.shown.iter().enumerate().skip(first) {
            let ry = list.y + k as f64 * ROW_H - self.scroll;
            if ry > list.y + list.h {
                break;
            }
            let r = Rect { x: list.x, y: ry, w: list.w, h: ROW_H - 4.0 };
            let sel = self.selected == Some(i);
            let hv = self.hover(Hit::Row(i));
            if sel || hv > 0.0 {
                cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 10.0, (fg.0, fg.1, fg.2, if sel { 0.16 } else { 0.07 }));
            }
            let (title, sub, editable, file) = if self.rules_tab {
                let rl = &self.rules[i];
                let mut parts = Vec::new();
                if let Some(fl) = rl.floating {
                    parts.push(if fl { "floating".to_string() } else { "tiled".to_string() });
                }
                if !rl.width.is_empty() || !rl.height.is_empty() {
                    parts.push(format!("{}x{}", rl.width, rl.height));
                }
                if !rl.workspace.is_empty() {
                    parts.push(format!("on {}", rl.workspace));
                }
                if !rl.opacity.is_empty() {
                    parts.push(format!("opacity {}", rl.opacity));
                }
                parts.extend(rl.other.iter().map(|o| o.trim().to_string()));
                let t = if rl.app_id.is_empty() { format!("title: {}", rl.title) } else { rl.app_id.clone() };
                (t, parts.join(" \u{b7} "), rl.editable, &rl.file)
            } else {
                let b = &self.binds[i];
                let combo = if b.mods.is_empty() { b.key.clone() } else { format!("{}+{}", b.mods, b.key) };
                (combo, b.action.clone(), b.editable, &b.file)
            };
            cb::draw_pango_text(cr, r.x + 12.0, r.y + 4.0, &title, &f(13.0, "Semi-Bold"), fg, Some((r.w * 0.45) as i32));
            let file = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let tag = if editable { file } else { format!("{file} (read-only)") };
            let (tgw, _) = cb::measure_pango_text(cr, &tag, &f(11.0, ""), 400, Some(1));
            cb::draw_pango_text(cr, r.x + r.w - 12.0 - tgw as f64, r.y + 6.0, &tag, &f(11.0, ""), dim, None);
            cb::draw_pango_text(cr, r.x + 12.0, r.y + 23.0, &sub, &f(12.0, ""), (fg.0, fg.1, fg.2, 0.75), Some((r.w - 24.0) as i32));
            self.hits.push((Hit::Row(i), r));
        }
        if self.shown.is_empty() {
            let msg = if self.comp.is_none() { "Run Prism on MangoWM, niri or Hyprland to edit its config." } else if self.rules_tab { "No window rules yet. New adds one." } else { "No shortcuts match." };
            cb::draw_pango_text(cr, list.x + 12.0, list.y + 12.0, msg, &f(13.0, ""), dim, Some(list.w as i32 - 24));
        }
        unsafe { cb::cairo_restore(cr) };

        // Right: the form.
        let fx = x + lw + 24.0;
        let fw = x + w - fx;
        let mut fy = card.y + PAD + 36.0 + 14.0;
        let heading = match (self.rules_tab, self.selected) {
            (false, None) => "New shortcut",
            (false, Some(_)) => "Shortcut",
            (true, None) => "New window rule",
            (true, Some(_)) => "Window rule",
        };
        cb::draw_pango_text(cr, fx, fy, heading, &f(17.0, "Bold"), fg, None);
        if let Some(i) = self.selected {
            let (file, line) = if self.rules_tab { (&self.rules[i].file, self.rules[i].line) } else { (&self.binds[i].file, self.binds[i].line) };
            cb::draw_pango_text(cr, fx, fy + 24.0, &format!("{}:{}", file.display(), line + 1), &f(11.0, ""), dim, Some(fw as i32));
        }
        fy += 48.0;
        let editable = self.editable();
        let labels: &[(Field, &str, &str)] = if self.rules_tab {
            &[
                (Field::App, "App", "app id, e.g. pavucontrol"),
                (Field::Title, "Title (optional)", "window title pattern"),
                (Field::Width, "Width", "e.g. 900"),
                (Field::Height, "Height", "e.g. 600"),
                (Field::Workspace, "Workspace / tag", "e.g. 2"),
                (Field::Opacity, "Opacity", "e.g. 0.9"),
            ]
        } else {
            &[(Field::Keys, "Keys", "e.g. SUPER+SHIFT+T"), (Field::Action, "Action", "")]
        };
        let half = (fw - 12.0) / 2.0;
        let mut col = 0;
        for (field, label, ph) in labels {
            // Width and Height, Workspace and Opacity share a row.
            let paired = matches!(field, Field::Width | Field::Height | Field::Workspace | Field::Opacity);
            let (rx, rw) = if paired { (fx + col as f64 * (half + 12.0), half) } else { (fx, fw) };
            cb::draw_pango_text(cr, rx, fy, label, &f(12.0, "Semi-Bold"), dim, None);
            let r = Rect { x: rx, y: fy + 18.0, w: rw, h: FIELD_H };
            let q = self.fields.iter().find(|(k, _)| k == field).map(|(_, q)| q.clone()).unwrap_or_default();
            let placeholder = if *field == Field::Action { self.action_hint() } else { ph };
            uikit::text_field(cr, r, &q.text, placeholder, (self.focus == Some(*field)).then(|| q.caret()), self.hover(Hit::Field(*field)), false, &p);
            if editable {
                self.hits.push((Hit::Field(*field), r));
            }
            if *field == Field::App && !self.apps.is_empty() {
                // Open windows' apps to pick from.
                let mut cx = fx;
                let cy = r.y + FIELD_H + 6.0;
                for (k, app) in self.apps.iter().enumerate().take(8) {
                    let (aw, _) = cb::measure_pango_text(cr, app, &f(11.0, ""), 400, Some(1));
                    let c = Rect { x: cx, y: cy, w: aw as f64 + 16.0, h: 24.0 };
                    if c.x + c.w > fx + fw {
                        break;
                    }
                    cb::fill_rounded_rect(cr, c.x, c.y, c.w, c.h, 12.0, (fg.0, fg.1, fg.2, 0.08 + 0.1 * self.hover(Hit::App(k))));
                    cb::draw_pango_text(cr, c.x + 8.0, c.y + 4.0, app, &f(11.0, ""), fg, None);
                    if editable {
                        self.hits.push((Hit::App(k), c));
                    }
                    cx += c.w + 6.0;
                }
                fy += 30.0;
            }
            if paired {
                col += 1;
                if col == 2 {
                    col = 0;
                    fy += 18.0 + FIELD_H + 12.0;
                }
            } else {
                fy += 18.0 + FIELD_H + 12.0;
            }
            if *field == Field::Title {
                // Floating: leave as is / float / tile.
                cb::draw_pango_text(cr, fx, fy, "Floating", &f(12.0, "Semi-Bold"), dim, None);
                let mut cx = fx;
                for (val, label) in [(None, "Default"), (Some(true), "Float"), (Some(false), "Tile")] {
                    let (lw2, _) = cb::measure_pango_text(cr, label, &f(13.0, ""), 400, Some(1));
                    let c = Rect { x: cx, y: fy + 18.0, w: lw2 as f64 + 28.0, h: 32.0 };
                    let on = self.floating == val;
                    cb::fill_rounded_rect(cr, c.x, c.y, c.w, c.h, 16.0, if on { style.accent } else { (fg.0, fg.1, fg.2, 0.08 + 0.08 * self.hover(Hit::Float(val))) });
                    cb::draw_pango_text(cr, c.x + 14.0, c.y + 7.0, label, &f(13.0, ""), if on { p.on_accent } else { fg }, None);
                    if editable {
                        self.hits.push((Hit::Float(val), c));
                    }
                    cx += c.w + 8.0;
                }
                fy += 18.0 + 32.0 + 12.0;
            }
        }
        if !self.rules_tab {
            cb::draw_pango_text_wrapped(cr, fx, fy, &format!("Action syntax for {}: {}", compositor_label(self.comp), self.action_hint()), &f(12.0, ""), dim, fw as i32, Some(3));
        }

        // Buttons and the last message.
        let by = card.y + card.h - PAD - 42.0;
        let save = Rect { x: fx + fw - 140.0, y: by, w: 140.0, h: 42.0 };
        uikit::button(cr, save, "Save", ButtonKind::Primary, self.hover(Hit::Save), editable, &p);
        self.hits.push((Hit::Save, save));
        if self.selected.is_some() {
            let del = Rect { x: save.x - 12.0 - 110.0, y: by, w: 110.0, h: 42.0 };
            uikit::button(cr, del, "Delete", ButtonKind::Danger, self.hover(Hit::Delete), editable, &p);
            self.hits.push((Hit::Delete, del));
        }
        if let Some((m, ok)) = &self.message {
            cb::draw_pango_text_wrapped(cr, fx, by - 46.0, m, &f(12.0, ""), if *ok { dim } else { p.error }, fw as i32, Some(2));
        } else if !editable && self.selected.is_some() {
            cb::draw_pango_text_wrapped(cr, fx, by - 46.0, "Prism cannot rewrite this entry (it spans several lines or is generated by code); edit it in the file above.", &f(12.0, ""), dim, fw as i32, Some(2));
        }
    }

    fn motion(&mut self, x: f64, y: f64) -> bool {
        self.pointer = (x, y);
        true
    }

    fn click(&mut self, x: f64, y: f64, _shift: bool) -> Effect {
        let Some((h, _)) = self.hits.iter().rev().find(|(_, r)| r.contains(x, y)).copied() else {
            self.focus = None;
            return Effect::Redraw;
        };
        match h {
            Hit::Tab(r) => self.set_tab(r),
            Hit::Row(i) => {
                self.selected = Some(i);
                self.message = None;
                self.load_form();
            }
            Hit::New => {
                self.selected = None;
                self.message = None;
                self.load_form();
                self.focus = Some(if self.rules_tab { Field::App } else { Field::Keys });
            }
            Hit::Field(f) => self.focus = Some(f),
            Hit::Float(v) => self.floating = v,
            Hit::App(k) => {
                if let Some(app) = self.apps.get(k).cloned() {
                    if let Some(q) = self.field_mut(Field::App) {
                        *q = Query::with_text(&app);
                    }
                }
            }
            Hit::Save => self.save(false),
            Hit::Delete => self.save(true),
        }
        Effect::Redraw
    }

    fn scroll(&mut self, delta: f64) -> bool {
        if self.list.contains(self.pointer.0, self.pointer.1) {
            self.scroll = (self.scroll + delta).max(0.0);
            return true;
        }
        false
    }

    fn key(&mut self, special: u32, typed: Option<char>, shift: bool, ctrl: bool) -> Effect {
        match special {
            ESC => {
                if self.focus.is_some() {
                    self.focus = None;
                    return Effect::Redraw;
                }
                if !self.search.text.is_empty() {
                    self.search.clear();
                    self.filter();
                    return Effect::Redraw;
                }
                return Effect::Close;
            }
            ENTER if self.focus.is_some() => {
                self.save(false);
                return Effect::Redraw;
            }
            TAB if ctrl => {
                self.set_tab(!self.rules_tab);
                return Effect::Redraw;
            }
            TAB => {
                let order: &[Field] = if self.rules_tab { &RULE_FIELDS } else { &BIND_FIELDS };
                let i = self.focus.and_then(|f| order.iter().position(|o| *o == f));
                self.focus = Some(match (i, shift) {
                    (None, _) => order[0],
                    (Some(i), false) => order[(i + 1) % order.len()],
                    (Some(i), true) => order[(i + order.len() - 1) % order.len()],
                });
                return Effect::Redraw;
            }
            UP | DOWN if self.focus.is_none() && !self.shown.is_empty() => {
                let pos = self.selected.and_then(|s| self.shown.iter().position(|&i| i == s));
                let next = match (pos, special == DOWN) {
                    (None, _) => 0,
                    (Some(p), true) => (p + 1).min(self.shown.len() - 1),
                    (Some(p), false) => p.saturating_sub(1),
                };
                self.selected = Some(self.shown[next]);
                let top = next as f64 * ROW_H;
                if top < self.scroll {
                    self.scroll = top;
                } else if top + ROW_H > self.scroll + self.list.h {
                    self.scroll = top + ROW_H - self.list.h;
                }
                self.load_form();
                return Effect::Redraw;
            }
            _ => {}
        }
        match self.focus {
            Some(f) => {
                let Some(q) = self.field_mut(f) else { return Effect::None };
                if q.key(special, typed, ctrl).is_none() {
                    return Effect::None;
                }
            }
            None => match self.search.key(special, typed, ctrl) {
                Some(true) => {
                    self.scroll = 0.0;
                    self.filter();
                }
                Some(false) => {}
                None => return Effect::None,
            },
        }
        Effect::Redraw
    }

    fn clickable(&self) -> bool {
        self.hits.iter().any(|(_, r)| r.contains(self.pointer.0, self.pointer.1))
    }
}
