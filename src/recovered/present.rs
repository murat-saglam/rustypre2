// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 scene present — recovered logic (pure).
//!
//! The non-gameplay scenes (the mode-select scroll in particular) present by **panning the
//! CRTC display start** and **flipping the draw/clear page offsets**, rather than by the
//! gameplay scroll engine. This recovers that present arithmetic from the mode-select loop
//! ``1030:9600`` (``9613..9639``), one bounded computation:
//!
//! * `compute_display_start` — the CRTC start address (the pan): `(scroll_x>>3 +
//!   scroll_y*0x28) & 0x1FFF`. The `& 0x1FFF` (`and bh,0x1f`) makes the display a
//!   `0x2000`-byte **circular page** — the background wraps (repeats) at the page edge.
//! * `present_pan_flip` — one present step: compute the display start, then set the
//!   page bookkeeping the text drawer uses.
//! * `pixel_pan` — the sub-byte horizontal pan (`scroll_x & 7`) written to the
//!   attribute controller's horizontal-pixel-panning register.
//! * `scroll_blit_column` — blit one fresh byte-column of the master pattern into all 4 EGA
//!   planes of the page (di wraps at 0x1FFF — the circular page).
//! * `scroll_shift_frame` — the menu/scene framebuffer SCROLL: a 4-plane A000->A000 latched
//!   self-copy that shifts the displayed buffer to follow the camera.
//!
//! Pure logic operating directly on byte slices and plane buffers. The VM↔memory translation belongs in the bridge.

pub const ROW_STRIDE: usize = 0x28;   // screen bytes per row [asm 9613: ax = 0x28 * scroll_y]
pub const PAGE_WRAP: usize = 0x1FFF;  // 0x2000-byte circular page [asm 9626: and bh,0x1f]
pub const SRC_STRIDE: usize = 0x50;  // master-pattern row stride [asm 9685]
pub const SCROLL_ROWS: usize = 0xC8; // 200 rows blitted per plane [asm 967F: cx=0xC8]

/// [asm 9613-9626] the CRTC display-start (pan) for the scrolling scene.
///
/// `scroll_x` (`[0xB19D]`) is the horizontal pan in pixels; `scroll_y` (`[0xB19F]`)
/// the vertical offset in rows. The high byte is masked to `0x1f` (`& 0x1FFF`), so the
/// display wraps as a `0x2000`-byte circular page.
pub fn compute_display_start(scroll_x: i32, scroll_y: i32, stride: usize, wrap: usize) -> usize {
    let base = ((scroll_x >> 3) + scroll_y * (stride as i32)) & 0xFFFF; // [asm 9613-9624]
    (base as usize) & wrap // [asm 9626: and bh,0x1f]
}

/// [asm 9654-9659] the fine (sub-byte) horizontal pan, `scroll_x & 7`.
pub fn pixel_pan(scroll_x: i32) -> u8 {
    (scroll_x & 7) as u8
}

/// [asm 965A-969C] blit the newly-exposed background column.
///
/// As the CRTC display start pans right (one byte every 8 px), the column it newly
/// exposes must be filled with the next slice of the master pattern. This runs once per
/// new byte-column (`scroll_x & 7 == 0`): for each of the 4 EGA planes it copies 200
/// rows of one byte from the master `source` into page column `(scroll_x>>3) - 1`
/// (`di` advanced `dst_stride` per row and wrapped to the `0x2000` circular page).
pub fn scroll_blit_column(
    planes: &mut [Vec<u8>; 4],
    source: &[u8],
    scroll_x: i32,
    wrap: usize,
    src_stride: usize,
    dst_stride: usize,
    rows: usize,
) {
    if scroll_x & 7 != 0 {
        // [asm 9662: test dl,7 / jne 969c]
        return;
    }
    let col = (((scroll_x >> 3) - 1) & 0xFFFF) as usize; // [asm 966E-9674]
    let mut si = col; // [asm 9675]
    let mut di = col; // [asm 9677]
    for plane in 0..4 {
        // [asm 967C-9695: al = 1,2,4,8]
        for _r in 0..rows {
            // [asm 967F: cx=0xC8]
            di &= wrap; // [asm 9682: and di,bp]
            planes[plane][di] = source[si]; // [asm 9684: movsb es:[di] <- ds:[si]]
            si = (si + src_stride) & 0xFFFF; // movsb si++ then [asm 9685: add si,0x4f]
            di = (di + dst_stride) & 0xFFFF; // movsb di++ then [asm 9688: add di,0x27]
        }
        di = (di.wrapping_sub(0x1F40)) & 0xFFFF; // [asm 968D: sub di,0x1f40]
    }
}

/// [asm 9804-9876] the menu/scene framebuffer scroll (the hottest menu op).
///
/// `planes` is the four EGA plane buffers (the A000 self-copy moves all four — an EGA
/// latched copy). `wrap` is the page mask (`bp`, normally 0x1FFF). Faithful planar
/// scroll; the semantic intent is just the camera moving by the scroll delta.
pub fn scroll_shift_frame(
    planes: &mut [Vec<u8>; 4],
    b199: i32,
    scroll_x: i32,
    scroll_y: i32,
    prev_scroll_y: i32,
    page_draw: usize,
    wrap: usize,
) {
    // Part 1: 8-px horizontal boundary -> shift the page_draw+0x27 column up one row [9804-9834]
    if (b199 & 8) != (scroll_x & 8) {
        // [asm 9804-9810]
        let mut si = (page_draw + 0x27) & 0xFFFF; // [asm 9812-9816]
        let mut di = (si.wrapping_sub(0x28)) & 0xFFFF; // [asm 9819]
        for _ in 0..0xC8 {
            // [asm 981C: cx=0xC8]
            si &= wrap; // [asm 9824]
            di &= wrap; // [asm 9826]
            for p in 0..4 {
                // [asm 9828: movsb (4-plane latched)]
                planes[p][di] = planes[p][si];
            }
            si = (si + 0x28) & 0xFFFF; // movsb +1 then [asm 9829: add si,0x27]
            di = (di + 0x28) & 0xFFFF; // movsb +1 then [asm 982C: add di,0x27]
        }
    }

    // Part 2: vertical scroll by the scroll_y delta [9836-9876]
    let mut delta = (scroll_y - prev_scroll_y) & 0xFFFF; // [asm 9836-983A]
    if delta & 0x8000 != 0 {
        delta -= 0x10000;
    }
    if delta == 0 {
        // [asm 983E: je]
        return;
    }
    let n: i32;
    let mut si: usize;
    let mut di: usize;
    let pd = page_draw as i32;
    if delta > 0 {
        // [asm 9840 jns -> 9842] scroll down
        si = ((pd - 1) & 0xFFFF) as usize; // [asm 9842-9846]
        di = ((si as i32 + 0x1F18) & 0xFFFF) as usize; // [asm 9847]
        n = delta;
    } else {
        // [asm 984D] scroll up
        n = -delta; // [asm 984D: neg dx]
        si = ((pd - 1 - 0x28 * n) & 0xFFFF) as usize; // [asm 984F-9858: si -= 0x28*|d|]
        di = si; // [asm 985A]
        si = ((si as i32 + 0x1F18) & 0xFFFF) as usize; // [asm 985C]
    }
    let count = ((0x29 * n) & 0xFFFF) as usize; // [asm 9865-9869: cx = 0x29*|d|]
    for _ in 0..count {
        // [asm 986B-9870]
        si &= wrap;
        di &= wrap;
        for p in 0..4 {
            // movsb (4-plane latched)
            planes[p][di] = planes[p][si];
        }
        si = (si + 1) & 0xFFFF;
        di = (di + 1) & 0xFFFF;
    }
}
