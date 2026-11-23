// SPDX-License-Identifier: GPL-3.0-only
//! Memory views for the level tile map and per-tile attribute tables.

use crate::engine::state::NativeGameState;
use crate::views::memory_adapter::DATA_SEG;

pub const VAR_LEVEL_SEG: usize    = 0x2DDA; // level-data block base segment

// Per-tile attribute lookup tables in DGROUP (indexed by tile id):
pub const TBL_PLANE_ATTR: usize   = 0x6988;
pub const TBL_TILE_FLAGS: usize   = 0x805E;
pub const TBL_TILE_TYPE: usize    = 0x4DF8;

pub const VAR_BLIT_TYPE: usize    = 0x4DF8;
pub const TILEMAP_WINDOW: usize   = 0x10000;

/// Row-major level tile map and its three per-tile attribute tables.
#[derive(Clone, Debug, Default)]
pub struct TileMap {
    /// `rows * stride` bytes of tile indices starting at offset 0 of the level segment.
    pub tiles: Vec<u8>,
    /// 256-entry table: tile index -> plane/attr flags (`1A0F:0x6988`).
    pub plane_attr: Vec<u8>,
    /// 256-entry table: tile index -> tile flags (`1A0F:0x805E`).
    pub tile_flags: Vec<u8>,
    /// 256-entry table: tile index -> type/dirty bits (`1A0F:0x4DF8`).
    pub tile_type: Vec<u8>,
}

/// Read the 256-entry sprite-type table (`1A0F:0x4DF8`).
pub fn read_blit_type_table(state: &NativeGameState) -> Vec<u8> {
    let base = ((DATA_SEG << 4) + VAR_BLIT_TYPE) & 0xFFFFF;
    state.data[base..base + 0x100].to_vec()
}

/// Read the level tile map and per-tile attribute tables from `state`.
pub fn read_tilemap(state: &NativeGameState, _rows: Option<usize>) -> TileMap {
    let seg = state.rw(VAR_LEVEL_SEG);
    let flat = ((seg as usize) << 4) & 0xFFFFF;
    let data = (DATA_SEG << 4) & 0xFFFFF;
    let end = (flat + TILEMAP_WINDOW).min(state.data.len());

    let tbl = |off: usize| state.data[data + off..data + off + 0x100].to_vec();

    TileMap {
        tiles: state.data[flat..end].to_vec(),
        plane_attr: tbl(TBL_PLANE_ATTR),
        tile_flags: tbl(TBL_TILE_FLAGS),
        tile_type: tbl(TBL_TILE_TYPE),
    }
}
