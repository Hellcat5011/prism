// ─────────────────────────────────────────────────────────────────────────
// desktop/edit.rs — desktop edit mode (`prism-ui desktop-edit`)
//
// Port of modules/DesktopEditMode.qml: a full-screen overlay showing the
// wallpaper with live previews of every widget at its working position.
//
//   drag a widget          move it (clamped on screen)
//   bottom-right grip      resize (per-widget minimum sizes; tray: thickness)
//   gear (top-right)       options popover: transparent background, send to
//                          background, hide date (clock), orientation (tray,
//                          shortcuts), shortcut icons, font
//   x (top-left)           remove the widget; re-add it from "+"
//   Save / Discard / Esc   save writes screen fractions to config.json; the
//                          desktop layer reloads it via inotify
// ─────────────────────────────────────────────────────────────────────────

use prism_common::config::DesktopLayoutConfig;
use prism_common::{Config, Theme};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape as CursorShape, WpCursorShapeDeviceV1};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use super::layout::{shortcuts_vertical, store_rect, tray_length, tray_rect, widget_rect, Rect, WidgetId};
use super::text::{draw_in_box, Align, TextStyle};
use super::widgets::*;
use super::ShmBuf;
use crate::cairo_backend::*;
use crate::ui::Style;

const GRIP: f64 = 18.0;
const BTN: f64 = 24.0;
const POP_W: f64 = 270.0;
const ROW_H: f64 = 40.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Move(WidgetId),
    Resize(WidgetId),
    Gear(WidgetId),
    Remove(WidgetId),
    Toggle(WidgetId, Opt),
    /// Open the font dropdown for a widget.
    Font(WidgetId),
    Add,
    AddWidget(WidgetId),
    Save,
    Discard,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Opt {
    Transparent,
    Background,
    HideDate,
    Vertical,
    Shortcut(usize),
}

struct Drag {
    id: WidgetId,
    resize: bool,
    start: (f64, f64),
    orig: Rect,
    cur: Rect,
}

struct Edit {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,
    cursor_mgr: Option<WpCursorShapeManagerV1>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    surface: Option<wl_surface::WlSurface>,
    layer: Option<ZwlrLayerSurfaceV1>,
    size: (i32, i32),
    bufs: Vec<ShmBuf>,
    frame_cb: Option<wl_callback::WlCallback>,
    dirty: bool,

    layout: DesktopLayoutConfig,
    look: Look,
    theme: Theme,
    tray_count: usize,
    /// "Default" + installed font families (the dropdown's options).
    fonts: Vec<String>,

    actions: Vec<(Action, Rect)>,
    hover: Option<Action>,
    popover: Option<WidgetId>,
    add_open: bool,
    drag: Option<Drag>,
    ptr: (f64, f64),
    serial: u32,
    exit: bool,
    /// Open font dropdown (widget, list state) and the combo it hangs from.
    font_dd: Option<(WidgetId, crate::uikit::Dropdown)>,
    font_anchor: Rect,
    xkb_context: xkbcommon::xkb::Context,
    xkb_state: Option<xkbcommon::xkb::State>,
}

/// Edit mode as a component of the prism-ui event loop.
pub struct EditSession {
    queue: EventQueue<Edit>,
    state: Edit,
}

impl EditSession {
    pub fn open(conn: &Connection) -> Option<EditSession> {
        let mut queue: EventQueue<Edit> = conn.new_event_queue();
        let qh = queue.handle();
        let config = Config::load();
        let theme = Theme::load();
        let style = Style::new(&config, &theme);
        set_global_font(&style.font_family);
        let mut e = Edit {
            compositor: None,
            shm: None,
            layer_shell: None,
            seat: None,
            cursor_mgr: None,
            cursor_dev: None,
            pointer: None,
            keyboard: None,
            surface: None,
            layer: None,
            size: (0, 0),
            bufs: Vec::new(),
            frame_cb: None,
            dirty: true,
            layout: config.desktop_layout,
            look: Look::new(&style, &theme),
            theme,
            tray_count: registered_tray_items(),
            fonts: std::iter::once("Default".to_string()).chain(crate::uikit::font_families()).collect(),
            actions: Vec::new(),
            hover: None,
            popover: None,
            add_open: false,
            drag: None,
            ptr: (0.0, 0.0),
            serial: 0,
            exit: false,
            font_dd: None,
            font_anchor: Rect::default(),
            xkb_context: xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
        };
        let _reg = conn.display().get_registry(&qh, ());
        queue.roundtrip(&mut e).ok()?;
        let (Some(comp), Some(ls)) = (e.compositor.clone(), e.layer_shell.clone()) else {
            eprintln!("[prism-edit] missing wl_compositor / layer-shell");
            return None;
        };
        let surface = comp.create_surface(&qh, ());
        let layer = ls.get_layer_surface(&surface, None, Layer::Overlay, "prism-desktop-edit".to_string(), &qh, ());
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();
        e.surface = Some(surface);
        e.layer = Some(layer);
        Some(EditSession { queue, state: e })
    }

    pub fn dispatch(&mut self) -> bool {
        self.queue.dispatch_pending(&mut self.state).is_ok()
    }

    pub fn finished(&self) -> bool {
        self.state.exit
    }

    pub fn close(&mut self) {
        self.state.exit = true;
    }

    pub fn teardown(mut self) {
        let e = &mut self.state;
        e.bufs.clear();
        if let Some(l) = e.layer.take() {
            l.destroy();
        }
        if let Some(s) = e.surface.take() {
            s.destroy();
        }
        if let Some(d) = e.cursor_dev.take() {
            d.destroy();
        }
        if let Some(p) = e.pointer.take() {
            p.release();
        }
        if let Some(k) = e.keyboard.take() {
            k.release();
        }
        let _ = self.queue.dispatch_pending(&mut self.state);
    }
}

impl Edit {
    fn screen(&self) -> (f64, f64) {
        (self.size.0.max(1) as f64, self.size.1.max(1) as f64)
    }

    fn rect(&self, id: WidgetId) -> Rect {
        if let Some(d) = &self.drag {
            if d.id == id {
                return d.cur;
            }
        }
        let (sw, sh) = self.screen();
        widget_rect(id, id.cfg(&self.layout), sw, sh, self.tray_count)
    }

    fn request(&mut self, qh: &QueueHandle<Self>) {
        self.dirty = true;
        if self.frame_cb.is_none() {
            self.draw(qh);
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (bw, bh) = self.size;
        if bw <= 0 || bh <= 0 {
            return;
        }
        self.bufs.retain(|b| b.w == bw && b.h == bh);
        let idx = match self.bufs.iter().position(|b| !b.busy) {
            Some(i) => i,
            None if self.bufs.len() < 2 => {
                let Some(shm) = &self.shm else { return };
                let Some(b) = ShmBuf::new(shm, qh, bw, bh, ()) else { return };
                self.bufs.push(b);
                self.bufs.len() - 1
            }
            None => return,
        };
        let surf = self.bufs[idx].cairo();
        let cr = unsafe { cairo_create(surf) };
        self.paint(cr);
        unsafe {
            cairo_destroy(cr);
            cairo_surface_destroy(surf);
        }
        let Some(surface) = self.surface.clone() else { return };
        self.bufs[idx].busy = true;
        surface.attach(Some(&self.bufs[idx].buffer), 0, 0);
        surface.damage_buffer(0, 0, bw, bh);
        if self.frame_cb.is_none() {
            self.frame_cb = Some(surface.frame(qh, ()));
        }
        surface.commit();
        self.dirty = false;
    }

    fn paint(&mut self, cr: *mut Cairo) {
        let (sw, sh) = self.screen();
        let look = self.look.clone();
        self.actions.clear();
        // 1. Wallpaper (the current one, as set by Prism), else the theme background.
        let home = std::env::var("HOME").unwrap_or_default();
        let wp = format!("{}/.wa.jpg", home);
        let painted = cached_image(&wp, sw as i32, sh as i32, false).map(|img| paint_cached(cr, img, 0.0, 0.0)).is_some();
        if !painted {
            let bg = hex_to_rgba(&self.theme.background, 1.0);
            unsafe {
                cairo_set_source_rgba(cr, bg.0, bg.1, bg.2, 1.0);
                cairo_paint(cr);
            }
        }

        // 2. Widgets with edit chrome
        let now = LocalTime::now();
        for id in WidgetId::ALL {
            let p = id.cfg(&self.layout).clone();
            if !p.visible {
                continue;
            }
            let r = self.rect(id);
            unsafe {
                cairo_save(cr);
                cairo_translate(cr, r.x, r.y);
            }
            match id {
                WidgetId::Clock => draw_clock(cr, r.w, r.h, &p, &look, &now),
                WidgetId::AnalogClock => {
                    let hands = ClockHands {
                        hour: ((now.hour % 12) as f64 + now.minute as f64 / 60.0) * 30.0,
                        minute: (now.minute as f64 + now.second as f64 / 60.0) * 6.0,
                        second: now.second as f64 * 6.0,
                    };
                    draw_analog_clock(cr, r.w, r.h, &p, &look, &hands)
                }
                WidgetId::Calendar => {
                    let cal = CalState { year: now.year, month: now.month0, view: CalView::Days };
                    draw_calendar(cr, r.w, r.h, &p, &look, &cal, &now);
                }
                WidgetId::Mpris => {
                    let v = MediaView {
                        media: None,
                        art: None,
                        palette: MediaPalette::fallback(&look),
                        wave_time: 0.0,
                        wave_height: 0.0,
                        position: 0.0,
                    };
                    draw_mpris(cr, r.w, r.h, &p, &look, &v);
                }
                WidgetId::Tray => {
                    let (_, vertical, _) = tray_rect(&p, self.tray_count, sw, sh);
                    draw_tray(cr, r.w, r.h, &p, &look, &[], &[], vertical, None, true);
                }
                WidgetId::Shortcuts => {
                    let lr = Rect { x: 0.0, y: 0.0, w: r.w, h: r.h };
                    draw_shortcuts(cr, r.w, r.h, &p, &look, shortcuts_vertical(&p, &lr), None, false, true);
                }
                WidgetId::Phone => {
                    let st = crate::phone_client::snapshot().1;
                    super::widgets::draw_phone(cr, r.w, r.h, &p, &look, st.as_ref(), None);
                }
            }
            unsafe { cairo_restore(cr) };

            // Outline (dashed when the widget has no background)
            let outline = if self.popover == Some(id) { look.primary } else { (look.fg.0, look.fg.1, look.fg.2, 0.55) };
            unsafe {
                if p.transparent_bg {
                    let dash = [6.0f64, 4.0];
                    cairo_set_dash(cr, dash.as_ptr(), 2, 0.0);
                }
            }
            stroke_rounded_rect(cr, r.x + 0.75, r.y + 0.75, r.w - 1.5, r.h - 1.5, 10.0, outline, 1.5);
            unsafe { cairo_set_dash(cr, std::ptr::null(), 0, 0.0) };

            self.actions.push((Action::Move(id), r));
            // Remove (top-left), gear (top-right), resize grip (bottom-right)
            let rm = Rect { x: r.x + 6.0, y: r.y + 6.0, w: BTN, h: BTN };
            let gear = Rect { x: r.x + r.w - BTN - 6.0, y: r.y + 6.0, w: BTN, h: BTN };
            let grip = Rect { x: r.x + r.w - GRIP, y: r.y + r.h - GRIP, w: GRIP, h: GRIP };
            self.round_button(cr, rm, "\u{2715}", self.hover == Some(Action::Remove(id)), false);
            self.round_button(cr, gear, "\u{2699}", self.hover == Some(Action::Gear(id)), self.popover == Some(id));
            unsafe {
                let c = if self.hover == Some(Action::Resize(id)) { look.primary } else { look.fg };
                cairo_set_source_rgba(cr, c.0, c.1, c.2, 0.85);
                cairo_set_line_width(cr, 2.0);
                for k in 0..3 {
                    let o = 4.0 + k as f64 * 4.0;
                    cairo_move_to(cr, grip.x + GRIP - 3.0, grip.y + o);
                    cairo_line_to(cr, grip.x + o, grip.y + GRIP - 3.0);
                }
                cairo_stroke(cr);
            }
            self.actions.push((Action::Resize(id), grip));
            self.actions.push((Action::Remove(id), rm));
            self.actions.push((Action::Gear(id), gear));
        }

        // 3. Popover for the selected widget
        if let Some(id) = self.popover {
            if id.cfg(&self.layout).visible {
                self.paint_popover(cr, id);
            }
        }

        // 4. Controls panel (top-centre, clear of the default widget column): + / Discard / Save
        let panel_w = 300.0;
        let panel = Rect { x: ((sw - panel_w) / 2.0).round(), y: 30.0, w: panel_w, h: 56.0 };
        fill_rounded_rect(cr, panel.x, panel.y, panel.w, panel.h, 12.0, look.card);
        stroke_rounded_rect(cr, panel.x, panel.y, panel.w, panel.h, 12.0, (look.fg.0, look.fg.1, look.fg.2, 0.18), 1.0);
        let add = Rect { x: panel.x + 10.0, y: panel.y + 10.0, w: 36.0, h: 36.0 };
        let discard = Rect { x: panel.x + 56.0, y: panel.y + 10.0, w: 112.0, h: 36.0 };
        let save = Rect { x: panel.x + 178.0, y: panel.y + 10.0, w: 112.0, h: 36.0 };
        let removed: Vec<WidgetId> = WidgetId::ALL.into_iter().filter(|w| !w.cfg(&self.layout).visible).collect();
        self.pill(cr, add, "+", self.hover == Some(Action::Add) || self.add_open, false, !removed.is_empty());
        self.pill(cr, discard, "Discard", self.hover == Some(Action::Discard), false, true);
        self.pill(cr, save, "Save", self.hover == Some(Action::Save), true, true);
        self.actions.push((Action::Add, add));
        self.actions.push((Action::Discard, discard));
        self.actions.push((Action::Save, save));

        if self.add_open && !removed.is_empty() {
            let dd = Rect { x: panel.x, y: panel.y + panel.h + 8.0, w: 220.0, h: 12.0 + removed.len() as f64 * 36.0 };
            fill_rounded_rect(cr, dd.x, dd.y, dd.w, dd.h, 12.0, look.card);
            stroke_rounded_rect(cr, dd.x, dd.y, dd.w, dd.h, 12.0, (look.fg.0, look.fg.1, look.fg.2, 0.18), 1.0);
            for (k, wid) in removed.iter().enumerate() {
                let row = Rect { x: dd.x + 6.0, y: dd.y + 6.0 + k as f64 * 36.0, w: dd.w - 12.0, h: 36.0 };
                if self.hover == Some(Action::AddWidget(*wid)) {
                    fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 8.0, (look.primary.0, look.primary.1, look.primary.2, 0.22));
                }
                draw_in_box(cr, &format!("+  {}", wid.label()), &TextStyle { family: &look.family, px: 13.0, bold: false, color: look.fg }, row.x + 10.0, row.y, row.w - 20.0, row.h, Align::Start);
                self.actions.push((Action::AddWidget(*wid), row));
            }
        }

        // 5. Font dropdown on top of everything.
        let pal = self.palette();
        let anchor = self.font_anchor;
        let fonts = self.fonts.clone();
        if let Some((id, dd)) = self.font_dd.as_mut() {
            let cur = id.cfg(&self.layout).font_family.clone();
            let sel = if cur.is_empty() { Some(0) } else { fonts.iter().position(|f| *f == cur) };
            dd.draw(cr, anchor, Rect { x: 0.0, y: 0.0, w: sw, h: sh }, &fonts, sel, true, &pal);
        }
    }

    fn palette(&self) -> crate::uikit::Palette {
        crate::uikit::Palette {
            fg: self.look.fg,
            accent: self.look.primary,
            card: self.look.card,
            on_accent: self.look.background,
            outline: hex_to_rgba(&self.theme.outline_variant, 1.0),
            error: hex_to_rgba(&self.theme.error, 1.0),
            family: self.look.family.clone(),
        }
    }

    fn paint_popover(&mut self, cr: *mut Cairo, id: WidgetId) {
        let (sw, sh) = self.screen();
        let look = self.look.clone();
        let p = id.cfg(&self.layout).clone();
        let r = self.rect(id);
        let mut rows: Vec<(&str, &str, Option<Opt>)> = vec![
            ("Transparent background", "Hide widget container fill", Some(Opt::Transparent)),
            ("Send to background", "Pass behind wallpaper scenery", Some(Opt::Background)),
        ];
        if id == WidgetId::Clock {
            rows.push(("Hide date", "Show only the time", Some(Opt::HideDate)));
        }
        if id == WidgetId::Tray || id == WidgetId::Shortcuts {
            rows.push(("Vertical", "Stack icons top to bottom", Some(Opt::Vertical)));
        }
        let has_font = id != WidgetId::AnalogClock && id != WidgetId::Shortcuts;
        let shortcut_rows = if id == WidgetId::Shortcuts { SHORTCUTS.len() } else { 0 };
        let h = 16.0 + rows.len() as f64 * ROW_H + shortcut_rows as f64 * 30.0 + if has_font { ROW_H } else { 0.0 } + 8.0;
        let x = if r.x + 6.0 + POP_W > sw { (r.x + r.w - POP_W - 6.0).max(0.0) } else { r.x + 6.0 };
        let y = if r.y + 36.0 + h > sh { (r.y - h - 6.0).max(0.0) } else { r.y + 36.0 };
        let pop = Rect { x, y, w: POP_W, h };
        fill_rounded_rect(cr, x, y, POP_W, h, 12.0, (look.card.0, look.card.1, look.card.2, 0.98));
        stroke_rounded_rect(cr, x, y, POP_W, h, 12.0, hex_to_rgba(&self.theme.outline_variant, 1.0), 1.0);
        // Swallow clicks inside the popover background.
        self.actions.push((Action::Gear(id), pop));

        let mut cy = y + 8.0;
        for (title, sub, opt) in rows {
            let on = match opt {
                Some(Opt::Transparent) => p.transparent_bg,
                Some(Opt::Background) => p.send_to_background,
                Some(Opt::HideDate) => p.hide_date,
                Some(Opt::Vertical) => {
                    if id == WidgetId::Tray {
                        tray_rect(&p, self.tray_count, sw, sh).1
                    } else {
                        shortcuts_vertical(&p, &r)
                    }
                }
                _ => false,
            };
            draw_in_box(cr, title, &TextStyle { family: &look.family, px: 12.0, bold: true, color: look.fg }, x + 14.0, cy + 2.0, POP_W - 90.0, 18.0, Align::Start);
            draw_in_box(cr, sub, &TextStyle { family: &look.family, px: 10.0, bold: false, color: (look.fg.0, look.fg.1, look.fg.2, 0.6) }, x + 14.0, cy + 20.0, POP_W - 90.0, 14.0, Align::Start);
            let sw_rect = Rect { x: x + POP_W - 14.0 - 40.0, y: cy + 8.0, w: 40.0, h: 22.0 };
            self.switch(cr, sw_rect, on);
            if let Some(o) = opt {
                self.actions.push((Action::Toggle(id, o), Rect { x, y: cy, w: POP_W, h: ROW_H }));
            }
            cy += ROW_H;
        }
        #[allow(clippy::needless_range_loop)] // k is also the shortcut's option index
        for k in 0..shortcut_rows {
            let on = shortcut_enabled(&p, k);
            let row = Rect { x: x + 8.0, y: cy, w: POP_W - 16.0, h: 30.0 };
            let check = if on { "\u{f012c}" } else { "" };
            fill_rounded_rect(cr, row.x + 6.0, row.y + 6.0, 18.0, 18.0, 4.0, if on { look.primary } else { (look.fg.0, look.fg.1, look.fg.2, 0.18) });
            draw_in_box(cr, check, &TextStyle { family: super::text::ICON_FONT, px: 13.0, bold: false, color: look.background }, row.x + 6.0, row.y + 6.0, 18.0, 18.0, Align::Center);
            draw_in_box(cr, SHORTCUTS[k].1, &TextStyle { family: &look.family, px: 12.0, bold: false, color: look.fg }, row.x + 34.0, row.y, row.w - 40.0, row.h, Align::Start);
            self.actions.push((Action::Toggle(id, Opt::Shortcut(k)), row));
            cy += 30.0;
        }
        if has_font {
            let name = if p.font_family.is_empty() { "Default".to_string() } else { p.font_family.clone() };
            draw_in_box(cr, "Font", &TextStyle { family: &look.family, px: 12.0, bold: true, color: look.fg }, x + 14.0, cy, 60.0, ROW_H, Align::Start);
            let combo = Rect { x: x + 64.0, y: cy + 5.0, w: POP_W - 78.0, h: 30.0 };
            let open = self.font_dd.as_ref().map(|(w, _)| *w == id).unwrap_or(false);
            let font = if p.font_family.is_empty() { None } else { Some(p.font_family.as_str()) };
            crate::uikit::combo(cr, combo, &name, open, if self.hover == Some(Action::Font(id)) { 1.0 } else { 0.0 }, &self.palette(), font);
            self.font_anchor = combo;
            self.actions.push((Action::Font(id), combo));
        }
    }

    fn round_button(&self, cr: *mut Cairo, r: Rect, glyph: &str, hover: bool, active: bool) {
        let look = &self.look;
        let hi = hover || active;
        let bg = if hi { look.primary } else { (look.card.0, look.card.1, look.card.2, 0.95) };
        let fg = if hi { hex_to_rgba(&self.theme.inverse_primary, 1.0) } else { look.fg };
        fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.w / 2.0, bg);
        stroke_rounded_rect(cr, r.x, r.y, r.w, r.h, r.w / 2.0, if hi { look.primary } else { hex_to_rgba(&self.theme.outline_variant, 1.0) }, 1.0);
        draw_in_box(cr, glyph, &TextStyle { family: &look.family, px: 13.0, bold: false, color: fg }, r.x, r.y, r.w, r.h, Align::Center);
    }

    fn pill(&self, cr: *mut Cairo, r: Rect, label: &str, hover: bool, primary: bool, enabled: bool) {
        let look = &self.look;
        let a = if enabled { 1.0 } else { 0.4 };
        let (bg, fg) = if primary {
            ((look.primary.0, look.primary.1, look.primary.2, if hover { 1.0 } else { 0.85 }), look.background)
        } else {
            ((look.fg.0, look.fg.1, look.fg.2, if hover { 0.22 } else { 0.10 }), look.fg)
        };
        fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 10.0, (bg.0, bg.1, bg.2, bg.3 * a));
        draw_in_box(cr, label, &TextStyle { family: &look.family, px: 13.0, bold: true, color: (fg.0, fg.1, fg.2, a) }, r.x, r.y, r.w, r.h, Align::Center);
    }

    fn switch(&self, cr: *mut Cairo, r: Rect, on: bool) {
        let look = &self.look;
        let track = if on { look.primary } else { (look.fg.0, look.fg.1, look.fg.2, 0.2) };
        fill_rounded_rect(cr, r.x, r.y, r.w, r.h, r.h / 2.0, track);
        let d = r.h - 6.0;
        let kx = if on { r.x + r.w - d - 3.0 } else { r.x + 3.0 };
        let knob = if on { look.background } else { look.fg };
        fill_rounded_rect(cr, kx, r.y + 3.0, d, d, d / 2.0, knob);
    }

    fn action_at(&self, x: f64, y: f64) -> Option<Action> {
        // Last pushed = topmost.
        self.actions.iter().rev().find(|(_, r)| r.contains(x, y)).map(|(a, _)| *a)
    }

    fn set_cursor(&mut self, qh: &QueueHandle<Self>, shape: CursorShape) {
        if self.cursor_dev.is_none() {
            if let (Some(m), Some(p)) = (&self.cursor_mgr, &self.pointer) {
                self.cursor_dev = Some(m.get_pointer(p, qh, ()));
            }
        }
        if let Some(d) = &self.cursor_dev {
            d.set_shape(self.serial, shape);
        }
    }

    fn on_press(&mut self, qh: &QueueHandle<Self>) {
        let (x, y) = self.ptr;
        // The open font dropdown takes the click first.
        if let Some((id, dd)) = self.font_dd.as_ref() {
            let id = *id;
            if dd.contains(x, y) {
                if let Some(i) = dd.pick(x, y) {
                    id.cfg_mut(&mut self.layout).font_family = if i == 0 { String::new() } else { self.fonts[i].clone() };
                    self.font_dd = None;
                }
                self.request(qh);
                return;
            }
            self.font_dd = None;
            if self.font_anchor.contains(x, y) {
                self.request(qh);
                return; // clicking the combo again closes it
            }
        }
        let action = self.action_at(x, y);
        // Clicking anywhere else closes popover / dropdown.
        if !matches!(action, Some(Action::Toggle(..) | Action::Font(_) | Action::Gear(_))) {
            self.popover = None;
        }
        if !matches!(action, Some(Action::Add | Action::AddWidget(_))) {
            self.add_open = false;
        }
        match action {
            Some(Action::Move(id)) | Some(Action::Resize(id)) => {
                let resize = matches!(action, Some(Action::Resize(_)));
                let r = self.rect(id);
                self.drag = Some(Drag { id, resize, start: (x, y), orig: r, cur: r });
            }
            Some(Action::Gear(id)) => {
                if self.popover == Some(id) && self.action_at(x, y) == Some(Action::Gear(id)) {
                    // Clicking the gear again closes; clicking the popover body keeps it.
                    let r = self.rect(id);
                    let gear = Rect { x: r.x + r.w - BTN - 6.0, y: r.y + 6.0, w: BTN, h: BTN };
                    if gear.contains(x, y) {
                        self.popover = None;
                    }
                } else {
                    self.popover = Some(id);
                }
            }
            Some(Action::Remove(id)) => {
                id.cfg_mut(&mut self.layout).visible = false;
                self.popover = None;
            }
            Some(Action::Toggle(id, opt)) => self.toggle(id, opt),
            Some(Action::Font(id)) => self.font_dd = Some((id, crate::uikit::Dropdown::default())),
            Some(Action::Add) => self.add_open = !self.add_open,
            Some(Action::AddWidget(id)) => {
                id.cfg_mut(&mut self.layout).visible = true;
                self.add_open = false;
            }
            Some(Action::Save) => {
                self.save();
                self.exit = true;
            }
            Some(Action::Discard) => self.exit = true,
            None => {}
        }
        self.request(qh);
    }

    fn toggle(&mut self, id: WidgetId, opt: Opt) {
        let (sw, sh) = self.screen();
        let r = self.rect(id);
        let count = self.tray_count;
        let p = id.cfg_mut(&mut self.layout);
        match opt {
            Opt::Transparent => p.transparent_bg = !p.transparent_bg,
            Opt::Background => p.send_to_background = !p.send_to_background,
            Opt::HideDate => p.hide_date = !p.hide_date,
            Opt::Shortcut(k) => {
                let s = &mut p.shortcuts;
                match k {
                    0 => s.launcher = !s.launcher,
                    1 => s.phone = !s.phone,
                    2 => s.notifications = !s.notifications,
                    3 => s.settings = !s.settings,
                    _ => s.power = !s.power,
                }
            }
            Opt::Vertical => {
                if id == WidgetId::Tray {
                    // updateTrayVertical: keep thickness, swap the axis.
                    let (cur, vertical, t) = tray_rect(p, count, sw, sh);
                    let len = tray_length(count, t);
                    let v = !vertical;
                    let nr = Rect { x: cur.x, y: cur.y, w: if v { t } else { len }, h: if v { len } else { t } };
                    p.vertical = v;
                    store_rect(p, &nr, sw, sh);
                } else {
                    // updateShortcutsVertical: swap width/height when switching orientation.
                    let v = !shortcuts_vertical(p, &r);
                    p.vertical = v;
                    if (v && r.w > r.h) || (!v && r.h > r.w) {
                        let (mw, mh) = id.min_size();
                        let nr = Rect { x: r.x, y: r.y, w: r.h.max(mw).min(sw), h: r.w.max(mh).min(sh) };
                        let nr = Rect { x: nr.x.min(sw - nr.w).max(0.0), y: nr.y.min(sh - nr.h).max(0.0), ..nr };
                        store_rect(p, &nr, sw, sh);
                    }
                }
            }
        }
    }

    fn on_motion(&mut self, qh: &QueueHandle<Self>) {
        let (x, y) = self.ptr;
        let (sw, sh) = self.screen();
        if let Some(d) = self.drag.as_mut() {
            let (dx, dy) = (x - d.start.0, y - d.start.1);
            let o = d.orig;
            d.cur = if d.resize {
                let (mw, mh) = d.id.min_size();
                if d.id == WidgetId::Tray {
                    // Resizing the tray changes its thickness; its length follows the icon count.
                    let p = d.id.cfg(&self.layout);
                    let (_, vertical, t) = tray_rect(p, self.tray_count, sw, sh);
                    let main = if vertical { o.h } else { o.w };
                    let delta = if vertical { dy } else { dx };
                    let nt = (t * ((main + delta) / main).max(0.1)).round().max(mw.min(mh));
                    let len = tray_length(self.tray_count, nt);
                    let (w, h) = if vertical { (nt, len) } else { (len, nt) };
                    Rect { x: o.x, y: o.y, w: w.min(sw), h: h.min(sh) }
                } else {
                    let w = (o.w + dx).max(mw).min(sw - o.x);
                    let h = (o.h + dy).max(mh).min(sh - o.y);
                    Rect { x: o.x, y: o.y, w: w.round(), h: h.round() }
                }
            } else {
                Rect {
                    x: (o.x + dx).round().min(sw - o.w).max(0.0),
                    y: (o.y + dy).round().min(sh - o.h).max(0.0),
                    ..o
                }
            };
            self.request(qh);
            return;
        }
        if let Some((_, dd)) = self.font_dd.as_mut() {
            if dd.contains(x, y) {
                let h = dd.pick(x, y);
                if h != dd.hover {
                    dd.hover = h;
                    self.request(qh);
                }
                return;
            }
        }
        let hover = self.action_at(x, y);
        if hover != self.hover {
            self.hover = hover;
            let shape = match hover {
                Some(Action::Move(_)) => CursorShape::Grab,
                Some(Action::Resize(_)) => CursorShape::NwseResize,
                Some(_) => CursorShape::Pointer,
                None => CursorShape::Default,
            };
            self.set_cursor(qh, shape);
            self.request(qh);
        }
    }

    fn on_release(&mut self, qh: &QueueHandle<Self>) {
        if let Some(d) = self.drag.take() {
            let (sw, sh) = self.screen();
            let p = d.id.cfg_mut(&mut self.layout);
            store_rect(p, &d.cur, sw, sh);
            if d.id == WidgetId::Tray {
                // Store the rectangle the tray really occupies for the current icon count.
                let (r, vertical, _) = tray_rect(p, self.tray_count, sw, sh);
                p.vertical = vertical;
                store_rect(p, &r, sw, sh);
            }
            self.request(qh);
        }
    }

    fn save(&mut self) {
        // Persist every widget as screen fractions so the layout survives resolution changes.
        let (sw, sh) = self.screen();
        for id in WidgetId::ALL {
            let r = widget_rect(id, id.cfg(&self.layout), sw, sh, self.tray_count);
            store_rect(id.cfg_mut(&mut self.layout), &r, sw, sh);
        }
        let mut cfg = Config::load();
        cfg.desktop_layout = self.layout.clone();
        if let Err(e) = cfg.save() {
            eprintln!("[prism-edit] failed to save layout: {}", e);
        }
    }
}

/// Number of tray items currently registered (for tray length in previews).
fn registered_tray_items() -> usize {
    let Ok(conn) = zbus::blocking::Connection::session() else { return 0 };
    conn.call_method(
        Some("org.kde.StatusNotifierWatcher"),
        "/StatusNotifierWatcher",
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.kde.StatusNotifierWatcher", "RegisteredStatusNotifierItems"),
    )
    .ok()
    .and_then(|m| m.body().deserialize::<zbus::zvariant::OwnedValue>().ok())
    .and_then(|v| Vec::<String>::try_from(v).ok())
    .map(|v| v.len())
    .unwrap_or(0)
}


// ── Wayland dispatch ──────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for Edit {
    fn event(e: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_compositor" => e.compositor = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_shm" => e.shm = Some(reg.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => e.layer_shell = Some(reg.bind(name, version.min(4), qh, ())),
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = reg.bind(name, version.min(5), qh, ());
                    e.pointer = Some(seat.get_pointer(qh, ()));
                    e.keyboard = Some(seat.get_keyboard(qh, ()));
                    e.seat = Some(seat);
                }
                "wp_cursor_shape_manager_v1" => e.cursor_mgr = Some(reg.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Edit {
    fn event(e: &mut Self, ls: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                e.size = (width as i32, height as i32);
                e.dirty = true;
                if e.frame_cb.is_none() {
                    e.draw(qh);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => e.exit = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Edit {
    fn event(e: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        e.frame_cb = None;
        if e.dirty {
            e.draw(qh);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Edit {
    fn event(e: &mut Self, buffer: &wl_buffer::WlBuffer, ev: wl_buffer::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            for b in e.bufs.iter_mut() {
                if &b.buffer == buffer {
                    b.busy = false;
                }
            }
            if e.dirty && e.frame_cb.is_none() {
                e.draw(qh);
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Edit {
    fn event(e: &mut Self, _: &wl_pointer::WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                e.serial = serial;
                e.ptr = (surface_x, surface_y);
                e.on_motion(qh);
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                e.ptr = (surface_x, surface_y);
                e.on_motion(qh);
            }
            wl_pointer::Event::Axis { value, .. } => {
                let (x, y) = e.ptr;
                if let Some((_, dd)) = e.font_dd.as_mut() {
                    if dd.contains(x, y) {
                        dd.scroll_by(value * 2.5);
                        e.request(qh);
                    }
                }
            }
            wl_pointer::Event::Button { button: 0x110, state, .. } => match state {
                WEnum::Value(wl_pointer::ButtonState::Pressed) => e.on_press(qh),
                _ => e.on_release(qh),
            },
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Edit {
    fn event(e: &mut Self, _: &wl_keyboard::WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                let km = unsafe {
                    xkbcommon::xkb::Keymap::new_from_fd(&e.xkb_context, fd, size as usize, xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1, xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS)
                };
                e.xkb_state = km.ok().flatten().map(|k| xkbcommon::xkb::State::new(&k));
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(x) = e.xkb_state.as_mut() {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                let (code, typed) = crate::translate_key(e.xkb_state.as_ref(), key);
                // Esc: close the dropdown / popover first, otherwise discard and exit.
                if code == 1 {
                    if e.font_dd.is_some() || e.popover.is_some() || e.add_open {
                        e.font_dd = None;
                        e.popover = None;
                        e.add_open = false;
                        e.request(qh);
                    } else {
                        e.exit = true;
                    }
                    return;
                }
                // Typing filters the open font list.
                if let Some((_, dd)) = e.font_dd.as_mut() {
                    if dd.key(if code == 14 { None } else { typed }, code == 14) {
                        e.request(qh);
                    }
                }
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Edit {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
    wl_seat::WlSeat,
    ZwlrLayerShellV1,
    WpCursorShapeManagerV1,
    WpCursorShapeDeviceV1
);
