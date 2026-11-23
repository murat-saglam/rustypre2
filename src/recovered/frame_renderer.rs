// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 frame renderer — recovered native logic.
//!
//! Recovers the draws that paint the scrolling tile background: draw_tile_row,
//! draw_tile_column, draw_grid, build_background_ring, scroll_copy, panel_copy,
//! calc_scroll_source, redraw_animated_grid.
//!
//! All functions operate directly on the four EGA A000 plane buffers (the
//! "planes" abstraction), operating directly on plain byte
//! buffers. The VM↔memory translation lives in the bridge layer.

use crate::recovered::renderer::*;

pub const SCROLL_WRAP_SRC: usize = 0x3F40;
pub const SCREEN_ROW: usize = 0x28;
pub const SCROLL_HEIGHT: usize = 0xB0;
pub const VISIBLE_COLS: usize = 0x14;
pub const VISIBLE_ROWS: usize = 0x0C;
pub const RING_COLS: usize = 0x14;
pub const BG_PTR_BIAS: usize = 0x7E80;
pub const GRID_SI_ROW_ADVANCE: usize = 0xEC;
pub const GRID_DI_ROW_ADVANCE: usize = 0x280;
pub const GRID_BG_ROW_ADVANCE: usize = 0x258;

/// The tilemap view (tiles + attribute tables).
pub struct TileMapView<'a> {
    pub tiles: &'a [u8],
    pub tile_flags: &'a [u8],
    pub tile_type: &'a [u8],
    pub plane_attr: &'a [u8],
}

/// OR-accumulated attribute flags across a row.
///
/// These mirror the in-memory accumulators (which are *not* reset per row):
/// `plane_attr` ⇄ `[0x6BBD]`, `tile_flags` ⇄ `[0x2DF2]`,
/// `tile_type` ⇄ `[0x2DF4]`. Seed them with the pre-call memory values so the
/// result matches the original's running OR.
#[derive(Clone, Copy, Default)]
pub struct RowFlags {
    pub plane_attr: u8,
    pub tile_flags: u8,
    pub tile_type: u8,
}

/// Reveal one 12-tile vertical column during the idle look-around camera pan
/// (1030:350C).
///
/// The vertical counterpart of `draw_tile_row`: same `lodsb` → OR-flags →
/// `blit_sprite` body, but it walks 12 rows *down* the map column (`si += 0x100`
/// per row) and the screen pointer advances by `0x27E` per row. Mutates `planes`
/// + `flags`; returns the final `bg_ptr` (= final `[0x2DF6]`).
///
/// * `cell` — the raw camera map cell (`[0x2DE6]<<8 | [0x2DE4]`). The map source
///   `si` starts at `cell + col_param` (3478-347F: `ax = cell ; ax += di`).
/// * `col_param` — `0` for a left pan (3414), `0x13` for a right pan (3435).
/// * `scroll_src` — `[0x2DBA]` (from `calc_scroll_source`).
/// * `camera_col` — `[0x2DE8]` (the ring column; gates the `-0x28` screen wrap).
pub fn draw_tile_column(
    planes: &mut [Vec<u8>; 4],
    tilemap: &TileMapView,
    cell: usize,
    col_param: usize,
    scroll_src: usize,
    camera_col: usize,
    blit_type: &[u8],
    mask_region: &[u8],
    flags: &mut RowFlags,
) -> usize {
    let mut bg_ptr = (col_param + BG_PTR_BIAS) & 0xFFFF; // [asm 350C-3510] [0x2DF6] = col_param + 0x7E80
    let mut di = ((col_param << 1) + scroll_src) & 0xFFFF; // [asm 3521-3523] di = col_param*2 + [0x2DBA]
    if ((col_param + camera_col) & 0xFFFF) >= 0x14 {
        // [asm 3527-352E] cmp ax,0x14
        di = (di.wrapping_sub(0x28)) & 0xFFFF; // [asm 3530]
    }
    let mut si = (cell + col_param) & 0xFFFF; // [asm 351D] si = ax (= cell + col_param, from 347F)

    for _ in 0..VISIBLE_ROWS {
        // [asm 3537: cx = 0xC]
        if di >= WRAP_AT {
            // [asm 353A-3540]
            di = (di.wrapping_sub(WRAP_SPAN)) & 0xFFFF;
        }
        let tile = tilemap.tiles[si] as usize; // [asm 3544: lodsb]
        flags.plane_attr |= tilemap.plane_attr[tile]; // [asm 3547-354C] -> [0x6BBD]
        flags.tile_flags |= tilemap.tile_flags[tile]; // [asm 3553-3558] -> [0x2DF2]
        flags.tile_type |= tilemap.tile_type[tile]; // [asm 355F-3564] -> [0x2DF4]

        let typ = blit_type[tile];
        let mask = if typ >= 2 {
            let off = (typ as usize - 2) * 0x20;
            &mask_region[off..(off + 0x20).min(mask_region.len())]
        } else {
            &[]
        };
        blit_sprite(planes, tile, di, typ, bg_ptr, mask); // [asm 3571: call 3B88] bg_off = [0x2DF6]
        bg_ptr = (bg_ptr + 0x40) & 0xFFFF; // [asm 3575: add [0x2DF6],0x40]
        si = (si + 0x100) & 0xFFFF; // [asm 3544 lodsb si++ + 357B add si,0xFF]
        di = (di + 2 + 0x27E) & 0xFFFF; // blit's di+=2 contract [3B88] + [asm 357F: add di,0x27E] = 0x280
    }
    bg_ptr
}

/// The full 12×20 visible-grid redraw (1030:35A1).
pub struct GridResult;

/// Recover ``1030:35A1`` — the full 12x20 visible-grid redraw.
///
/// Guarded by a prev-camera/dirty check (the early-exit path); on redraw, draws
/// only the non-opaque (type>=1) tiles via the recovered blit, accumulates
/// `tile_flags` over *all* tiles, and updates the dirty flags + prev camera.
/// Returns a `GridResult`; `di` and the other registers are preserved by the
/// ASM (push/pop), so nothing register-side is part of the contract.
pub fn draw_grid(
    planes: &mut [Vec<u8>; 4],
    tilemap: &TileMapView,
    camera_x: u8,
    camera_y: u8,
    prev_x: u8,
    prev_y: u8,
    dirty: u8,
    dirty_rows: u8,
    scroll_src: usize,
    col_ring: usize,
    fine_scroll: usize,
    blit_type: &[u8],
    mask_region: &[u8],
) -> GridResult {
    // --- dirty / early-exit decision (mirrors 3590-35B9, incl. the prev stores) ---
    let (redrew, _new_prev_x, _new_prev_y) = if dirty_rows != 0 {
        (true, prev_x, prev_y)
    } else {
        let npx = camera_x;
        if camera_x != prev_x {
            (dirty != 0, npx, prev_y)
        } else {
            let npy = camera_y;
            let r = if camera_y != prev_y {
                dirty != 0
            } else {
                false
            };
            (r, npx, npy)
        }
    };

    if !redrew {
        return GridResult;
    }

    // --- redraw the 12x20 grid (mirrors 35C3-363A) ---
    let mut tile_flags_acc = 0u8;
    let mut _new_dirty = 0u8;
    let mut si = (camera_y as usize * 0x100 + camera_x as usize) & 0xFFFF;
    let mut di = scroll_src & 0xFFFF;
    let mut bg_ptr = BG_PTR_BIAS;

    for _row in 0..VISIBLE_ROWS {
        let mut dx = col_ring;
        let _row_start_di = di;
        for _col in 0..VISIBLE_COLS {
            let tile = tilemap.tiles[si] as usize;
            tile_flags_acc |= tilemap.tile_flags[tile];
            let typ = blit_type[tile];
            if di >= WRAP_AT {
                di = (di.wrapping_sub(WRAP_SPAN)) & 0xFFFF;
            }
            let mask = if typ >= 2 {
                let off = (typ as usize - 2) * 0x20;
                &mask_region[off..(off + 0x20).min(mask_region.len())]
            } else {
                &[]
            };
            let bg_off = (bg_ptr.wrapping_sub(ROW_STRIDE * fine_scroll)) & 0xFFFF;
            blit_sprite(planes, tile, di, typ, bg_off, mask);
            if typ >= 1 {
                _new_dirty = 1;
            }
            dx += 1;
            if dx >= RING_COLS {
                di = (di.wrapping_sub(ROW_STRIDE)) & 0xFFFF;
                dx = 0;
            }
            bg_ptr = (bg_ptr + 2) & 0xFFFF;
            di = (di + 2) & 0xFFFF;
            si = (si + 1) & 0xFFFF;
        }
        bg_ptr = (bg_ptr + GRID_BG_ROW_ADVANCE) & 0xFFFF;
        di = (di + GRID_DI_ROW_ADVANCE) & 0xFFFF;
        si = (si + GRID_SI_ROW_ADVANCE) & 0xFFFF;
    }

    let _ = tile_flags_acc;
    GridResult
}

/// Full background/ring REBUILD from a clean ring.
///
/// Draws **every** visible tile (all 12x20) into the ring, including the
/// **type-0 opaque** tiles that `draw_grid` skips (the per-frame path leaves
/// opaque tiles in the ring from scrolling; this path does not assume any prior
/// ring contents). It mirrors `draw_grid`'s exact ring coordinate walk (so
/// type>=1 tiles land identically) and applies the `WRAP_AT` ring wrap to *all*
/// tiles (as `draw_tile_row` does), since opaque tiles are now drawn too.
///
/// Returns the OR-accumulated `tile_flags`.
pub fn build_background_ring(
    planes: &mut [Vec<u8>; 4],
    tilemap: &TileMapView,
    camera_x: u8,
    camera_y: u8,
    scroll_src: usize,
    col_ring: usize,
    fine_scroll: usize,
    blit_type: &[u8],
    mask_region: &[u8],
) -> u8 {
    let mut tile_flags_acc = 0u8;
    let mut si = (camera_y as usize * 0x100 + camera_x as usize) & 0xFFFF;
    let mut di = scroll_src & 0xFFFF;
    let mut bg_ptr = BG_PTR_BIAS;

    for _row in 0..VISIBLE_ROWS {
        let mut dx = col_ring;
        for _col in 0..VISIBLE_COLS {
            let tile = tilemap.tiles[si] as usize;
            tile_flags_acc |= tilemap.tile_flags[tile];
            let typ = blit_type[tile];
            if di >= WRAP_AT {
                // ring wrap for ALL tiles (cf draw_tile_row)
                di = (di.wrapping_sub(WRAP_SPAN)) & 0xFFFF;
            }
            let mask = if typ >= 2 {
                let off = (typ as usize - 2) * 0x20;
                &mask_region[off..(off + 0x20).min(mask_region.len())]
            } else {
                &[]
            };
            let bg_off = (bg_ptr.wrapping_sub(ROW_STRIDE * fine_scroll)) & 0xFFFF;
            blit_sprite(planes, tile, di, typ, bg_off, mask); // ALL types, incl type-0 opaque
            dx += 1;
            if dx >= RING_COLS {
                di = (di.wrapping_sub(ROW_STRIDE)) & 0xFFFF;
                dx = 0;
            }
            bg_ptr = (bg_ptr + 2) & 0xFFFF;
            di = (di + 2) & 0xFFFF;
            si = (si + 1) & 0xFFFF;
        }
        bg_ptr = (bg_ptr + GRID_BG_ROW_ADVANCE) & 0xFFFF;
        di = (di + GRID_DI_ROW_ADVANCE) & 0xFFFF;
        si = (si + GRID_SI_ROW_ADVANCE) & 0xFFFF;
    }
    tile_flags_acc
}

/// Latched 4-plane copy of `n` bytes (es:di <- ds:si); returns (si, di).
fn copy_run(planes: &mut [Vec<u8>; 4], si: usize, di: usize, n: usize) -> (usize, usize) {
    let mut si = si;
    let mut di = di;
    for _ in 0..n {
        for p in 0..4 {
            planes[p][di] = planes[p][si];
        }
        si = (si + 1) & 0xFFFF;
        di = (di + 1) & 0xFFFF;
    }
    (si, di)
}

/// The vertical-scroll screen copy (1030:3A27).
///
/// A write-mode-1 latched 4-plane block copy (helper 452F) of the visible window
/// from the scroll ring buffer (`[0x2DB6]`) to the display page (`[0x2DD4]`).
/// Each row is split into `dl` + `dh` byte segments around the column ring
/// (`[0x2DE4]`) with a one-row source back-step; the copy runs over `bp` main
/// rows then `bx` rows from the ring wrap (`0x3F40`). Finally the leading
/// `S = 0x28*row_factor` strip at `dest` is cleared on all planes.
pub fn scroll_copy(
    planes: &mut [Vec<u8>; 4],
    scroll_src: usize,
    dest: usize,
    col_ring: usize,
    fine_scroll: usize,
    row_ring: usize,
    row_factor: usize,
) {
    let s = (SCREEN_ROW * row_factor) & 0xFFFF; // [asm 3A12] S = 0x28 * [0x6BF4]
    let mut si = scroll_src & 0xFFFF; // [asm 3A1C]
    let mut di = (dest + s) & 0xFFFF; // [asm 3A20/3A35] di = [0x2DD4] + S
    let dh = (col_ring << 1) & 0xFF; // [asm 3A26/3A2E]
    let dl = ((0x14 - col_ring) << 1) & 0xFF; // [asm 3A24/3A2A/3A2C] dl + dh == 0x28

    // row-count split between the main copy and the ring-wrap copy [asm 3A3A-3A68]
    let mut bp = (0xC0usize.wrapping_sub(fine_scroll).wrapping_sub(row_ring << 4)) & 0xFFFF;
    let mut bx;
    if bp >= SCROLL_HEIGHT {
        // [asm 3A4A: cmp bp,0xB0 / jb]
        bp = (SCROLL_HEIGHT - row_factor) & 0xFFFF;
        bx = 0;
    } else {
        bx = (SCROLL_HEIGHT - bp - row_factor) & 0xFFFF;
        if bx & 0x8000 != 0 {
            // [asm 3A64: jns] negative -> fold into bp
            bp = (bp + bx) & 0xFFFF;
            bx = 0;
        }
    }

    si = (si + SCREEN_ROW * fine_scroll) & 0xFFFF; // [asm 3A6A] si += 0x28 * fine
    if bp == 0 {
        // [asm 3A7E-3A82] xchg bp,bx
        std::mem::swap(&mut bp, &mut bx);
    }

    for _ in 0..bp {
        // [asm 3A84-3A91] main rows
        let (nsi, ndi) = copy_run(planes, si, di, dl);
        si = (nsi.wrapping_sub(SCREEN_ROW)) & 0xFFFF;
        let (nsi2, ndi2) = copy_run(planes, si, ndi, dh);
        si = (nsi2 + SCREEN_ROW) & 0xFFFF;
        di = ndi2;
    }

    if bx != 0 {
        // [asm 3A93-3AAB] ring-wrap rows
        si = (SCROLL_WRAP_SRC + dh) & 0xFFFF;
        for _ in 0..bx {
            let (nsi, ndi) = copy_run(planes, si, di, dl);
            si = (nsi.wrapping_sub(SCREEN_ROW)) & 0xFFFF;
            let (nsi2, ndi2) = copy_run(planes, si, ndi, dh);
            si = (nsi2 + SCREEN_ROW) & 0xFFFF;
            di = ndi2;
        }
    }

    // clear the leading strip on all four planes [asm 3AB2-3ACB]: S>>1 words at dest
    let words = s >> 1;
    for k in 0..words * 2 {
        let off = (dest + k) & 0xFFFF;
        for p in 0..4 {
            planes[p][off] = 0;
        }
    }
}

/// Compute the scroll-copy source offset (1030:3588).
///
/// Returns the offset stored in `[0x2DBA]`: `2*camera_col + 0x280*camera_row +
/// 0x3F40` (mod 0x10000). `camera_row` is the byte `[0x2DEA]`; the ASM builds the
/// `0x280*row` term as `(row<<9) + (row<<7)` via the dx/bx shifts.
pub fn calc_scroll_source(camera_col: usize, camera_row: usize) -> usize {
    let ax = (camera_col << 1) & 0xFFFF; // [asm 3588: shl ax,1]
    let dx = (camera_row << 8) & 0xFFFF; // [asm 358A-358E: dh=[2DEA], dl=0
    let bx = (dx << 1) & 0xFFFF; // [asm 3592: shl bx,1]  (row<<9)
    let dx = (dx >> 1) & 0xFFFF; // [asm 3594: shr dx,1]  (row<<7)
    (ax + dx + bx + SCROLL_WRAP_SRC) & 0xFFFF // [asm 3596-359A: +0x3F40]
}

/// Redraw the animated background tiles (1030:3668).
///
/// Walks the 12x20 visible grid (same ring buffer as `draw_grid`): for every
/// tile it ORs `type_tbl[tile]` into the accumulated flags, and **only where**
/// `flag_tbl[tile] != 0` (the animated tiles) it remaps the tile through the
/// current animation frame (`anim_xlat[tile]` = `[[0x6BC2] + tile]`) and blits
/// it opaque.
///
/// Returns `(tile_flags_acc, any_drawn)` (the `[0x2DF2]` / `[0x6BBD]` contract).
/// Every animated tile observed is type 0 (opaque); a non-type-0 tile is
/// unrecovered (the ASM keeps no background pointer here) and panics (fail loud).
pub fn redraw_animated_grid(
    planes: &mut [Vec<u8>; 4],
    tiles: &[u8],
    type_tbl: &[u8],
    flag_tbl: &[u8],
    anim_xlat: &[u8],
    blit_type: &[u8],
    camera_col: u8,
    camera_row: u8,
    fine_col: usize,
    scroll_dest: usize,
) -> (u8, u8) {
    let mut tile_flags_acc = 0u8; // [asm 36A9] [0x2DF2] reset, then OR
    let mut any_drawn = 0u8; // [asm 36AE] [0x6BBD] reset
    let mut si = (camera_row as usize * 0x100 + camera_col as usize) & 0xFFFF; // [asm 36B3-36BA] si = ah:al
    let mut di = scroll_dest & 0xFFFF; // [asm 36C0] di = [0x2DBA]

    for _ in 0..VISIBLE_ROWS {
        // [asm 36C4] ch = 0x0C
        let mut dx = fine_col; // [asm 36C4] dx = [0x2DE8] (per row)
        for _ in 0..VISIBLE_COLS {
            // [asm 36C8] cl = 0x14
            let tile = tiles[si] as usize; // [asm 36CA] bl = es:[si]
            tile_flags_acc |= type_tbl[tile]; // [asm 36CD-36D1] -> [0x2DF2]
            if flag_tbl[tile] != 0 {
                // [asm 36D5] draw only flagged tiles
                any_drawn = 1; // [asm 36DC] [0x6BBD] = 1
                let remapped = anim_xlat[tile] as usize; // [asm 36E1-36E7] xlat via [0x6BC2]
                if blit_type[remapped] != 0 {
                    // Only opaque (type-0) animated tiles are redrawn in this pass
                    dx += 1;
                    if dx >= RING_COLS { di = (di.wrapping_sub(ROW_STRIDE)) & 0xFFFF; dx = 0; }
                    di = (di + 2) & 0xFFFF;
                    si = (si + 1) & 0xFFFF;
                    continue;
                }
                if di >= WRAP_AT {
                    // [asm 36EA] pre-blit ring wrap
                    di = (di.wrapping_sub(WRAP_SPAN)) & 0xFFFF;
                }
                blit_sprite(planes, remapped, di, 0, 0, &[]); // [asm 36F4 -> 3B88] opaque blit
            }
            dx += 1; // [asm 36F9] inc dx
            if dx >= RING_COLS {
                // [asm 36FA-3702] row-buffer ring
                di = (di.wrapping_sub(ROW_STRIDE)) & 0xFFFF;
                dx = 0;
            }
            di = (di + 2) & 0xFFFF; // [asm 3704] per-tile (blit nets +2)
            si = (si + 1) & 0xFFFF; // [asm 3706] next tile
        }
        di = (di + GRID_DI_ROW_ADVANCE) & 0xFFFF; // [asm 370B] +0x280 next screen row
        si = (si + GRID_SI_ROW_ADVANCE) & 0xFFFF; // [asm 370F] +0xEC next tilemap row
    }

    (tile_flags_acc, any_drawn)
}
