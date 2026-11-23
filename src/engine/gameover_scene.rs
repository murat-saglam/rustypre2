// SPDX-License-Identifier: GPL-3.0-only
//! The GAME OVER scene — recovered native driver for [asm 9B23..9DFB] (the 5063 game-over tail).
//!
//! After the death-bounce, the VM shows the GAMEOVER.SQZ diorama with the falling GAME-OVER
//! letters, the crying caveman + friends, and three circling birds, scrolling down to the
//! tableau, until the fire key or a ~9 s timeout, then DAC-fades out. The RENDER side is
//! already recovered (bridge/gameover_scene build_gameover_scene = diorama background +
//! object/HUD overlays, the hybrid faithful renderer's path); this module recovers the
//! STATE side:
//!
//! * setup [9B35..9C5F]: camera reset, the 16-colour palette at DGROUP 0xAFE8, free every
//!   object slot, spawn the 8 letters (ids from the [0xB018] table + 0xB0, staggered X,
//!   bounce seed from the 39DF RNG), the 4 tableau characters, and the 3 bird records
//!   (phase 0/0x55/0xAA + anim scripts 0xB020/32/54);
//! * the per-frame tick [9CC0..9DD9]: scroll [0x6BC4] to 0xB9, the crying-caveman anim cycle
//!   (0x68..0x6D every 4th frame), the letters' bounce oscillator, the birds' circular orbit
//!   (sin/cos tables DS:0x6F90/0x7090, phase +2, H-flip from the Y sign) + Y-sort + the
//!   near-side handoff into the render slots;
//! * the loop protocol [9C62..9C86]: tick + render + input, exit on fire or [0x27F0] >= 0x276,
//!   9286 fade.
//!
//! [0x2879]!=0 (demo playback) skips the whole scene, exactly as the ASM's 9B2C gate.

use crate::engine::state::{NativeGameState, DS_BASE};
use crate::engine::dgroup_offsets::*;
use crate::views::tables::Tables;
use crate::recovered::prng;

/// DGROUP paragraph base
const DS: usize = DS_BASE; // 0x1A0F0

/// [asm 9BA3] 8 bytes: sprite id - 0xB0 per GAME/OVER letter
const LETTER_TABLE: usize = 0xB018;
/// [asm 9C35] the 3 bird records
const BIRD_BASE: usize = 0x5138;

/// Convert a byte to signed i8.
#[inline]
fn s8(v: u16) -> i16 {
    let v = (v & 0xFF) as u8;
    v as i8 as i16
}

/// Convert a word to signed i16.
#[inline]
fn s16(v: u16) -> i16 {
    v as i16
}

/// [asm 9B35..9C5F] Build the scene state (camera, slots, letters, tableau, birds).
/// The GAMEOVER.SQZ image + palette are the render/runner side (bridge gameover_background
/// + the 0xAFE8 DAC load).
pub fn native_gameover_setup(state: &mut NativeGameState) {
    // [9B35-9B3B] camera reset
    state.ww(0x2DE4, 0);  // cam_col_word
    state.ww(0x2DE6, 0);  // cam_row_word
    state.ww(0x6BF8, 0);   // row_factor
    // [9B3E] scroll = top
    state.wb(0x6BC4, 0);   // fine_scroll

    // [9B8C-9B9B] free every object slot (0x74 slots, stride 0x12, [di+4] = 0xFFFF)
    for i in 0..0x74usize {
        state.ww(RENDER_SLOTS_BASE + i * 0x12 + 4, 0xFFFF);
    }

    // --- the 8 GAME/OVER letters [9B9D-9BF1] ---
    // ids [0xB018+i]+0xB0, X staggered 0x18 (+0x30 gap after 4), Y=0xE0,
    // bounce seed [di+0xE] = rand&7 (forced nonzero for the first 4)
    let mut di = PLAYER_SLOT;
    let mut x: u16 = 0x2C;
    for i in 0..8usize {
        if i == 4 {
            // [9BD1] the GAME|OVER word gap
            x = x.wrapping_add(0x30);
        }
        let letter_id = state.rb(LETTER_TABLE + i);
        state.ww(di + 4, (letter_id as u16).wrapping_add(0xB0));
        state.ww(di, x);
        state.ww(di + 2, 0xE0);

        // [9BBA/9BE2] 39DF & 7 — advance the RNG
        let a = state.rb(0x2CEC);
        let b = state.rb(0x2CED);
        let c = state.rb(0x2CEE);
        let d = state.rw(0x2CEF);
        let (na, nb, nc, nd, ret) = prng::rng_lcg(a, b, c, d);
        state.wb(0x2CEC, na);
        state.wb(0x2CED, nb);
        state.wb(0x2CEE, nc);
        state.ww(0x2CEF, nd);
        let mut r = (ret & 7) as u16;
        if i < 4 && r == 0 {
            // [9BC0-9BC2] first word: nonzero seed
            r = 1;
        }
        state.ww(di + 0xE, r);
        x = x.wrapping_add(0x18);
        di += 0x12;
    }

    // --- the 4 tableau characters [9BF3-9C34] (below the fold; the scroll reveals them) ---
    // [9BF3] -> 0x5060
    di += 0xB4;
    let tableau = [(0x96u16, 0x135u16, 0x64u16), (0x97, 0x108, 0x1C4), (0x96, 0x121, 0x68), (0x96, 0x119, 0x62)];
    for &(tx, ty, sid) in &tableau {
        state.ww(di, tx);
        state.ww(di + 2, ty);
        state.ww(di + 4, sid);
        di += 0x12;
    }

    // --- the 3 circling birds [9C35-9C5B]: phase + anim-script ptr (X/Y/id computed per tick) ---
    let birds = [(0u8, 0xB020u16), (0x55, 0xB032), (0xAA, 0xB054)];
    for (k, &(phase, script)) in birds.iter().enumerate() {
        let bird_off = BIRD_BASE + k * 0x12;
        // phase = _U8(8) at bird_off + 8
        state.wb(bird_off + 8, phase);
        // script_ptr = _U16(0xC) at bird_off + 0xC
        state.ww(bird_off + 0xC, script);
    }

    // [9C5C] reset the idle/timeout counter
    state.ww(0x27F0, 0);  // idle_clock
}

/// [asm 9CC0..9DD9] One scene frame: scroll, caveman-cry anim, letter bounce, bird orbits +
/// sort + near-side handoff. Transcribed instruction-for-instruction.
pub fn native_gameover_tick(state: &mut NativeGameState) {
    // [9CC6-9CCD] scroll down to the tableau
    let fine_scroll = state.rb(0x6BC4);
    if fine_scroll < 0xB9 {
        state.wb(0x6BC4, fine_scroll + 1);
    }

    // [9CD1-9CD6] every 4th frame
    let frame_stamp = state.rw(0x6BD5);
    if (frame_stamp & 3) == 0 {
        // [9CD8] the crying caveman's sprite id
        let ax = state.rw(CRY_CAVEMAN_SPRITE);
        // [9CDB-9CDE] and ah,0x1f ; inc
        let ax = ((ax & 0x1FFF) + 1) & 0xFFFF;
        // [9CDF-9CE4] cycle 0x68..0x6D
        let ax = if ax >= 0x6E { 0x68 } else { ax };
        state.ww(CRY_CAVEMAN_SPRITE, ax);
    }

    // [9CEA-9D04] the letters' bounce oscillator (8 letters at PLAYER_SLOT, stride 0x12)
    let mut si = PLAYER_SLOT;
    for _ in 0..8 {
        // [9CF0-9CF3] vel += 1
        let mut ax = state.rw(si + 0xE).wrapping_add(1);
        // [9CF4-9CF6] cmp al,8 ; jl
        if s8(ax) >= 8 {
            // [9CF8-9CFA] neg ; inc
            ax = (0u16.wrapping_sub(ax)).wrapping_add(1);
        }
        state.ww(si + 0xE, ax);
        // [9CFE] Y += vel
        let y = state.rw(si + 2).wrapping_add(s16(ax) as u16);
        state.ww(si + 2, y);
        si += 0x12;
    }

    // [9D06-9D95] the bird orbits (3 birds at BIRD_BASE, stride 0x12)
    let mut si = BIRD_BASE;
    for _ in 0..3 {
        // [9D0C] free the paired render slot (at si - 0x164)
        state.ww(si.wrapping_sub(0x164), 0xFFFF);

        // phase = _U8(8) at si + 8
        let phase = state.rb(si + 8) as usize;

        // [9D12-9D19] xlatb + cbw (0x6F90 = Tables.cos)
        let cos_val = Tables::cos().get(phase);
        let sin = s8(cos_val as u16);

        // [9D1A-9D3A] X = 0x96 + sin*0x41/64
        let x = (0x96u16).wrapping_add(((sin as i32 * 0x41) >> 6) as u16);
        state.ww(si, x);

        // [9D3C-9D43] (0x7090 = Tables.sin)
        let sin_val = Tables::sin().get(phase);
        let cos = s8(sin_val as u16);

        // [9D44-9D5F] the Y orbit component
        let v = ((cos as i32 * 0x0A) >> 6) as i16;
        state.ww(si + 6, v as u16);

        // [9D64-9D67] Y = v + 0x135
        let y = (v as i32 + 0x135) as u16;
        state.ww(si + 2, y);

        // [9D6A] phase advance
        let new_phase = (phase + 2) & 0xFF;
        state.wb(si + 8, new_phase as u8);

        // [9D6E] the anim-script advance
        let mut bx = state.rw(si + 0xC) as usize;
        let mut ax = state.rw(bx);
        // [9D73-9D79] loop marker: rewind
        while ax & 0x8000 != 0 {
            bx = bx.wrapping_add(s16(ax) as usize);
            ax = state.rw(bx);
        }
        // [9D7B]
        ax = ax.wrapping_add(0x138);
        // [9D7E-9D85] H-flip on the far side (ah |= [si+6] & 0x8000)
        let flip = state.rw(si + 6);
        if flip & 0x8000 != 0 {
            ax |= 0x8000;
        }
        // [9D87]
        state.ww(si + 4, ax);
        // [9D8A-9D8C]
        state.ww(si + 0xC, (bx + 2) as u16);

        si += 0x12;
    }

    // [9D98-9DB1] bubble-sort the 3 bird records DESCENDING by Y (swap + restart on any inversion)
    let mut swapped = true;
    while swapped {
        swapped = false;
        for k in 0..2usize {
            let a = BIRD_BASE + k * 0x12;
            let b = a + 0x12;
            let y_a = s16(state.rw(a + 2));
            let y_b = s16(state.rw(b + 2));
            // [9DA4-9DA7] jl skips the swap (swap if y_b >= y_a)
            if y_b >= y_a {
                // [9DDA] record swap — swap the 18-byte records at DS+a and DS+b
                let pa = DS + a;
                let pb = DS + b;
                let rec_a: [u8; 0x12] = state.data[pa..pa + 0x12].try_into().unwrap();
                let rec_b: [u8; 0x12] = state.data[pb..pb + 0x12].try_into().unwrap();
                state.data[pa..pa + 0x12].copy_from_slice(&rec_b);
                state.data[pb..pb + 0x12].copy_from_slice(&rec_a);
                swapped = true;
                break;
            }
        }
    }

    // [9DB3-9DD1] the FIRST TWO bird slots: a bird on the near side (Y >= 0x135) moves to the
    // render slots at 0x4FD0/0x4FE2 (drawn earlier = behind the tableau) and its 0x5138 record
    // is freed for this frame
    for k in 0..2usize {
        let si = BIRD_BASE + k * 0x12;
        let di = BIRD_PAIR_SLOTS + k * 0x12;
        let y = s16(state.rw(si + 2));
        // [9DBC-9DC1]
        if y >= 0x135 {
            // [9DF0] copy the record
            let ps = DS + si;
            let pd = DS + di;
            let rec: [u8; 0x12] = state.data[ps..ps + 0x12].try_into().unwrap();
            state.data[pd..pd + 0x12].copy_from_slice(&rec);
            // [9DC6] free the original
            state.ww(si + 4, 0xFFFF);
        }
    }
}
