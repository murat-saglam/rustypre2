// SPDX-License-Identifier: GPL-3.0-only
//! Native host-input adapter — maps host keyboard events into the DGROUP key table (`0x27F4`).

use crate::engine::state::NativeGameState;
use crate::engine::state::DS_BASE;

pub const KEY_TABLE: usize = 0x27F4;
pub const JOYSTICK_CFG: usize = 0x27E4;

/// Establish the keyboard-play input configuration (`[0x27E4] = 0xFF`, joystick absent).
pub fn init_keyboard_input(state: &mut NativeGameState) {
    let off = (DS_BASE + JOYSTICK_CFG) & 0xFFFFF;
    state.data[off] = 0xFF;
}

/// Set the raw key-table flag (`0xFF` = pressed, `0x00` = released) for one PC XT scancode.
pub fn set_key(state: &mut NativeGameState, scancode: u8, down: bool) {
    let off = (DS_BASE + KEY_TABLE + (scancode as usize & 0x7F)) & 0xFFFFF;
    state.data[off] = if down { 0xFF } else { 0 };
}
