// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 level-password generator and cheat-code validator (`1030:932F` / `1030:9A3A`).

pub const PASSWORD_XOR: u16 = 0x55A3;
pub const DEFAULT_SEED: u16 = 0x20;
pub const DEFAULT_ROT: u8 = 3;
pub const LEVELS_PER_MODE: usize = 10;
const MAX_VALID_INDEX: usize = 0x12;

/// Rotate-left 16-bit value by `c` bits.
#[inline]
fn rol16(v: u16, c: u8) -> u16 {
    let c = c & 15;
    if c == 0 { return v; }
    v.rotate_left(c as u32)
}

/// Compute the 16-bit password value for a level warp index.
pub fn level_code(index: u16, seed: u16, rot: u8) -> u16 {
    let val = ((index ^ PASSWORD_XOR) as u32 * seed as u32) & 0xFFFF;
    rol16(val as u16, rot)
}

/// Compute the one-time BIOS-ROM checksum seed (`1030:932F`).
/// Returns `0x20` when option ROMs and BIOS date bytes are zeroed.
pub fn bios_seed(mem: &[u8]) -> u16 {
    let mut dl: u8 = 0;
    let mut dh: u8 = 0;

    for si in 0xFFF0..0x10000 {
        let b = mem[0xF0000 + si];
        let s = dl as u16 + b as u16;
        let cf = (s >> 8) as u8;
        dl = s as u8;
        dh = dh.wrapping_sub(b).wrapping_sub(cf);
    }

    let mut ax: u16 = 0xC000;
    while (ax >> 8) < 0xF0 {
        let base = (ax as usize) << 4;
        let sig = mem[base] as u16 | ((mem[base + 1] as u16) << 8);
        if sig == 0xAA55 {
            for si in 0..0x80 {
                let b = mem[base + si];
                let s = dl as u16 + b as u16;
                dl = s as u8;
                dh ^= b;
            }
            let bh = mem[base + 2];
            ax = ax.wrapping_add(((bh as u16) << 8) >> 3);
        } else {
            ax = ax.wrapping_add(0x400);
        }
    }

    let dx = ((dh as u16) << 8) | dl as u16;
    if dx != 0 { dx } else { 0x20 }
}

/// Validate an entered 16-bit `ENTER CODE` value.
/// Returns `Some((level_0based, expert))` if valid.
pub fn validate_code(entered: u16, seed: u16, rot: u8) -> Option<(usize, bool)> {
    for index in 0..=MAX_VALID_INDEX {
        if level_code(index as u16, seed, rot) == entered {
            let expert = index >= LEVELS_PER_MODE;
            let level = if expert { index - LEVELS_PER_MODE } else { index };
            return Some((level, expert));
        }
    }
    None
}

const CHEAT_HASH: (u16, u16, u16) = (0x36C8, 0x8BD1, 0x8E71);

fn rol(v: u32, c: u32, bits: u32) -> u32 {
    let c = c % bits;
    let m = (1u32 << bits) - 1;
    let v = v & m;
    if c == 0 { v } else { ((v << c) | (v >> (bits - c))) & m }
}

/// Compute the obfuscated hash of the three prior `ENTER CODE` groups (`1030:9A3A..9A52`).
pub fn password_cheat_hash(di: u16, si: u16, bp: u16) -> (u16, u16, u16) {
    let mut ax = rol(di as u32, 2, 16) as u16;
    ax = ((ax & 0xFF) as u16 * ((ax >> 8) & 0xFF) as u16) & 0xFFFF;
    ax ^= si;
    let prod = (ax as u32 * bp as u32) & 0xFFFFFFFF;
    let ax = prod as u16;
    let dx = (prod >> 16) as u16;
    let mut bx = ax.wrapping_add(si);
    bx = rol(bx as u32, 15, 16) as u16;
    let bl = (rol((bx & 0xFF) as u32, 1, 8) as u8) ^ ((bx >> 8) as u8);
    bx = (((bx >> 8) as u16) << 8) | (bl as u16 & 0xFF);
    bx ^= bp;
    (ax, bx, dx)
}

/// Return true if the three prior groups match the level-warp cheat sequence.
pub fn is_cheat_sequence(di: u16, si: u16, bp: u16) -> bool {
    password_cheat_hash(di, si, bp) == CHEAT_HASH
}

/// Map a warp index to `(level_0based, expert)`.
pub fn warp_index_to_level(index: usize) -> (usize, bool) {
    let expert = index >= LEVELS_PER_MODE;
    let level = if expert { index - LEVELS_PER_MODE } else { index };
    (level, expert)
}
