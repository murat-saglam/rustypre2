// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 point-particle draw (1030:4B8E) — recovered native primitive.
//!
//! The effect system draws short-lived **point particles** (spider-thread bits, fireflies,
//! sparkles): up to 20 slots at `[0x7DE6]` (6 bytes each: X word, Y word, angle byte, speed
//! byte). Per frame the producer spawns particles into the array; `4B8E` advances each by its
//! angle/speed (sin/cos tables `[0x6F90]`/`[0x7090]`), plots ONE white pixel (the bit set in all
//! four planes via the EGA OR write), and then **kills the slot** (`[si]=0xFFFF`) — so the
//! particles are one-shot and gone by the per-frame commit boundary.
//!
//! All functions operate directly on the four EGA plane buffers embedded in the
//! `state.data` address space (`EGA_APERTURE`, stride `EGA_PLANE_STRIDE`).

use crate::engine::state::NativeGameState;

const DS_BASE: usize = 0x1A0F << 4;
pub const PARTICLE_BASE: usize = 0x7DE6;   // [0x7DE6] particle array
pub const PARTICLE_COUNT: usize = 0x14;    // 20 slots (asm 4BA6 bp=0x14)
pub const PARTICLE_STRIDE: usize = 6;      // 6 bytes/slot (asm 4C21 add si,6): X.w Y.w angle.b speed.b
pub const COS_TABLE: usize = 0x6F90;      // [0x6F90] signed cos table (X velocity), indexed by angle
pub const SIN_TABLE: usize = 0x7090;      // [0x7090] signed sin table (Y velocity)
pub const DEAD_SLOT: u16 = 0xFFFF;        // the X-word sentinel 4B8E writes to kill a slot [asm 4C1D]

const VIEW_W: u16 = 0x140;                 // 320 — off-screen X cull (asm 4C00 cmp 0x140)
const VIEW_H: u16 = 0xB0;                  // 176 — off-screen Y cull (asm 4BEB cmp 0xb0)
const ROW: usize = 0x28;                   // bytes per screen row (asm 4BF0 imul/lea)

use crate::engine::vga::{EGA_APERTURE, EGA_PLANE_STRIDE as PLANE_STRIDE};

const CAM_COL: usize = 0x2DE4;            // [0x2DE4] tile camera column
const CAM_ROW: usize = 0x2DE6;            // [0x2DE6] tile camera row
const Y_BIAS: usize = 0x6BC4;             // [0x6BC4] vertical bias subtracted from the particle Y
const PAGE_DRAW: usize = 0x2DD8;          // [0x2DD8] the EGA page 4B8E plots into (add di,[0x2DD8] at 4C12)

/// Sign-extend a byte to i32.
#[inline]
fn s8(v: i32) -> i32 {
    let v = v & 0xFF;
    if v & 0x80 != 0 { v - 0x100 } else { v }
}

/// Advance one particle by its angle/speed [asm 4BB0-4BD2]. Returns `(nx, ny)` (wrapped 16-bit).
///
/// `nx = x + ((s8(cos[angle])>>2) * s8(speed)) >> 4` (all arithmetic shifts), `ny` the sin
/// equivalent.
pub fn advance_particle(state: &NativeGameState, x: u16, y: u16, angle: u8, speed: u8) -> (u16, u16) {
    let cos_val = s8(state.data[(DS_BASE + COS_TABLE + angle as usize) & 0xFFFFF] as i32);
    let sin_val = s8(state.data[(DS_BASE + SIN_TABLE + angle as usize) & 0xFFFFF] as i32);
    let spd = s8(speed as i32);
    let dx = ((cos_val >> 2) * spd) >> 4;                                  // [asm 4BB0-4BC0]
    let dy = ((sin_val >> 2) * spd) >> 4;                                  // [asm 4BC2-4BD2]
    let nx = ((x as i32 + dx) & 0xFFFF) as u16;
    let ny = ((y as i32 + dy) & 0xFFFF) as u16;
    (nx, ny)
}

/// Plot one advanced particle's white pixel if on-screen [asm 4BD5-4C1A].
///
/// OR's bit `0x80>>(sx&7)` into all 4 EGA planes at `page + sy*0x28 + (sx>>3)`.
fn plot_particle(state: &mut NativeGameState, nx: u16, ny: u16, cam_x: u16, cam_y: u16, yb: i32, page: u16) {
    let sy = ((ny as i32 - yb - cam_y as i32) & 0xFFFF) as u16;            // [asm 4BDA-4BE9]
    if sy >= VIEW_H {                                                     // [asm 4BEB jae]
        return;
    }
    let sx = ((nx as i32 - cam_x as i32) & 0xFFFF) as u16;                // [asm 4BF6-4BFE]
    if sx >= VIEW_W {                                                     // [asm 4C00 jae]
        return;
    }
    let off = (page as usize + sy as usize * ROW + (sx >> 3) as usize) & 0xFFFF;  // [asm 4BF0-4C12]
    let bit = 0x80u8 >> (sx & 7);                                         // [asm 4C16]
    for p in 0..4usize {                                                  // [asm 4C1A xchg, OR all planes]
        let addr = EGA_APERTURE + p * PLANE_STRIDE + off;
        if addr < state.data.len() {
            state.data[addr] |= bit;
        }
    }
}

/// Execute the `1030:4B8E` particle pass: advance each active slot, plot its white pixel into
/// the EGA planes, write the advanced Y back to `[slot+2]`, and mark the slot dead (`[slot]=0xFFFF`).
pub fn consume_particles(state: &mut NativeGameState) {
    let cam_col = state.rw(CAM_COL) as i32;
    let cam_row = state.rw(CAM_ROW) as i32;
    let y_bias = state.rb(Y_BIAS) as i32;
    let page = state.rw(PAGE_DRAW);
    let cam_x = ((cam_col << 4) & 0xFFFF) as u16;
    let cam_y = ((cam_row << 4) & 0xFFFF) as u16;
    let yb = s8(y_bias);
    let mut b = PARTICLE_BASE;
    for _ in 0..PARTICLE_COUNT {
        let x = state.rw(b);
        if x != DEAD_SLOT {
            let y = state.rw(b + 2);
            let angle = state.rb(b + 4);
            let speed = state.rb(b + 5);
            let (nx, ny) = advance_particle(state, x, y, angle, speed);
            plot_particle(state, nx, ny, cam_x, cam_y, yb, page);
            // Writeback: [slot+2]=ny (the advanced Y persists) ...
            state.ww(b + 2, ny);
            // ... then kill the slot ([slot]=0xFFFF); the advanced-X write at 4BC0 is
            // overwritten by the 4C1D kill, so only Y persists.
            state.ww(b, DEAD_SLOT);
        }
        b += PARTICLE_STRIDE;
    }
}
