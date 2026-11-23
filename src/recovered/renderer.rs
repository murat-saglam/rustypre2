// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 sprite blit / background restore — recovered native logic.
//!
//! The per-frame draw renders each sprite/tile from the planar VRAM cache
//! (0xA000:0x5E80, filled by sprite_decode) onto the screen, dispatching on the
//! sprite's transparency class.

pub const ROW_STRIDE: usize = 0x28;   // screen bytes per sprite row (40 = 320px/8)
pub const ROWS: usize = 16;           // sprite height
pub const SPRITE_WIDTH: usize = 2;     // bytes per row (16 px)
pub const CACHE_BASE: usize = 0x5E80;  // planar sprite cache base within each plane
pub const SLOT_BYTES: usize = 0x20;    // 32 bytes per cache slot
pub const WRAP_AT: usize = 0x5D40;    // the scrolled background buffer wraps vertically here
pub const WRAP_SPAN: usize = 0x1E00;  // ... by this many bytes (192 rows)

/// Yield (row, offset) for each of the 16 sprite rows, applying the background
/// buffer's vertical wrap.
pub fn dest_rows(di: usize) -> Vec<(usize, usize)> {
    let mut d = di & 0xFFFF;
    let mut rows = Vec::with_capacity(ROWS);
    for r in 0..ROWS {
        if d >= WRAP_AT {
            d = (d.wrapping_sub(WRAP_SPAN)) & 0xFFFF;
        }
        rows.push((r, d));
        d = (d + ROW_STRIDE) & 0xFFFF;
    }
    rows
}

/// Type 0: copy the 16×16 sprite from the cache to the screen, all 4 planes.
pub fn blit_opaque(planes: &mut [Vec<u8>; 4], idx: usize, di: usize) {
    let src = CACHE_BASE + idx * SLOT_BYTES;
    for (r, d) in dest_rows(di) {
        for p in 0..4 {
            for c in 0..SPRITE_WIDTH {
                let s = src + r * SPRITE_WIDTH + c;
                planes[p][d + c] = planes[p][s];
            }
        }
    }
}

/// Type 1 (and the first phase of type ≥2): copy the scrolled background into the slot.
pub fn restore_background(planes: &mut [Vec<u8>; 4], di: usize, bg_off: usize) {
    for (r, d) in dest_rows(di) {
        let s = (bg_off + r * ROW_STRIDE) & 0xFFFF;
        for p in 0..4 {
            for c in 0..SPRITE_WIDTH {
                planes[p][d + c] = planes[p][s + c];
            }
        }
    }
}

/// Type ≥2: restore the background, then composite (bg AND mask) OR sprite.
pub fn blit_masked(planes: &mut [Vec<u8>; 4], idx: usize, di: usize, bg_off: usize, mask: &[u8]) {
    restore_background(planes, di, bg_off);
    let src = CACHE_BASE + idx * SLOT_BYTES;
    for (r, d) in dest_rows(di) {
        for p in 0..4 {
            for c in 0..SPRITE_WIDTH {
                let k = r * SPRITE_WIDTH + c;
                if k < mask.len() {
                    planes[p][d + c] = (mask[k] & planes[p][d + c]) | planes[p][src + k];
                }
            }
        }
    }
}

/// Dispatch one sprite blit on its transparency class (1030:3B88).
pub fn blit_sprite(planes: &mut [Vec<u8>; 4], idx: usize, di: usize, sprite_type: u8, bg_off: usize, mask: &[u8]) {
    if sprite_type == 0 {
        blit_opaque(planes, idx, di);
    } else if sprite_type == 1 {
        restore_background(planes, di, bg_off);
    } else {
        blit_masked(planes, idx, di, bg_off, mask);
    }
}
