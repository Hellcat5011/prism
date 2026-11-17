// ─────────────────────────────────────────────────────────────────────────
// shadow.rs — soft drop shadow under modal cards
//
// Mango blurs the whole layer surface, so the card surface cannot grow to hold
// its own shadow (the blur would show as a rectangle around the card). The
// shadow lives instead on a subsurface of the scrim: unblurred like the dim
// (same `prism-scrim` layer rule), stacked between the dim (Top layer) and the
// card (Overlay layer), and it takes no input, so clicks reach the scrim.
//
// The shadow is drawn once per card size at a quarter of the resolution (it is
// all blur, so the compositor's upscaling is invisible) and follows the card's
// enter/exit pose by position, viewport size and alpha modifier only: nothing
// is redrawn while the card animates. The card's own area is cut out, so a
// translucent card is not darkened by the shadow beneath it.
// ─────────────────────────────────────────────────────────────────────────

use wayland_client::protocol::{wl_compositor, wl_subcompositor, wl_subsurface, wl_surface};
use wayland_client::QueueHandle;
use wayland_protocols::wp::alpha_modifier::v1::client::{wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1, wp_alpha_modifier_v1::WpAlphaModifierV1};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::cairo_backend as cb;
use crate::{create_buffer_slot, BufferSlot, UiState};

/// Buffer resolution relative to the screen.
const RES: f64 = 0.25;
/// Shadow reach around the card, its downward offset (design px) and peak opacity.
const SPREAD: f64 = 52.0;
const OFFSET_Y: f64 = 14.0;
const OPACITY: f64 = 0.55;

pub struct Shadow {
    surface: wl_surface::WlSurface,
    sub: wl_subsurface::WlSubsurface,
    viewport: WpViewport,
    alpha: WpAlphaModifierSurfaceV1,
    buffer: Option<BufferSlot>,
    /// Card size, radius and scale the buffer was drawn for (screen px).
    drawn: Option<(i32, i32, i32, i32)>,
    /// Last applied (x, y, w, h, alpha multiplier); None = unmapped.
    applied: Option<(i32, i32, i32, i32, u32)>,
}

impl Shadow {
    /// Create the shadow as a subsurface of the scrim `parent`. None without the needed globals.
    pub fn new(
        comp: &wl_compositor::WlCompositor,
        subcomp: Option<&wl_subcompositor::WlSubcompositor>,
        viewporter: Option<&WpViewporter>,
        alpha_modifier: Option<&WpAlphaModifierV1>,
        parent: &wl_surface::WlSurface,
        qh: &QueueHandle<UiState>,
    ) -> Option<Shadow> {
        let (subcomp, viewporter, alpha_modifier) = (subcomp?, viewporter?, alpha_modifier?);
        let surface = comp.create_surface(qh, ());
        let sub = subcomp.get_subsurface(&surface, parent, qh, ());
        // Takes no input: the pointer goes to the scrim below (click outside closes).
        let region = comp.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let viewport = viewporter.get_viewport(&surface, qh, ());
        let alpha = alpha_modifier.get_surface(&surface, qh, ());
        Some(Shadow { surface, sub, viewport, alpha, buffer: None, drawn: None, applied: None })
    }

    /// Follow the card. `rest`: the card at rest (screen px), `radius`/`scale`: corner radius
    /// (design px) and interface scale, `visible`: where the card is drawn now (screen px,
    /// scaled/moved by its transition), `alpha`: its opacity; `enabled` false hides the shadow.
    /// Commits the shadow surface; true when the parent (scrim) must commit to apply it.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        shm: &wayland_client::protocol::wl_shm::WlShm,
        qh: &QueueHandle<UiState>,
        rest: (i32, i32, i32, i32),
        radius: f64,
        scale: f64,
        visible: (f64, f64, f64, f64),
        alpha: f64,
        enabled: bool,
    ) -> bool {
        let (_, _, w, h) = rest;
        let show = enabled && alpha > 0.002 && w > 1 && h > 1 && visible.2 >= 1.0 && visible.3 >= 1.0;
        if !show {
            if self.applied.take().is_some() {
                self.surface.attach(None, 0, 0);
                self.surface.commit();
                return true;
            }
            return false;
        }
        let key = (w, h, (radius * scale).round() as i32, (scale * 100.0).round() as i32);
        let redrawn = self.drawn != Some(key);
        if redrawn && !self.draw(shm, qh, w, h, radius * scale, scale) {
            return false;
        }
        self.drawn = Some(key);
        // Place the shadow around the visible card, scaled with it.
        let pad = SPREAD * scale;
        let k = visible.2 / w as f64;
        let x = (visible.0 - pad * k).round() as i32;
        let y = (visible.1 - pad * k).round() as i32;
        let dw = ((w as f64 + 2.0 * pad) * k).round().max(1.0) as i32;
        let dh = ((h as f64 + 2.0 * pad) * (visible.3 / h as f64)).round().max(1.0) as i32;
        let m = (alpha.clamp(0.0, 1.0) * u32::MAX as f64).round() as u32;
        let next = (x, y, dw, dh, m);
        let mapped = self.applied.is_some();
        if !redrawn && mapped && self.applied == Some(next) {
            return false;
        }
        let prev = self.applied.replace(next);
        if prev.map(|p| (p.0, p.1)) != Some((x, y)) {
            self.sub.set_position(x, y);
        }
        if prev.map(|p| (p.2, p.3)) != Some((dw, dh)) {
            self.viewport.set_destination(dw, dh);
        }
        if prev.map(|p| p.4) != Some(m) {
            self.alpha.set_multiplier(m);
        }
        if redrawn || !mapped {
            if let Some(b) = &self.buffer {
                self.surface.attach(Some(&b.buffer), 0, 0);
                self.surface.damage_buffer(0, 0, b.width, b.height);
            }
        }
        self.surface.commit();
        true
    }

    /// Draw the shadow for a `w` x `h` card (screen px) into a new quarter-size buffer.
    fn draw(&mut self, shm: &wayland_client::protocol::wl_shm::WlShm, qh: &QueueHandle<UiState>, w: i32, h: i32, radius: f64, scale: f64) -> bool {
        let pad = SPREAD * scale;
        let bw = (((w as f64 + 2.0 * pad) * RES).ceil() as i32).max(1);
        let bh = (((h as f64 + 2.0 * pad) * RES).ceil() as i32).max(1);
        let stride = bw * 4;
        let size = (stride * bh) as usize;
        let Some(slot) = create_buffer_slot(shm, qh, bw, bh, stride, size) else { return false };
        // Buffer px per screen px (the buffer is stretched to the shadow's full size).
        let (sx, sy) = (bw as f64 / (w as f64 + 2.0 * pad), bh as f64 / (h as f64 + 2.0 * pad));
        let card = (pad * sx, pad * sy, w as f64 * sx, h as f64 * sy);
        let r = radius * sx;
        unsafe {
            libc::memset(slot.ptr as *mut libc::c_void, 0, size);
            let surf = cb::cairo_image_surface_create_for_data(slot.ptr, cb::CAIRO_FORMAT_ARGB32, bw, bh, stride);
            let cr = cb::cairo_create(surf);
            // The card's silhouette, slightly inset and moved down, then blurred.
            let inset = 2.0 * scale * sx;
            cb::fill_rounded_rect(cr, card.0 + inset, card.1 + OFFSET_Y * scale * sy, card.2 - 2.0 * inset, card.3 - 2.0 * inset, r, (0.0, 0.0, 0.0, OPACITY));
            cb::cairo_surface_flush(surf);
            let pixels = std::slice::from_raw_parts_mut(slot.ptr, size);
            blur_alpha(pixels, bw as usize, bh as usize, stride as usize, (pad * 0.6 * sx).max(1.0));
            cb::cairo_surface_mark_dirty(surf);
            // Cut the card out: a translucent card must not be darkened by its own shadow.
            // One buffer px inside the edge, so no light seam shows along the card's border.
            cb::cairo_set_operator(cr, cb::CAIRO_OPERATOR_DEST_OUT);
            let e = 1.0;
            cb::fill_rounded_rect(cr, card.0 + e, card.1 + e, card.2 - 2.0 * e, card.3 - 2.0 * e, (r - e).max(0.0), (0.0, 0.0, 0.0, 1.0));
            cb::cairo_destroy(cr);
            cb::cairo_surface_destroy(surf);
        }
        // The old buffer is replaced in the same commit; the compositor has copied or holds a
        // reference to it until then, so destroying the proxy now is safe (as for the scrim).
        if let Some(old) = self.buffer.replace(slot) {
            old.destroy();
        }
        self.applied = None; // re-attach and re-place on this update
        true
    }

    pub fn destroy(mut self) {
        self.alpha.destroy();
        self.viewport.destroy();
        self.sub.destroy();
        self.surface.destroy();
        if let Some(b) = self.buffer.take() {
            b.destroy();
        }
    }
}

/// Three box-blur passes (close to a gaussian of the given radius) over the alpha bytes of a
/// black premultiplied ARGB32 buffer; the colour bytes stay 0.
fn blur_alpha(px: &mut [u8], w: usize, h: usize, stride: usize, radius: f64) {
    let r = (radius / 3.0f64.sqrt()).round().max(1.0) as usize;
    let mut a: Vec<u16> = (0..w * h).map(|i| px[(i / w) * stride + (i % w) * 4 + 3] as u16).collect();
    let mut tmp = vec![0u16; w * h];
    for _ in 0..3 {
        box_pass(&a, &mut tmp, w, h, r, true);
        box_pass(&tmp, &mut a, w, h, r, false);
    }
    for y in 0..h {
        for x in 0..w {
            px[y * stride + x * 4 + 3] = a[y * w + x].min(255) as u8;
        }
    }
}

/// One running-sum box blur of radius `r` along rows (`horizontal`) or columns.
fn box_pass(src: &[u16], dst: &mut [u16], w: usize, h: usize, r: usize, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let n = (2 * r + 1) as u32;
    for line in 0..lines {
        let mut sum: u32 = 0;
        // Window over [i - r, i + r], zero outside the buffer.
        for i in 0..=r.min(len - 1) {
            sum += src[at(line, i)] as u32;
        }
        for i in 0..len {
            dst[at(line, i)] = ((sum + n / 2) / n) as u16;
            if i + r + 1 < len {
                sum += src[at(line, i + r + 1)] as u32;
            }
            if i >= r {
                sum -= src[at(line, i - r)] as u32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_spreads_and_keeps_total() {
        let (w, h) = (21usize, 21usize);
        let mut px = vec![0u8; w * h * 4];
        for i in 0..w * h {
            px[i * 4 + 3] = if (i / w).abs_diff(10) <= 2 && (i % w).abs_diff(10) <= 2 { 200 } else { 0 };
        }
        let before: u32 = (0..w * h).map(|i| px[i * 4 + 3] as u32).sum();
        blur_alpha(&mut px, w, h, w * 4, 3.0);
        let after: u32 = (0..w * h).map(|i| px[i * 4 + 3] as u32).sum();
        assert!(px[(10 * w + 15) * 4 + 3] > 0, "spreads outwards");
        assert!(px[(10 * w + 10) * 4 + 3] < 200, "centre softens");
        assert!((after as f64 - before as f64).abs() / (before as f64) < 0.05, "{before} vs {after}");
        assert!((0..w * h).all(|i| px[i * 4] == 0), "colour stays black");
    }
}
