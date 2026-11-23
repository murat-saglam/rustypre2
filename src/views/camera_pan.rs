// SPDX-License-Identifier: GPL-3.0-only
//! Bridge for the idle look-around camera pan (``1030:3414`` left / ``3435`` right).
//!
//! The FSM ``anim13`` (idle look-around) path scrolls the viewport one column and reveals the newly-exposed tile
//! column. This composes `calc_scroll_source` (``3588``) and `draw_tile_column` (``350C``) over the tile map and
//! EGA planes.

use crate::engine::state::NativeGameState;
use crate::engine::vga::{EGA_APERTURE, EGA_PLANE_STRIDE};
use crate::engine::dgroup_offsets::{
    ANY_ANIMATED_FLAG, BG_RESTORE_PTR, CAM_COL, CAM_ROW, COL_RING, GRID_DIRTY, PLAYER_SLOT,
    ROW_RING, SCROLL_ACCUM, SCROLL_COPY_SRC, TILE_FLAGS_ACC,
};
use crate::recovered::frame_renderer::{RowFlags, calc_scroll_source, draw_tile_column, TileMapView};
use crate::views::frame::{read_blit_type_table, read_tilemap};

const DS: usize = 0x1A0F;

/// Sign-extend a 16-bit value to i32.
#[inline]
fn s16(v: u16) -> i32 {
    let v = v as i32;
    if v & 0x8000 != 0 {
        v - 0x10000
    } else {
        v
    }
}

/// The direction of the camera pan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanDirection {
    Left,
    Right,
}

/// Run the camera pan ``1030:3414`` (``direction='left'``) / ``3435`` (``'right'``) on ``mem`` in place:
/// update the camera column state, recompute the scroll source, and blit the revealed 12-tile column into the
/// EGA planes. Returns ``true`` if the pan happened (the inner edge gate passed), ``false`` if it was at the
/// edge (the ASM's ``stc`` no-op path).
pub fn apply_camera_pan(mem: &mut NativeGameState, direction: PanDirection) -> bool {
    let (col_param, de8) = match direction {
        PanDirection::Left => {
            // [3414]
            let col_param = 0usize;
            if mem.rw(CAM_COL) == 0 {
                // [341F-3424] at the left map edge -> no pan
                return false;
            }
            mem.ww(CAM_COL, mem.rw(CAM_COL).wrapping_sub(1)); // [3426] dec [0x2DE4]
            let ax = mem.rw(COL_RING).wrapping_sub(1) as usize; // [342A-342D] dec
            let de8 = if (ax as u16) & 0x8000 != 0 {
                0x13 // [342E jns / 3430] underflow -> 0x13
            } else {
                ax & 0xFFFF
            };
            (col_param, de8)
        }
        PanDirection::Right => {
            // [3435]
            let col_param = 0x13usize;
            let px = (s16(mem.rw(PLAYER_SLOT)) >> 4) - 0x14; // [343E-3449] player tile X - 0x14
            let px_i32 = px as i32;
            let scroll_accum = s16(mem.rw(SCROLL_ACCUM));
            // [344C-345A] jge: clamp = scroll_accum if scroll_accum >= px else 0xEC
            let clamp = if scroll_accum >= px_i32 {
                mem.rw(SCROLL_ACCUM) as usize
            } else {
                0xECusize
            };
            if mem.rw(CAM_COL) as usize >= clamp {
                // [345A-345E] jae -> at the right limit, no pan
                return false;
            }
            mem.ww(CAM_COL, mem.rw(CAM_COL).wrapping_add(1)); // [3463] inc [0x2DE4]
            let ax = mem.rw(COL_RING).wrapping_add(1) as usize; // [3467-346A] inc
            let de8 = if ax >= 0x14 {
                0 // [346B-3470] wrap at 0x14 -> 0
            } else {
                ax & 0xFFFF
            };
            (col_param, de8)
        }
    };

    mem.ww(COL_RING, de8 as u16); // [3472] [0x2DE8] = ax

    let scroll_src = calc_scroll_source(de8, mem.rb(ROW_RING) as usize); // [3475 -> 3588] [0x2DBA]
    mem.ww(SCROLL_COPY_SRC, scroll_src as u16);

    let cell = ((mem.rb(CAM_ROW) as usize) << 8 | mem.rb(CAM_COL) as usize) & 0xFFFF; // [3478-347C] camera map cell

    // Read the four EGA planes from the aperture
    let mut planes: [Vec<u8>; 4] = [
        mem.data[EGA_APERTURE..EGA_APERTURE + EGA_PLANE_STRIDE].to_vec(),
        mem.data[EGA_APERTURE + EGA_PLANE_STRIDE..EGA_APERTURE + 2 * EGA_PLANE_STRIDE].to_vec(),
        mem.data[EGA_APERTURE + 2 * EGA_PLANE_STRIDE..EGA_APERTURE + 3 * EGA_PLANE_STRIDE].to_vec(),
        mem.data[EGA_APERTURE + 3 * EGA_PLANE_STRIDE..EGA_APERTURE + 4 * EGA_PLANE_STRIDE].to_vec(),
    ];

    let mut flags = RowFlags {
        plane_attr: mem.rb(ANY_ANIMATED_FLAG),
        tile_flags: mem.rb(TILE_FLAGS_ACC),
        tile_type: mem.rb(GRID_DIRTY),
    };

    let tilemap = read_tilemap(mem, None);
    let blit_type = read_blit_type_table(mem);
    let ds_base = DS << 4;
    let mask_region: Vec<u8> = mem.data[ds_base + 0x2DF8..ds_base + 0x4DF8].to_vec();

    let tilemap_view = TileMapView {
        tiles: &tilemap.tiles,
        tile_flags: &tilemap.tile_flags,
        tile_type: &tilemap.tile_type,
        plane_attr: &tilemap.plane_attr,
    };

    let bg_ptr = draw_tile_column(
        &mut planes,
        &tilemap_view,
        cell,
        col_param,
        scroll_src,
        de8,
        &blit_type,
        &mask_region,
        &mut flags,
    ); // [3481 -> 350C]

    // Write the revealed column back to the planes
    for i in 0..4 {
        let base = EGA_APERTURE + i * EGA_PLANE_STRIDE;
        mem.data[base..base + EGA_PLANE_STRIDE].copy_from_slice(&planes[i]);
    }

    mem.ww(BG_RESTORE_PTR, bg_ptr as u16);
    mem.wb(ANY_ANIMATED_FLAG, flags.plane_attr);
    mem.wb(TILE_FLAGS_ACC, flags.tile_flags);
    mem.wb(GRID_DIRTY, flags.tile_type);
    true
}
