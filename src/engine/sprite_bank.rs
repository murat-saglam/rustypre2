// SPDX-License-Identifier: GPL-3.0-only
//! Build the global sprite bank into a NativeGameState (the VM-less counterpart of 1030:2DFA).
//!
//! The recovered transform (sprite_bank.rs) is pure; this places its output into the
//! NativeGameState exactly where the ASM does: the 5-plane bank at the load segment,
//! the per-sprite far pointers in the [0x5f48]/[0x62e8] DGROUP tables, and the bumped
//! load top [0x2875].

use std::path::Path;
use crate::codecs::sqz::unpack_sqz;
use crate::codecs::sprite_bank::{bottom_anchor_y_offsets, build_sprite_bank, build_sprite_offset_tables};
use crate::engine::state::{NativeGameState, DS_BASE};

const TABLE: usize = 0x7190;       // sprite descriptor table (static DGROUP data)
const YOFFTAB: usize = 0x752A;     // per-sprite (x_off, y_off) draw-offset table

/// [asm 2dfa] Decode SPRITES.SQZ and build the 5-plane global sprite bank into state.
///
/// Writes the [0x5f48]/[0x62e8] far-pointer tables and bumps [0x2875].
/// Returns the new load top.
pub fn native_build_sprite_bank(state: &mut NativeGameState, game_root: &Path) -> u16 {
    let sprites_seg = state.rw(0x2875); // load_top

    // Read the 0x7190 descriptor table (enough for 460 entries + terminator)
    let table: Vec<u8> = state.data[DS_BASE + TABLE..DS_BASE + TABLE + 0x400].to_vec();

    // [asm 2E15-2E29] one-time Y draw-offset fixup: y_off = height - y_off per id
    let draw: Vec<u8> = state.data[DS_BASE + YOFFTAB..DS_BASE + YOFFTAB + 0x400].to_vec();
    let fixed = bottom_anchor_y_offsets(&table, &draw);
    let end = (DS_BASE + YOFFTAB + fixed.len()).min(state.data.len());
    state.data[DS_BASE + YOFFTAB..end].copy_from_slice(&fixed);

    // Decode SPRITES.SQZ (case-insensitive for Linux/macOS filesystems)
    let path = crate::engine::assets::resolve_game_path(game_root, "SPRITES.SQZ")
        .unwrap_or_else(|_| game_root.join("SPRITES.SQZ"));
    let raw = std::fs::read(&path).expect("SPRITES.SQZ not found");
    let decoded = unpack_sqz(&raw);

    // Build the 5-plane bank
    let bank = build_sprite_bank(&decoded, &table);
    let base = (sprites_seg as usize) << 4;
    state.data[base..base + bank.len()].copy_from_slice(&bank);

    // Build and write offset/segment tables
    let (offsets, segments) = build_sprite_offset_tables(&table, sprites_seg);
    for (i, (off, seg)) in offsets.iter().zip(segments.iter()).enumerate() {
        // [0x5F48 + i*2] = offset, [0x62E8 + i*2] = segment
        let off_addr = DS_BASE + 0x5F48 + i * 2;
        let seg_addr = DS_BASE + 0x62E8 + i * 2;
        if off_addr + 1 < state.data.len() {
            state.data[off_addr] = *off as u8;
            state.data[off_addr + 1] = (*off >> 8) as u8;
        }
        if seg_addr + 1 < state.data.len() {
            state.data[seg_addr] = *seg as u8;
            state.data[seg_addr + 1] = (*seg >> 8) as u8;
        }
    }

    // Bump load_top: count + count/4 + 1 (the 1.25x reserve)
    let count = (decoded.len() + 15) / 16; // paragraphs
    let new_top = sprites_seg + count as u16 + (count >> 2) as u16 + 1;

    // Store sprites_seg at [0x2DB4]
    state.ww(0x2DB4, sprites_seg);
    // Store new load_top at [0x2875]
    state.ww(0x2875, new_top);

    log::info!("Sprite bank: {} sprites, {} bytes bank, seg=0x{:04X}, new_top=0x{:04X}",
        offsets.len(), bank.len(), sprites_seg, new_top);

    new_top
}
