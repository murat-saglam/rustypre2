// SPDX-License-Identifier: GPL-3.0-only
//! Native audio bridge — SFX + music from `NativeGameState`.

use crate::engine::state::NativeGameState;

/// Load the SFX sample bank (`SAMPLE.SQZ`) into memory and populate the SFX offset table.
pub fn native_load_sfx_bank(state: &mut NativeGameState, game_root: &std::path::Path) {
    crate::engine::audio::native_load_sfx_bank(state, game_root);
}

/// Play a sound effect (`1030:0282`).
pub fn native_play_sfx(state: &mut NativeGameState, dl: u8) {
    let x = crate::engine::audio::player_sfx_x(state);
    crate::engine::audio::native_play_sfx(state, dl, Some(x));
}
