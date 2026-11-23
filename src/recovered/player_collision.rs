// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 player ground/tile collision (`1030:5A96`).
//!
//! The full collision routine at 1030:5A96: tile cell computation, slope
//! handling, wall collision, ceiling collision, ground landing, fall damage,
//! bridge dip, side-collision scan, off-camera death trigger.
//!
//! Operates directly on NativeGameState via rb/rw/wb/ww — no dict/overlay.

use crate::engine::state::NativeGameState;
use crate::recovered::player::{
    player_emit_trail, player_gravity, P_FACING, P_MOTION_MODE, P_SPRITE, P_XVEL, P_YVEL,
    PLAYER_BASE,
};

// ════════════════════════════════════════════════════════════════════
// Constants — table base offsets
// ════════════════════════════════════════════════════════════════════

pub const AIRBORNE_FLAG: usize = 0x6BF3;
pub const TILE_PROP_TABLE: usize = 0x8E1D;
pub const TILE_CEIL_SOLID_TABLE: usize = 0x7E5E;
pub const TILE_CEIL_HANDLER_TABLE: usize = 0x805E;
pub const GROUND_REMAP_TABLE: usize = 0x7F5E;
pub const DIRTY_KIND_TABLE: usize = 0x4DF8;
pub const WALL_MARKER_LIST: usize = 0x6EA9;
pub const PLAYER_ANIM_HEIGHT_TABLE: usize = 0x7191;

// Player-record + player-state offsets
pub const PLAYER_SLOT: usize = 0x4F1C;
pub const PLAYER_Y: usize = PLAYER_SLOT + 2;
pub const FALL_FRAMES: usize = 0x6BD2;
pub const FALL_LATCH: usize = 0x6BD1;
pub const ANIM_GATE: usize = 0x6BD0;
pub const FALL_GRACE: usize = 0x6BE0;
pub const PENDING_PICKUP: usize = 0x6BE1;
pub const SHAKE_MAGNITUDE: usize = 0x6BEA;
pub const LOW_GRAVITY: usize = 0x6BC7;
pub const END_SIGNAL: usize = 0x6BE5;
pub const GRID_DIRTY: usize = 0x2DF4;
pub const PAGE_DIRTY: usize = 0x6BBD;
pub const RESPAWN_STATE: usize = 0x6BE4;
pub const ENERGY: usize = 0x27D6;
pub const LIVES: usize = 0x27D8;

// Additional DGROUP offsets used by the collision island
const DIPPING_TILE: usize = 0x6BAB;
const CURRENT_HIT_OBJECT: usize = 0x6BB1;
const MAP_ROWS: usize = 0x2CF5;
const MAP_SEG: usize = 0x2DDA;
const CAM_COL_WORD: usize = 0x2DE4;
const CAM_ROW_WORD: usize = 0x2DE6;
const CAM_COL_BYTE: usize = 0x2DE4;
const CAM_ROW_BYTE: usize = 0x2DE6;
const LEVEL_FLAGS: usize = 0x8166;
const LEVEL: usize = 0x2D8A;
const GRID_DIRTY_TOKEN: usize = 0x2DE0;
const GLIDER_GATE: usize = 0x6BC5;
const INPUT_LR: usize = 0x6BDB;
const LAST_LAND_Y: usize = 0x6BCA;
const UNK_6BFE: usize = 0x6BFE;

// ════════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════════

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 {
        v - 0x10000
    } else {
        v
    }
}

#[inline]
fn s8(v: i32) -> i32 {
    let v = v & 0xFF;
    if v & 0x80 != 0 {
        v - 0x100
    } else {
        v
    }
}

/// Saturating decrement of a byte (sub v,1 ; adc v,0) — clamps at 0.
#[inline]
fn dec_floor8(v: u8) -> u8 {
    if v > 0 {
        v - 1
    } else {
        0
    }
}

/// Read a byte from the level map (es:[off], es = [0x2DDA]).
#[inline]
fn read_es(state: &NativeGameState, off: u16) -> u8 {
    let map_seg = state.rw(MAP_SEG) as usize;
    let addr = ((map_seg << 4) + (off as usize & 0xFFFF)) & 0xFFFFF;
    if addr < state.data.len() {
        state.data[addr]
    } else {
        0
    }
}

/// Write a byte to the level map (es:[off], es = [0x2DDA]).
#[inline]
fn write_es(state: &mut NativeGameState, off: u16, val: u8) {
    let map_seg = state.rw(MAP_SEG) as usize;
    let addr = ((map_seg << 4) + (off as usize & 0xFFFF)) & 0xFFFFF;
    if addr < state.data.len() {
        state.data[addr] = val;
    }
}

// ════════════════════════════════════════════════════════════════════
// Table lookups
// ════════════════════════════════════════════════════════════════════

/// Tile property byte (solid / slope 0x30 / dir 0x10 / height 0x0F).
pub fn tile_props(state: &NativeGameState, tile_id: u8) -> u8 {
    state.rb((TILE_PROP_TABLE + tile_id as usize) & 0xFFFF)
}

/// Ground handler index (cs:[0x7D9B]) for the foot tile.
pub fn ground_remap(state: &NativeGameState, tile_id: u8) -> u8 {
    state.rb((GROUND_REMAP_TABLE + tile_id as usize) & 0xFFFF)
}

/// Ceiling-solid flag (bit0) for the side-nudge.
pub fn ceil_props(state: &NativeGameState, tile_id: u8) -> u8 {
    state.rb((TILE_CEIL_SOLID_TABLE + tile_id as usize) & 0xFFFF)
}

/// Ceiling-handler index (cs:[0x7DA9]): 0 noop / 1 head-bump / 2 trigger.
/// Also used for bridge flag (bit 0x20) and side-solid flag (bit 0x10).
pub fn ceil_handler(state: &NativeGameState, tile_id: u8) -> u8 {
    state.rb((TILE_CEIL_HANDLER_TABLE + tile_id as usize) & 0xFFFF)
}

/// Dirty-kind: 0 = redraw one tile, >=1 = grid-dirty.
fn dirty_kind(state: &NativeGameState, tile_id: u8) -> u8 {
    state.rb((DIRTY_KIND_TABLE + tile_id as usize) & 0xFFFF)
}

/// Player anim height: vertical extent (scan-loop row count).
fn player_anim_height(state: &NativeGameState, anim_idx: usize) -> u8 {
    state.rb((PLAYER_ANIM_HEIGHT_TABLE + anim_idx) & 0xFFFF)
}

// ════════════════════════════════════════════════════════════════════
// Pure leaf primitives
// ════════════════════════════════════════════════════════════════════

/// Slope-height offset (1030:661A).
///
/// For a slope tile (property bits 0x30 set), computes height based on X
/// position across the slope: `quot = (X & 0x0F) // 3`; on an up-slope
/// (prop & 0x10) the offset is `quot + (prop & 0x0F)`, otherwise
/// `(prop & 0x0F) - quot` — sign-extended to a word. A non-slope tile
/// returns `prop` unchanged. Pure.
pub fn collision_slope_offset(prop: u8, player_x: u16) -> u16 {
    let al = prop;
    if al & 0x30 == 0 {
        // [661A-661C] not a slope -> ax = cwde(prop)
        return (s8(al as i32) & 0xFFFF) as u16;
    }
    let quot = ((player_x as usize) & 0x0F) / 3; // [6620-6628] div bl=3
    let low = (al & 0x0F) as usize; // [6630 / 6637] and bl,0xF
    let res = if al & 0x10 != 0 {
        // [662B] up-slope
        (quot + low) as u8 // [6633] add al,bl
    } else {
        // [663A-663C] down-slope: neg al ; add bl
        (low.wrapping_sub(quot)) as u8
    };
    (s8(res as i32) & 0xFFFF) as u16 // [663E] cwde
}

/// Fall / no-ground response (1030:6401) — set the airborne flag bit0.
pub fn collision_fall(flag: u8) -> u8 {
    flag | 1
}

/// Horizontal-block response (1030:6407) — undo X step and stop.
/// Returns (new_x, new_xvel).
pub fn collision_hblock(x: u16, xvel: u16) -> (u16, u16) {
    let new_x = ((s16(x as i32) - (s16(xvel as i32) >> 4)) & 0xFFFF) as u16;
    (new_x, 0)
}

// ════════════════════════════════════════════════════════════════════
// Air physics (1030:62B1 / 6309 / 63B5)
// ════════════════════════════════════════════════════════════════════

/// Air horizontal drift + clamp (1030:62B1). `bp` = max air X speed.
/// When input_lr is set, add a facing-direction accel (facing<<4 sar
/// motion_mode) to Xvel; clamp to [-bp, +bp]. Returns new Xvel.
fn air_drift_x(state: &mut NativeGameState, bp: i32) -> u16 {
    let input_lr = state.rb(INPUT_LR);
    let facing = state.rw(P_FACING);
    let motion_mode = state.rb(P_MOTION_MODE) as u32;
    let xvel = state.rw(P_XVEL);

    let ax = if input_lr != 0 {
        s16(((facing as i32) << 4) & 0xFFFF) >> motion_mode
    } else {
        0
    };
    let dx = s16((s16(xvel as i32) + ax) & 0xFFFF);
    let bp = s16(bp & 0xFFFF);
    let clamped = if dx >= bp {
        bp
    } else if dx <= -bp {
        -bp
    } else {
        dx
    };
    (clamped & 0xFFFF) as u16
}

/// Off-top / in-air physics (1030:63B5): air drift (±0x50) + gravity
/// (terminal 0xC0) + fall animation.
pub fn collision_airborne(state: &mut NativeGameState) {
    // [63B7-63BA] air drift X (limit 0x50)
    let new_xvel = air_drift_x(state, 0x50);
    state.ww(P_XVEL, new_xvel);

    // [63BD-63C0] gravity (terminal 0xC0)
    let yvel = state.rw(P_YVEL);
    let low_grav = state.rb(LOW_GRAVITY);
    let new_yvel = player_gravity(yvel, low_grav, 0xC0);
    state.ww(P_YVEL, new_yvel);

    let yvel_s = s16(new_yvel as i32);

    // [63C3] glider branch (dormant in normal play)
    let glider = state.rb(GLIDER_GATE);
    if glider != 0 {
        let al: u8 = if yvel_s < 0 { 0x2D } else { 0x2E }; // [63CA-63D3] fly frames
        let facing_lo = state.rb(P_FACING);
        let frame = (((facing_lo & 0x80) as u16) << 8) | al as u16; // [63F4-63FB]
        state.ww(P_SPRITE, frame);
        return;
    }

    // [63D6] rising / apex -> no anim change
    if yvel_s <= 0 {
        return;
    }

    state.wb(FALL_GRACE, 6); // [63DD]

    if state.rb(ANIM_GATE) != 0 {
        // [63E2] gated -> no anim change
        return;
    }

    let fall_frames = state.rb(FALL_FRAMES);
    let al: u8 = if fall_frames >= 0x0C { 0x0D } else { 0x0C }; // [63E9-63F2]
    let facing_lo = state.rb(P_FACING);
    let frame = (((facing_lo & 0x80) as u16) << 8) | al as u16; // [63F4-63FB]
    state.ww(P_SPRITE, frame);
}

// ════════════════════════════════════════════════════════════════════
// Landing (1030:64D9 / 641F)
// ════════════════════════════════════════════════════════════════════

/// Soft-land exit (1030:64D9): zero Yvel + set grounded state flags.
fn soft_land(state: &mut NativeGameState, new_y: u16) {
    state.ww(P_YVEL, 0); // [64D9]
    state.wb(FALL_GRACE, dec_floor8(state.rb(FALL_GRACE))); // [64DF-64E4]
    state.wb(FALL_LATCH, 0); // [64E9]
    state.wb(AIRBORNE_FLAG, 2); // [64EE]
    state.ww(LAST_LAND_Y, new_y); // [64F3-64F6]
}

/// Player land-on-ground routine (1030:641F).
///
/// Snaps Y to the tile top, applies the slope offset (capped by the
/// descent sar(Yvel,4)), then resolves the landing impact — soft (zero
/// Yvel + grounded flags) vs hard (landing dust, camera shake, bounce,
/// land anim). `di` is the player's tile pointer.
fn collision_land(state: &mut NativeGameState, di: u16) {
    state.wb(P_MOTION_MODE, 0); // [641F]

    let yvel = state.rw(P_YVEL);
    if s16(yvel as i32) < 0 {
        // [6424] rising -> airborne
        state.wb(AIRBORNE_FLAG, collision_fall(state.rb(AIRBORNE_FLAG))); // [6401]
        return;
    }

    state.wb(LOW_GRAVITY, 0); // [642D]

    // [6432] snap Y to tile top
    let y = state.rw(PLAYER_Y) & 0xFFF0;
    state.ww(PLAYER_Y, y);
    let mut new_y = y;

    // [6437-643C] foot tile property
    let foot = read_es(state, di);
    let foot_prop = tile_props(state, foot);
    if foot_prop != 0 {
        // [6445] slope offset
        let mut off = s16(collision_slope_offset(foot_prop, state.rw(PLAYER_BASE)) as i32);
        let cap = s16(yvel as i32) >> 4; // [6448] sar Yvel,4
        if cap > 0 && off >= cap {
            // [6454/6456]
            off = cap;
        }
        new_y = ((y as i32 + off) & 0xFFFF) as u16;
        state.ww(PLAYER_Y, new_y); // [6478]
    } else {
        // [645E] below tile
        let below = read_es(state, di.wrapping_sub(0x100));
        let below_prop = tile_props(state, below);
        if below_prop != 0 {
            // [6469]
            let off = s16(collision_slope_offset(below_prop, state.rw(PLAYER_BASE)) as i32);
            if off < 0x10 {
                // [6470]
                new_y = ((y as i32 + ((off - 0x10) & 0xFFFF) as i32) & 0xFFFF) as u16;
                state.ww(PLAYER_Y, new_y); // [6475-6478]
            }
        }
    }

    let bd2 = state.rb(FALL_FRAMES);
    if bd2 <= 4 {
        // [647C] jbe 64D9
        soft_land(state, new_y);
        return;
    }

    // [6483] 5E18 landing dust (ungated, blink=0)
    player_emit_trail(state, 0);

    let yvel_s = s16(yvel as i32);
    let last_land = s16(state.rw(LAST_LAND_Y) as i32);
    if (s16(new_y as i32) - last_land) < 0x20 || yvel_s < 0x50 {
        // [6486-6497]
        soft_land(state, new_y);
        return;
    }

    state.ww(LAST_LAND_Y, new_y); // [6499]

    if bd2 >= 0x14 && yvel_s > 0xA0 {
        // [649F-64AC]
        state.wb(SHAKE_MAGNITUDE, 8); // [64AE] camera shake
    }

    if bd2 <= 0x0A {
        // [64B3] jbe 64D9
        soft_land(state, new_y);
        return;
    }

    // [64BA] hard land: bounce unless level_flags bit0
    let level_flags = state.rb(LEVEL_FLAGS);
    if level_flags & 1 == 0 {
        state.ww(P_YVEL, 0xFFE0); // [64C1]
    }

    // [64C7] land anim frame 0xC
    let sprite = state.rw(P_SPRITE);
    state.ww(P_SPRITE, (sprite & 0xE000) | 0x000C);

    state.wb(FALL_FRAMES, 0); // [64D3]
}

// ════════════════════════════════════════════════════════════════════
// Ceiling collision (1030:5C16 / 668B)
// ════════════════════════════════════════════════════════════════════

/// [asm 668B] The Yvel==0 head-bump branch — "push out of a solid ceiling"
/// nudge. On the PLAYER's own collision, current_hit_object is NULL (0), so
/// this dereferences low DGROUP as a bogus X/Y, lands on a far-off cell (a
/// 0xFF border tile that is never ceiling-solid), and returns without
/// writing. Transcribed LITERALLY so the null-pointer computation stays
/// byte-exact vs the VM. Anim in {0x0A, 0x15} skips it outright.
fn ceiling_headbump_pushout(state: &mut NativeGameState) {
    let sprite_lo = state.rb(P_SPRITE);
    if sprite_lo == 0x0A || sprite_lo == 0x15 {
        // [668E/6692]
        return;
    }

    let ptr = state.rw(CURRENT_HIT_OBJECT) as usize; // [6698] NULL on player path
    let target_x = state.rw(ptr) as i32; // [ptr] X word
    let target_y = state.rw(ptr + 2) as i32; // [ptr+2] Y word

    let col = (s16(target_x) >> 4) & 0xFF; // [66A5-66A9] sar (X),4
    let row = (s16(target_y) >> 4) & 0xFF; // [669E-66A3] sar (Y),4
    let cell = (((row << 8) | col) & 0xFFFF) as u16; // [66AB] di = (row<<8)|col

    if ceil_props(state, read_es(state, cell)) & 1 == 0 {
        // [66B4-66BA] not solid -> nothing
        return;
    }

    let mut dx: i32 = 0x10; // [66BC] default: push right one tile
    if ceil_props(state, read_es(state, cell.wrapping_sub(1))) & 1 == 0 {
        // [66BF-66C6] left tile open?
        dx = -0x10; // [66C8] push toward open (left) side
    }

    let new_x = ((s16(target_x) + dx) & 0xFFFF) as u16;
    state.ww(ptr, new_x); // [66CA-66CE] add [[ptr]], dx
}

/// Player ceiling (head-bump) collision (1030:5C16..5C76).
///
/// `di` is the tile-above pointer (player tile minus one row). Reads the
/// tile above + player's tile, dispatches the ceiling-tile handler
/// (cs:[0x7DA9]) indexed by 0x805E[tile_above] & 0xF: idx 0 = no-op,
/// idx 1 = head-bump (zero Yvel + snap Y below ceiling), idx 2 = hazard
/// ceiling (off-camera death trigger). Then, if the player's tile is
/// ceiling-solid and Y>0, a sideways corner-slip nudge.
fn collision_ceiling(state: &mut NativeGameState, di: u16) {
    let tile_above = read_es(state, di); // [5C18]
    let player_tile = read_es(state, di.wrapping_add(0x100)); // [5C1B]
    let solid = ceil_props(state, player_tile) & 1 != 0; // [5C20-5C24]
    let idx = ceil_handler(state, tile_above) & 0x0F; // [5C26-5C2A]

    // cur_y tracks the player's own Y across this function — only the
    // idx==1/yvel!=0 branch changes it.
    let mut cur_y = state.rw(PLAYER_Y);

    match idx {
        0 => {
            // [6672] no-op
        }
        1 => {
            // [6673] head-bump
            let yvel = state.rw(P_YVEL);
            if yvel != 0 {
                // [6678] rising -> zero Yvel + snap below ceiling
                state.ww(P_YVEL, 0); // [667A]
                cur_y = ((state.rw(PLAYER_Y) & 0xFFF0) + 0x10) & 0xFFFF; // [6680-6685]
                state.ww(PLAYER_Y, cur_y);
            } else {
                // [668B] Yvel==0: push out of solid ceiling (null-pointer no-op for player)
                ceiling_headbump_pushout(state);
            }
        }
        2 => {
            // [65AF -> 65B3] hazard ceiling: off-camera death trigger
            offcamera_trigger(state);
        }
        _ => {
            // idx 3-15 still unwitnessed
            panic!("ceiling handler idx {} not recovered", idx);
        }
    }

    // [5C38-5C42] solid + Y>0 -> corner-slip nudge
    if solid && s16(cur_y as i32) > 0 {
        let xvel_s = s16(state.rw(P_XVEL) as i32);
        let dx: i32 = if xvel_s > 0 {
            -1 // [5C44-5C51] step away from the facing edge
        } else {
            1
        };

        let n1 = ceil_props(state, read_es(state, ((di as i32 + dx + 0x100) & 0xFFFF) as u16));
        if n1 == 0 {
            // [5C5E] this side is open -> slip into it
            let px = state.rw(PLAYER_BASE);
            state.ww(PLAYER_BASE, ((px as i32 + dx * 2) & 0xFFFF) as u16); // [5C70-5C72]
        } else {
            // [5C60-5C64] other side
            let n2 = ceil_props(state, read_es(state, ((di as i32 - dx + 0x100) & 0xFFFF) as u16));
            if n2 == 0 {
                // [5C6E] only the far side is open
                let px = state.rw(PLAYER_BASE);
                state.ww(PLAYER_BASE, ((px as i32 - dx * 2) & 0xFFFF) as u16); // [5C70-5C72]
            }
            // else: wedged between two solid tiles -> no nudge
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Ground handler dispatch (cs:[0x7D9B])
// ════════════════════════════════════════════════════════════════════

/// Ground handler 0 (1030:65EF): if at rest (Yvel==0) and a reachable
/// solid/slope tile sits a row below, step the player down one row and land
/// on it; otherwise mark airborne. Keeps the caveman stuck to descending
/// ground instead of floating off the lip of a step.
fn ground_snap_or_fall(state: &mut NativeGameState, di: u16) {
    let yvel = state.rw(P_YVEL);
    if yvel != 0 {
        // [65EF] still moving vertically -> fall
        state.wb(AIRBORNE_FLAG, collision_fall(state.rb(AIRBORNE_FLAG)));
        return;
    }

    let below = read_es(state, di.wrapping_add(0x100)); // [65F6] tile one row below
    let prop = tile_props(state, below); // [65FB-65FE]
    if prop == 0 {
        // [65FF] empty below -> fall
        state.wb(AIRBORNE_FLAG, collision_fall(state.rb(AIRBORNE_FLAG)));
        return;
    }

    let off = s16(collision_slope_offset(prop, state.rw(PLAYER_BASE)) as i32); // [6603]
    if off >= 0x10 {
        // [6606-6609] too far down -> fall
        state.wb(AIRBORNE_FLAG, collision_fall(state.rb(AIRBORNE_FLAG)));
        return;
    }

    // [660F] drop one tile row
    let stepped_y = state.rw(PLAYER_Y).wrapping_add(0x10);
    state.ww(PLAYER_Y, stepped_y);

    // [660B/6614 -> 0x641F] land on the row below
    collision_land(state, di.wrapping_add(0x100));

    // setdefault(PLAYER_Y, stepped_y): if collision_land returned early
    // (rising->airborne), Y is still stepped_y (correct). If it wrote Y,
    // the state already has the correct value. No override needed.
}

/// Ground tile-handler dispatch (cs:[0x7D9B]).
///
/// idx 0 = snap-down-or-fall; 1 = plain land; 2/3/4 = land + slope-shift
/// (motion_mode=idx-1); 5 = conditional land/fall (drop_gate); 6 = special
/// level trigger; 7 = no-op.
pub fn collision_ground_handler(state: &mut NativeGameState, idx: u8, di: u16) {
    match idx {
        0 => {
            // [65EF]
            ground_snap_or_fall(state, di);
        }
        1 => {
            // [6641] plain land
            collision_land(state, di);
        }
        2 | 3 | 4 => {
            // [6657/6660/6669] land + slope shift
            collision_land(state, di);
            state.wb(P_MOTION_MODE, idx - 1); // 2->1, 3->2, 4->3
        }
        5 => {
            // [6645] conditional land/fall
            if state.rb(PENDING_PICKUP) != 0 {
                // [664A] blocked -> fall
                state.wb(P_MOTION_MODE, 0);
                state.wb(AIRBORNE_FLAG, collision_fall(state.rb(AIRBORNE_FLAG)));
            } else {
                // [6651 -> 0x641F]
                collision_land(state, di);
                state.wb(P_MOTION_MODE, 0); // [6645] (641F also zeroes it)
            }
        }
        6 => {
            // [65AF] off-camera / special trigger
            offcamera_trigger(state);
        }
        7 => {
            // [6672] no-op
        }
        _ => {
            panic!("ground handler idx {} not recovered", idx);
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Bridge / platform sag (1030:5C7B / 5BB8)
// ════════════════════════════════════════════════════════════════════

/// Bridge sag/spring dirty step (1030:5C7B). For [0x4DF8+tile] >= 1 mark
/// the whole grid dirty. For ==0 the ASM redraws that ONE tile directly
/// (5C8E -> 653D): if on-screen, queue a re-blit and set page-dirty.
fn bridge_dirty(state: &mut NativeGameState, new_tile: u8, off: u16, redraws: &mut Vec<u16>) {
    if dirty_kind(state, new_tile) >= 1 {
        // [5C7B-5C80] jb 5c8e
        state.wb(GRID_DIRTY, 1); // [5C82]
        state.ww(GRID_DIRTY_TOKEN, 0x55AA); // [5C87]
        return;
    }

    // [6546-6558] 653D on-screen test vs camera
    let col = (off & 0xFF) as u8;
    let row = ((off >> 8) & 0xFF) as u8;
    let cam_col = state.rb(CAM_COL_BYTE);
    let cam_row = state.rb(CAM_ROW_BYTE);

    if col.wrapping_sub(cam_col) < 0x14 && row.wrapping_sub(cam_row) < 0x0C {
        redraws.push(off); // [6599 -> 3B77] re-blit the one tile
        state.wb(PAGE_DIRTY, 1); // [659C]
    }
}

/// Bridge/platform sag-under-weight (1030:5BB8..5C01).
///
/// Springs the previously-dipping tile back up (walk graphic id down one
/// frame at a time while it's still a sag frame, 0x805E[id-1] & 0x20),
/// writing+dirtying each, until it clears, then reset dipping_tile=0x55AA.
/// Then, if the new foot tile is a bridge frame (0x805E[id] & 0x20), dip
/// it down (graphic id+1), mark it as the dipping tile, and dirty it.
fn collision_bridge_dip(state: &mut NativeGameState, di: u16, redraws: &mut Vec<u16>) {
    let bab = state.rw(DIPPING_TILE);
    if bab != 0x55AA {
        // [5BBB] something is currently dipping
        let sdi = bab & 0xFFFF;
        let mut cur = read_es(state, sdi); // [5BC8]
        loop {
            cur = cur.wrapping_sub(1); // [5BCD] dec to previous sag frame
            if ceil_handler(state, cur) & 0x20 == 0 {
                // [5BCE-5BD3]
                state.ww(DIPPING_TILE, 0x55AA); // [5BD5] sprung fully back
                break;
            }
            write_es(state, sdi, cur); // [5BDE-5BE0] es:[di] = id-1
            bridge_dirty(state, cur, sdi, redraws); // [5BE3]
        }
    }

    let cur = read_es(state, di); // [5BE8] the new foot tile
    if ceil_handler(state, cur) & 0x20 != 0 {
        // [5BEB-5BF2] is it a bridge frame?
        state.ww(DIPPING_TILE, di); // [5BF4] now dipping
        let nw = cur.wrapping_add(1); // [5BF8] inc -> next sag frame
        write_es(state, di, nw); // [5BFB] es:[di] = id+1
        bridge_dirty(state, nw, di, redraws); // [5BFE]
    }
}

// ════════════════════════════════════════════════════════════════════
// Wall-impact marker (1030:64FA)
// ════════════════════════════════════════════════════════════════════

/// Wall-impact marker registration (1030:64FA): drop (X<<3, Y<<3) into the
/// first free marker slot (free = leading word 0x55AA). Does nothing if the
/// list is full. Never reached in current demos (side-solid 0x805E&0x10
/// tile never occurs; walls block via collision_hblock).
fn wall_marker_push(state: &mut NativeGameState) {
    let px = state.rw(PLAYER_BASE);
    let py = state.rw(PLAYER_Y);

    for slot in 0..10 {
        // [64FD-6529] the 64FA slot scan
        let base = WALL_MARKER_LIST + slot * 8;
        if state.rw(base) == 0x55AA {
            // [64FD] leading word == 0x55AA = free
            state.ww(base, (px as u32).wrapping_shl(3) as u16); // [6505-650A] X<<3
            state.ww(base + 2, (py as u32).wrapping_shl(3) as u16); // [650C-6511] Y<<3
            state.wb(base + 4, 0); // [6514] b4
            state.wb(base + 5, 0); // [6518] b5
            state.wb(base + 7, 0); // [651C] b7
            return;
        }
    }
    // [6522/6529] list full -> nothing
}

// ════════════════════════════════════════════════════════════════════
// Side-collision dispatch (cs:[0x7D95])
// ════════════════════════════════════════════════════════════════════

/// Horizontal/body side-collision dispatch (cs:[0x7D95]).
///
/// idx 0 = if tile is side-solid (0x805E[tile] & 0x10), register wall
/// marker; idx 1 = wall block (collision_hblock); idx 2 = hazard trigger;
/// idx 3-8 = ground handlers (unwitnessed in side scan).
pub fn collision_side_handler(state: &mut NativeGameState, idx: u8, di: u16) {
    match idx {
        0 => {
            // [652C]
            let tile = read_es(state, di);
            if ceil_handler(state, tile) & 0x10 != 0 {
                // [6531] side-solid -> wall marker
                wall_marker_push(state); // [6536 -> 64FA]
            }
            // [6538] not solid -> nothing
        }
        1 => {
            // [6539] wall block
            let (new_x, new_xvel) = collision_hblock(state.rw(PLAYER_BASE), state.rw(P_XVEL));
            state.ww(PLAYER_BASE, new_x);
            state.ww(P_XVEL, new_xvel);
        }
        2 => {
            // [65AF -> 65B3] hazard tile (e.g. head-spike)
            offcamera_trigger(state);
        }
        3..=8 => {
            // ground handlers, unwitnessed in side scan
            panic!(
                "side scan ground handler idx {} (0x7D95) not witnessed",
                idx
            );
        }
        _ => {
            panic!("side handler idx {} (0x7D95) not recovered", idx);
        }
    }
}

/// One vertical scan-loop cell (5C92 first / 5CAC rest).
/// `conditional` = only dispatch tile types 2 & 4 (for subsequent cells).
fn side_scan(state: &mut NativeGameState, cell: u16, conditional: bool) {
    let tile = read_es(state, cell); // [5C97 / 5CB1]
    let idx = ceil_props(state, tile); // [5C9A / 5CB4] remap 0x7E5E
    if conditional && idx != 2 && idx != 4 {
        // [5CB8-5CBE] 5CAC dispatch filter
        return;
    }
    collision_side_handler(state, idx, cell);
}

// ════════════════════════════════════════════════════════════════════
// Off-camera death/respawn trigger (1030:65B3)
// ════════════════════════════════════════════════════════════════════

/// Off-camera death/respawn trigger (1030:65B3, called as 65AF).
/// If not already triggered (respawn_state == 0): consume a life, reset
/// energy, arm the respawn (respawn_state = 2); if no lives remain, set
/// the game-over flag (end_signal = 1).
fn offcamera_trigger(state: &mut NativeGameState) {
    let respawn = state.rb(RESPAWN_STATE);
    if respawn != 0 {
        // [65B3] already triggered
        return;
    }
    let lives = state.rb(LIVES);
    if lives == 0 {
        // [65BA] no lives left -> game over
        state.wb(END_SIGNAL, 1); // [65D0]
        return;
    }
    state.wb(LIVES, lives - 1); // [65C1]
    state.wb(ENERGY, 0); // [65C5]
    state.wb(RESPAWN_STATE, 2); // [65C9]
}

// ════════════════════════════════════════════════════════════════════
// Tile-interaction worker (1030:5B81)
// ════════════════════════════════════════════════════════════════════

/// The tile-interaction worker (1030:5B81). `cell_bx` = the tile one row
/// above the foot. Off-top (Y<=-1) + foot-tile remap + bridge-dip + ground
/// dispatch + ceiling.
fn collision_worker(state: &mut NativeGameState, cell_bx: u16, redraws: &mut Vec<u16>) {
    let py = s16(state.rw(PLAYER_Y) as i32);

    if py <= -1 {
        // [5B84 cmp Y,-1 / 5B89 jg 5B96]
        // Off-top: above the level ceiling. No tile up here, so skip ALL
        // tile interaction and just run in-air physics, mark firmly
        // airborne, and return.
        collision_airborne(state); // [5B8B -> 63B5]
        state.wb(AIRBORNE_FLAG, 0xFF); // [5B8E]
        return; // [5B93]
    }

    let di = cell_bx.wrapping_add(0x100); // [5B97] foot tile
    let map_rows = state.rb(MAP_ROWS) as u16;
    let foot_tile = if map_rows > (di >> 8) {
        // [5B9D-5BA6] map-bounds clamp
        read_es(state, di)
    } else {
        0
    };
    let idx = ground_remap(state, foot_tile); // [5BA8] cs:[0x7D9B] index

    if state.rw(DIPPING_TILE) != di as u16 {
        // [5BB2] not already dipping here -> bridge-dip
        collision_bridge_dip(state, di, redraws); // [5BB8]
    }

    collision_ground_handler(state, idx, di); // [5C04]

    // [5C09]
    let cbx = cell_bx as i32 - 0x100;
    let yvel = state.rw(P_YVEL);
    if cbx >= 0 && s16(yvel as i32) <= 0 {
        // [5C0D jb / 5C0F jg] in-bounds + not falling
        collision_ceiling(state, (cbx & 0xFFFF) as u16); // [5C16]
    }
}

// ════════════════════════════════════════════════════════════════════
// Camera visibility test (1030:5ACD-5B16)
// ════════════════════════════════════════════════════════════════════

/// True when the player is off the visible map (the only branch with side
/// effects).
fn out_of_camera_range(state: &NativeGameState) -> bool {
    let py = s16(state.rw(PLAYER_Y) as i32);
    let px = s16(state.rw(PLAYER_BASE) as i32);
    let cam_row = s16(state.rw(CAM_ROW_WORD) as i32);
    let cam_col = s16(state.rw(CAM_COL_WORD) as i32);
    let map_rows = state.rb(MAP_ROWS) as i32;
    let level_flags = state.rb(LEVEL_FLAGS);

    // [5ACD-5ADD]
    if ((py >> 4) - cam_row).abs() > 0x0B {
        return true;
    }
    // [5ADF-5AEF]
    if ((px >> 4) - cam_col).abs() > 0x14 {
        return true;
    }
    // [5AF1-5B01]
    if level_flags & 4 != 0 && py < ((cam_row << 4) & 0xFFFF) {
        return true;
    }
    // [5B03] above the top -> in range (no trigger)
    if py < 0 {
        return false;
    }
    // [5B0A-5B16] below the map bottom
    py > (((map_rows + 1) << 4) & 0xFFFF)
}

// ════════════════════════════════════════════════════════════════════
// Full collision entry point (1030:5A96)
// ════════════════════════════════════════════════════════════════════

/// The full player ground/tile collision (1030:5A96), called from the
/// player update at 5A41 after the Y integrate.
///
/// Computes the player's tile cell from X/Y, range-checks vs the camera,
/// runs the tile-interaction worker (bridge-dip + ground dispatch +
/// ceiling), resolves the post-worker fall/land state, then scans the
/// player's vertical extent for horizontal/body collisions.
///
/// Returns the list of tile offsets a bridge-dip queued for a direct
/// on-page re-blit (the 653D render side-effect). DS and map writes are
/// applied directly to `state`.
pub fn collision(state: &mut NativeGameState) -> Vec<u16> {
    let mut redraws: Vec<u16> = Vec::new();

    let px = s16(state.rw(PLAYER_BASE) as i32);
    let py = s16(state.rw(PLAYER_Y) as i32);

    // --- tile cell + scan parameters [5A99-5AC4] ---
    let row_m1 = ((py >> 4) - 1) & 0xFFFF; // (Y>>4) - 1
    // [5AA4] xchg al,ah — byte-swap row_m1
    let cell_high = (((row_m1 & 0xFF) << 8) | ((row_m1 >> 8) & 0xFF)) as i32;
    // [5AC5-5ACB] X>>4 + (row<<8)
    let cell_bx = (((px >> 4) + cell_high) & 0xFFFF) as u16;

    // [5AA6-5AAD] anim frame *2
    let anim_idx = ((state.rw(P_SPRITE) & 0x1FFF) << 1) as usize;
    let dh = player_anim_height(state, anim_idx); // [5AAF] player vertical extent

    let xvel_s = s16(state.rw(P_XVEL) as i32);
    // [5AB3-5AC1] leading-edge X offset
    let x_edge: i32 = if xvel_s > 0 {
        9
    } else if xvel_s < 0 {
        -9
    } else {
        0
    };

    // --- camera range check [5ACD-5B16] ---
    if out_of_camera_range(state) {
        let level = state.rb(LEVEL);
        if level == 0x0E {
            // [5B18] the final level: falling out = THE END
            state.wb(END_SIGNAL, 0xFF); // [5B1F]
        } else {
            offcamera_trigger(state); // [5B26 -> 65B3]
        }
    }

    state.wb(AIRBORNE_FLAG, 0); // [5B29] clear the airborne flag

    // [5B2E] worker: tile interaction
    collision_worker(state, cell_bx, &mut redraws);

    // --- post-worker fall / land [5B31-5B54] ---
    let airborne = state.rb(AIRBORNE_FLAG);
    if airborne == 1 {
        // [5B31] airborne after the worker
        if state.rb(UNK_6BFE) == 0 {
            // [5B38]
            collision_airborne(state); // [5B44 -> 63B5]
            if s16(state.rw(P_YVEL) as i32) > 0 {
                // [5B47] still descending
                let ff = state.rb(FALL_FRAMES);
                state.wb(FALL_FRAMES, ff.wrapping_add(1)); // [5B4E]
            }
        } else {
            // [5B3F -> 64DF] soft-land tail
            state.wb(FALL_GRACE, dec_floor8(state.rb(FALL_GRACE)));
            state.wb(FALL_LATCH, 0);
            state.wb(AIRBORNE_FLAG, 2);
            state.ww(LAST_LAND_Y, state.rw(PLAYER_Y));
            state.wb(FALL_FRAMES, 0);
        }
    } else {
        state.wb(FALL_FRAMES, 0); // [5B54]
    }

    // --- vertical side-collision scan [5B5B-5B7B] ---
    if s16(state.rw(PLAYER_Y) as i32) > 0 {
        // [5B5B]
        // [5B62-5B66]
        let bx = (((px + x_edge) >> 4) & 0xFFFF) as u16;
        // [5B68]
        let mut bx = (bx.wrapping_add(cell_high as u16)) & 0xFFFF;

        // [5B6A] first cell — unconditional
        side_scan(state, bx, false);

        let mut dh_left = dh;
        loop {
            if bx < 0x100 {
                // [5B72-5B76] sub bx,0x100 ; jb
                break;
            }
            bx = bx.wrapping_sub(0x100);
            dh_left = dh_left.wrapping_sub(0x10); // [5B78]
            if dh_left == 0 || dh_left > 0x80 {
                // [5B7B] ja (unsigned >0 after sub means no borrow)
                break;
            }
            // [5B6F] subsequent cells — conditional
            side_scan(state, bx, true);
        }
    }

    redraws
}
