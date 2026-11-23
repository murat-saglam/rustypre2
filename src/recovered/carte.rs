// SPDX-License-Identifier: GPL-3.0-only
//! The recovered CARTE (world-map) scene background — composed straight from the decoded asset.
//!
//! The carte/map "scroll-in" (the `1030:9600` loop, `9613..96AB`) reveals the map by panning
//! the CRTC display start across a `0x2000`-byte circular page while filling the
//! newly-exposed column each 8 px from the master map asset (segment `[0x2875]`, 640 px /
//! 80 bytes wide, 4 EGA planes at source offsets 0 / 0x3E80 / 0x7D00 / 0xBB80).
//!
//! The decisive property (proven byte-exact vs the VM page): **the page is a pure,
//! stateless function of `scroll_x`** — `scroll_blit_column` blits asset -> page (not a
//! VRAM self-copy), so the whole page is reconstructible by replaying the blits.
//!
//! Pure logic operating directly on byte slices and plane buffers.

use crate::recovered::present::{
    compute_display_start, pixel_pan, scroll_blit_column, PAGE_WRAP, ROW_STRIDE, SCROLL_ROWS,
    SRC_STRIDE,
};

pub const PAGE_LEN: usize = PAGE_WRAP + 1; // 0x2000 — the circular page
pub const SCROLL_START: i32 = 8; // [asm 95E2: mov [0xB19D],8] — the carte begins at scroll_x = 8

/// [asm 9613] Compose the carte map page for a given horizontal scroll position.
///
/// `asset` is the decoded map master (segment `[0x2875]` bytes: 4 planes at offsets
/// 0 / 0x3E80 / 0x7D00 / 0xBB80, 80-byte source rows). `scroll_x` is the current pan
/// (`[0xB19D]`). Returns the four EGA plane buffers (`0x2000` each) the carte loop has
/// built up by the time the scroll reaches `scroll_x` — i.e. the blits for `k` in
/// `[scroll_start, scroll_x)` applied to a black ring (carte is horizontal-only,
/// `scroll_y` = 0).
pub fn build_carte_page(asset: &[u8], scroll_x: i32, scroll_start: i32) -> [Vec<u8>; 4] {
    let mut planes = [
        vec![0u8; PAGE_LEN],
        vec![0u8; PAGE_LEN],
        vec![0u8; PAGE_LEN],
        vec![0u8; PAGE_LEN],
    ];
    let end = scroll_x & 0xFFFF;
    let mut k = scroll_start;
    while k < end {
        // no-op unless k & 7 == 0 [asm 9662]
        scroll_blit_column(
            &mut planes,
            asset,
            k,
            PAGE_WRAP,
            SRC_STRIDE,
            ROW_STRIDE,
            SCROLL_ROWS,
        );
        k += 1;
    }
    planes
}

/// [asm 9543] Stamp the carte's per-level 'you are here' marker onto the decoded map master.
///
/// `asset` is the raw map master (`[0x2875]` bytes). `marker` is the `5*w*h`-byte source
/// at `[0x667a]:[0x62da]` — a mask plane followed by four colour planes, each `w*h` bytes.
/// `di` / `w` / `h` come from the per-level position table and the marker dimensions.
/// Returns a new master with the marker composited in
/// (`dst = (dst & ~mask) | colour` on each of the 4 planes at offsets 0/0x3E80/0x7D00/0xBB80).
pub fn stamp_carte_marker(asset: &[u8], marker: &[u8], di: usize, w: usize, h: usize) -> Vec<u8> {
    const PLANES: [usize; 4] = [0x0000, 0x3E80, 0x7D00, 0xBB80]; // [asm 958B-9598]
    let wh = w * h;
    let mut buf = vec![0u8; 0x10000]; // a full 64K segment
    let n = asset.len().min(0x10000);
    buf[..n].copy_from_slice(&asset[..n]);
    let mut lin: usize = 0;
    let mut d = di & 0xFFFF;
    for _row in 0..h {
        // [asm 95C9-95CB] dh rows
        for _col in 0..w {
            // [asm 95C5] cx=w byte columns
            let mask = marker[lin]; // [asm 9587] mask byte
            let inv = (!mask) & 0xFF; // [asm 9589] not al
            for p in 0..4 {
                let off = (d + PLANES[p]) & 0xFFFF;
                let color = marker[(p + 1) * wh + lin];
                buf[off] = (buf[off] & inv) | color; // [asm 958B..95B8] and mask, or colour
            }
            lin += 1; // [asm 95C3] inc si (source runs linearly)
            d = (d + 1) & 0xFFFF; // [asm 95C4] inc di
        }
        d = (d + (0x50 - w)) & 0xFFFF; // [asm 95C7] di += 0x50 - w (next row)
    }
    buf[..asset.len()].to_vec()
}

/// [asm 954D-9560] The map byte-offset `di` of the per-level marker:
/// `((y-0x20)&0xFF)*0x50 + (x>>3)`.
pub fn carte_marker_offset(x_word: i32, y_word: i32) -> usize {
    ((((y_word - 0x20) & 0xFF) * 0x50 + (x_word >> 3)) & 0xFFFF) as usize
}

/// The on-screen view parameters for the carte at `scroll_x`: `(display_start, pel_pan)`.
///
/// `display_start` is the CRTC start address (the 0x2000-wrapped pan); `pel_pan` the
/// sub-byte horizontal pixel pan written to the attribute controller.
pub fn carte_display(scroll_x: i32, scroll_y: i32) -> (usize, u8) {
    (
        compute_display_start(scroll_x, scroll_y, ROW_STRIDE, PAGE_WRAP),
        pixel_pan(scroll_x),
    )
}
