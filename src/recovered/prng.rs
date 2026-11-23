// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 pseudo-random generators — recovered native logic (pure).
//!

pub const RNG_ROR_STATE: usize = 0x28C1;
pub const RNG_LCG_A: usize = 0x2CEC;
pub const RNG_LCG_B: usize = 0x2CED;
pub const RNG_LCG_C: usize = 0x2CEE;
pub const RNG_LCG_D: usize = 0x2CEF;

#[inline]
fn ror16(v: u16, n: u32) -> u16 {
    let n = n & 15;
    if n == 0 { v } else { v.rotate_right(n) }
}

/// 1030:26CF — advance the rotate-generator. Returns the new state.
pub fn rng_ror(state: u16) -> u16 {
    ror16(state.wrapping_add(0x9248), 3)
}

/// 1030:39DF — advance the four-byte mixing generator.
/// Returns (a', b', c', d', ret) where ret = b'.
pub fn rng_lcg(a: u8, b: u8, c: u8, d: u16) -> (u8, u8, u8, u16, u8) {
    let d = d.wrapping_add(a as u16);
    let a = a.wrapping_add(3).wrapping_add((d >> 8) as u8);
    let b = ((b.wrapping_add(c) as u16).wrapping_mul(2) + a as u16) as u8;
    let c = c ^ a ^ b;
    (a, b, c, d, b)
}
