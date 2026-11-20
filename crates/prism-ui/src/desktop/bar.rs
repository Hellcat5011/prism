// ─────────────────────────────────────────────────────────────────────────
// desktop/bar.rs — the bar's layout and modules
//
// Three sections (left, center, right) of modules along a screen edge. On a
// left or right edge the same modules stack vertically. Draws in surface
// coordinates and returns hit regions; panel.rs owns the surface and input.
// ─────────────────────────────────────────────────────────────────────────

use prism_common::config::BarConfig;

use crate::cairo_backend::*;
use crate::ui::{Fades, SpringValue, Style};

use super::dbus::{MediaState, TrayItem};
use super::layout::Rect;
use super::panel::PanelHit;
use super::sysmon::Stats;
use super::text::{draw_in_box, measure, Align, TextStyle};
use super::widgets::{LocalTime, Look, Rgba, TrayIcon};
use super::windows::WorkspaceView;

const ICON_LAUNCHER: &str = "M4 4h6v6H4Z M14 4h6v6h-6Z M4 14h6v6H4Z M14 14h6v6h-6Z";
const ICON_BELL: &str = "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9 M10.3 21a1.94 1.94 0 0 0 3.4 0";
const ICON_POWER: &str = "M12 2v10 M18.4 6.6a9 9 0 1 1-12.77.04";
const ICON_CPU: &str = "M6 6h12v12H6Z M9 9h6v6H9Z M9 2v2 M15 2v2 M9 20v2 M15 20v2 M2 9h2 M2 15h2 M20 9h2 M20 15h2";
const ICON_MEMORY: &str = "M3 7h18v9H3Z M7 11v2 M12 11v2 M17 11v2 M6 16v3 M18 16v3";
const ICON_PACKAGE: &str = "M16.5 9.4 7.55 4.24M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16zM3.27 6.96 12 12.01l8.73-5.05M12 22.08V12";
const ICON_TEMPERATURE: &str = "M14 14.76V5a2 2 0 0 0-4 0v9.76a4 4 0 1 0 4 0Z";
// Filled.
const ICON_PREVIOUS: &str = "M19 5v14L8 12Z M5 5h2v14H5Z";
const ICON_NEXT: &str = "M5 5v14l11-7Z M17 5h2v14h-2Z";
const ICON_PLAY: &str = "M7 4v16l13-8Z";
const ICON_PAUSE: &str = "M7 4h4v16H7Z M13 4h4v16h-4Z";

/// Space between modules, and from the bar's ends to the first and last module.
const MODULE_GAP: f64 = 6.0;
const END_PAD: f64 = 8.0;
/// Longest media title on a horizontal bar.
const TITLE_MAX: f64 = 220.0;
/// The active workspace's marker gliding between workspaces.
const SPRING_GLIDE: (f64, f64) = (0.32, 0.82);

#[derive(Default)]
pub struct BarState {
    pub fades: Fades<PanelHit>,
    glide: Option<SpringValue>,
    /// Accumulated wheel movement over the workspaces.
    pub wheel: f64,
    /// The clock text last drawn, to redraw when it changes.
    pub clock: String,
}

/// What the modules show.
pub struct BarView<'a> {
    pub cfg: &'a BarConfig,
    pub look: &'a Look,
    pub style: &'a Style,
    pub workspaces: &'a [WorkspaceView],
    pub tray: &'a [TrayItem],
    pub tray_icons: &'a [TrayIcon],
    pub media: Option<&'a MediaState>,
    pub stats: Stats,
    pub updates: Option<super::updates::Updates>,
    pub now: &'a LocalTime,
    pub hover: Option<PanelHit>,
}

pub fn is_vertical(cfg: &BarConfig) -> bool {
    matches!(cfg.position.as_str(), "left" | "right")
}

/// The text of the clock module on a horizontal bar.
pub fn clock_text(cfg: &BarConfig, now: &LocalTime) -> String {
    let time = now.format(&cfg.clock_format);
    if cfg.clock_date {
        format!("{}  {}", now.format("%a %d %b"), time)
    } else {
        time
    }
}

/// Whether the clock shows seconds and so changes every second.
pub fn clock_ticks(cfg: &BarConfig) -> bool {
    ["%S", "%s", "%T", "%r", "%X", "%c"].iter().any(|f| cfg.clock_format.contains(f))
}

fn shown_workspaces<'a>(view: &BarView<'a>) -> Vec<&'a WorkspaceView> {
    view.workspaces.iter().filter(|w| !view.cfg.workspace_hide_empty || w.occupied || w.active).collect()
}

fn stat_count(cfg: &BarConfig) -> usize {
    [cfg.sysmon_cpu, cfg.sysmon_memory, cfg.sysmon_temperature].iter().filter(|on| **on).count()
}

/// Size of one clickable place in the bar.
fn cell(t: f64) -> f64 {
    (t * 0.76).round()
}

fn workspace_cell(cfg: &BarConfig, t: f64) -> f64 {
    if cfg.workspace_style == "numbers" { cell(t) } else { (t * 0.52).round() }
}

fn font<'a>(look: &'a Look, px: f64, bold: bool, color: Rgba) -> TextStyle<'a> {
    TextStyle { family: &look.family, px, bold, color }
}

/// Length of a module along the bar; 0 when it has nothing to show.
fn module_len(cr: *mut Cairo, id: &str, v: &BarView, vertical: bool, t: f64) -> f64 {
    match id {
        "workspaces" => shown_workspaces(v).len() as f64 * workspace_cell(v.cfg, t),
        "clock" if vertical => 44.0,
        "clock" => measure(cr, &clock_text(v.cfg, v.now), &font(v.look, 14.0, true, v.look.fg)).0.ceil() + 16.0,
        "sysmon" if vertical => stat_count(v.cfg) as f64 * 38.0,
        "sysmon" => stat_count(v.cfg) as f64 * 62.0,
        "tray" => v.tray.len() as f64 * cell(t),
        "mpris" => match v.media {
            None => 0.0,
            Some(_) if vertical => cell(t),
            Some(m) => {
                let title = if v.cfg.media_title { measure(cr, &media_text(m), &font(v.look, 13.0, false, v.look.fg)).0.ceil().min(TITLE_MAX) + 10.0 } else { 0.0 };
                title + 3.0 * cell(t)
            }
        },
        "updates" => match v.updates.map(|u| u.total()) {
            None | Some(0) => 0.0,
            Some(_) if vertical => cell(t),
            Some(n) => cell(t) + measure(cr, &n.to_string(), &font(v.look, 13.0, true, v.look.fg)).0.ceil() + 8.0,
        },
        "launcher" | "notifications" | "power" => cell(t),
        _ => 0.0,
    }
}

fn media_text(m: &MediaState) -> String {
    if m.artist.is_empty() { m.title.clone() } else { format!("{} \u{2014} {}", m.title, m.artist) }
}

/// Where each section starts along a bar of length `total`. The center section stays in the
/// middle unless a side section needs its place.
fn arrange(left: f64, center: f64, right: f64, total: f64) -> (f64, f64, f64) {
    let start = END_PAD;
    let end = total - END_PAD - right;
    let low = start + left + if left > 0.0 { MODULE_GAP } else { 0.0 };
    let high = end - center - if right > 0.0 { MODULE_GAP } else { 0.0 };
    let middle = (total - center) / 2.0;
    let mid = if high >= low { middle.clamp(low, high) } else { low };
    (start, mid, end)
}

/// Rectangle `len` long at `pos` along the bar, across its whole thickness.
fn along(vertical: bool, pos: f64, len: f64, t: f64) -> Rect {
    if vertical {
        Rect { x: 0.0, y: pos, w: t, h: len }
    } else {
        Rect { x: pos, y: 0.0, w: len, h: t }
    }
}

/// A square of side `size` centered in `r`.
fn centered(r: Rect, size: f64) -> Rect {
    Rect { x: r.x + (r.w - size) / 2.0, y: r.y + (r.h - size) / 2.0, w: size, h: size }
}

fn draw_hover(cr: *mut Cairo, r: Rect, level: f64, look: &Look) {
    if level > 0.0 {
        let radius = r.w.min(r.h) / 2.0;
        fill_rounded_rect(cr, r.x, r.y, r.w, r.h, radius.min(10.0), (look.fg.0, look.fg.1, look.fg.2, 0.14 * level));
    }
}

/// A round button with an icon.
fn icon_button(cr: *mut Cairo, r: Rect, hit: PanelHit, icon: &str, filled: bool, v: &BarView, st: &mut BarState, hits: &mut Vec<(PanelHit, Rect)>) {
    let b = centered(r, r.w.min(r.h));
    let level = st.fades.level(hit, v.hover == Some(hit), v.style);
    draw_hover(cr, b, level, v.look);
    let i = (b.w * 0.56).round();
    crate::svg::draw_path(cr, icon, b.x + (b.w - i) / 2.0, b.y + (b.h - i) / 2.0, i, v.look.fg, if filled { None } else { Some(2.0) });
    hits.push((hit, b));
}

fn draw_workspaces(cr: *mut Cairo, r: Rect, vertical: bool, t: f64, v: &BarView, st: &mut BarState, hits: &mut Vec<(PanelHit, Rect)>) {
    let list = shown_workspaces(v);
    let size = workspace_cell(v.cfg, t);
    let start = if vertical { r.y } else { r.x };
    let place = |i: f64| centered(along(vertical, start + i * size, size, t), size);
    let active = list.iter().position(|w| w.active);
    // The marker glides to the active workspace.
    let marker = active.map(|a| {
        let target = a as f64;
        let glide = st.glide.get_or_insert_with(|| SpringValue::new(target));
        if glide.target() != target {
            glide.set(target, SPRING_GLIDE, v.style);
        }
        glide.animated(0.002)
    });
    let opaque_card = (v.look.card.0, v.look.card.1, v.look.card.2, 1.0);
    if let Some(m) = marker {
        let p = place(m);
        match v.cfg.workspace_style.as_str() {
            "numbers" => fill_rounded_rect(cr, p.x + 3.0, p.y + 3.0, p.w - 6.0, p.h - 6.0, 8.0, v.look.fg),
            _ => {
                let (w, h) = if v.cfg.workspace_style == "pills" { if vertical { (8.0, p.h - 4.0) } else { (p.w - 4.0, 8.0) } } else { (10.0, 10.0) };
                fill_rounded_rect(cr, p.x + (p.w - w) / 2.0, p.y + (p.h - h) / 2.0, w, h, w.min(h) / 2.0, v.look.fg);
            }
        }
    }
    for (i, w) in list.iter().enumerate() {
        let p = place(i as f64);
        let hit = PanelHit::Workspace(w.index);
        let level = st.fades.level(hit, v.hover == Some(hit), v.style);
        if !w.active {
            draw_hover(cr, Rect { x: p.x + 3.0, y: p.y + 3.0, w: p.w - 6.0, h: p.h - 6.0 }, level, v.look);
        }
        // How much of this place the gliding marker covers.
        let over = marker.map_or(0.0, |m| 1.0 - (m - i as f64).abs().min(1.0));
        let dim = if w.occupied || w.active { 1.0 } else { 0.5 };
        let base = if w.urgent { v.look.error } else { (v.look.fg.0, v.look.fg.1, v.look.fg.2, dim) };
        match v.cfg.workspace_style.as_str() {
            "numbers" => {
                let color = crate::ui::mix(base, opaque_card, over);
                draw_in_box(cr, &w.name, &font(v.look, 13.0, w.active, color), p.x, p.y, p.w, p.h, Align::Center);
            }
            "pills" => {
                let (bw, bh) = if vertical { (4.0, p.h - 8.0) } else { (p.w - 8.0, 4.0) };
                fill_rounded_rect(cr, p.x + (p.w - bw) / 2.0, p.y + (p.h - bh) / 2.0, bw, bh, 2.0, base);
            }
            _ => fill_circle(cr, p.x + p.w / 2.0, p.y + p.h / 2.0, if w.occupied { 3.5 } else { 2.5 }, base),
        }
        hits.push((hit, p));
    }
}

fn draw_clock(cr: *mut Cairo, r: Rect, vertical: bool, v: &BarView) {
    if vertical {
        let (h, m) = (v.now.format("%H"), v.now.format("%M"));
        draw_in_box(cr, &h, &font(v.look, 15.0, true, v.look.fg), r.x, r.y + 2.0, r.w, 20.0, Align::Center);
        draw_in_box(cr, &m, &font(v.look, 15.0, false, v.look.fg), r.x, r.y + 22.0, r.w, 20.0, Align::Center);
    } else {
        draw_in_box(cr, &clock_text(v.cfg, v.now), &font(v.look, 14.0, true, v.look.fg), r.x, r.y, r.w, r.h, Align::Center);
    }
}

fn draw_sysmon(cr: *mut Cairo, r: Rect, vertical: bool, v: &BarView) {
    let mut entries: Vec<(&str, String)> = Vec::new();
    if v.cfg.sysmon_cpu {
        entries.push((ICON_CPU, format!("{}%", v.stats.cpu)));
    }
    if v.cfg.sysmon_memory {
        entries.push((ICON_MEMORY, format!("{}%", v.stats.memory)));
    }
    if v.cfg.sysmon_temperature {
        entries.push((ICON_TEMPERATURE, v.stats.temperature.map_or_else(|| "\u{2014}".to_string(), |t| format!("{t}\u{b0}"))));
    }
    for (i, (icon, text)) in entries.iter().enumerate() {
        if vertical {
            let y = r.y + i as f64 * 38.0;
            crate::svg::draw_path(cr, icon, r.x + (r.w - 15.0) / 2.0, y + 3.0, 15.0, v.look.fg, Some(2.0));
            draw_in_box(cr, text, &font(v.look, 11.0, true, v.look.fg), r.x, y + 19.0, r.w, 16.0, Align::Center);
        } else {
            let x = r.x + i as f64 * 62.0;
            crate::svg::draw_path(cr, icon, x + 4.0, r.y + (r.h - 16.0) / 2.0, 16.0, v.look.fg, Some(2.0));
            draw_in_box(cr, text, &font(v.look, 13.0, true, v.look.fg), x + 24.0, r.y, 36.0, r.h, Align::Start);
        }
    }
}

fn draw_tray(cr: *mut Cairo, r: Rect, vertical: bool, t: f64, v: &BarView, st: &mut BarState, hits: &mut Vec<(PanelHit, Rect)>) {
    let size = cell(t);
    let icon = (t * 0.46).round();
    let start = if vertical { r.y } else { r.x };
    for (i, item) in v.tray.iter().enumerate() {
        let b = centered(along(vertical, start + i as f64 * size, size, t), size);
        let hit = PanelHit::Tray(i);
        let level = st.fades.level(hit, v.hover == Some(hit), v.style);
        draw_hover(cr, b, level, v.look);
        let image = v.tray_icons.get(i).and_then(|c| c.surface).or_else(|| item.icon_path.as_ref().and_then(|p| cached_image(&p.to_string_lossy(), icon as i32, icon as i32, true)));
        match image {
            Some(img) => unsafe {
                let (iw, ih) = image_size(img);
                let k = (icon / iw).min(icon / ih);
                cairo_save(cr);
                cairo_translate(cr, b.x + (b.w - iw * k) / 2.0, b.y + (b.h - ih * k) / 2.0);
                cairo_scale(cr, k, k);
                cairo_set_source_surface(cr, img, 0.0, 0.0);
                cairo_pattern_set_filter(cairo_get_source(cr), CAIRO_FILTER_BILINEAR);
                cairo_paint(cr);
                cairo_restore(cr);
            },
            None => {
                let letter: String = item.title.chars().next().unwrap_or('?').to_uppercase().collect();
                draw_in_box(cr, &letter, &font(v.look, 13.0, true, v.look.fg), b.x, b.y, b.w, b.h, Align::Center);
            }
        }
        hits.push((hit, b));
    }
}

fn draw_media(cr: *mut Cairo, r: Rect, vertical: bool, t: f64, v: &BarView, st: &mut BarState, hits: &mut Vec<(PanelHit, Rect)>) {
    let Some(m) = v.media else { return };
    let play = if m.playing { ICON_PAUSE } else { ICON_PLAY };
    if vertical {
        icon_button(cr, r, PanelHit::MediaPlay, play, true, v, st, hits);
        return;
    }
    let size = cell(t);
    let title_w = r.w - 3.0 * size;
    if title_w > 0.0 {
        draw_in_box(cr, &media_text(m), &font(v.look, 13.0, false, v.look.fg), r.x + 4.0, r.y, title_w - 6.0, r.h, Align::Start);
    }
    let x = r.x + title_w;
    icon_button(cr, along(false, x, size, t), PanelHit::MediaPrev, ICON_PREVIOUS, true, v, st, hits);
    icon_button(cr, along(false, x + size, size, t), PanelHit::MediaPlay, play, true, v, st, hits);
    icon_button(cr, along(false, x + 2.0 * size, size, t), PanelHit::MediaNext, ICON_NEXT, true, v, st, hits);
}

/// Package icon with the number of updates (vertical: a dot instead of the number).
fn draw_updates(cr: *mut Cairo, r: Rect, vertical: bool, t: f64, v: &BarView, st: &mut BarState, hits: &mut Vec<(PanelHit, Rect)>) {
    let Some(n) = v.updates.map(|u| u.total()).filter(|n| *n > 0) else { return };
    let level = st.fades.level(PanelHit::Updates, v.hover == Some(PanelHit::Updates), v.style);
    let pad = centered(r, r.w.min(r.h));
    let b = if vertical { pad } else { Rect { x: r.x, y: pad.y, w: r.w, h: pad.h } };
    draw_hover(cr, b, level, v.look);
    let size = cell(t);
    let i = (size * 0.56).round();
    let ix = if vertical { b.x + (b.w - i) / 2.0 } else { r.x + (size - i) / 2.0 };
    crate::svg::draw_path(cr, ICON_PACKAGE, ix, b.y + (b.h - i) / 2.0, i, v.look.fg, Some(2.0));
    if vertical {
        fill_rounded_rect(cr, b.x + b.w - 9.0, b.y + 3.0, 6.0, 6.0, 3.0, v.look.fg);
    } else {
        draw_in_box(cr, &n.to_string(), &font(v.look, 13.0, true, v.look.fg), r.x + size - 2.0, r.y, r.w - size, r.h, Align::Start);
    }
    hits.push((PanelHit::Updates, b));
}

fn draw_module(cr: *mut Cairo, id: &str, r: Rect, vertical: bool, t: f64, v: &BarView, st: &mut BarState, hits: &mut Vec<(PanelHit, Rect)>) {
    match id {
        "workspaces" => draw_workspaces(cr, r, vertical, t, v, st, hits),
        "clock" => draw_clock(cr, r, vertical, v),
        "sysmon" => draw_sysmon(cr, r, vertical, v),
        "tray" => draw_tray(cr, r, vertical, t, v, st, hits),
        "mpris" => draw_media(cr, r, vertical, t, v, st, hits),
        "launcher" => icon_button(cr, r, PanelHit::Launcher, ICON_LAUNCHER, false, v, st, hits),
        "notifications" => icon_button(cr, r, PanelHit::Notifications, ICON_BELL, false, v, st, hits),
        "power" => icon_button(cr, r, PanelHit::Power, ICON_POWER, false, v, st, hits),
        "updates" => draw_updates(cr, r, vertical, t, v, st, hits),
        _ => {}
    }
}

/// The modules of a section that have something to show, with their lengths.
fn section_modules(cr: *mut Cairo, section: usize, v: &BarView, vertical: bool, t: f64) -> Vec<(String, f64)> {
    v.cfg.section(section).iter().map(|id| (id.clone(), module_len(cr, id, v, vertical, t))).filter(|(_, len)| *len > 0.0).collect()
}

fn section_len(modules: &[(String, f64)]) -> f64 {
    modules.iter().map(|(_, len)| *len).sum::<f64>() + modules.len().saturating_sub(1) as f64 * MODULE_GAP
}

/// Draw the bar on a surface of `w` by `h`.
pub fn draw(cr: *mut Cairo, w: f64, h: f64, v: &BarView, st: &mut BarState) -> Vec<(PanelHit, Rect)> {
    let vertical = is_vertical(v.cfg);
    let (t, total) = if vertical { (w, h) } else { (h, w) };
    let card = (v.look.card.0, v.look.card.1, v.look.card.2, v.cfg.opacity as f64);
    let edge = (v.look.fg.0, v.look.fg.1, v.look.fg.2, 0.12);
    let radius = (v.cfg.radius as f64).min(t / 2.0);
    match v.cfg.style.as_str() {
        "full" => fill_rounded_rect(cr, 0.0, 0.0, w, h, 0.0, card),
        "islands" => {}
        _ => {
            fill_rounded_rect(cr, 0.0, 0.0, w, h, radius, card);
            stroke_rounded_rect(cr, 0.5, 0.5, w - 1.0, h - 1.0, radius, edge, 1.0);
        }
    }
    let sections = [section_modules(cr, 0, v, vertical, t), section_modules(cr, 1, v, vertical, t), section_modules(cr, 2, v, vertical, t)];
    let lens = [section_len(&sections[0]), section_len(&sections[1]), section_len(&sections[2])];
    let (a, b, c) = arrange(lens[0], lens[1], lens[2], total);
    let mut hits = Vec::new();
    for (k, start) in [a, b, c].into_iter().enumerate() {
        if sections[k].is_empty() {
            continue;
        }
        if v.cfg.style == "islands" {
            let pill = along(vertical, start - END_PAD / 2.0, lens[k] + END_PAD, t);
            fill_rounded_rect(cr, pill.x, pill.y, pill.w, pill.h, radius, card);
            stroke_rounded_rect(cr, pill.x + 0.5, pill.y + 0.5, pill.w - 1.0, pill.h - 1.0, radius, edge, 1.0);
        }
        let mut pos = start;
        for (id, len) in &sections[k] {
            draw_module(cr, id, along(vertical, pos, *len, t), vertical, t, v, st, &mut hits);
            pos += len + MODULE_GAP;
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_center_section_stays_in_the_middle_when_there_is_room() {
        let (left, mid, right) = arrange(100.0, 200.0, 100.0, 1000.0);
        assert_eq!(left, END_PAD);
        assert_eq!(mid, 400.0);
        assert_eq!(right, 1000.0 - END_PAD - 100.0);
    }

    #[test]
    fn a_long_side_section_pushes_the_center_aside() {
        let (_, mid, _) = arrange(600.0, 200.0, 50.0, 1000.0);
        assert_eq!(mid, END_PAD + 600.0 + MODULE_GAP);
        let (_, mid, right) = arrange(50.0, 200.0, 600.0, 1000.0);
        assert_eq!(mid, right - MODULE_GAP - 200.0);
    }

    #[test]
    fn sections_keep_clear_of_each_other_on_a_short_bar() {
        let (_, mid, _) = arrange(300.0, 300.0, 300.0, 500.0);
        assert_eq!(mid, END_PAD + 300.0 + MODULE_GAP);
    }

    #[test]
    fn only_clock_formats_with_seconds_tick() {
        let cfg = |f: &str| BarConfig { clock_format: f.into(), ..BarConfig::default() };
        assert!(!clock_ticks(&cfg("%H:%M")));
        assert!(clock_ticks(&cfg("%H:%M:%S")));
        assert!(clock_ticks(&cfg("%T")));
    }
}
