use crate::font::{FONT_DATA, FONT_HEIGHT, FONT_WIDTH};

/// Converts a hex color string (e.g. "#0f1417" or "#8bd0f0") into 32-bit ARGB.
pub fn hex_to_argb(hex: &str, alpha: u8) -> u32 {
    let clean = hex.trim_start_matches('#');
    if clean.len() >= 6 {
        // By checked range: a malformed palette may hold non-ASCII text.
        let byte = |at: usize| clean.get(at..at + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
        let r = byte(0);
        let g = byte(2);
        let b = byte(4);
        ((alpha as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    } else {
        ((alpha as u32) << 24) | 0x00FFFFFF
    }
}

/// Fills a solid rectangle on a 32-bit ARGB pixel buffer.
pub fn fill_rect(
    buffer: &mut [u32],
    stride_width: i32,
    stride_height: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: u32,
) {
    let x_end = (x + w).min(stride_width);
    let y_end = (y + h).min(stride_height);
    let x_start = x.max(0);
    let y_start = y.max(0);

    for py in y_start..y_end {
        let row_offset = (py * stride_width) as usize;
        for px in x_start..x_end {
            buffer[row_offset + px as usize] = color;
        }
    }
}

/// Draws a rounded card rectangle at a specified position on the buffer.
pub fn draw_rounded_rect(
    buffer: &mut [u32],
    stride_width: i32,
    stride_height: i32,
    card_x: i32,
    card_y: i32,
    w: i32,
    h: i32,
    radius: i32,
    color: u32,
) {
    let r2 = radius * radius;
    let x_start = card_x.max(0);
    let y_start = card_y.max(0);
    let x_end = (card_x + w).min(stride_width);
    let y_end = (card_y + h).min(stride_height);

    for y in y_start..y_end {
        let rel_y = y - card_y;
        let row_offset = (y * stride_width) as usize;
        for x in x_start..x_end {
            let rel_x = x - card_x;
            let mut inside = true;

            // Top-left
            if rel_x < radius && rel_y < radius {
                let dx = radius - rel_x;
                let dy = radius - rel_y;
                if dx * dx + dy * dy > r2 {
                    inside = false;
                }
            }
            // Top-right
            else if rel_x >= w - radius && rel_y < radius {
                let dx = rel_x - (w - radius - 1);
                let dy = radius - rel_y;
                if dx * dx + dy * dy > r2 {
                    inside = false;
                }
            }
            // Bottom-left
            else if rel_x < radius && rel_y >= h - radius {
                let dx = radius - rel_x;
                let dy = rel_y - (h - radius - 1);
                if dx * dx + dy * dy > r2 {
                    inside = false;
                }
            }
            // Bottom-right
            else if rel_x >= w - radius && rel_y >= h - radius {
                let dx = rel_x - (w - radius - 1);
                let dy = rel_y - (h - radius - 1);
                if dx * dx + dy * dy > r2 {
                    inside = false;
                }
            }

            if inside {
                buffer[row_offset + x as usize] = color;
            }
        }
    }
}

/// Draws a rounded card rectangle filling the whole buffer.
pub fn draw_rounded_card(
    buffer: &mut [u32],
    stride_width: i32,
    stride_height: i32,
    radius: i32,
    color: u32,
) {
    draw_rounded_rect(buffer, stride_width, stride_height, 0, 0, stride_width, stride_height, radius, color);
}

/// Renders a horizontal OSD progress bar.
pub fn draw_osd_bar(
    buffer: &mut [u32],
    stride_width: i32,
    stride_height: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    percent: f32,
    fg_color: u32,
    bg_color: u32,
) {
    // Background track
    fill_rect(buffer, stride_width, stride_height, x, y, w, h, bg_color);

    // Foreground filled portion
    let fill_w = ((w as f32) * percent.clamp(0.0, 1.0)).round() as i32;
    if fill_w > 0 {
        fill_rect(buffer, stride_width, stride_height, x, y, fill_w, h, fg_color);
    }
}

/// Draws a single character using the embedded 8x12 bitmap font.
/// Returns the width advanced in pixels.
pub fn draw_char(
    buffer: &mut [u32],
    stride_width: i32,
    stride_height: i32,
    x: i32,
    y: i32,
    ch: char,
    color: u32,
    scale: i32,
) -> i32 {
    let scale = scale.max(1);
    let code = ch as u32;
    if !(32..=126).contains(&code) {
        return FONT_WIDTH * scale;
    }
    let glyph = &FONT_DATA[(code - 32) as usize];

    for row in 0..FONT_HEIGHT {
        let byte = glyph[row as usize];
        for col in 0..FONT_WIDTH {
            if (byte & (1 << (7 - col))) != 0 {
                let px = x + col * scale;
                let py = y + row * scale;
                fill_rect(buffer, stride_width, stride_height, px, py, scale, scale, color);
            }
        }
    }

    FONT_WIDTH * scale
}

/// Draws an ASCII text string on the buffer with optional scaling.
pub fn draw_text(
    buffer: &mut [u32],
    stride_width: i32,
    stride_height: i32,
    x: i32,
    y: i32,
    text: &str,
    color: u32,
    scale: i32,
) {
    let mut cur_x = x;
    for ch in text.chars() {
        if ch == '\n' {
            continue;
        }
        let advance = draw_char(buffer, stride_width, stride_height, cur_x, y, ch, color, scale);
        cur_x += advance;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_to_argb() {
        assert_eq!(hex_to_argb("#000000", 255), 0xFF000000);
        assert_eq!(hex_to_argb("#FFFFFF", 255), 0xFFFFFFFF);
        assert_eq!(hex_to_argb("#8bd0f0", 200), 0xC88BD0F0);
    }

    #[test]
    fn test_fill_rect() {
        let mut buf = vec![0u32; 100];
        fill_rect(&mut buf, 10, 10, 2, 2, 4, 4, 0xFFAABBCC);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[2 * 10 + 2], 0xFFAABBCC);
        assert_eq!(buf[5 * 10 + 5], 0xFFAABBCC);
        assert_eq!(buf[6 * 10 + 6], 0);
    }

    #[test]
    fn test_draw_rounded_rect() {
        let mut buf = vec![0u32; 100 * 100];
        draw_rounded_rect(&mut buf, 100, 100, 10, 10, 80, 80, 8, 0xFF112233);
        assert_eq!(buf[0], 0);
        // Center of rect should be colored
        assert_eq!(buf[50 * 100 + 50], 0xFF112233);
        // Outside rect should be transparent
        assert_eq!(buf[5 * 100 + 5], 0);
    }

    #[test]
    fn test_draw_text() {
        let mut buf = vec![0u32; 100 * 50];
        draw_text(&mut buf, 100, 50, 5, 5, "Hello", 0xFFFFFFFF, 1);
        // Verify non-zero pixels written
        assert!(buf.iter().any(|&p| p == 0xFFFFFFFF));
    }
}
