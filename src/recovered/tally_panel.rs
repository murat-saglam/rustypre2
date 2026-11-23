// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 level-end TALLY text panel (1030:51A3).
//!
//! "SCORE ..." / "LEVEL COMPLETED ...%" text panel over the black background.

use std::collections::HashMap;

const GLYPH_ROWS: usize = 11;
const ROW: usize = 0x28;
const DIGIT_SZ: usize = 0x58;

/// Blit one 16x11 plane-major glyph (88 bytes) at di.
pub fn blit_glyph(planes: &mut [Vec<u8>; 4], glyph_bytes: &[u8], di: usize) {
    let mut src = 0;
    for p in 0..4 {
        let mut d = di & 0xFFFF;
        for _ in 0..GLYPH_ROWS {
            if src + 1 < glyph_bytes.len() {
                planes[p][d] = glyph_bytes[src];
                planes[p][(d + 1) & 0xFFFF] = glyph_bytes[src + 1];
            }
            src += 2;
            d = (d + ROW) & 0xFFFF;
        }
    }
}

/// 32-bit value → 10 ASCII digits (leading zeros) + '$' terminator.
pub fn number_to_digits(value: u32) -> Vec<u8> {
    let powers: [u32; 10] = [
        1000000000, 100000000, 10000000, 1000000, 100000,
        10000, 1000, 100, 10, 1,
    ];
    let mut v = value & 0xFFFFFFFF;
    let mut out = Vec::with_capacity(11);
    for &p in &powers {
        let mut d = 0;
        while v >= p { v -= p; d += 1; }
        out.push(0x30 + d as u8);
    }
    out.push(0x24); // '$'
    out
}

/// Level-completed % = (collected & 0xff) * 100 / total (0 if total==0).
pub fn compute_percent(c74: u16, c76: u16, c78: u16, c7a: u16) -> u32 {
    let total = (c74 + c78) & 0xFFFF;
    if total == 0 { return 0; }
    let collected = (c76 + c7a) & 0xFFFF;
    ((collected & 0xFF) as u32 * 100) / total as u32
}

/// Draw the full tally panel.
pub fn render_tally_panel(
    planes: &mut [Vec<u8>; 4], score: u32, percent: u32, page: usize,
    digit_font: &[u8], letters: &HashMap<char, Vec<u8>>, pct_glyph: &[u8],
) {
    let score_label_di = 0x230 + page;
    let score_digits_di = 0x23C + page;
    let pct_label_di = 0x410 + page;
    let pct_digits_di = 0x430 + page;

    // Line 1: "SCORE" + digits
    let mut di = score_label_di & 0xFFFF;
    for ch in "SCORE".chars() {
        if let Some(glyph) = letters.get(&ch) {
            blit_glyph(planes, glyph, di);
        }
        di = (di + 2) & 0xFFFF;
    }

    // Score digits (low 6 of the 10-digit string)
    let digits = number_to_digits(score);
    let mut di = score_digits_di & 0xFFFF;
    for &d in &digits[4..] {
        if d == 0x24 { break; }
        let idx = (d - 0x30) as usize;
        blit_glyph(planes, &digit_font[idx * DIGIT_SZ..(idx + 1) * DIGIT_SZ], di);
        di = (di + 2) & 0xFFFF;
    }
    // Trailing '0'
    blit_glyph(planes, &digit_font[0..DIGIT_SZ], di);

    // Line 2: "LEVEL COMPLETED" + percentage + '%'
    let mut di = pct_label_di & 0xFFFF;
    for ch in "LEVEL COMPLETED".chars() {
        if ch != ' ' {
            if let Some(glyph) = letters.get(&ch) {
                blit_glyph(planes, glyph, di);
            }
        }
        di = (di + 2) & 0xFFFF;
    }

    // Percentage digits (last 3, skip leading zero)
    let pbuf = number_to_digits(percent);
    let pbuf = &pbuf[7..];
    let pbuf = if pbuf.first() == Some(&0x30) { &pbuf[1..] } else { pbuf };
    let mut di = pct_digits_di & 0xFFFF;
    for &d in pbuf {
        if d == 0x24 { break; }
        let idx = (d - 0x30) as usize;
        blit_glyph(planes, &digit_font[idx * DIGIT_SZ..(idx + 1) * DIGIT_SZ], di);
        di = (di + 2) & 0xFFFF;
    }
    // '%' glyph
    blit_glyph(planes, pct_glyph, di);
}
