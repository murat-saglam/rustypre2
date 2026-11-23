// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 secondary-entity update pass — effects, particles, projectiles, debris.
//!
//! Four per-frame slot-array walkers operating on NativeGameState.

use crate::engine::state::NativeGameState;

const DS_BASE: usize = 0x1A0F << 4;
const STRIDE: usize = 0x12;
const DEAD: u16 = 0xFFFF;

#[inline]
fn s16(v: i32) -> i32 { let v = v & 0xFFFF; if v & 0x8000 != 0 { v - 0x10000 } else { v } }
#[inline]
fn neg16(v: u16) -> u16 { ((-s16(v as i32)) & 0xFFFF) as u16 }
#[inline]
fn sar16(v: u16, n: u32) -> i32 { (s16(v as i32) >> n) & 0xFFFF }

// ── Debris pool (16 slots @ 0x5450) ────────────────────────────

const DEBRIS_POOL_LO: usize = 0x5450;
const DEBRIS_POOL_N: usize = 0x10;

/// 60DF: tick the 16-slot debris/effect pool.
pub fn tick_debris_pool(state: &mut NativeGameState) {
    let mut b = DEBRIS_POOL_LO;
    for _ in 0..DEBRIS_POOL_N {
        let sprite = state.rw(b + 4);
        if sprite != DEAD {
            // dec [+2] timer
            state.ww(b + 2, state.rw(b + 2).wrapping_sub(1));
            // dec [+0xC] lifetime
            let life = state.rw(b + 0xC).wrapping_sub(1);
            state.ww(b + 0xC, life);
            if life == 0 {
                state.ww(b + 4, DEAD); // free
            }
        }
        b += STRIDE;
    }
}

// ── Popup/score ring (5 slots via [0x6BBE]) ─────────────────────

const POPUP_RING_LO: usize = 0x4F76;
const POPUP_RING_HI: usize = 0x4FBE;
const POPUP_RING_N: usize = 5;
const POPUP_ANIM_END: u16 = 0x3A;

/// 581E: tick the 5-slot popup/score ring.
pub fn tick_popup_ring(state: &mut NativeGameState) {
    let mut si = state.rw(0x6BBE) as usize;
    for _ in 0..POPUP_RING_N {
        let ax = state.rw(si + 4) & 0x1FFF;
        state.ww(si + 2, state.rw(si + 2).wrapping_sub(1));
        let new_anim = ax.wrapping_add(1);
        if new_anim < POPUP_ANIM_END {
            state.ww(si + 4, new_anim);
        } else {
            state.ww(si + 4, DEAD);
        }
        si = si.wrapping_sub(STRIDE);
        if si < POPUP_RING_LO {
            si = POPUP_RING_HI;
        }
    }
}

// ── Particle pool (32 slots @ 0x50A8) ──────────────────────────

const PARTICLE_LO: usize = 0x50A8;
const PARTICLE_N: usize = 0x20;
const GRAVITY: i32 = 9;
const YVEL_CAP: i32 = 0x100;
const X_MAX: u16 = 0x1000;
const LIFE_CLAMP: u16 = 0x32;
const ANIM_WRAP_AT: u16 = 0x49;
const ANIM_WRAP_TO: u16 = 0x46;

const ID_LIFE_CLAMP: [u16; 4] = [0x136, 0x134, 0x12C, 0x132];
const ID_SKIP: u16 = 0xE5;

fn read_tile(state: &NativeGameState, off: u16) -> u8 {
    let map_seg = state.rw(0x2DDA) as usize;
    let addr = ((map_seg << 4) + (off as usize & 0xFFFF)) & 0xFFFFF;
    if addr < state.data.len() { state.data[addr] } else { 0 }
}

fn floor_props(state: &NativeGameState, tile: u8) -> u8 {
    state.data[(DS_BASE + 0x7F5E + tile as usize) & 0xFFFFF]
}

fn ceil_props(state: &NativeGameState, tile: u8) -> u8 {
    state.data[(DS_BASE + 0x7E5E + tile as usize) & 0xFFFFF]
}

/// 60FE: tick the 32-slot physics-particle pool.
pub fn tick_particles(state: &mut NativeGameState) {
    let freeze = state.rb(0x6BD5) & 1 != 0;
    let mut b = PARTICLE_LO;
    for _ in 0..PARTICLE_N {
        let sprite = state.rw(b + 4);
        if sprite == DEAD { b += STRIDE; continue; }

        let life = state.rw(b + 0xC).wrapping_sub(1);
        state.ww(b + 0xC, life);
        let sl = s16(life as i32);

        if sl < 0 {
            // expired
            if state.rb(b + 0x11) == 0 {
                state.ww(b + 4, DEAD);
            }
            b += STRIDE;
            continue;
        }
        if sl == 0 {
            state.wb(b + 0x11, 0x0F);
            b += STRIDE;
            continue;
        }

        // Physics (lifetime > 0)
        let mut x = state.rw(b);
        let mut xv = state.rw(b + 6);
        let mut y = state.rw(b + 2);
        let mut yv = state.rw(b + 0xE);
        let idv = sprite;

        x = ((s16(x as i32) + sar16(xv, 4)) & 0xFFFF) as u16;
        if x & 0x8000 != 0 {
            x = 0;
            xv = neg16(xv);
        }
        if x >= X_MAX {
            xv = neg16(xv);
        }

        let ydelta = s16(yv as i32) >> 4;
        y = ((s16(y as i32) + ydelta) & 0xFFFF) as u16;
        let g = (s16(yv as i32) + GRAVITY) & 0xFFFF;
        if s16(g) < YVEL_CAP {
            yv = g as u16;
        }

        let sid = idv & 0x1FFF;
        let mut new_id: Option<u16> = None;

        if ID_LIFE_CLAMP.contains(&sid) {
            if life > LIFE_CLAMP {
                state.ww(b + 0xC, LIFE_CLAMP);
            }
        } else if sid == ID_SKIP {
            // skip
        } else {
            // tile collision
            let col = (s16(x as i32) >> 4) as u16 & 0xFF;
            let row = (s16(y as i32) >> 4) as u16 & 0xFF;
            let bx = (col | (row << 8)) & 0xFFFF;
            if ydelta > 0 {
                if floor_props(state, read_tile(state, bx)) != 0 {
                    yv = (sar16(neg16(yv), 1) & 0xFFFF) as u16;
                    let d: i32 = if s16(xv as i32) >= 0 { 8 } else { -8 };
                    let axv = ((s16(xv as i32) - d) & 0xFFFF) as u16;
                    let facing = state.rb(b + 7);
                    xv = if (((axv >> 8) as u8) ^ facing) & 0xFF == 0 { axv } else { 0 };
                }
            } else {
                if ceil_props(state, read_tile(state, bx.wrapping_sub(0x100))) != 0 {
                    xv = neg16(xv);
                    x = ((s16(x as i32) + sar16(xv, 4)) & 0xFFFF) as u16;
                }
            }
            // sprite animation
            if !freeze && xv != 0 {
                let masked = idv & 0x1FFF;
                if masked < ANIM_WRAP_AT {
                    new_id = Some(idv.wrapping_add(1));
                } else if masked == ANIM_WRAP_AT {
                    new_id = Some(ANIM_WRAP_TO);
                }
            }
        }

        state.ww(b, x);
        state.ww(b + 2, y);
        state.ww(b + 6, xv);
        state.ww(b + 0xE, yv);
        if let Some(nid) = new_id {
            state.ww(b + 4, nid);
        }
        b += STRIDE;
    }
}

// ── Projectile slots (4 @ 0x4F2E) ─────────────────────────────

const PROJECTILE_LO: usize = 0x4F2E;
const PROJECTILE_N: usize = 4;
const PROJ_ALIVE: u8 = 0x20;

/// 6210: tick the 4 thrown-weapon projectile slots.
pub fn tick_projectiles(state: &mut NativeGameState) {
    let mut b = PROJECTILE_LO;
    for _ in 0..PROJECTILE_N {
        let sprite = state.rw(b + 4);
        if sprite == DEAD { b += STRIDE; continue; }
        if state.rb(b + 5) & PROJ_ALIVE == 0 {
            state.ww(b + 4, DEAD); // free
            b += STRIDE;
            continue;
        }

        // Integrate X/Y
        state.ww(b, ((s16(state.rw(b) as i32) + sar16(state.rw(b + 6), 4)) & 0xFFFF) as u16);
        state.ww(b + 2, ((s16(state.rw(b + 2) as i32) + sar16(state.rw(b + 0xE), 4)) & 0xFFFF) as u16);

        // Anim script
        let mut ptr = state.rw(b + 0xC);
        let mut word = state.rw(ptr as usize);
        if word & 0x8000 != 0 {
            ptr = ((ptr as i32 + s16(word as i32)) & 0xFFFF) as u16;
            word = state.rw(ptr as usize);
        }
        state.ww(b + 0xC, ptr.wrapping_add(2));

        let facing = state.rb(b + 7) & 0x80;
        state.ww(b + 4, word | ((facing as u16) << 8));

        // Handler dispatch
        let idx = state.rb(b + 8) as usize;
        let dyv: i32 = match idx {
            0 => 0x20,
            1 => -0x10,
            _ => 0, // unknown — skip
        };
        state.ww(b + 0xE, (state.rw(b + 0xE) as i32 + dyv & 0xFFFF) as u16);

        b += STRIDE;
    }
}
