// SPDX-License-Identifier: GPL-3.0-only
//! Planar EGA/VGA to 24-bit RGB framebuffer decoder.
//!
//! Decodes 4-plane Mode `0x0D` EGA framebuffers at native 320x200 resolution
//! using the 256-entry VGA DAC palette, CRTC display start, horizontal pixel panning,
//! and circular page wrap mask.

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 200;
pub const PLANAR_ROW_BYTES: usize = 40; // 320 px / 8 px per byte

/// Decode four EGA plane buffers to a `320 * 200 * 3` RGB buffer.
pub fn render_planar_rgb_from_planes(
    planes: &[Vec<u8>; 4],
    display_start: usize,
    palette: Option<&[[u8; 3]; 256]>,
    pel_pan: u8,
    active_width: usize,
) -> Vec<u8> {
    render_planar_rgb_from_planes_wrapped(planes, display_start, palette, pel_pan, active_width, 0xFFFF)
}

/// Decode four EGA plane buffers to RGB with an explicit circular page wrap mask
/// (e.g. `0x1FFF` for menu and world-map screens).
pub fn render_planar_rgb_from_planes_wrapped(
    planes: &[Vec<u8>; 4],
    display_start: usize,
    palette: Option<&[[u8; 3]; 256]>,
    pel_pan: u8,
    active_width: usize,
    wrap: usize,
) -> Vec<u8> {
    let mut rgb = vec![0u8; HEIGHT * WIDTH * 3];
    render_planar_rgb_into(planes, display_start, palette, pel_pan, active_width, wrap, &mut rgb);
    rgb
}

/// Decode four EGA plane buffers into an existing `320 * 200 * 3` RGB buffer with zero heap allocations.
pub fn render_planar_rgb_into(
    planes: &[Vec<u8>; 4],
    display_start: usize,
    palette: Option<&[[u8; 3]; 256]>,
    pel_pan: u8,
    active_width: usize,
    wrap: usize,
    rgb: &mut [u8],
) {
    let pal: &[[u8; 3]; 256] = palette.unwrap_or(&DEFAULT_PAL);

    let start = display_start & 0xFFFF;
    let pel = (pel_pan & 7) as usize;
    let aw = active_width.min(WIDTH);
    let ncols = ((pel + aw + 7) / 8).min(PLANAR_ROW_BYTES + 2);
    let wrap = if wrap == 0 { 0xFFFF } else { wrap };

    let p0 = &planes[0];
    let p1 = &planes[1];
    let p2 = &planes[2];
    let p3 = &planes[3];
    let mut color = [0u8; (PLANAR_ROW_BYTES + 2) * 8];

    for y in 0..HEIGHT {
        let rowbase = (start + y * PLANAR_ROW_BYTES) & wrap;

        for xb in 0..ncols {
            let off = (rowbase + xb) & wrap;
            let b0 = if off < p0.len() { p0[off] } else { 0 };
            let b1 = if off < p1.len() { p1[off] } else { 0 };
            let b2 = if off < p2.len() { p2[off] } else { 0 };
            let b3 = if off < p3.len() { p3[off] } else { 0 };
            let base = xb * 8;
            for bit in 0..8 {
                let mask = 0x80u8 >> bit;
                color[base + bit] = ((b0 & mask != 0) as u8)
                    | (((b1 & mask != 0) as u8) << 1)
                    | (((b2 & mask != 0) as u8) << 2)
                    | (((b3 & mask != 0) as u8) << 3);
            }
        }

        let row_rgb_off = y * WIDTH * 3;
        for x in 0..WIDTH {
            let idx = if x < aw { color[pel + x] as usize } else { 0 };
            let c = pal[idx & 0xFF];
            let off = row_rgb_off + x * 3;
            rgb[off] = c[0];
            rgb[off + 1] = c[1];
            rgb[off + 2] = c[2];
        }
    }
}

const DEFAULT_PAL: [[u8; 3]; 256] = {
    let mut p = [[0u8; 3]; 256];
    p[0] = [0x00, 0x00, 0x00]; p[1] = [0x00, 0x00, 0xAA]; p[2] = [0x00, 0xAA, 0x00]; p[3] = [0x00, 0xAA, 0xAA];
    p[4] = [0xAA, 0x00, 0x00]; p[5] = [0xAA, 0x00, 0xAA]; p[6] = [0xAA, 0x55, 0x00]; p[7] = [0xAA, 0xAA, 0xAA];
    p[8] = [0x55, 0x55, 0x55]; p[9] = [0x55, 0x55, 0xFF]; p[10] = [0x55, 0xFF, 0x55]; p[11] = [0x55, 0xFF, 0xFF];
    p[12] = [0xFF, 0x55, 0x55]; p[13] = [0xFF, 0x55, 0xFF]; p[14] = [0xFF, 0xFF, 0x55]; p[15] = [0xFF, 0xFF, 0xFF];
    p
};
