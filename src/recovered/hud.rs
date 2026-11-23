// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 status-bar (HUD) rendering primitives (`1030:4580` / `1030:45B8` / `1030:473D`).
//!
//! Renders the fixed 320x23 status bar at the bottom of the EGA page (`HUD_BAR_DI = 0x1B80`)
//! and blits the dynamic 16x12 planar glyphs for lives, 6-digit score, energy hearts, and
//! collected B/O/N/U/S letters.

// Static status-bar background blit (`1030:4580`).
pub const HUD_BAR_DI: usize = 0x1B80;
pub const HUD_BAR_PLANE_BYTES: usize = 0x398; // 40 bytes/row x 23 rows per plane

// Dynamic status-bar layout (`1030:45B8`).
pub const HUD_LIVES_DI: usize = 0x1CED;
pub const HUD_SCORE_DI: usize = 0x1CF1;
pub const HUD_ENERGY_DI: usize = 0x1D01;
pub const HUD_MAX_HEARTS: usize = 3;
const HEART_FULL: u8 = 0x0A;
const HEART_EMPTY: u8 = 0x0B;
const SCORE_DIGITS: usize = 6;

// BONUS letters B/O/N/U/S (`1030:46AD`, table `0x6F86`).
pub const HUD_BONUS_DI: [usize; 5] = [0x1C91, 0x1BF2, 0x1CE3, 0x1C6C, 0x1C1D];
const BONUS_GLYPH0: u8 = 0x0C;

pub const HUD_GLYPH_BASE: usize = 0xE60 + 0x7B0; // 0x1610
pub const HUD_GLYPH_BYTES: usize = 0x60;         // 96 bytes/glyph (4 planes x 12 rows x 2 bytes)
pub const HUD_GLYPH_ROWS: usize = 0x0C;          // 12 rows
const ROW_STRIDE: usize = 0x28;                  // 40 bytes per screen row

/// HUD state values to render for the current frame.
pub struct HudState {
    pub score: u32,
    pub lives: u8,
    pub energy: u8,
    pub bonus_mask: u8,
}

/// Resolve the effective BONUS-letter bitmask (`1030:4683`).
///
/// During the pickup celebration (`flash_active`), all five letters flash in unison
/// on the frame parity; otherwise the collected bitmask is drawn.
pub fn effective_bonus_mask(collected: u8, flash_active: bool, frame_parity: u8) -> u8 {
    if flash_active {
        if frame_parity & 1 != 0 { 0x1F } else { 0 }
    } else {
        collected & 0xFF
    }
}

/// Blit one 16x12 planar HUD glyph (`1030:473D`) into the four EGA plane buffers.
pub fn blit_hud_glyph(planes: &mut [Vec<u8>; 4], glyph: u8, di: usize, font: &[u8]) {
    let mut src = HUD_GLYPH_BASE + (glyph as usize & 0xFF) * HUD_GLYPH_BYTES;
    for p in 0..4 {
        let mut d = di & 0xFFFF;
        for _ in 0..HUD_GLYPH_ROWS {
            if src + 1 < font.len() && d + 1 < planes[p].len() {
                planes[p][d] = font[src];
                planes[p][(d + 1) & 0xFFFF] = font[src + 1];
            }
            src += 2;
            d = (d + ROW_STRIDE) & 0xFFFF;
        }
    }
}

/// Draw the dynamic status-bar values (`1030:45B8`): lives, score, energy hearts, and BONUS letters.
pub fn draw_hud(planes: &mut [Vec<u8>; 4], hud: &HudState, font: &[u8], page: usize) {
    blit_hud_glyph(planes, hud.lives.min(9), (HUD_LIVES_DI + page) & 0xFFFF, font);

    let mut di = (HUD_SCORE_DI + page) & 0xFFFF;
    let score_val = (hud.score / 10) % 10u32.pow(SCORE_DIGITS as u32);
    let score_str = format!("{:06}", score_val);
    for ch in score_str.chars() {
        blit_hud_glyph(planes, (ch as u8) - 0x30, di, font);
        di = (di + 2) & 0xFFFF;
    }
    blit_hud_glyph(planes, 0, di, font);

    let mut di = (HUD_ENERGY_DI + page) & 0xFFFF;
    let full = hud.energy.min(HUD_MAX_HEARTS as u8) as usize;
    for _ in 0..full {
        blit_hud_glyph(planes, HEART_FULL, di, font);
        di = (di + 2) & 0xFFFF;
    }
    for _ in 0..(HUD_MAX_HEARTS - full) {
        blit_hud_glyph(planes, HEART_EMPTY, di, font);
        di = (di + 2) & 0xFFFF;
    }

    for (i, &bdi) in HUD_BONUS_DI.iter().enumerate() {
        if (hud.bonus_mask >> i) & 1 != 0 {
            blit_hud_glyph(planes, BONUS_GLYPH0 + i as u8, (bdi + page) & 0xFFFF, font);
        }
    }
}

/// Draw the static 320x23 planar status-bar background (`1030:4580`).
pub fn draw_status_bar(planes: &mut [Vec<u8>; 4], page: usize, bar: &[u8]) {
    let di = (page + HUD_BAR_DI) & 0xFFFF;
    let n = HUD_BAR_PLANE_BYTES;
    for p in 0..4 {
        let s = p * n;
        if s + n <= bar.len() && di + n <= planes[p].len() {
            planes[p][di..di + n].copy_from_slice(&bar[s..s + n]);
        }
    }
}
