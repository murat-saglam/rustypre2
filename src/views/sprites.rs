// SPDX-License-Identifier: GPL-3.0-only
//! Memory views for sprite-sheet decoding (`1030:4316` / `1030:4389`) and the planar VRAM cache.

use crate::engine::state::NativeGameState;
use crate::engine::vga::{EGA_APERTURE, EGA_PLANE_STRIDE};
use crate::codecs::sprite_decode::*;

pub const CACHE_OFF: usize = 0x5E80;
pub const CACHE_PLANE_BYTES: usize = NUM_SLOTS * SLOT_BYTES; // 0x2000

const VAR_INDEX_COPY: usize = 0x25CE;
const DATA_SEG: usize = crate::views::memory_adapter::DATA_SEG;

/// Read a decompressed sprite sheet starting at segment `seg`.
pub fn read_sprite_sheet(mem: &NativeGameState, seg: u16) -> SpriteSheet {
    let base = ((seg as usize) << 4) & 0xFFFFF;
    let end = base + PIXEL_BASE + NUM_SLOTS * SPRITE_BYTES;
    SpriteSheet::from_bytes(&mem.data[base..end.min(mem.data.len())])
}

#[inline]
fn plane_base(plane: usize) -> usize {
    EGA_APERTURE + plane * EGA_PLANE_STRIDE + CACHE_OFF
}

/// Read the four planar sprite-cache planes (`0xA000:0x5E80`) from `mem`.
pub fn read_sprite_cache(mem: &NativeGameState) -> SpriteCache {
    let planes: Vec<Vec<u8>> = (0..PLANES)
        .map(|p| {
            let b = plane_base(p);
            mem.data[b..b + CACHE_PLANE_BYTES].to_vec()
        })
        .collect();
    SpriteCache { planes }
}

/// Read the 256-entry sprite index table copy at `DGROUP:0x25CE`.
pub fn shared_index_codes(mem: &NativeGameState) -> Vec<u16> {
    let base = (DATA_SEG << 4) + VAR_INDEX_COPY;
    (0..NUM_SLOTS)
        .map(|i| {
            let off = base + 2 * i;
            mem.data[off] as u16 | ((mem.data[off + 1] as u16) << 8)
        })
        .collect()
}

/// Demultiplex local sprite slots (`code < 0x100`) from the sprite sheet at `src_seg`.
pub fn compute_local_slots(mem: &NativeGameState, src_seg: u16) -> Vec<(usize, Vec<Vec<u8>>)> {
    let sheet = read_sprite_sheet(mem, src_seg);
    let mut out = Vec::new();
    for (slot, &code) in sheet.index_table.iter().enumerate() {
        if code < LOCAL_CODE_MAX {
            out.push((slot, demux_sprite(&sheet.local_sprite(code))));
        }
    }
    out
}

#[inline]
fn shared_sprite_segment(base: u16, code: u16) -> u16 {
    ((code.wrapping_sub(LOCAL_CODE_MAX)) << 3).wrapping_add(base) & 0xFFFF
}

/// Demultiplex shared sprite slots (`code >= 0x100`) from the union bank at `shared_base`.
pub fn compute_shared_slots(mem: &NativeGameState, shared_base: u16) -> Vec<(usize, Vec<Vec<u8>>)> {
    let mut out = Vec::new();
    for (slot, &code) in shared_index_codes(mem).iter().enumerate() {
        if code >= LOCAL_CODE_MAX {
            let seg = shared_sprite_segment(shared_base, code);
            let b = ((seg as usize) << 4) & 0xFFFFF;
            let sprite_data = &mem.data[b..(b + SPRITE_BYTES).min(mem.data.len())];
            out.push((slot, demux_sprite(sprite_data)));
        }
    }
    out
}

/// Write demultiplexed `(slot, planes)` entries into the EGA planar sprite cache (`0xA000:0x5E80`).
pub fn write_slots(mem: &mut NativeGameState, slot_map: &[(usize, Vec<Vec<u8>>)]) {
    for (slot, planes) in slot_map {
        let off = slot * SLOT_BYTES;
        for p in 0..PLANES {
            if p < planes.len() {
                let b = plane_base(p) + off;
                if b + SLOT_BYTES <= mem.data.len() {
                    mem.data[b..b + SLOT_BYTES].copy_from_slice(&planes[p]);
                }
            }
        }
    }
}
