// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 OLDIES / credits screen rendering (`1030:2505`).
//!
//! Renders centered bitmap text onto a black Mode `0x0D` page using the 8x12 4-plane font.

const GLYPH_SZ: usize = 0x30; // 4 planes x 12 rows x 1 byte
const ROWS: usize = 12;
const ROW: usize = 0x28;

/// Map an ASCII character byte to its font glyph index (`1030:0C7B`).
pub fn char_to_glyph(ch: u8) -> u8 {
    let al = ch.wrapping_sub(0x30);
    if al <= 9 { al } else { al.wrapping_sub(2) }
}

/// Format a year into 5 right-aligned character cells (`1030:0BEF`, e.g. `2026` -> `" 2026"`).
pub fn format_year(year: i32) -> String {
    format!("{:5}", year)
}

/// Blit (or clear for space) one 8x12 4-plane character cell at `di`.
pub fn blit_char(planes: &mut [Vec<u8>; 4], ch: u8, di: usize, font: &[u8]) {
    if ch == 0x20 {
        for p in 0..4 {
            let mut d = di & 0xFFFF;
            for _ in 0..ROWS {
                planes[p][d] = 0;
                d = (d + ROW) & 0xFFFF;
            }
        }
        return;
    }
    let mut src = (char_to_glyph(ch) as usize) * GLYPH_SZ;
    for p in 0..4 {
        let mut d = di & 0xFFFF;
        for _ in 0..ROWS {
            if src < font.len() { planes[p][d] = font[src]; }
            src += 1;
            d = (d + ROW) & 0xFFFF;
        }
    }
}

/// Draw one credit line starting at `(row, indent)` on `page`.
pub fn draw_credit_line(planes: &mut [Vec<u8>; 4], row: usize, indent: usize, text: &str, page: usize, font: &[u8]) {
    let mut col = indent;
    for ch in text.bytes() {
        blit_char(planes, ch, (row + col + page) & 0xFFFF, font);
        col += 1;
    }
}
