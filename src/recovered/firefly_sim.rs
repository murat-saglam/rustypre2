// SPDX-License-Identifier: GPL-3.0-only
//! Firefly swarm simulation (`1030:54AB`).
//!
//! Advances the 20-slot persistent firefly swarm (`[0x6EA9]`, stride 8) toward the target
//! coordinates (`[0x4F1C]` / `[0x4F1E]`) using the shared PRNG generators `26CF` (`[0x28C1]`)
//! and `39DF` (`[0x2CEF]` / `[0x2CEC..0x2CEE]`).

use crate::engine::state::NativeGameState;

const NUM_SLOTS: usize = 20;
const SLOT: usize = 8;

/// [0x6EA9] — the 20-slot firefly swarm array (stride 8).
const SLOTS_BASE: usize = 0x6EA9;

/// [0x28C1] — RNG-A (26CF): 16-bit LCG seed.
const RNG_A: usize = 0x28C1;

/// [0x2CEF] — RNG-B (39DF): the 4-byte generator word.
const RNG_B_WORD: usize = 0x2CEF;
/// [0x2CEC] — RNG-B byte A.
const RNG_B_CEC: usize = 0x2CEC;
/// [0x2CED] — RNG-B byte B (the returned value).
const RNG_B_CED: usize = 0x2CED;
/// [0x2CEE] — RNG-B byte C.
const RNG_B_CEE: usize = 0x2CEE;

/// [0x4F1C] — target X (the player slot word), signed.
const TARGET_X: usize = 0x4F1C;
/// [0x4F1E] — target Y (the player Y word), signed.
const TARGET_Y: usize = 0x4F1E;

/// [0x6BD5] — the free-running frame counter (gate for the flags decrement).
const FRAME_GATE: usize = 0x6BD5;

/// [0x6BC0] — scratch byte A (vx candidate sign).
const SCRATCH_A: usize = 0x6BC0;
/// [0x6BC1] — scratch byte B (vy candidate sign).
const SCRATCH_B: usize = 0x6BC1;

/// Sign-extend a byte to i32.
#[inline]
fn s8(v: i32) -> i32 {
    let v = v & 0xFF;
    if v & 0x80 != 0 { v - 0x100 } else { v }
}

/// Sign-extend a 16-bit word to i32.
#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

/// 1030:26CF — advance the rotate-generator (RNG-A).
///
/// `s = ror((s + 0x9248), 3)`, returns the new state's low byte.
/// Writes the new state back to [0x28C1].
fn rng_a(state: &mut NativeGameState) -> u8 {
    let ax = state.rw(RNG_A).wrapping_add(0x9248);
    let ax = ax.rotate_right(3); // ror ax, 1 × 3
    state.ww(RNG_A, ax);
    (ax & 0xFF) as u8
}

/// 1030:39DF — advance the four-byte mixing generator (RNG-B).
///
/// `word = word + [0x2CEC]`;
/// `[0x2CEC] += 3 + (word >> 8)`;
/// `[0x2CED] = (([0x2CED] + [0x2CEE]) * 2 + [0x2CEC])`;
/// `[0x2CEE] ^= [0x2CEC] ^ [0x2CED]`;
/// Returns [0x2CED].
/// Writes all four bytes/word back to state.
fn rng_b(state: &mut NativeGameState) -> u8 {
    let mut word = state.rw(RNG_B_WORD);
    let mut cec = state.rb(RNG_B_CEC);
    let mut ced = state.rb(RNG_B_CED);
    let mut cee = state.rb(RNG_B_CEE);

    word = word.wrapping_add(cec as u16); // dx += [0x2CEC]
    let dh = (word >> 8) as u8;
    cec = cec.wrapping_add(3).wrapping_add(dh); // [0x2CEC] += 3 + dh
    ced = ced.wrapping_add(cee); // ced + cee
    ced = ced.wrapping_add(ced); // (ced + cee) * 2
    ced = ced.wrapping_add(cec); // + cec  => [0x2CED]
    cee = cee ^ cec ^ ced; // [0x2CEE] ^= cec ^ ced

    state.ww(RNG_B_WORD, word);
    state.wb(RNG_B_CEC, cec);
    state.wb(RNG_B_CED, ced);
    state.wb(RNG_B_CEE, cee);

    ced // returns [0x2CED]
}

/// Per-slot update [asm 54C0-5570].
///
/// Skip if dead (first word == 0x55AA). Decrement the timer; when it underflows (high bit set),
/// recompute a new timer `((rng_a & 7) + 3)` and a new velocity aimed at the target point:
/// two `rng_b` draws set the vx/vy candidate magnitudes (& 0xF), one `rng_a` sets the final vx
/// (& 0x1F), and the vy is `min(0x20, abs(vy_cand)*8 / abs(vx_cand))`. Signs are chosen toward
/// the target and flipped by `flags & 7`. Then move: `x += s8(vx)` (undone on signed overflow),
/// `y += s8(vy)`.
fn update_slot(state: &mut NativeGameState, base: usize) {
    let x = state.rw(base); // s[base] | (s[base+1] << 8)
    if x == 0x55AA {
        // dead slot [asm 54C2]
        return;
    }

    let timer = state.rb(base + 6).wrapping_sub(1); // dec [si+6]
    state.wb(base + 6, timer);

    if timer & 0x80 != 0 {
        // js -> underflow -> recompute [asm 54D2]
        // new timer = (rng_a & 7) + 3
        let new_timer = (rng_a(state) & 7).wrapping_add(3);
        state.wb(base + 6, new_timer);
        let flags7 = state.rb(base + 7) & 7;

        // vx candidate aimed at target_x
        let mut al = rng_b(state) & 0xF;
        let dx = s16(x as i32) >> 3;
        let target_x = s16(state.rw(TARGET_X) as i32);
        if !(target_x >= dx) {
            // cmp [0x4F1C],dx ; jge skip-neg
            al = al.wrapping_neg();
        }
        if flags7 != 0 {
            // test [si+7],7 ; jne -> neg
            al = al.wrapping_neg();
        }
        state.wb(SCRATCH_A, al);

        // vy candidate aimed at target_y
        let mut al = rng_b(state) & 0xF;
        let y = state.rw(base + 2);
        let dy = (s16(y as i32) >> 3) + 0x20;
        let target_y = s16(state.rw(TARGET_Y) as i32);
        if !(target_y >= dy) {
            al = al.wrapping_neg();
        }
        if flags7 != 0 {
            al = al.wrapping_neg();
        }
        state.wb(SCRATCH_B, al);

        let scratch_a = state.rb(SCRATCH_A);
        let scratch_b = state.rb(SCRATCH_B);

        // vx final = (rng_a & 0x1F), signed like scratch[0] [asm 5518]
        let mut al = rng_a(state) & 0x1F;
        if s8(scratch_a as i32) < 0 {
            // cmp [0x6BC0],0 ; jge skip
            al = al.wrapping_neg();
        }
        state.wb(base + 4, al);

        // vy final [asm 5529]
        let mut al: u8;
        if scratch_a == 0 {
            al = 2;
        } else {
            let mut bl = scratch_a as i32;
            if bl & 0x80 != 0 {
                bl = (-bl) & 0xFF; // abs(scratch[0])
            }
            let mut ax = s8(scratch_b as i32);
            if ax < 0 {
                ax = -ax; // abs(vy candidate)
            }
            let ax = (ax << 3) & 0xFFFF; // *8
            al = ((ax / bl) & 0xFF) as u8; // div bl -> AL quotient
        }
        if al >= 0x20 {
            // cmp al,0x20 ; jb skip -> min(al,0x20)
            al = 0x20;
        }
        if s8(scratch_b as i32) < 0 {
            // cmp [0x6BC1],0 ; jge skip
            al = al.wrapping_neg();
        }
        state.wb(base + 5, al);

        // flags decrement, gated by the global frame counter [asm 5562]
        let frame_gate = state.rb(FRAME_GATE);
        let flags = state.rb(base + 7);
        if frame_gate & 0xF == 0 && flags & 7 != 0 {
            state.wb(base + 7, flags.wrapping_sub(1));
        }
    }

    // move [asm 5573]: x += s8(vx) with signed-overflow undo
    let vx = state.rb(base + 4);
    let ax = (s8(vx as i32) & 0xFFFF) as u16;
    let res = x.wrapping_add(ax);
    let of = ((x ^ res) & (ax ^ res) & 0x8000) != 0;
    let nx = if of { x } else { res };
    state.ww(base, nx);

    // y += s8(vy)
    let y = state.rw(base + 2);
    let vy = state.rb(base + 5);
    let ny = y.wrapping_add((s8(vy as i32) & 0xFFFF) as u16);
    state.ww(base + 2, ny);
}

/// 1030:54AB — Advance the whole swarm one frame (the ANIMATION pass).
///
/// Iterates all 20 slots at [0x6EA9]: decrement each timer, on underflow recompute
/// (timer=(rng_a&7)+3, velocity aimed at target [0x4F1C]/[0x4F1E] via 2× rng_b + 1× rng_a,
/// vy=min(0x20,abs(vy)*8/abs(vx))) then move x+=s8(vx) (signed-overflow undo) / y+=s8(vy).
/// Drives the two SHARED RNGs 26CF ([0x28C1] LCG) + 39DF ([0x2CEF/0x2CEC/0x2CED/0x2CEE])
/// byte-exact, plus the [0x6BC0]/[0x6BC1] scratch.
pub fn step_fireflies(state: &mut NativeGameState) {
    for i in 0..NUM_SLOTS {
        let base = SLOTS_BASE + i * SLOT;
        update_slot(state, base);
    }
}
