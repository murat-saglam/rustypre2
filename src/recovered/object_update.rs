// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 object-update system — recovered AI handlers (1030:684E leaves).
//!
//! Per-type AI handlers + shared leaves (despawn, on_screen, anim seek, dying_state,
//! terrain_collision, spawn_effects, orbit_position).
//!
//! Each handler reads/writes directly from NativeGameState at si/def_ptr offsets
//! using the state.rb/rw/wb/ww API (DS-relative DGROUP access).

use crate::engine::state::NativeGameState;
use crate::recovered::prng::{rng_lcg, rng_ror, RNG_LCG_A, RNG_LCG_B, RNG_LCG_C, RNG_LCG_D, RNG_ROR_STATE};
use std::sync::atomic::{AtomicI32, Ordering};

pub const NO_X_MOVE: u16 = 0xFFFF;
pub const VEL_SHIFT: u32 = 4;
pub const FRAME_BASE: u16 = 0x138;
pub const ID_FLAGS_MASK: u16 = 0x6000;
pub const FLIP_BIT: u16 = 0x8000;
pub const FRAME_MASK: u16 = 0x1FFF;
pub const EMPTY_ID: u16 = 0xFFFF;

pub const FAR_X: i32 = 0x140;
pub const FAR_Y: i32 = 0x12C;

const REMAP_TABLE: u16 = 0xA801;
const EFFECT_LIST: usize = 0x7DE6;
const EFFECT_ENTRIES: usize = 32;

const STATE_KEEP: u8 = 0xFF;
const DRAWN_BIT: u8 = 0x20;
const STATE_FREE_SLOT: u8 = 0x0A;

const ONSCREEN_X_LO: i32 = -2;
const ONSCREEN_X_HI: i32 = 0x16;
const ONSCREEN_Y_LO: i32 = -2;
const ONSCREEN_Y_HI: i32 = 0x0D;

static ACTIVE_ZONE_X: AtomicI32 = AtomicI32::new(0);

// -- signed helpers (8-bit and 16-bit sign-extension helpers) ---------------------------------------- //

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

#[inline]
fn s8(v: i32) -> i32 {
    let v = v & 0xFF;
    if v & 0x80 != 0 { v - 0x100 } else { v }
}

#[inline]
fn abs16(d: u16) -> i32 {
    let d = d as i32;
    if d < 0x8000 { d } else { 0x10000 - d }
}

// -- apply_velocity (1030:6861) ---------------------------------------------------------- //

/// Integrate one object's position by its velocity. Y += sar(yvel,4) always;
/// X += sar(xvel,4) unless xvel == NO_X_MOVE.
pub fn apply_velocity(x: u16, y: u16, xvel: u16, yvel: u16) -> (u16, u16) {
    let new_y = ((s16(y as i32) + (s16(yvel as i32) >> VEL_SHIFT as i32)) & 0xFFFF) as u16;
    let new_x = if xvel == NO_X_MOVE {
        x
    } else {
        ((s16(x as i32) + (s16(xvel as i32) >> VEL_SHIFT as i32)) & 0xFFFF) as u16
    };
    (new_x, new_y)
}

// -- advance_animation (1030:6881) ------------------------------------------------------- //

/// Advance one object's animation script. Returns (new_sprite_id, new_script_ptr, a340_byte, shake).
/// Includes the full 0xA801 region-remap walk for non-zero scale (zoom) levels.
pub fn advance_animation(
    state: &NativeGameState,
    script_ptr: u16,
    old_id: u16,
    flip_byte: u8,
    scale: u16,
) -> (u16, u16, u8, bool) {
    let mut bx = script_ptr;
    let mut raw = 0u16;
    for _ in 0..256 {
        raw = state.rw(bx as usize);
        if raw < 0x8000 {
            break;
        }
        bx = ((bx as i32 + (raw as i32 - 0x10000)) & 0xFFFF) as u16;
    }
    let a340 = (((raw >> 8) & 0xE0) as u8) | (scale as u8);
    let mut frame = ((raw & FRAME_MASK) + FRAME_BASE) & FRAME_MASK;
    let mut advance = true;
    let mut shake = false;

    if scale != 0 {
        shake = scale == 7;
        // 0xA801 region-remap walk: 6-byte entries {lo, hi, remapped_frame}
        let mut di = REMAP_TABLE;
        for _ in 0..256 {
            if frame < state.rw(di as usize) {
                break;
            }
            if frame <= state.rw(di.wrapping_add(2) as usize) {
                frame = state
                    .rw(di.wrapping_add(4) as usize)
                    .wrapping_add(0x35)
                    & FRAME_MASK;
                advance = false;
                break;
            }
            di = di.wrapping_add(6);
        }
    }

    let val = frame | if flip_byte & 0x80 != 0 { FLIP_BIT } else { 0 };
    let new_id = (old_id & ID_FLAGS_MASK) | val;
    let new_ptr = if advance { bx.wrapping_add(2) } else { script_ptr };
    (new_id, new_ptr, a340, shake)
}

// -- DespawnResult + despawn_check (1030:8084 + 7CFF tail) -------------------------------- //

/// The contract of one despawn check: post-values of the four fields it may write.
/// When the object is KEPT the fields equal their inputs (no write).
#[derive(Clone, Copy, Debug)]
pub struct DespawnResult {
    pub sprite_id: u16,
    pub def2: u16,
    pub def4: u8,
    pub def7: u8,
}

/// Pure despawn check — recovers 1030:8084 + 7CFF tail.
/// Returns the post-values of the four written fields (unchanged when kept).
pub fn despawn_check_values(
    obj_x: u16,
    obj_y: u16,
    obj_state: u8,
    flags5: u8,
    old_id: u16,
    player_x: u16,
    player_y: u16,
    def2: u16,
    def4: u8,
    def7: u8,
) -> DespawnResult {
    let keep = DespawnResult {
        sprite_id: old_id,
        def2,
        def4,
        def7,
    };
    if obj_state == STATE_KEEP {
        return keep;
    }
    if flags5 & DRAWN_BIT != 0 {
        return keep;
    }
    let far = abs16(obj_x.wrapping_sub(player_x)) > FAR_X
        || abs16(obj_y.wrapping_sub(player_y)) > FAR_Y;
    if !far {
        return keep;
    }
    let new_def4 = def4 & 0xFB;
    let mut new_def2 = def2;
    if obj_state >= STATE_FREE_SLOT && (new_def4 & 0x02) == 0 {
        new_def2 = EMPTY_ID;
    }
    DespawnResult {
        sprite_id: EMPTY_ID,
        def2: new_def2,
        def4: new_def4,
        def7: 0,
    }
}

/// Read state and apply despawn_check result. Used by most handlers.
fn do_despawn(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let dr = despawn_check_values(
        state.rw(si),
        state.rw(si + 2),
        state.rb(si + 0xE),
        (state.rw(si + 4) >> 8) as u8,
        state.rw(si + 4),
        state.rw(0x4F1C),
        state.rw(0x4F1E),
        state.rw(def_ptr + 2),
        state.rb(def_ptr + 4),
        state.rb(def_ptr + 7),
    );
    state.ww(si + 4, dr.sprite_id);
    state.ww(def_ptr + 2, dr.def2);
    state.wb(def_ptr + 4, dr.def4);
    state.wb(def_ptr + 7, dr.def7);
}

// -- despawn_full (1030:7CFF) ------------------------------------------------------------ //

/// The unconditional full despawn — [def+7]=0, [si+4]=0xFFFF, [def+4]&=0xFB,
/// and free the spawn slot [def+2]=0xFFFF unless [def+4] bit1.
pub fn despawn_full(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    state.wb(def_ptr + 7, 0);
    state.ww(si + 4, EMPTY_ID);
    let d4 = state.rb(def_ptr + 4) & 0xFB;
    state.wb(def_ptr + 4, d4);
    if d4 & 0x02 == 0 {
        state.ww(def_ptr + 2, EMPTY_ID);
    }
}

// -- on_screen_tile (1030:8022) ---------------------------------------------------------- //

/// Is the pixel (x, y) within the visible tile window around the camera?
/// Tile = pixel >> 4 (arithmetic), then signed tile offset from camera.
pub fn on_screen_tile(x: u16, y: u16, cam_x: i32, cam_y: i32) -> bool {
    let mx = ACTIVE_ZONE_X.load(Ordering::Relaxed);
    let tx = s16(((s16(x as i32) >> 4) - cam_x) & 0xFFFF);
    if !(ONSCREEN_X_LO - mx <= tx && tx <= ONSCREEN_X_HI + mx) {
        return false;
    }
    let ty = s16(((s16(y as i32) >> 4) - cam_y) & 0xFFFF);
    ONSCREEN_Y_LO <= ty && ty <= ONSCREEN_Y_HI
}

// -- anim_script_rewind / anim_script_forward (1030:8048 / 8058) ------------------------- //

/// Seek the script pointer BACK to the loop marker: step -2 while the entry is
/// non-negative, stop ON the first negative (back-jump) word.
pub fn anim_script_rewind(state: &NativeGameState, mut ptr: u16) -> u16 {
    for _ in 0..256 {
        ptr = ptr.wrapping_sub(2);
        if state.rw(ptr as usize) >= 0x8000 {
            return ptr;
        }
    }
    ptr
}

/// Seek the script pointer FORWARD past the loop marker: step +2 while the entry
/// is non-negative, then +2 once more past the negative (back-jump) word.
pub fn anim_script_forward(state: &NativeGameState, mut ptr: u16) -> u16 {
    for _ in 0..256 {
        if state.rw(ptr as usize) >= 0x8000 {
            return ptr.wrapping_add(2);
        }
        ptr = ptr.wrapping_add(2);
    }
    ptr
}

// -- saturating_counter (1030:8001) ----------------------------------------------------- //

/// Saturating-increment the timer [def+7] (caps at 0xFF), and report whether
/// ([def+7] >> 2) >= [def+6]. Returns (new_def7, ready).
pub fn saturating_counter(def6: u8, def7: u8) -> (u8, bool) {
    let nd7 = if def7 < 0xFF { def7 + 1 } else { 0xFF };
    let ready = (nd7 >> 2) >= def6;
    (nd7, ready)
}

// -- dying_state (1030:7CDA) ------------------------------------------------------------- //

/// The shared 'dying' state: despawn unless the object is "held" (def4 bit0 set)
/// and either drawn (id bit13) or the player is at/below it, in which case apply
/// gravity (yvel += 0xF capped at 0xF0).
pub fn dying_state(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let d4 = state.rb(def_ptr + 4);
    if d4 & 1 == 0 {
        despawn_full(state, si, def_ptr);
    } else {
        let id = state.rw(si + 4);
        let obj_y = s16(state.rw(si + 2) as i32);
        let player_y = s16(state.rw(0x4F1E) as i32);
        if id & 0x2000 != 0 || player_y >= obj_y {
            let yvel = state.rw(si + 0xA);
            if s16(yvel as i32) < 0xF0 {
                state.ww(si + 0xA, yvel.wrapping_add(0xF));
            }
        } else {
            despawn_full(state, si, def_ptr);
        }
    }
}

// -- orbit_position (1030:7B53) ---------------------------------------------------------- //

/// Position math: X = centreX + ((cos>>2)*radius)>>4, Y likewise with sin.
/// cos_val/sin_val are signed table bytes; radius is a signed byte.
pub fn orbit_position(
    center_x: u16,
    center_y: u16,
    radius: u8,
    cos_val: u8,
    sin_val: u8,
) -> (u16, u16) {
    let r = s8(radius as i32);
    let cos = s8(cos_val as i32);
    let sin = s8(sin_val as i32);
    let x = ((center_x as i32 + (((cos >> 2) * r) >> 4)) & 0xFFFF) as u16;
    let y = ((center_y as i32 + (((sin >> 2) * r) >> 4)) & 0xFFFF) as u16;
    (x, y)
}

// -- spawn_effects (1030:7FD9) ---------------------------------------------------------- //

/// Spawn 3 entries into the secondary effect list (0x7DE6). Each 6-byte entry:
/// [0]=X=def9, [2]=Y=defB-0x18, [4]=arg, [5]=angle=(k+1)*(dl>>2).
pub fn spawn_effects(state: &mut NativeGameState, def9: u16, def_b: u16, arg: u8, dl: u8) {
    let step = (s8(dl as i32) >> 2) as u8;
    let y = def_b.wrapping_sub(0x18);
    let mut ang = step;
    for _ in 0..3 {
        // Find first free slot (X == 0xFFFF)
        let mut found: Option<usize> = None;
        for i in 0..EFFECT_ENTRIES {
            let off = EFFECT_LIST + i * 6;
            if state.rw(off) == 0xFFFF {
                found = Some(off);
                break;
            }
        }
        if let Some(off) = found {
            state.ww(off, def9);
            state.ww(off + 2, y);
            state.wb(off + 4, arg);
            state.wb(off + 5, ang);
        }
        ang = ang.wrapping_add(step);
    }
}

// -- terrain_collision (1030:698C) + slope helper 6A7D ----------------------------------- //

/// Read a byte from the level map at the given tile index.
fn read_map(state: &NativeGameState, idx: usize) -> u8 {
    let map_seg = state.rw(0x2DDA) as usize;
    let addr = ((map_seg << 4) + (idx & 0xFFFF)) & 0xFFFFF;
    state.rb_abs(addr)
}

/// Tile property A: [0x7E5E + tile].
fn prop_a(state: &NativeGameState, tile: u8) -> u8 {
    state.rb(0x7E5E + tile as usize)
}

/// Tile property B: [0x7F5E + tile].
fn prop_b(state: &NativeGameState, tile: u8) -> u8 {
    state.rb(0x7F5E + tile as usize)
}

/// Tile property B via level-map lookup: [0x7F5E + map[tileY*0x100+tileX]].
fn tile_prop(state: &NativeGameState, tile_x: u8, tile_y: u8) -> u8 {
    let map_seg = state.rw(0x2DDA) as usize;
    let idx = ((tile_y as usize) << 8) | (tile_x as usize);
    let tile = state.rb_abs(((map_seg << 4) + idx) & 0xFFFFF);
    state.rb(0x7F5E + tile as usize)
}

/// Slope-height helper (1030:6A7D): sub-tile Y offset for a (possibly sloped) ground tile.
fn surface_offset(state: &NativeGameState, obj_x: u16, tile: u8) -> i32 {
    let s = state.rb(0x8E1D + tile as usize);
    if s & 0x30 == 0 {
        return s8(s as i32); // flat: raw height
    }
    let q = (obj_x & 0xF) / 3;
    let al: u8 = if s & 0x10 != 0 {
        (q as u8).wrapping_add(s & 0xF) // rising
    } else {
        ((-(q as i32) & 0xFF) as u8).wrapping_add(s & 0xF) // falling
    };
    s8(al as i32)
}

/// Landing tail (1030:6A3E): snap Y onto the ground tile's surface and resolve
/// the vertical velocity (stop if [def+4]&0x20, else bounce at -Yvel/2 when fast enough).
fn settle(state: &mut NativeGameState, si: usize, def_ptr: usize, tile: u8) {
    let obj_x = state.rw(si);
    let off = surface_offset(state, obj_x, tile);
    let obj_y = state.rw(si + 2);
    let new_y = ((obj_y & 0xFFF0) as i32 + off) & 0xFFFF;
    state.ww(si + 2, new_y as u16);
    let d4 = state.rb(def_ptr + 4);
    if d4 & 0x20 != 0 {
        state.ww(si + 0xA, 0); // non-bouncing -> stop
    } else {
        let yvel = s16(state.rw(si + 0xA) as i32);
        let neg_yvel = (-yvel) & 0xFFFF;
        let dx = s16(neg_yvel as i32) >> 1; // neg, arithmetic sar 1
        if dx.abs() > 0x20 {
            state.ww(si + 0xA, (dx & 0xFFFF) as u16);
        } else {
            state.ww(si + 0xA, 0);
        }
    }
}

/// Per-object level-map TERRAIN COLLISION (1030:698C). Walker calls it when [def+4]&8.
pub fn terrain_collision(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let ty = (s16(state.rw(si + 2) as i32) >> 4) as u8;
    let tx = (s16(state.rw(si) as i32) >> 4) as u8;
    let mut idx = ((ty as usize) << 8) | (tx as usize);
    let tile_here = read_map(state, idx);
    let tile_above = read_map(state, idx.wrapping_sub(0x100));
    let xv = s16(state.rw(si + 8) as i32);
    let direction = if xv == 0 { 0 } else if xv > 0 { 1 } else { -1 };
    idx = (idx as i32 + direction) as usize & 0xFFFF;
    let tile_ahead = read_map(state, idx.wrapping_sub(0x100));
    let a = prop_a(state, tile_ahead);
    let flags = state.rb(def_ptr + 4);

    if flags & 0x40 != 0 && flags & 0x80 != 0 {
        // climbing
        if a != 0 {
            // wall ahead-above -> back out X
            let xvel = s16(state.rw(si + 8) as i32);
            let new_x = (s16(state.rw(si) as i32) - (xvel >> 4)) & 0xFFFF;
            state.ww(si, new_x as u16);
        } else {
            // reached the top -> dismount
            let new_ptr = anim_script_rewind(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
            state.ww(si + 0xA, 0); // yvel = 0
            state.wb(def_ptr + 4, flags & 0x7F); // clear 0x80
            let new_y = (s16(state.rw(si + 2) as i32) - 0x10) & 0xFFFF;
            state.ww(si + 2, new_y as u16);
        }
        return;
    }

    if a != 0 {
        // wall ahead
        if flags & 0x40 != 0 {
            // start climbing
            let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
            state.wb(def_ptr + 4, flags | 0x80);
            state.ww(si + 0xA, 0xFFF0); // climb up (-16)
            let xvel = s16(state.rw(si + 8) as i32);
            let new_x = (s16(state.rw(si) as i32) + (xvel >> 2)) & 0xFFFF;
            state.ww(si, new_x as u16);
            return;
        }
        // bounce off the wall, fall through to vertical check
        let xvel = s16(state.rw(si + 8) as i32);
        let new_xvel = (-xvel) & 0xFFFF;
        state.ww(si + 8, new_xvel as u16);
        let new_xvel_s = s16(new_xvel as i32);
        let new_x = (s16(state.rw(si) as i32) + (new_xvel_s >> 4)) & 0xFFFF;
        state.ww(si, new_x as u16);
    }

    // vertical check
    let y_minus = (s16(state.rw(si + 2) as i32) - 0x10) & 0xFFFF;
    state.ww(si + 2, y_minus as u16);
    if prop_b(state, tile_above) != 0 {
        // solid above -> settle (Y still -0x10)
        settle(state, si, def_ptr, tile_above);
    } else {
        let y_restored = (s16(state.rw(si + 2) as i32) + 0x10) & 0xFFFF;
        state.ww(si + 2, y_restored as u16);
        if prop_b(state, tile_here) == 0 {
            // nothing underfoot -> fall (gravity capped at 0x100)
            let yvel = s16(state.rw(si + 0xA) as i32);
            if yvel < 0x100 {
                state.ww(si + 0xA, (yvel + 0x10) as u16 & 0xFFFF);
            }
        } else {
            settle(state, si, def_ptr, tile_here);
        }
    }
}

// -- _pounce (1030:7827) — shared by idx8 states 0 and 0xC -------------------------------- //

/// Pounce kick-off: jump up by [def+0xE]<<4, horizontal speed [def+0xF]<<4 toward player,
/// state -> 0xA, d4 = (d4 & 0xD3) | 0x2C, anim forward.
fn pounce(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let de = s8(state.rb(def_ptr + 0xE) as i32);
    state.ww(si + 0xA, ((-(de << 4)) & 0xFFFF) as u16); // yvel = -(dE << 4)

    let df = s8(state.rb(def_ptr + 0xF) as i32);
    let spd = ((df << 4) & 0xFFFF) as u16;
    let px = s16(state.rw(0x4F1C) as i32);
    let ox = s16(state.rw(si) as i32);
    state.ww(si + 8, if px > ox { spd } else { spd.wrapping_neg() });

    state.wb(si + 0xE, 0xA); // state = 0xA

    let d4 = state.rb(def_ptr + 4);
    state.wb(def_ptr + 4, (d4 & 0xD3) | 0x2C);

    let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
    state.ww(si + 0xC, new_ptr);
}

// =========================================================================== //
//                          PER-TYPE AI HANDLERS                               //
// =========================================================================== //

// -- idx0: handle_object_7c90 (1030:7C90) — ground enemy/collectible --------------------- //

/// Ground enemy/collectible: once the player is near, flags itself for the walker's
/// tile-collision pass ([def+4]|=8) then chases on the ground.
pub fn handle_object_7c90(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let (d7, ready) = saturating_counter(state.rb(def_ptr + 6), state.rb(def_ptr + 7));
            state.wb(def_ptr + 7, d7);
            if !ready {
                return;
            }
            let dy = s16(state.rw(si + 2).wrapping_sub(state.rw(0x4F1E)) as i32);
            if dy >= 0xB0 {
                despawn_full(state, si, def_ptr);
            } else {
                let d4 = state.rb(def_ptr + 4);
                state.wb(def_ptr + 4, d4 | 8);
                state.wb(si + 0xE, 1);
            }
        }
        1 => {
            if state.rw(si + 0xA) == 0 {
                // yvel == 0
                state.wb(si + 0xE, 2);
                let ox = s16(state.rw(si) as i32);
                let px = s16(state.rw(0x4F1C) as i32);
                state.ww(si + 8, if ox < px { 0x20u16 } else { 0xFFE0 }); // (-0x20)&0xFFFF
                let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
                state.ww(si + 0xC, new_ptr);
            }
        }
        2 => {} // idle
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// -- idx1: handle_object_7c8c (1030:7C8C) — passive despawn-only ------------------------- //

/// idx1 handler = call 8084; ret — a passive object that only despawns when far.
pub fn handle_object_7c8c(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
}

// -- idx2: handle_object_7c2d (1030:7C2D) — vertical bob oscillator ---------------------- //

/// A vertical BOB oscillator that floats down/up around [def+0xB] within amplitude
/// [def+0xD] at speed [def+0xE], trailing effects (7FD9) each frame.
pub fn handle_object_7c2d(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    if st >= 2 {
        if st == 0xFF {
            dying_state(state, si, def_ptr);
        }
        return;
    }
    let obj_y = state.rw(si + 2);
    let d_b = state.rw(def_ptr + 0xB);
    let rel_y = obj_y.wrapping_sub(d_b);
    spawn_effects(
        state,
        state.rw(def_ptr + 9),
        d_b,
        0,
        (rel_y & 0xFF) as u8,
    );
    if st == 0 {
        // moving DOWN
        let d_e = s8(state.rb(def_ptr + 0xE) as i32);
        state.ww(si + 0xA, ((d_e << 4) & 0xFFFF) as u16);
        let d_d = s8(state.rb(def_ptr + 0xD) as i32);
        if d_d < s8((rel_y & 0xFF) as i32) {
            state.wb(si + 0xE, 1);
            let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
        }
    } else {
        // state 1, moving UP
        let d_e = s8(state.rb(def_ptr + 0xE) as i32);
        state.ww(si + 0xA, ((-(d_e << 4)) & 0xFFFF) as u16);
        if s16(rel_y as i32) < 0 {
            state.wb(si + 0xE, 0);
            let new_ptr = anim_script_rewind(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
        }
    }
}

// -- idx3: handle_object_7b91 (1030:7B91) — falling/landing enemy ------------------------ //

/// A FALLING/LANDING enemy. state 0: wait until player within [def+0xD] tiles of
/// spawn X [def+9], then start falling (Yvel=0x20). state 1: trail effects + check
/// the level tile under it; once it hits solid ground, snap to tile, stop, flag for
/// collision ([def+4]|=0x48), dash toward player (±0x30), state 2. state 2: bounce
/// X velocity if it wraps off the left edge. state 0xFF: dying.
pub fn handle_object_7b91(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let (d7, ready) = saturating_counter(state.rb(def_ptr + 6), state.rb(def_ptr + 7));
            state.wb(def_ptr + 7, d7);
            if !ready {
                return;
            }
            let d9 = state.rw(def_ptr + 9);
            let dist_x = (abs16(d9.wrapping_sub(state.rw(0x4F1C))) >> 4) as u8;
            if state.rb(def_ptr + 0xD) < dist_x {
                return;
            }
            let d4 = state.rb(def_ptr + 4);
            state.wb(def_ptr + 4, d4 & 0xEF);
            state.wb(si + 0xE, 1);
            state.ww(si + 0xA, 0x20); // start falling
            let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
        }
        1 => {
            let d9 = state.rw(def_ptr + 9);
            let d_b = state.rw(def_ptr + 0xB);
            spawn_effects(
                state,
                d9,
                d_b,
                0,
                state.rw(si + 2).wrapping_sub(d_b) as u8,
            );
            let tile_x = (state.rw(si) >> 4) as u8;
            let tile_y = (state.rw(si + 2) >> 4) as u8;
            let prop = tile_prop(state, tile_x, tile_y);
            if prop == 0 {
                return; // no ground yet -> keep falling
            }
            state.ww(si + 2, state.rw(si + 2) & 0xFFF0); // snap to tile
            state.ww(si + 0xA, 0); // yvel = 0
            state.wb(si + 0xE, 2);
            let d4 = state.rb(def_ptr + 4);
            state.wb(def_ptr + 4, d4 | 0x48);
            let px = s16(state.rw(0x4F1C) as i32);
            let ox = s16(state.rw(si) as i32);
            state.ww(si + 8, if px >= ox { 0x30u16 } else { 0x30u16.wrapping_neg() });
            let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
        }
        2 => {
            if s16(state.rw(si) as i32) < 0 {
                let xvel = s16(state.rw(si + 8) as i32);
                state.ww(si + 8, ((-xvel) & 0xFFFF) as u16);
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// -- idx4: handle_object_7adf (1030:7ADF) — orbit/pendulum enemy -------------------------- //

/// An ORBIT/PENDULUM enemy. Despawns by its ORBIT CENTRE ([def+9]/[def+0xB]).
/// state 0: descend until objY-centreY >= [def+0xD]; state 1: spin the angle
/// [def+0xF] up by 4/frame to [def+0xE]; state 2: pendulum SHM. In states 1/2
/// the position is set by the orbit (7B53). state 0xFF: dying.
pub fn handle_object_7adf(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    // despawn by orbit centre (8089: by centre, not by obj position)
    let dr = despawn_check_values(
        state.rw(def_ptr + 9),
        state.rw(def_ptr + 0xB),
        state.rb(si + 0xE),
        (state.rw(si + 4) >> 8) as u8,
        state.rw(si + 4),
        state.rw(0x4F1C),
        state.rw(0x4F1E),
        state.rw(def_ptr + 2),
        state.rb(def_ptr + 4),
        state.rb(def_ptr + 7),
    );
    state.ww(si + 4, dr.sprite_id);
    state.ww(def_ptr + 2, dr.def2);
    state.wb(def_ptr + 4, dr.def4);
    state.wb(def_ptr + 7, dr.def7);

    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let (d7, ready) = saturating_counter(state.rb(def_ptr + 6), state.rb(def_ptr + 7));
            state.wb(def_ptr + 7, d7);
            if !ready {
                return;
            }
            let rel_y = state.rw(si + 2).wrapping_sub(state.rw(def_ptr + 0xB));
            spawn_effects(
                state,
                state.rw(def_ptr + 9),
                state.rw(def_ptr + 0xB),
                0,
                (rel_y & 0xFF) as u8,
            );
            if state.rb(def_ptr + 0xD) <= (rel_y & 0xFF) as u8 {
                state.wb(si + 0xE, 1); // reached depth -> orbit
            } else {
                let new_y = state.rw(si + 2).wrapping_add(2);
                state.ww(si + 2, new_y);
            }
        }
        1 => {
            // spin up the angle — set position on the orbit + trail
            orbit_step(state, si, def_ptr);
            let new_d_f = state.rb(def_ptr + 0xF).wrapping_add(4);
            state.wb(def_ptr + 0xF, new_d_f);
            if new_d_f >= state.rb(def_ptr + 0xE) {
                state.wb(def_ptr + 0xF, state.rb(def_ptr + 0xE)); // clamp
                state.wb(si + 0xE, 2);
            }
        }
        2 => {
            // pendulum oscillation
            orbit_step(state, si, def_ptr);
            let d_f = s8(state.rb(def_ptr + 0xF) as i32);
            let dl: i32 = if d_f < 0 { 1 } else { -1 }; // restoring direction
            let new_d10 = state.rb(def_ptr + 0x10).wrapping_add(dl as u8);
            state.wb(def_ptr + 0x10, new_d10);
            let new_d_f = state.rb(def_ptr + 0xF).wrapping_add(s8(new_d10 as i32) as u8);
            state.wb(def_ptr + 0xF, new_d_f);
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

/// [7B53] set the position on the orbit + trail (the _orbit inner function).
fn orbit_step(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let ang = state.rb(def_ptr + 0xF) as usize;
    let cos_val = state.rb(0x6F90 + ang);
    let sin_val = state.rb(0x7090 + ang);
    let (x, y) = orbit_position(
        state.rw(def_ptr + 9),
        state.rw(def_ptr + 0xB),
        state.rb(def_ptr + 0xD),
        cos_val,
        sin_val,
    );
    state.ww(si, x);
    state.ww(si + 2, y);
    spawn_effects(
        state,
        state.rw(def_ptr + 9),
        state.rw(def_ptr + 0xB),
        state.rb(def_ptr + 0xF),
        state.rb(def_ptr + 0xD),
    );
}

// -- idx5: handle_object_7a60 (1030:7A60) — 2D-proximity pouncer ------------------------- //

/// A 2D-proximity POUNCER. state 0 faces the player (Xvel=±1) and, once the player
/// is within BOTH [def+0xD] horizontal and [def+0xE] vertical tiles, leaps:
/// state -> 0xA, Yvel = [def+0xF]<<4 (UNSIGNED), Xvel = ±that mirrored by the facing.
/// state 0xA flies until within 8px of the player's Y, then lands: anim advances,
/// state -> 0xB, Yvel=0. state 0xB idles; 0xFF dies.
pub fn handle_object_7a60(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let ox = s16(state.rw(si) as i32);
            let px = s16(state.rw(0x4F1C) as i32);
            state.ww(si + 8, if ox <= px { 1u16 } else { 0xFFFFu16 });
            let distx = (abs16(state.rw(0x4F1C).wrapping_sub(state.rw(si))) >> 4) as u8;
            if state.rb(def_ptr + 0xD) < distx {
                return;
            }
            let disty = (abs16(state.rw(0x4F1E).wrapping_sub(state.rw(si + 2))) >> 4) as u8;
            if state.rb(def_ptr + 0xE) < disty {
                return;
            }
            state.wb(si + 0xE, 0xA); // leap
            let spd = ((state.rb(def_ptr + 0xF) as u16) << 4) & 0xFFFF; // UNSIGNED
            state.ww(si + 0xA, spd); // yvel
            let xvel = state.rw(si + 8);
            let spd_final = if (xvel >> 8) & 0x80 != 0 {
                spd.wrapping_neg()
            } else {
                spd
            };
            state.ww(si + 8, spd_final); // xvel
        }
        0xA => {
            if abs16(state.rw(si + 2).wrapping_sub(state.rw(0x4F1E))) <= 8 {
                let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
                state.ww(si + 0xC, new_ptr);
                state.wb(si + 0xE, 0xB);
                state.ww(si + 0xA, 0); // yvel = 0
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {} // state 0xB and any other -> ret
    }
}

// -- idx6: handle_object_78ec (1030:78EC) — earthquake / screen-shake driver -------------- //

/// The EARTHQUAKE / screen-shake driver. state 0 faces the player and, once the
/// player is within [def+0xD] horizontal tiles, arms: state->1, resets the shake
/// counters and seeds the position accumulators. state 1 each frame: faces the
/// player, computes dist²; if the player is close and quakes are enabled it kicks
/// a directional shake velocity; otherwise it counts the shake timer down and, on
/// underflow, RANDOM-reseeds the shake. Then accumulates the position from the
/// accumulators. state 0xFF dies.
pub fn handle_object_78ec(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);

    let st = state.rb(si + 0xE);
    match st {
        0 => {
            // face the player
            face_player(state, si);
            let dist = (abs16(state.rw(0x4F1C).wrapping_sub(state.rw(si))) >> 4) as u8;
            if state.rb(def_ptr + 0xD) >= dist {
                // in range -> arm
                state.wb(si + 0xE, 1);
                state.wb(def_ptr + 0x10, 0x18); // d10 = 0x18
                state.wb(def_ptr + 0xE, 0); // dE = 0
                state.wb(def_ptr + 0xF, 0); // dF = 0
                let w11 = (s16(state.rw(si) as i32) << 3) & 0xFFFF;
                let w13 = (s16(state.rw(si + 2) as i32) << 3) & 0xFFFF;
                state.wb(def_ptr + 0x11, (w11 & 0xFF) as u8);
                state.wb(def_ptr + 0x12, (w11 >> 8) as u8);
                state.wb(def_ptr + 0x13, (w13 & 0xFF) as u8);
                state.wb(def_ptr + 0x14, (w13 >> 8) as u8);
            }
        }
        1 => {
            face_player(state, si);
            let dx = s16(state.rw(0x4F1C).wrapping_sub(state.rw(si)) as i32);
            let dy = s16(state.rw(0x4F1E).wrapping_sub(state.rw(si + 2)) as i32);
            let adx = dx.abs();
            let ady = dy.abs();
            let dist_sq: u32 = (adx as u32) * (adx as u32) + (ady as u32) * (ady as u32);
            state.ww(0xA30E, (dist_sq & 0xFFFF) as u16);
            state.ww(0xA310, ((dist_sq >> 16) & 0xFFFF) as u16);
            let a30e = state.rw(0xA30E);
            let a310 = state.rw(0xA310);
            let bd0 = state.rb(0x6BD0);
            let close = a310 == 0 && a30e < 0x2710 && bd0 != 0;
            if close {
                // directional shake kick
                let mut al: i32 = if a30e < 0xE10 { 0x68 } else { 0x40 };
                if ady >= 0x30 {
                    // big vertical sep -> shake Y
                    state.wb(def_ptr + 0xF, 0xC0);
                } else {
                    if dx > 0 {
                        al = (-al) & 0xFF;
                    }
                    state.wb(def_ptr + 0xE, al as u8);
                }
            } else {
                // count the shake timer down
                let d10 = state.rb(def_ptr + 0x10).wrapping_sub(1);
                state.wb(def_ptr + 0x10, d10);
                if d10 & 0x80 != 0 {
                    // underflow -> random reseed
                    let ror = state.rw(RNG_ROR_STATE);
                    let r = rng_ror(ror);
                    state.ww(RNG_ROR_STATE, r);
                    state.wb(def_ptr + 0x10, ((r & 0xF) + 3) as u8);

                    let la = state.rb(RNG_LCG_A);
                    let lb = state.rb(RNG_LCG_B);
                    let lc = state.rb(RNG_LCG_C);
                    let ld = state.rw(RNG_LCG_D);
                    let (la, lb, lc, ld, ret) = rng_lcg(la, lb, lc, ld);
                    state.wb(RNG_LCG_A, la);
                    state.wb(RNG_LCG_B, lb);
                    state.wb(RNG_LCG_C, lc);
                    state.ww(RNG_LCG_D, ld);
                    let mut al: i32 = (ret & 0xF) as i32;
                    if s16(state.rw(0x4F1C) as i32) < s16(state.rw(si) as i32) {
                        al = (-al) & 0xFF;
                    }
                    state.wb(0x6BC0, al as u8);

                    let (la, lb, lc, ld, ret) = rng_lcg(la, lb, lc, ld);
                    state.wb(RNG_LCG_A, la);
                    state.wb(RNG_LCG_B, lb);
                    state.wb(RNG_LCG_C, lc);
                    state.ww(RNG_LCG_D, ld);
                    let mut al: i32 = (ret & 0xF) as i32;
                    let obj_y_plus = state.rw(si + 2).wrapping_add(0x32);
                    if s16(state.rw(0x4F1E) as i32) < s16(obj_y_plus as i32) {
                        al = (-al) & 0xFF;
                    }
                    state.wb(0x6BC1, al as u8);

                    let ror = state.rw(RNG_ROR_STATE);
                    let r = rng_ror(ror);
                    state.ww(RNG_ROR_STATE, r);
                    let mut al: i32 = (r & 0x1F) as i32;
                    if s8(state.rb(0x6BC0) as i32) < 0 {
                        al = (-al) & 0xFF;
                    }
                    state.wb(def_ptr + 0xE, al as u8);

                    let bc0 = state.rb(0x6BC0);
                    let bc1 = state.rb(0x6BC1);
                    let mut al: i32 = if bc0 == 0 {
                        2
                    } else {
                        let ch = (s8(bc0 as i32).abs() & 0xFF) as u8;
                        let ax = ((s8(bc1 as i32).abs() << 3) & 0xFFFF) as u16;
                        (ax / ch as u16) as i32
                    };
                    if al >= 0x20 {
                        al = 0x20;
                    }
                    if s8(bc1 as i32) < 0 {
                        al = (-al) & 0xFF;
                    }
                    state.wb(def_ptr + 0xF, al as u8);
                }
            }
            accumulate(state, si, def_ptr);
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

/// [7A50] face the player: Xvel = ±1 based on player_x vs obj_x.
fn face_player(state: &mut NativeGameState, si: usize) {
    let ox = s16(state.rw(si) as i32);
    let px = s16(state.rw(0x4F1C) as i32);
    state.ww(si + 8, if ox <= px { 1u16 } else { 0xFFFFu16 });
}

/// [7A28] drive the position from the accumulators.
fn accumulate(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let w11 = state
        .rw(def_ptr + 0x11)
        .wrapping_add(s8(state.rb(def_ptr + 0xE) as i32) as u16);
    let w13 = state
        .rw(def_ptr + 0x13)
        .wrapping_add(s8(state.rb(def_ptr + 0xF) as i32) as u16);
    state.wb(def_ptr + 0x11, (w11 & 0xFF) as u8);
    state.wb(def_ptr + 0x12, (w11 >> 8) as u8);
    state.wb(def_ptr + 0x13, (w13 & 0xFF) as u8);
    state.wb(def_ptr + 0x14, (w13 >> 8) as u8);
    state.ww(si, (s16(w11 as i32) >> 3 & 0xFFFF) as u16);
    state.ww(si + 2, (s16(w13 as i32) >> 3 & 0xFFFF) as u16);
}

// -- idx7: handle_object_7898 (1030:7898) — creeper/leaper enemy ------------------------- //

/// A creeper/LEAPER enemy. state 0 faces the player horizontally (Xvel = ±1) and,
/// once the player is within [def+0xD] horizontal tiles, LEAPS: state -> 0xA,
/// Yvel = [def+0xE]<<4 (signed), Xvel = ±that mirrored by the facing. state 0xA
/// just flies; 0xFF dies.
pub fn handle_object_7898(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let ox = s16(state.rw(si) as i32);
            let px = s16(state.rw(0x4F1C) as i32);
            state.ww(si + 8, if ox <= px { 1u16 } else { 0xFFFFu16 });
            let dist = (abs16(state.rw(0x4F1C).wrapping_sub(state.rw(si))) >> 4) as u8;
            if state.rb(def_ptr + 0xD) < dist {
                return;
            }
            state.wb(si + 0xE, 0xA); // launch
            let d_e = s8(state.rb(def_ptr + 0xE) as i32);
            let leap = ((d_e << 4) & 0xFFFF) as u16;
            state.ww(si + 0xA, leap); // yvel
            let xvel = state.rw(si + 8); // current xvel (just set)
            let leap_final = if (xvel >> 8) & 0x50 != 0 {
                leap.wrapping_neg()
            } else {
                leap
            };
            state.ww(si + 8, leap_final); // xvel
            let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {} // state 0xA and any other -> no change
    }
}

// -- idx8: handle_object_77de (1030:77DE) — pouncing enemy ------------------------------- //

/// A POUNCING enemy: wait (timer 8001) facing the player, and when the player is
/// within [def+0xD] (tile X) × [def+0x10] (tile Y) leap up+toward them
/// ([def+0xE] height / [def+0xF] speed); states 0xA rise → 0xB land → 0xC cooldown
/// → pounce again.
pub fn handle_object_77de(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    if state.rw(si + 8) == 0 {
        // face the player when stationary
        let ox = s16(state.rw(si) as i32);
        let px = s16(state.rw(0x4F1C) as i32);
        state.ww(si + 8, if ox <= px { 1u16 } else { 0xFFFFu16 });
    }
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let (d7, ready) = saturating_counter(state.rb(def_ptr + 6), state.rb(def_ptr + 7));
            state.wb(def_ptr + 7, d7);
            if !ready {
                return;
            }
            let dist_x =
                (abs16(state.rw(0x4F1C).wrapping_sub(state.rw(si))) >> 4) as u8;
            if state.rb(def_ptr + 0xD) < dist_x {
                return;
            }
            let dist_y =
                (abs16(state.rw(0x4F1E).wrapping_sub(state.rw(si + 2))) >> 4) as u8;
            if state.rb(def_ptr + 0x10) >= dist_y {
                pounce(state, si, def_ptr);
            }
        }
        0xA => {
            // rising -> until apex (yvel >= 0)
            if s16(state.rw(si + 0xA) as i32) >= 0 {
                state.wb(si + 0xE, 0xB);
                let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
                state.ww(si + 0xC, new_ptr);
            }
        }
        0xB => {
            // falling -> until landed (yvel <= 0)
            if s16(state.rw(si + 0xA) as i32) <= 0 {
                state.wb(si + 0xE, 0xC);
                let p = anim_script_rewind(state, state.rw(si + 0xC));
                let p = anim_script_rewind(state, p);
                state.ww(si + 0xC, p);
                state.ww(si + 8, 0); // xvel = 0
                state.wb(def_ptr + 7, 0); // restart cooldown timer
            }
        }
        0xC => {
            // cooldown -> pounce again when ready
            let (d7, ready) = saturating_counter(state.rb(def_ptr + 6), state.rb(def_ptr + 7));
            state.wb(def_ptr + 7, d7);
            if ready {
                pounce(state, si, def_ptr);
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// -- idx9: handle_object_773d (1030:773D) — horizontal-patrol enemy ---------------------- //

/// A horizontal-patrol enemy that accelerates back and forth between def bounds,
/// with its OWN proximity despawn (it does NOT call the shared 8084).
pub fn handle_object_773d(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let left = s16(state.rw(def_ptr + 0xD) as i32);
    let right = s16(state.rw(def_ptr + 0xF) as i32);
    let id = state.rw(si + 4);
    let drawn = id & 0x2000 != 0;
    let st = state.rb(si + 0xE);

    if !drawn && st != 0xFF {
        // proximity despawn
        let obj_y = state.rw(si + 2);
        let player_y = state.rw(0x4F1E);
        let keep = if abs16(obj_y.wrapping_sub(player_y)) >= 0xBE {
            false
        } else {
            let px = s16(state.rw(0x4F1C) as i32);
            if px >= left {
                right + 0x1E0 > px
            } else {
                px + 0x1E0 >= left
            }
        };
        if !keep {
            state.ww(si + 4, EMPTY_ID);
            let d4 = state.rb(def_ptr + 4);
            state.wb(def_ptr + 4, d4 & 0xFB);
            return;
        }
    }

    match st {
        0 => {
            // patrol right: accelerate +3 up to +d12
            let sp = s8(state.rb(def_ptr + 0x11) as i32);
            state.ww(si + 8, (sp & 0xFFFF) as u16);
            let cap = state.rb(def_ptr + 0x12) as i32;
            if sp + 3 <= cap {
                state.wb(def_ptr + 0x11, (sp + 3) as u8);
            }
            if right < s16(state.rw(si) as i32) {
                state.wb(si + 0xE, 1);
            }
        }
        1 => {
            // patrol left: accelerate -3 down to -d12
            let sp = s8(state.rb(def_ptr + 0x11) as i32);
            state.ww(si + 8, (sp & 0xFFFF) as u16);
            let cap = state.rb(def_ptr + 0x12) as i32;
            if sp - 3 >= -cap {
                state.wb(def_ptr + 0x11, (sp - 3) as u8);
            }
            if left >= s16(state.rw(si) as i32) {
                state.wb(si + 0xE, 0);
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// -- idx10: handle_object_7665 (1030:7665) — charging/falling enemy ---------------------- //

/// A charging/falling enemy state machine on [si+0xE] (settle 0 -> arm 1 ->
/// charge-at-player 2 -> expire 3 -> dying 0xFF). Reads game-mode [0x2D8A], shake
/// [0x6BEA], anim-ready [0xA340], frame [0x6BD5], player [0x4F1C]/[0x4F1E].
pub fn handle_object_7665(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    let mode = state.rb(0x2D8A);
    let shake = state.rb(0x6BEA);
    if mode == 5 && shake != 0 && st != 3 && st != 0xFF {
        // freeze-on-shake
        state.wb(def_ptr + 7, 1);
    }

    match st {
        0 => {
            // settle: wait until vertical motion stops
            let d4 = state.rb(def_ptr + 4);
            state.wb(def_ptr + 4, d4 | 0x18);
            if state.rw(si + 0xA) == 0 {
                // yvel == 0
                state.wb(si + 0xE, 1);
            }
        }
        1 => {
            // arm
            if state.rw(si + 0xA) != 0 {
                // yvel != 0
                state.wb(si + 0xE, 0);
            } else {
                state.wb(def_ptr + 7, 0x1E); // arm the active timer
                state.wb(si + 0xE, 2);
                let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
                state.ww(si + 0xC, new_ptr);
            }
        }
        2 => {
            // charge at the player, then expire
            let xvel_abs = s16(state.rw(si + 8) as i32).abs();
            if xvel_abs < 0x10 {
                // only re-aim when nearly stopped
                if state.rb(0xA340) == 0 {
                    return; // not anim-ready -> idle this frame
                }
                state.wb(def_ptr + 4, 0x0F);
                let spd = state.rb(def_ptr + 0xD);
                let obj_x = s16(state.rw(si) as i32);
                let px = s16(state.rw(0x4F1C) as i32);
                state.ww(si + 8, if obj_x < px {
                    spd as u16
                } else {
                    (-(spd as i32) & 0xFFFF) as u16
                });
            }
            if state.rb(0x6BD5) & 3 != 0 {
                return; // count down only every 4th frame
            }
            let d7 = state.rb(def_ptr + 7).wrapping_sub(1);
            state.wb(def_ptr + 7, d7);
            if d7 != 0 {
                return;
            }
            // timer expired -> stop + die soon
            state.wb(si + 0xE, 3);
            state.ww(si + 0xA, 0); // yvel = 0
            let xvel = state.rw(si + 8);
            state.ww(si + 8, if xvel & 0x8000 != 0 { 0xFFFF } else { 0 }); // keep only the sign
            state.wb(def_ptr + 4, 0x36);
            let new_ptr = anim_script_forward(state, state.rw(si + 0xC));
            state.ww(si + 0xC, new_ptr);
        }
        3 => {
            // wait for the death anim, then despawn
            if state.rb(0xA340) != 0 {
                despawn_full(state, si, def_ptr);
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// -- idx11: handle_object_760f (1030:760F) — leaping enemy (flying squirrel) -------------- //

/// The LEAPING enemy. When anim-ready it leaps toward the player
/// (Xvel = ±[def+0xD]<<4) and up (Yvel = -[def+0xE]<<4, state 1), then falls under
/// gravity (Yvel += 8 each frame until it reaches the terminal [def+0xF]<<4).
pub fn handle_object_760f(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    do_despawn(state, si, def_ptr);
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            if state.rb(0xA340) == 0 {
                return; // wait for anim
            }
            state.wb(si + 0xE, 1);
            let d4 = state.rb(def_ptr + 4);
            state.wb(def_ptr + 4, d4 & 0xEF);
            let d_d = s8(state.rb(def_ptr + 0xD) as i32);
            let leap = (d_d << 4) & 0xFFFF;
            let px = s16(state.rw(0x4F1C) as i32);
            let ox = s16(state.rw(si) as i32);
            state.ww(si + 8, if px > ox {
                leap as u16
            } else {
                ((-leap) & 0xFFFF) as u16
            });
            let d_e = s8(state.rb(def_ptr + 0xE) as i32);
            state.ww(si + 0xA, ((-(d_e << 4)) & 0xFFFF) as u16); // jump up
        }
        1 => {
            // gravity until terminal velocity
            let yvel = s16(state.rw(si + 0xA) as i32);
            let d_f = s8(state.rb(def_ptr + 0xF) as i32);
            let terminal = s16((d_f << 4) & 0xFFFF);
            if yvel < terminal {
                state.ww(si + 0xA, state.rw(si + 0xA).wrapping_add(8));
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// -- idx12: handle_object_75c4 (1030:75C4) — falling / earthquake-spawned object ----------- //

/// A falling / earthquake-spawned object. state 0 sets Xvel = ±([def+0xD]<<4)
/// toward the player and advances to state 1. state 1 keeps the off-screen timer
/// [def+7] at 0 while the object is drawn; once it has been off-screen long
/// enough ([def+7] reaches 0x9A) it flips to state 0xFF. state 0xFF dies.
pub fn handle_object_75c4(state: &mut NativeGameState, si: usize, def_ptr: usize) {
    let st = state.rb(si + 0xE);
    match st {
        0 => {
            let mut ax = s8(state.rb(def_ptr + 0xD) as i32);
            if s16(state.rw(0x4F1C) as i32) < s16(state.rw(si) as i32) {
                ax = -ax;
            }
            state.ww(si + 8, ((ax << 4) & 0xFFFF) as u16);
            state.wb(si + 0xE, 1);
        }
        1 => {
            let id = state.rw(si + 4);
            if id & 0x2000 != 0 {
                // drawn (on-screen) -> reset timer
                state.wb(def_ptr + 7, 0);
            } else {
                let d7 = state.rb(def_ptr + 7).wrapping_add(1);
                state.wb(def_ptr + 7, d7);
                if d7 >= 0x9A {
                    state.wb(si + 0xE, 0xFF);
                }
            }
        }
        0xFF => {
            dying_state(state, si, def_ptr);
        }
        _ => {}
    }
}

// =========================================================================== //
//                       HANDLER DISPATCH TABLE                                //
// =========================================================================== //

/// Dispatch to the appropriate AI handler by its code-segment address.
/// Maps each handler address to its function, matching the native 1030:xxxx table.
pub fn dispatch_handler(state: &mut NativeGameState, handler_addr: u16, si: usize, def_ptr: usize) {
    match handler_addr {
        0x7C90 => handle_object_7c90(state, si, def_ptr),
        0x7C8C => handle_object_7c8c(state, si, def_ptr),
        0x7C2D => handle_object_7c2d(state, si, def_ptr),
        0x7B91 => handle_object_7b91(state, si, def_ptr),
        0x7ADF => handle_object_7adf(state, si, def_ptr),
        0x7A60 => handle_object_7a60(state, si, def_ptr),
        0x78EC => handle_object_78ec(state, si, def_ptr),
        0x7898 => handle_object_7898(state, si, def_ptr),
        0x77DE => handle_object_77de(state, si, def_ptr),
        0x773D => handle_object_773d(state, si, def_ptr),
        0x7665 => handle_object_7665(state, si, def_ptr),
        0x760F => handle_object_760f(state, si, def_ptr),
        0x75C4 => handle_object_75c4(state, si, def_ptr),
        _ => {} // unknown handler -> no-op
    }
}
