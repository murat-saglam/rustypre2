// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 per-frame input-source decoder — 1030:0DC1.
//!
//! Produces the six input flags [0x27E8..0x27ED] from keyboard or demo playback.

use crate::engine::state::NativeGameState;

pub const MODE: usize = 0x2879;
pub const DEMO_PTR: usize = 0x287A;
pub const DEMO_BYTE: usize = 0x287C;
pub const DEMO_CNT: usize = 0x287D;
pub const DEMO_FLAG: usize = 0x6BE5;
pub const PENDING_KEY: usize = 0x2874;
pub const DEMO_HDR_LEVEL: usize = 0x83E;
pub const LEVEL: usize = 0x2D8A;
pub const RECORD_LIMIT: u16 = 0x7FC;

const UNPACK_ORDER: [usize; 6] = [0x27ED, 0x27EC, 0x27EA, 0x27EB, 0x27E8, 0x27E9];
const PACK_ORDER: [usize; 6] = [0x27E9, 0x27E8, 0x27EB, 0x27EA, 0x27EC, 0x27ED];

const KBD_SOURCES: [(usize, [usize; 4]); 6] = [
    (0x27EA, [0x283C, 0x2870, 0x281D, 0x2804]),
    (0x27EB, [0x2844, 0x286F, 0x283E, 0x2812]),
    (0x27EC, [0x2841, 0x286D, 0x2842, 0x281B]),
    (0x27ED, [0x283F, 0x286E, 0x281F, 0x281A]),
    (0x27E8, [0x2810, 0x282D, 0x2868, 0x284B]),
    (0x27E9, [0x2840, 0, 0, 0]), // single source
];

const JOYSTICK_CFG: usize = 0x27E4;
const JOYSTICK_DISABLE: usize = 0x27D9;

fn joystick_absent(state: &NativeGameState) -> bool {
    (state.rb(JOYSTICK_CFG) & 0x80) != 0 || state.rb(JOYSTICK_DISABLE) != 0
}

/// Run DC1 over DGROUP; writes the six input flags + demo buffer side effects.
/// This is the live keyboard path (mode 0); demo playback (mode 1) is not needed for play.
pub fn decode_input(state: &mut NativeGameState) {
    let mode = state.rb(MODE);
    let mut ptr = state.rw(DEMO_PTR);

    // Demo-decode head (modes 1 and 2)
    if mode != 0 {
        if state.rb(PENDING_KEY) != 0 {
            state.wb(DEMO_FLAG, 1);
        }
        if ptr != 0 && state.rb(DEMO_CNT) != 0 {
            state.wb(DEMO_CNT, state.rb(DEMO_CNT).wrapping_sub(1));
            let al = state.rb(DEMO_BYTE);
            unpack_flags(state, al);
        } else {
            let entry = state.rw((ptr as usize + 0x3F) & 0xFFFF);
            let lo = (entry & 0xFF) as u8;
            let hi = (entry >> 8) as u8;
            ptr = ptr.wrapping_add(2);
            if entry == 0x55AA {
                state.wb(DEMO_FLAG, 1);
            }
            state.ww(DEMO_PTR, ptr);
            state.wb(DEMO_BYTE, lo);
            state.wb(DEMO_CNT, hi);
            unpack_flags(state, lo);
        }
        if mode == 1 {
            return;
        }
    }

    // Live read (modes 0 and 2)
    if !joystick_absent(state) {
        // Joystick present — not supported in native (gated absent by init_keyboard_input)
        return;
    }

    let mut out = [0u8; 6];
    for (i, &(off, ref srcs)) in KBD_SOURCES.iter().enumerate() {
        let mut v = 0u8;
        for &src in srcs.iter() {
            if src != 0 {
                v |= state.rb(src);
            }
        }
        out[i] = v;
        state.wb(off, v);
    }

    // Record tail: append packed input to demo buffer (RLE) [asm 0F0A]
    if ptr >= RECORD_LIMIT {
        return;
    }
    let mut al = 0u8;
    for (_i, &off) in PACK_ORDER.iter().enumerate() {
        al = (al << 1) | (state.rb(off) & 1);
    }

    if ptr == 0 {
        state.wb(DEMO_HDR_LEVEL, state.rb(LEVEL));
        state.wb((ptr as usize + 0x3F) & 0xFFFF, al);
        state.wb((ptr as usize + 0x40) & 0xFFFF, 0);
        state.ww(DEMO_PTR, ptr.wrapping_add(2));
    } else {
        let prev_entry = state.rw((ptr as usize + 0x3D) & 0xFFFF);
        let prev = (prev_entry & 0xFF) as u8;
        let cnt = (prev_entry >> 8) as u8;
        if prev == al && cnt != 0xFF {
            state.wb((ptr as usize + 0x3E) & 0xFFFF, cnt.wrapping_add(1));
        } else {
            state.wb((ptr as usize + 0x3F) & 0xFFFF, al);
            state.wb((ptr as usize + 0x40) & 0xFFFF, 0);
            state.ww(DEMO_PTR, ptr.wrapping_add(2));
        }
    }
}

fn unpack_flags(state: &mut NativeGameState, mut al: u8) {
    for &off in &UNPACK_ORDER {
        state.wb(off, al & 1);
        al >>= 1;
    }
}
