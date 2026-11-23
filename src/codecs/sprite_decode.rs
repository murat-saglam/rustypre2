// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 sprite-sheet decode primitives (`1030:4316` / `1030:4389`).
//!
//! Demultiplexes decompressed 16x16 4-plane sprite sheets into the planar VRAM
//! sprite cache (`0xA000:0x5E80`, 256 slots of 32 bytes per plane).

pub const NUM_SLOTS: usize = 256;
pub const PLANES: usize = 4;
pub const SLOT_BYTES: usize = 0x20; // 32 bytes per plane per sprite (16x16, 1bpp)
pub const SPRITE_BYTES: usize = PLANES * SLOT_BYTES; // 128 bytes per sprite
pub const PIXEL_BASE: usize = 0x200; // sheet pixel data starts after the 256-entry index table
pub const LOCAL_CODE_MAX: u16 = 0x100; // code < this -> local bank; otherwise shared

/// Split a 128-byte sprite into its four 32-byte EGA planes.
pub fn demux_sprite(sprite: &[u8]) -> Vec<Vec<u8>> {
    (0..PLANES)
        .map(|p| sprite[p * SLOT_BYTES..(p + 1) * SLOT_BYTES].to_vec())
        .collect()
}

/// Decompressed sprite sheet: 256-entry `u16` index table + pixel data.
#[derive(Clone)]
pub struct SpriteSheet {
    pub index_table: Vec<u16>,
    pub pixel_data: Vec<u8>,
}

impl SpriteSheet {
    pub fn from_bytes(data: &[u8]) -> Self {
        let index_table: Vec<u16> = (0..NUM_SLOTS)
            .map(|i| {
                let off = 2 * i;
                if off + 1 < data.len() {
                    data[off] as u16 | ((data[off + 1] as u16) << 8)
                } else {
                    0
                }
            })
            .collect();
        let pixel_data = if data.len() > PIXEL_BASE {
            data[PIXEL_BASE..].to_vec()
        } else {
            Vec::new()
        };
        Self {
            index_table,
            pixel_data,
        }
    }

    /// Extract the 128-byte local sprite at `code` (`PIXEL_BASE + code * 128`).
    pub fn local_sprite(&self, code: u16) -> Vec<u8> {
        let off = code as usize * SPRITE_BYTES;
        if off + SPRITE_BYTES <= self.pixel_data.len() {
            self.pixel_data[off..off + SPRITE_BYTES].to_vec()
        } else {
            vec![0u8; SPRITE_BYTES]
        }
    }
}

/// Planar VRAM sprite cache: four parallel plane buffers (`NUM_SLOTS * SLOT_BYTES` bytes each).
pub struct SpriteCache {
    pub planes: Vec<Vec<u8>>,
}
