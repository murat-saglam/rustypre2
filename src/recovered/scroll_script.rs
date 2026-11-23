// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 scripted-camera-scroll / LEVELG falling-snow (1030:3922).
//!
//! State half: frame counter + wind accumulate. Render half: snow flake plot.

use crate::engine::state::NativeGameState;
use crate::recovered::prng;

#[inline]
fn s16(v: i32) -> i32 { let v = v & 0xFFFF; if v & 0x8000 != 0 { v - 0x10000 } else { v } }

/// State half (1030:3922): advance frame counter + script entry + wind accumulate/clamp.
pub fn scroll_script_state(state: &mut NativeGameState) {
    let counter = state.rw(0x2DBE).wrapping_add(1); // frame_counter [asm 3922]
    state.ww(0x2DBE, counter);

    let tick = state.rb(0x6BD5); // frame_blink used as tick
    if tick & 3 != 0 { return; }

    // Read current script entry (6 bytes: threshold.w, delta.w, clamp.w, next_threshold.w)
    let script_ptr = state.rw(0x2DBC) as usize; // script_ptr [asm 3930]
    let threshold = state.rw(script_ptr);
    if threshold == 0xFFFF { return; } // end-of-script

    let mut scroll = state.rw(0x6BF6); // wind / friction
    if threshold < counter {
        let delta = state.rw(script_ptr + 2);
        scroll = scroll.wrapping_add(delta);
        let next_threshold = state.rw(script_ptr + 6);
        if next_threshold < counter {
            state.ww(0x2DBC, (script_ptr + 6) as u16); // advance to next entry
        }
    }

    // Clamp to [0, clamp] (signed)
    let clamp = s16(state.rw(script_ptr + 4) as i32);
    let clamped = s16(scroll as i32).max(0).min(clamp);
    state.ww(0x6BF6, clamped as u16);
}

/// Render half (1030:396A): LEVELG falling snow.
/// Steps the flake array + rng and returns `Vec<(page_offset, bit_mask)>` pairs to OR into planes.
pub fn scroll_script_snow(state: &mut NativeGameState) -> Vec<(u16, u8)> {
    let mut plots: Vec<(u16, u8)> = Vec::new();
    let wind = state.rw(0x6BF6); // wind == flake count
    if wind == 0 {
        return plots;
    }

    let cam = state.rw(0x2DE4) as u16; // camera X
    let dx = wind;
    let bp = dx >> 1;

    // Flake array at 0x6CA9 (256-word flake-position array, see ScrollScriptView.flakes)
    let flake_base: usize = 0x6CA9;
    let mut i = 0usize;
    let mut remaining = dx;

    loop {
        let v = state.rw(flake_base + i * 2).wrapping_add(0x4F);
        state.ww(flake_base + i * 2, v);

        let al = (v & 0xFF) as u8;
        let cl = (((al << 1) | (al >> 7)) & 3) as u8;
        let mut bx = v.wrapping_sub(cam) & 0x1FFF;
        if bx >= 0x1B58 {
            bx = bx.wrapping_sub(0x1B58);
            state.ww(flake_base + i * 2, bx);
        }
        i += 1;

        let ax = (0x302u16 << cl) & 0xFFFF;
        plots.push((bx, (ax & 0xFF) as u8));

        if remaining > bp {
            plots.push((bx.wrapping_add(0x28), (ax >> 8) as u8));
        }

        remaining = remaining.wrapping_sub(1);
        if remaining == 0 {
            break;
        }
    }

    // Refresh one flake slot from RNG
    let mut a = state.rb(0x2CEC);
    let mut b = state.rb(0x2CED);
    let mut c = state.rb(0x2CEE);
    let mut d = state.rw(0x2CEF);
    let (na, nb, nc, nd, r1) = prng::rng_lcg(a, b, c, d);
    a = na; b = nb; c = nc; d = nd;
    let (na, nb, nc, nd, r2) = prng::rng_lcg(a, b, c, d);
    a = na; b = nb; c = nc; d = nd;
    let (na, nb, nc, nd, r3) = prng::rng_lcg(a, b, c, d);
    a = na; b = nb; c = nc; d = nd;
    let idx = r1 as usize;
    let val = ((r2 as u16) << 8) | r3 as u16;
    if idx < 0x100 {
        state.ww(flake_base + idx * 2, val);
    }
    state.wb(0x2CEC, a);
    state.wb(0x2CED, b);
    state.wb(0x2CEE, c);
    state.ww(0x2CEF, d);
    plots
}
