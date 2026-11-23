// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 text / font renderer — recovered logic (pure).
//!
//! draw_string (1030:9886) — bitmap-font string drawer into VGA planes 2 and 3.

pub const GLYPH_BYTES: usize = 0x60;
pub const GLYPH_HEADER: usize = 6;
pub const PLANE_BLOCK: usize = 0x30;
pub const CELL_BYTES: usize = 3;   // 24 px
pub const CELL_ROWS: usize = 0x0C; // 12 rows
pub const ROW_STRIDE: usize = 0x28;
pub const WRAP_MASK: usize = 0x1FFF;
pub const SPACE_GLYPH: usize = 0x2B;

/// Map an ASCII byte to a font glyph index, or None if it terminates the string.
pub fn glyph_index(ch: u8) -> Option<usize> {
    if ch == 0x20 {
        return Some(SPACE_GLYPH);
    }
    let v = ch.wrapping_sub(0x30) as i32;
    if v < 0 {
        return None;
    }
    if v <= 9 {
        return Some(v as usize);
    }
    Some((v - 7) as usize)
}

/// Blit one glyph cell: clear planes 2|3 then copy the two plane blocks.
fn blit_glyph(
    planes: &mut [Vec<u8>; 4], font: &[u8], src: usize,
    di_draw: usize, di_clear: usize,
) {
    // Clear loop
    let mut d = di_clear;
    for _ in 0..CELL_ROWS {
        let dm = d & WRAP_MASK;
        for b in 0..CELL_BYTES {
            planes[2][dm + b] = 0;
            planes[3][dm + b] = 0;
        }
        d = (d + ROW_STRIDE) & 0xFFFF;
    }
    // Draw loop — plane 2 (block 0), plane 3 (block PLANE_BLOCK)
    for (plane, block) in [(2usize, 0usize), (3, PLANE_BLOCK)] {
        let mut d = di_draw;
        let mut s = src + block;
        for _ in 0..CELL_ROWS {
            let dm = d & WRAP_MASK;
            for b in 0..CELL_BYTES {
                if s + b < font.len() {
                    planes[plane][dm + b] = font[s + b];
                }
            }
            s += CELL_BYTES;
            d = (d + ROW_STRIDE) & 0xFFFF;
        }
    }
}

/// Draw a font string into VGA planes 2 and 3 only.
/// Returns the final pen position.
pub fn draw_string(
    planes: &mut [Vec<u8>; 4], text: &[u8], font: &[u8],
    font_base: usize, pen: usize, advance: usize,
    page_draw: usize, page_clear: usize,
) -> usize {
    let mut pen = pen;
    for &ch in text {
        let gi = match glyph_index(ch) {
            Some(gi) => gi,
            None => break,
        };
        let src = font_base + gi * GLYPH_BYTES + GLYPH_HEADER;
        pen = (pen + advance) & 0xFFFF;
        let base = (pen + 0x50) & 0xFFFF;
        let di_draw = (base + page_draw) & 0xFFFF;
        let di_clear = (base + page_clear) & 0xFFFF;
        blit_glyph(planes, font, src, di_draw, di_clear);
    }
    pen
}
