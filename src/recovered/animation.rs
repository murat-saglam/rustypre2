// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 animated-tile cycle state machine (`1030:367D..36A6`).

pub const ANIM_END: u16 = 0x6988;
pub const FAST_THRESHOLD: u16 = 0x14;

/// Advance one redraw's animated-tile cycle step (`1030:367D..36A6`).
/// Returns `(new_frame_ptr, new_throttle, advanced)`.
pub fn advance_animation(frame_ptr: u16, throttle: u8, active: bool, speed: u16) -> (u16, u8, bool) {
    if !active {
        return (frame_ptr, throttle, false);
    }
    let mask: u8 = if speed >= FAST_THRESHOLD { 1 } else { 3 };
    let new_throttle = throttle.wrapping_add(1);
    if new_throttle & mask != 0 {
        return (frame_ptr, new_throttle, false);
    }
    let hi = (((frame_ptr >> 8) + 1) & 0xFF) << 8;
    let lo = frame_ptr & 0xFF;
    let mut ax = hi | lo;
    if ax == ANIM_END {
        let new_hi = ((ax >> 8).wrapping_sub(3)) & 0xFF;
        ax = (new_hi << 8) | (ax & 0xFF);
    }
    (ax & 0xFFFF, new_throttle, true)
}
