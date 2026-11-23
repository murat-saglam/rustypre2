// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 sprite classifier — recovered native logic (pure).
//!
//! Recovers 1030:4232 — classifies each cache slot into transparency type + saves masks.

use crate::codecs::sprite_decode::{SpriteCache, NUM_SLOTS, SLOT_BYTES};

pub const TYPE_OPAQUE: u8 = 0;
pub const TYPE_EMPTY: u8 = 1;
pub const FIRST_PARTIAL_ID: u8 = 2;

/// The classifier's output.
pub struct ClassifyResult {
    pub types: Vec<u8>,      // NUM_SLOTS bytes: per-slot class
    pub masks: Vec<u8>,      // partial masks, compacted
    pub partial_count: usize,
}

/// The 32 transparency-mask bytes for one slot: bit=1 where pixel is colour 0 (transparent).
/// ~(p0|p1|p2|p3) per byte — software model of EGA read-mode-1 colour compare (cmp=0).
pub fn slot_mask(cache: &SpriteCache, slot: usize) -> Vec<u8> {
    let off = slot * SLOT_BYTES;
    let p0 = &cache.planes[0];
    let p1 = &cache.planes[1];
    let p2 = &cache.planes[2];
    let p3 = &cache.planes[3];
    (0..SLOT_BYTES)
        .map(|k| {
            let val = p0[off + k] | p1[off + k] | p2[off + k] | p3[off + k];
            !val & 0xFF
        })
        .collect()
}

/// Recover 1030:4232 — assign each cache slot a transparency class + save masks.
pub fn classify_sprites(cache: &SpriteCache) -> ClassifyResult {
    let mut types = vec![0u8; NUM_SLOTS];
    let mut masks = vec![0u8; NUM_SLOTS * SLOT_BYTES];
    let mut counter: u8 = 1;

    for slot in 0..NUM_SLOTS {
        let m = slot_mask(cache, slot);
        let mut dh: u8 = 0;   // OR of mask bytes
        let mut dl: u8 = 0xFF; // AND of mask bytes
        for &b in &m {
            dh |= b;
            dl &= b;
        }
        if dh == 0 {
            types[slot] = TYPE_OPAQUE;
        } else if dl == 0xFF {
            types[slot] = TYPE_EMPTY;
        } else {
            counter = counter.wrapping_add(1);
            types[slot] = counter;
            let base = (counter as usize - FIRST_PARTIAL_ID as usize) * SLOT_BYTES;
            if base + SLOT_BYTES <= masks.len() {
                masks[base..base + SLOT_BYTES].copy_from_slice(&m);
            }
        }
    }

    ClassifyResult {
        types,
        masks,
        partial_count: counter as usize - 1,
    }
}
