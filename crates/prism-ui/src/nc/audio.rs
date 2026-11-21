//! Control Centre Sound page (what pavucontrol does): output and input devices (default, volume,
//! mute, balance, port), playback and recording streams (volume, mute, which device they use),
//! and card profiles. State comes from `pw_watch` (pw-dump); changes go through its helpers.

use super::*;
use crate::pw_watch::{Audio, Card, Device, Stream, StreamKind};

/// Section tabs of the page.
const SECTIONS: [&str; 5] = ["Output", "Input", "Playback", "Recording", "Cards"];
pub(super) const SEC_OUTPUT: usize = 0;
const SEC_INPUT: usize = 1;
const SEC_PLAYBACK: usize = 2;
const SEC_RECORDING: usize = 3;
/// Volume sliders reach 150 % (wpctl's limit), with a mark at 100 %.
const VOL_MAX: f64 = 1.5;
const PADDING: f64 = 10.0;
const HEAD_H: f64 = 34.0;
const ROW_H: f64 = 32.0;
const BAL_H: f64 = 28.0;
const CHOICE_H: f64 = 30.0;
const GAP: f64 = 6.0;

/// The dropdown open on the page: a device's ports, a stream's device, a card's profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Pick {
    Port(u32),
    Target(u32),
    Profile(u32),
}

/// What an option of the open dropdown does.
#[derive(Clone, Debug)]
pub(super) enum Choice {
    Port(u32, u32),
    Move(u32, Option<String>),
    Profile(u32, u32),
}

/// An option row: label, selected, enabled, action.
type Opt = (String, bool, bool, Choice);

fn percent(v: f64) -> String {
    format!("{}%", (v * 100.0).round() as i64)
}

impl NcView {
    /// The page shows device level meters now (Output or Input section).
    pub(super) fn meters_shown(&self) -> bool {
        self.tab == TAB_AUDIO && (self.audio_sec == SEC_OUTPUT || self.audio_sec == SEC_INPUT)
    }

    /// Value of a slider: the dragged value for a moment after a change, else PipeWire's.
    fn audio_value(&self, h: Hit, actual: f64) -> f64 {
        match self.audio_local {
            Some((k, v, t)) if k == h && t.elapsed() < SLIDER_HOLD => v,
            _ => actual,
        }
    }

    pub(super) fn draw_audio(&mut self, cr: *mut Cairo, r: Rect, p: &Pal) {
        self.audio = crate::pw_watch::audio();
        self.streams = crate::pw_watch::snapshot();
        self.audio_choices.clear();
        let mut y = r.y;
        cb::draw_pango_text(cr, r.x, y, "Sound", &font(16.0, "Bold"), p.fg, None);
        let sub = match self.audio.sinks.iter().find(|d| d.is_default) {
            Some(d) => format!("Output: {}", d.description),
            None => "No output device".into(),
        };
        cb::draw_pango_text(cr, r.x, y + 21.0, &sub, &font(12.0, ""), alpha(p.fg, 0.65), Some(r.w as i32));
        y += 40.0 + 6.0;

        // Section switcher: a pill that springs between the labels.
        let seg = Rect { x: r.x, y, w: r.w, h: 32.0 };
        cb::fill_rounded_rect(cr, seg.x, seg.y, seg.w, seg.h, 16.0, alpha(p.fg, 0.1));
        let sw = seg.w / SECTIONS.len() as f64;
        self.audio_seg.set_spring(self.audio_sec as f64, crate::ui::SPRING_KNOB, self.time_scale);
        let pos = self.audio_seg.value();
        cb::fill_rounded_rect(cr, seg.x + pos * sw + 2.0, seg.y + 2.0, sw - 4.0, seg.h - 4.0, 14.0, p.fg);
        let f = font(12.0, "Medium");
        for (i, label) in SECTIONS.iter().enumerate() {
            let cell = Rect { x: seg.x + i as f64 * sw, y: seg.y, w: sw, h: seg.h };
            let on = (1.0 - (pos - i as f64).abs()).clamp(0.0, 1.0);
            let (tw, _) = text_size(cr, label, &f);
            let tx = cell.x + ((cell.w - tw) / 2.0).max(4.0).round();
            let hover = hv!(self, Hit::AudioSec(i));
            let off = alpha(p.fg, 0.75 + 0.25 * hover);
            if on < 1.0 {
                text_v(cr, tx, cell.y, cell.h, label, &f, alpha(off, 1.0 - on), Some(cell.w - 8.0));
            }
            if on > 0.0 {
                text_v(cr, tx, cell.y, cell.h, label, &f, alpha(p.inv, on), Some(cell.w - 8.0));
            }
            self.hits.push((Hit::AudioSec(i), cell));
        }
        y += seg.h + 10.0;

        let list = Rect { x: r.x, y, w: r.w, h: (r.y + r.h - y).max(0.0) };
        let audio = self.audio.clone();
        let sec = self.audio_sec;
        let p = *p;
        match sec {
            SEC_OUTPUT | SEC_INPUT => {
                let devices = if sec == SEC_OUTPUT { audio.sinks.clone() } else { audio.sources.clone() };
                if devices.is_empty() {
                    let msg = if sec == SEC_OUTPUT { "No output devices" } else { "No input devices" };
                    self.empty_state(cr, list, if sec == SEC_OUTPUT { "speaker" } else { "mic" }, true, msg, &p);
                    return;
                }
                crate::meters::want(devices.iter().map(|d| crate::meters::Want { id: d.id, name: d.name.clone(), sink: d.sink }).collect());
                self.scrolled(cr, TAB_AUDIO, list, |v, mut y| {
                    let start = y;
                    for d in &devices {
                        y += v.audio_device(cr, list.x, y, list.w, d, audio.card(d.card), &p) + GAP;
                    }
                    y - start
                });
            }
            SEC_PLAYBACK | SEC_RECORDING => {
                let want = if sec == SEC_PLAYBACK { StreamKind::Playback } else { StreamKind::Record };
                let streams: Vec<Stream> = self.streams.iter().filter(|s| s.kind == want).cloned().collect();
                if streams.is_empty() {
                    let msg = if sec == SEC_PLAYBACK { "No apps are playing audio" } else { "No apps are recording audio" };
                    self.empty_state(cr, list, if sec == SEC_PLAYBACK { "volume" } else { "mic" }, false, msg, &p);
                    return;
                }
                self.scrolled(cr, TAB_AUDIO, list, |v, mut y| {
                    let start = y;
                    for s in &streams {
                        y += v.audio_stream(cr, list.x, y, list.w, s, &audio, &p) + GAP;
                    }
                    y - start
                });
            }
            _ => {
                if audio.cards.is_empty() {
                    self.empty_state(cr, list, "speaker", true, "No sound cards", &p);
                    return;
                }
                self.scrolled(cr, TAB_AUDIO, list, |v, mut y| {
                    let start = y;
                    for c in &audio.cards {
                        y += v.audio_card(cr, list.x, y, list.w, c, &p) + GAP;
                    }
                    y - start
                });
            }
        }
    }

    /// Height of an open option list.
    fn opts_height(&self, pick: Pick, opts: &[Opt]) -> f64 {
        if self.audio_pick == Some(pick) { 4.0 + opts.len() as f64 * CHOICE_H } else { 0.0 }
    }

    /// The option rows of an open dropdown, from `y`.
    fn draw_opts(&mut self, cr: *mut Cairo, x: f64, y: f64, w: f64, opts: Vec<Opt>, p: &Pal) {
        let mut y = y + 4.0;
        for (label, selected, enabled, choice) in opts {
            let row = Rect { x, y, w, h: CHOICE_H };
            let k = self.audio_choices.len();
            self.audio_choices.push(choice);
            let hover = if enabled { hv!(self, Hit::AudioChoice(k)) } else { 0.0 };
            if hover > 0.0 {
                cb::fill_rounded_rect(cr, row.x, row.y, row.w, row.h, 8.0, alpha(p.fg, 0.12 * hover));
            }
            if selected {
                svg::icon(cr, "check", row.x + 8.0, row.y + 7.0, 16.0, p.fg, false);
            }
            let c = alpha(p.fg, if enabled { 1.0 } else { 0.45 });
            text_v(cr, row.x + 32.0, row.y, row.h, &label, &font(13.0, if selected { "Bold" } else { "" }), c, Some(row.w - 40.0));
            if enabled && !selected {
                self.hits.push((Hit::AudioChoice(k), row));
            }
            y += CHOICE_H;
        }
    }

    /// A dropdown button: the current value and a chevron.
    fn chip(&mut self, cr: *mut Cairo, r: Rect, text: &str, pick: Pick, p: &Pal) {
        let open = self.audio_pick == Some(pick);
        let hover = hv!(self, Hit::AudioPick(pick));
        cb::fill_rounded_rect(cr, r.x, r.y, r.w, r.h, 8.0, alpha(p.fg, 0.12 + 0.1 * hover + if open { 0.08 } else { 0.0 }));
        text_v(cr, r.x + 10.0, r.y, r.h, text, &font(12.0, ""), p.fg, Some(r.w - 36.0));
        svg::icon_rotated(cr, "chevron-down", r.x + r.w - 24.0, r.y + (r.h - 16.0) / 2.0, 16.0, p.fg, if open { 180.0 } else { 0.0 });
        self.hits.push((Hit::AudioPick(pick), r));
    }

    /// Mute button, 0..150 % slider and its value: one row from `y`.
    #[allow(clippy::too_many_arguments)]
    fn volume_row(&mut self, cr: *mut Cairo, x: f64, y: f64, w: f64, id: u32, volume: f64, muted: bool, icon: &str, level: Option<f64>, p: &Pal) {
        let mute = Rect { x, y, w: 32.0, h: ROW_H };
        let mh = hv!(self, Hit::AudioMute(id));
        if mh > 0.0 {
            cb::fill_circle(cr, mute.x + 16.0, mute.y + 16.0, 16.0, alpha(p.fg, 0.2 * mh));
        }
        svg::icon(cr, icon, mute.x + 7.0, mute.y + 7.0, 18.0, alpha(p.fg, if muted { 0.5 } else { 1.0 }), muted);
        self.hits.push((Hit::AudioMute(id), mute));
        let v = self.audio_value(Hit::AudioVol(id), volume);
        let slider = Rect { x: x + 40.0, y, w: w - 40.0 - 50.0, h: ROW_H };
        // 100 % mark.
        let mx = slider.x + 7.0 + (slider.w - 14.0) / VOL_MAX;
        cb::fill_rounded_rect(cr, mx - 1.0, slider.y + 6.0, 2.0, slider.h - 12.0, 1.0, alpha(p.fg, 0.35));
        // Level meter under the track (pavucontrol's peak bar), on the slider's 0..150 % scale.
        if let Some(l) = level {
            let mw = slider.w - 14.0;
            let my = slider.y + slider.h - 4.0;
            cb::fill_rounded_rect(cr, slider.x + 7.0, my, mw, 3.0, 1.5, alpha(p.fg, 0.12));
            let lw = mw * (l / VOL_MAX).min(1.0);
            if lw >= 1.0 {
                cb::fill_rounded_rect(cr, slider.x + 7.0, my, lw, 3.0, 1.5, alpha(p.fg, if muted { 0.35 } else { 0.8 }));
            }
        }
        widgets::slider(cr, slider, v / VOL_MAX, p);
        self.hits.push((Hit::AudioVol(id), slider));
        let label = percent(v);
        let f = font(12.0, "Medium");
        let (tw, _) = text_size(cr, &label, &f);
        text_v(cr, x + w - tw, y, ROW_H, &label, &f, alpha(p.fg, if muted { 0.5 } else { 0.85 }), None);
    }

    /// An output or input device; returns its height.
    #[allow(clippy::too_many_arguments)]
    fn audio_device(&mut self, cr: *mut Cairo, x: f64, y: f64, w: f64, d: &Device, card: Option<&Card>, p: &Pal) -> f64 {
        let ports: Vec<Opt> = card
            .map(|c| {
                let active = c.active_port(d);
                c.ports_for(d)
                    .into_iter()
                    .map(|pt| {
                        let label = if pt.available { pt.description.clone() } else { format!("{} (unplugged)", pt.description) };
                        (label, Some(pt.index) == active, pt.available, Choice::Port(d.id, pt.index))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let current_port = ports.iter().find(|o| o.1).map(|o| o.0.clone()).unwrap_or_else(|| "Unknown".into());
        let show_ports = ports.len() > 1;
        let pick = Pick::Port(d.id);
        let h = PADDING + HEAD_H + ROW_H + if d.has_balance() { BAL_H } else { 0.0 } + if show_ports { 4.0 + ROW_H + self.opts_height(pick, &ports) } else { 0.0 } + PADDING;
        cb::fill_rounded_rect(cr, x, y, w, h, RADIUS, alpha(p.fg, if d.is_default { 0.14 } else { 0.07 }));
        let ix = x + PADDING;
        let iw = w - 2.0 * PADDING;
        let mut cy = y + PADDING;

        // Header: default radio, glyph, name.
        let head = Rect { x: ix, y: cy, w: iw, h: HEAD_H };
        let hh = if d.is_default { 0.0 } else { hv!(self, Hit::AudioDefault(d.id)) };
        if hh > 0.0 {
            cb::fill_rounded_rect(cr, head.x - 4.0, head.y, head.w + 8.0, head.h, 8.0, alpha(p.fg, 0.1 * hh));
        }
        let mid = head.y + HEAD_H / 2.0;
        cb::stroke_circle(cr, head.x + 9.0, mid, 6.5, p.fg, 1.0);
        if d.is_default {
            cb::fill_circle(cr, head.x + 9.0, mid, 4.0, p.fg);
        }
        let glyph = if d.sink { model::sink_glyph(&d.name, &d.description) } else { "mic" };
        svg::icon(cr, glyph, head.x + 24.0, mid - 9.0, 18.0, p.fg, false);
        text_v(cr, head.x + 50.0, head.y, HEAD_H, &d.description, &font(13.0, if d.is_default { "Bold" } else { "" }), p.fg, Some(head.w - 50.0));
        if !d.is_default {
            self.hits.push((Hit::AudioDefault(d.id), head));
        }
        cy += HEAD_H;

        let icon = if d.sink { "speaker" } else { "mic" };
        self.volume_row(cr, ix, cy, iw, d.id, d.volume, d.muted, icon, Some(crate::meters::level(d.id).unwrap_or(0.0)), p);
        cy += ROW_H;

        if d.has_balance() {
            let bal = self.audio_value(Hit::AudioBal(d.id), d.balance());
            let f = font(11.0, "Medium");
            text_v(cr, ix + 12.0, cy, BAL_H, "L", &f, alpha(p.fg, 0.65), None);
            let slider = Rect { x: ix + 40.0, y: cy, w: iw - 40.0 - 50.0, h: BAL_H };
            let mx = slider.x + slider.w / 2.0;
            cb::fill_rounded_rect(cr, mx - 1.0, slider.y + 5.0, 2.0, slider.h - 10.0, 1.0, alpha(p.fg, 0.35));
            // Track, and a fill from the centre towards the knob.
            let ty = slider.y + (slider.h - 4.0) / 2.0;
            cb::fill_rounded_rect(cr, slider.x, ty, slider.w, 4.0, 2.0, alpha(p.fg, 0.25));
            let kx = slider.x + 7.0 + (bal + 1.0) / 2.0 * (slider.w - 14.0);
            let (a, b) = if kx < mx { (kx, mx) } else { (mx, kx) };
            cb::fill_rounded_rect(cr, a, ty, b - a, 4.0, 2.0, p.fg);
            cb::fill_circle(cr, kx, slider.y + slider.h / 2.0, 6.0, p.fg);
            self.hits.push((Hit::AudioBal(d.id), slider));
            text_v(cr, slider.x + slider.w + 10.0, cy, BAL_H, "R", &f, alpha(p.fg, 0.65), None);
            cy += BAL_H;
        }

        if show_ports {
            cy += 4.0;
            text_v(cr, ix + 2.0, cy, ROW_H, "Port", &font(11.0, "Bold"), alpha(p.fg, 0.6), None);
            self.chip(cr, Rect { x: ix + 40.0, y: cy + 2.0, w: iw - 40.0, h: ROW_H - 4.0 }, &current_port, pick, p);
            cy += ROW_H;
            if self.audio_pick == Some(pick) {
                self.draw_opts(cr, ix + 40.0, cy, iw - 40.0, ports, p);
            }
        }
        h
    }

    /// A playing or recording app stream; returns its height.
    #[allow(clippy::too_many_arguments)]
    fn audio_stream(&mut self, cr: *mut Cairo, x: f64, y: f64, w: f64, s: &Stream, audio: &Audio, p: &Pal) -> f64 {
        let playback = s.kind == StreamKind::Playback;
        let devices = if playback { &audio.sinks } else { &audio.sources };
        let current = s.target.and_then(|t| devices.iter().find(|d| d.id == t));
        let default = devices.iter().find(|d| d.is_default);
        let mut opts: Vec<Opt> = Vec::new();
        if let Some(dd) = default {
            opts.push((format!("Default ({})", dd.description), false, true, Choice::Move(s.id, None)));
        }
        for d in devices {
            opts.push((d.description.clone(), current.is_some_and(|c| c.id == d.id), true, Choice::Move(s.id, Some(d.name.clone()))));
        }
        let pick = Pick::Target(s.id);
        let h = PADDING + HEAD_H + 4.0 + ROW_H + 4.0 + ROW_H + self.opts_height(pick, &opts) + PADDING;
        cb::fill_rounded_rect(cr, x, y, w, h, RADIUS, alpha(p.fg, 0.07));
        let ix = x + PADDING;
        let iw = w - 2.0 * PADDING;
        let mut cy = y + PADDING;

        crate::toasts::draw_notif_icon(cr, ix + 2.0, cy + 5.0, 24.0, &s.app, (!s.icon.is_empty()).then_some(s.icon.as_str()), None, &s.icon, p.fg, 1.0);
        let tx = ix + 36.0;
        let tw = iw - 36.0;
        if s.media.is_empty() || s.media == s.app {
            text_v(cr, tx, cy, HEAD_H, &s.app, &font(13.0, "Bold"), p.fg, Some(tw));
        } else {
            text_v(cr, tx, cy, 18.0, &s.app, &font(13.0, "Bold"), p.fg, Some(tw));
            text_v(cr, tx, cy + 18.0, 16.0, &s.media, &font(11.0, ""), alpha(p.fg, 0.65), Some(tw));
        }
        cy += HEAD_H + 4.0;

        let icon = if playback { "speaker" } else { "mic" };
        self.volume_row(cr, ix, cy, iw, s.id, s.volume, s.muted, icon, None, p);
        cy += ROW_H + 4.0;

        text_v(cr, ix + 2.0, cy, ROW_H, if playback { "To" } else { "From" }, &font(11.0, "Bold"), alpha(p.fg, 0.6), None);
        let label = current.map(|d| d.description.clone()).unwrap_or_else(|| if s.active { "Unknown".into() } else { "Not connected".into() });
        self.chip(cr, Rect { x: ix + 40.0, y: cy + 2.0, w: iw - 40.0, h: ROW_H - 4.0 }, &label, pick, p);
        cy += ROW_H;
        if self.audio_pick == Some(pick) {
            self.draw_opts(cr, ix + 40.0, cy, iw - 40.0, opts, p);
        }
        h
    }

    /// A sound card and its profile; returns its height.
    fn audio_card(&mut self, cr: *mut Cairo, x: f64, y: f64, w: f64, c: &Card, p: &Pal) -> f64 {
        // Unavailable profiles cannot be chosen (pavucontrol greys them out); the active one is
        // always listed.
        let opts: Vec<Opt> = c
            .profiles
            .iter()
            .filter(|pr| pr.available || Some(pr.index) == c.profile)
            .map(|pr| (pr.description.clone(), Some(pr.index) == c.profile, true, Choice::Profile(c.id, pr.index)))
            .collect();
        let current = opts.iter().find(|o| o.1).map(|o| o.0.clone()).unwrap_or_else(|| "Unknown".into());
        let pick = Pick::Profile(c.id);
        let h = PADDING + HEAD_H + ROW_H + self.opts_height(pick, &opts) + PADDING;
        cb::fill_rounded_rect(cr, x, y, w, h, RADIUS, alpha(p.fg, 0.07));
        let ix = x + PADDING;
        let iw = w - 2.0 * PADDING;
        let mut cy = y + PADDING;
        svg::icon(cr, model::sink_glyph("", &c.description), ix + 2.0, cy + 8.0, 18.0, p.fg, false);
        text_v(cr, ix + 30.0, cy, HEAD_H, &c.description, &font(13.0, "Bold"), p.fg, Some(iw - 30.0));
        cy += HEAD_H;
        text_v(cr, ix + 2.0, cy, ROW_H, "Profile", &font(11.0, "Bold"), alpha(p.fg, 0.6), None);
        self.chip(cr, Rect { x: ix + 52.0, y: cy + 2.0, w: iw - 52.0, h: ROW_H - 4.0 }, &current, pick, p);
        cy += ROW_H;
        if self.audio_pick == Some(pick) {
            self.draw_opts(cr, ix + 52.0, cy, iw - 52.0, opts, p);
        }
        h
    }

    // ── Input ────────────────────────────────────────────────────────────

    /// A slider of the Sound page is being dragged to `x`.
    pub(super) fn audio_drag(&mut self, h: Hit, x: f64) {
        let Some(r) = self.hit_rect(h) else { return };
        let v = widgets::slider_value(r, x);
        match h {
            Hit::AudioVol(id) => {
                let mut vol = v * VOL_MAX;
                if (vol - 1.0).abs() < 0.03 {
                    vol = 1.0;
                }
                self.audio_local = Some((h, vol, Instant::now()));
                crate::pw_watch::set_volume(&[id], vol);
            }
            Hit::AudioBal(id) => {
                let mut bal = v * 2.0 - 1.0;
                if bal.abs() < 0.06 {
                    bal = 0.0;
                }
                self.audio_local = Some((h, bal, Instant::now()));
                if let Some(d) = self.audio.device(id) {
                    crate::pw_watch::set_balance(d, self.audio.card(d.card), bal);
                }
            }
            _ => {}
        }
    }

    pub(super) fn audio_activate(&mut self, h: Hit) {
        match h {
            Hit::AudioSec(i) => {
                if i != self.audio_sec {
                    self.audio_sec = i;
                    self.audio_pick = None;
                    self.scroll[TAB_AUDIO] = Default::default();
                }
            }
            Hit::AudioDefault(id) => crate::pw_watch::set_default(id),
            Hit::AudioMute(id) => {
                let muted = self.audio.device(id).map(|d| d.muted).or_else(|| self.streams.iter().find(|s| s.id == id).map(|s| s.muted));
                if let Some(m) = muted {
                    crate::pw_watch::set_mute(&[id], !m);
                }
            }
            Hit::AudioPick(pk) => self.audio_pick = if self.audio_pick == Some(pk) { None } else { Some(pk) },
            Hit::AudioChoice(k) => {
                match self.audio_choices.get(k).cloned() {
                    Some(Choice::Port(node, index)) => crate::pw_watch::set_port(node, index),
                    Some(Choice::Move(stream, target)) => crate::pw_watch::move_stream(stream, target.as_deref()),
                    Some(Choice::Profile(card, index)) => crate::pw_watch::set_profile(card, index),
                    None => {}
                }
                self.audio_pick = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_labels() {
        assert_eq!(percent(0.5), "50%");
        assert_eq!(percent(1.5), "150%");
    }

    /// Draws every section with the live PipeWire state to PNGs (needs a session):
    /// `PRISM_NC_RENDER_DIR=<dir> cargo test --release -p prism-ui render_sound_page -- --ignored`.
    #[test]
    #[ignore]
    fn render_sound_page() {
        let Ok(dir) = std::env::var("PRISM_NC_RENDER_DIR") else { return };
        let cfg = prism_common::Config::load();
        let theme = prism_common::Theme::load();
        let style = crate::ui::Style::new(&cfg, &theme);
        let mut v = NcView::new(crate::waker::Waker::new().unwrap());
        let t = Instant::now();
        while crate::pw_watch::generation() == 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
        v.set_tab(TAB_AUDIO);
        v.time_scale = None;
        let (w, h) = (440, 900);
        for sec in 0..SECTIONS.len() {
            v.audio_sec = sec;
            v.audio_seg = Tween::new(sec as f64);
            for open in [false, true] {
                v.audio_pick = None;
                let mut surf = std::ptr::null_mut();
                let mut cr = std::ptr::null_mut();
                // The first draw records the hits a dropdown is opened from.
                for pass in 0..2 {
                    if pass == 1 {
                        unsafe {
                            cb::cairo_destroy(cr);
                            cb::cairo_surface_destroy(surf);
                        }
                    }
                    surf = unsafe { cb::cairo_image_surface_create(cb::CAIRO_FORMAT_ARGB32, w, h) };
                    cr = unsafe { cb::cairo_create(surf) };
                    v.page = std::array::from_fn(|i| Tween::new(if i == TAB_AUDIO { 1.0 } else { 0.0 }));
                    v.draw(cr, Rect { x: 20.0, y: 20.0, w: 400.0, h: 860.0 }, &style, &theme);
                    if open && pass == 0 {
                        v.audio_pick = v.hits.iter().find_map(|(h, _)| if let Hit::AudioPick(pk) = h { Some(*pk) } else { None });
                    }
                }
                let path = std::ffi::CString::new(format!("{dir}/sound-{sec}-{}.png", if open { "open" } else { "closed" })).unwrap();
                unsafe {
                    cb::cairo_surface_write_to_png(surf, path.as_ptr());
                    cb::cairo_destroy(cr);
                    cb::cairo_surface_destroy(surf);
                }
            }
        }
    }
}
