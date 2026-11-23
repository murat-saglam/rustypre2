// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 object-update walker — the per-frame 12-slot tick (1030:684E).
//!
//! Walks 12 object slots at 0x4FD0, stride 0x12: velocity, terrain collision, anim, AI dispatch.
//! The AI handlers (object_update.py) are complex; this module provides the walker structure
//! and delegates the leaf primitives (apply_velocity, advance_animation, terrain_collision,
//! dispatch_handler) to `object_update`.

use crate::engine::state::NativeGameState;
use crate::engine::boot_data::CS_BASE;
use crate::recovered::object_update;

pub const OBJ_BASE: usize = 0x4FD0;
pub const OBJ_STRIDE: usize = 0x12;
pub const OBJ_COUNT: usize = 12;

/// The object-AI handler dispatch table lives in the CS window at `cs:[idx*2 + 0x6AA9]`
/// (boot_data's CS_TABLES window 0x6AA0; the table starts at +9 = 0x6AA9). It is NOT in
/// DGROUP — read it from the absolute address `CS_BASE + 0x6AA9 + byte_offset`.
const HANDLER_TABLE: usize = CS_BASE + 0x6AA9;

/// Read the handler code-entry address. `off` is the BYTE offset into the table
/// (already `idx*2`, computed by the caller) — do NOT multiply again.
fn handler_addr(state: &NativeGameState, off: u8) -> u16 {
    state.rw_abs(HANDLER_TABLE + off as usize)
}

/// Run one full object-update pass over the 12 slots (1030:684E..6913).
///
/// Per the disasm and pre2/recovered/object_tick.py: for each non-empty slot apply velocity
/// (NO_X_MOVE aware), run terrain collision when `def[+4] & 8`, advance the animation script
/// (the full +0x138 / 0xA801 region-remap walk), then dispatch the AI handler.
pub fn object_tick(state: &mut NativeGameState) {
    let scale = state.rw(0x6BE2); // [0x6BE2] the global sprite scale level (mem.scale())

    for slot in 0..OBJ_COUNT {
        let si = OBJ_BASE + slot * OBJ_STRIDE;
        let sprite = state.rw(si + 4);
        if sprite == 0xFFFF {
            continue; // [asm 6856-685E] empty slot -> skip
        }

        // [6861-6873] apply velocity (xvel at +0x08, yvel at +0x0A; NO_X_MOVE aware)
        let x = state.rw(si);
        let y = state.rw(si + 2);
        let xvel = state.rw(si + 0x08);
        let yvel = state.rw(si + 0x0A);
        let (nx, ny) = object_update::apply_velocity(x, y, xvel, yvel);
        state.ww(si, nx);
        state.ww(si + 2, ny);

        // [6875-687E] terrain collision gate: def[+4] & 8
        let def_ptr = state.rw(si + 0x06) as usize; // def_ptr at [si+6] (object type definition)
        if def_ptr > 0 && def_ptr < 0x10000 {
            let d4 = state.rb(def_ptr + 4);
            if d4 & 8 != 0 {
                // Full terrain collision (1030:698C)
                object_update::terrain_collision(state, si, def_ptr);
            }
        }

        // [6881-68E6] animation advance (the faithful +0x138 / 0xA801 region-remap walk)
        let anim_ptr = state.rw(si + 0x0C);
        let flip = state.rb(si + 0x09); // flip_byte = xvel's high byte
        let (new_sprite, new_ptr, a340, shake) =
            object_update::advance_animation(state, anim_ptr, sprite, flip, scale);
        state.ww(si + 4, new_sprite);
        state.ww(si + 0x0C, new_ptr);
        state.wb(0xA340, a340); // g.anim_ready = anim.attr_a340

        if shake {
            state.wb(0x6BEA, 9); // [asm 68B1] zoom level 7 arms the screen shake
        }

        // [68EC-68F1] AI handler dispatch: idx = def[+1]; tbl = (idx<<1)&0xFF; tgt = handler_addr(tbl)
        let d1 = state.rb(def_ptr + 1);
        let tbl = (d1 << 1) & 0xFF;
        let tgt = handler_addr(state, tbl);

        // Dispatch to the appropriate AI handler (all 13 recovered)
        object_update::dispatch_handler(state, tgt as u16, si, def_ptr);
    }
}
