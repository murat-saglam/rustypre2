// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 camera-shake apply (1030:4C30).
//!
//! Converts the shake magnitude [0x6BEA] into the renderer-visible vertical offset [0x6BF8].

use crate::engine::state::NativeGameState;

const H_NUDGE: u16 = 3;
const H_NUDGE_SKIP: [u8; 2] = [5, 0x20];

/// Apply camera shake (1030:4C30).
/// Reads [0x6BEA] (magnitude), [0x6BD5] (frame parity), [0x4F27] (anim state).
/// Writes [0x6BF8] (row_factor), [0x6BEA] (magnitude+1 jitter), [0x4F1E] (h_nudge).
pub fn apply_camera_shake(state: &mut NativeGameState) {
    let mag = state.rb(0x6BEA);

    // mag 0 -> leave [0x6BF8] unchanged (no write)
    if mag == 0 { return; }

    // mag 1 -> [0x6BF8] = 0
    if mag == 1 {
        state.ww(0x6BF8, 0);
        return;
    }

    // mag > 1: check frame parity
    let frame = state.rb(0x6BD5);
    if frame & 1 == 0 {
        // EVEN frame -> [0x6BF8] = 0
        state.ww(0x6BF8, 0);
        return;
    }

    // ODD frame: conditional horizontal nudge
    let anim_b = state.rb(0x4F27);
    if !H_NUDGE_SKIP.contains(&anim_b) {
        let y = state.rw(0x4F1E);
        state.ww(0x4F1E, y.wrapping_sub(H_NUDGE));
    }

    // inc [0x6BEA] (+1 jitter, read AFTER the inc)
    let new_mag = mag.wrapping_add(1);
    state.wb(0x6BEA, new_mag);

    // [0x6BF8] = new magnitude
    state.ww(0x6BF8, new_mag as u16);
}
