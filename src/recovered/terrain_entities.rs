// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 terrain / moving-entity system (1030:4907).
//!
//! 16-slot source list at 0x9107 (stride 0xF): falling objects (type 8) + 8-dir platforms.
//! Projects on-screen entities into render array 0x5570 (stride 0x12, budget 7),
//! runs player-ride collision (4B05) using the hitbox overlap test (8D7B).
//!
//! All functions use the direct NativeGameState API (state.rb/rw/wb/ww) — no dict-based
//! interface. All operations mutate `NativeGameState` directly in place
//! so subsequent reads immediately observe earlier writes.

use crate::engine::state::NativeGameState;
use crate::recovered::combat_interaction::hitbox_overlap;

// --- entity / render list layout ---
const ENTITY_LO: usize = 0x9107;
const ENTITY_N: usize = 0x10;
const SRC_STRIDE: usize = 0xF;
const RENDER_LO: usize = 0x5570;
const RENDER_STRIDE: usize = 0x12;
const RENDER_BUDGET: usize = 7;

// --- player struct (base 0x4F1C — render slot #1 with kinematics appended) ---
const PLAYER_BASE: usize = 0x4F1C;
const PLAYER_X: usize = 0x4F1C;
const PLAYER_Y: usize = 0x4F1E;
const PLAYER_SPRITE: usize = 0x4F20;
const PLAYER_MOTION_MODE: usize = 0x4F24;
const PLAYER_YVEL: usize = 0x4F2A;

// --- player globals (DGROUP) ---
const ENTITY_VX_SCRATCH: usize = 0x6BFA;
const ENTITY_VY_SCRATCH: usize = 0x6BFC;
const UNK_6BFE: usize = 0x6BFE;
const FALL_GRACE: usize = 0x6BE0;
const FALL_LATCH: usize = 0x6BD1;
const AIRBORNE: usize = 0x6BF3;
const LAST_LAND_Y: usize = 0x6BCA;
const MAP_ROWS: usize = 0x2CF5;
const CAM_COL_WORD: usize = 0x2DE4;
const CAM_ROW_WORD: usize = 0x2DE6;
const MAP_SEG: usize = 0x2DDA;

// --- TerrainEntity field offsets (relative to base, stride 0xF) ---
// Two movement families share the record:
//   type-8 fall/settle:  speed_ramp[+7], state[+0xA], fall_vel[+0xB word],
//                        settle_timer[+0xD], settle_reload[+9]
//   8-dir platform:      speed_ramp[+7] (target), speed[+0xE] (current, ramped toward [+7]),
//                        dwell as WORDS at [+7]/[+0xA]/[+0xC] (a third interpretation)
const TE_X: usize = 0x00;
const TE_Y: usize = 0x02;
const TE_SPRITE: usize = 0x04;
const TE_TYPE_DIR: usize = 0x06;
const TE_SPEED_RAMP: usize = 0x07;
const TE_SETTLE_RELOAD: usize = 0x09;
const TE_STATE: usize = 0x0A;
const TE_FALL_VEL: usize = 0x0B;
const TE_SETTLE_TIMER: usize = 0x0D;
const TE_SPEED: usize = 0x0E;

// --- RenderSlot field offsets (relative to base, stride 0x12) ---
const RS_X: usize = 0x00;
const RS_Y: usize = 0x02;
const RS_SPRITE: usize = 0x04;
const RS_SOURCE: usize = 0x09;

// --- tables ---
const FLOOR_PROPS: usize = 0x7F5E;
const SPRITE_GEOM: usize = 0x7190; // (id & 0x1FFF)<<1 -> word: lo=half_w, hi=half_h

// (vx_sign, vy_sign) for direction [+6]&7 — the 4A2A..4A5F negate/zero pattern.
const DIR8: [(i32, i32); 8] = [
    (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1),
];

// --- helpers ---

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
fn s8(v: u8) -> i32 {
    v as i8 as i32
}

/// Arithmetic (sign-preserving) right shift of a 16-bit value.
#[inline]
fn sar16(v: u16, n: u32) -> u16 {
    ((v as i16) >> n) as u16
}

// --- level-map + table readers ---

/// Read a byte from the level-map segment (es = [0x2DDA]).
fn read_tile(state: &NativeGameState, off: u16) -> u8 {
    let map_seg = state.rw(MAP_SEG) as usize;
    let addr = ((map_seg << 4) + (off as usize & 0xFFFF)) & 0xFFFFF;
    if addr < state.data.len() {
        state.data[addr]
    } else {
        0
    }
}

/// Tile id -> ground property (solidity): 0 = empty, 6 = non-solid, else solid.
#[inline]
fn floor_props(state: &NativeGameState, tile: u8) -> u8 {
    state.rb(FLOOR_PROPS + tile as usize)
}

/// Sprite Y half-extent byte = [0x7191 + (id & 0x1FFF) * 2].
#[inline]
fn sprite_half_h(state: &NativeGameState, sprite: u16) -> u8 {
    let idx = ((sprite & 0x1FFF) as usize) << 1;
    state.rb(SPRITE_GEOM + 1 + idx)
}

// --- move_type8 (asm 492D..49F2) -------------------------------------------------------

/// Falling/settling (type-8) entity movement -> source-slot + scratch writes.
///
/// 3-state machine at [+0xA]:
///   0 = wait/brake (vel -= 8/frame, floor 0; when vel hits 0 and bit6 set, settle-timer counts down)
///   1 = fall+accelerate (speed_ramp += 8/frame to cap 0xC0; accel = speed_ramp >> 4;
///       test landing tile; on solid or 3+ rows past bottom -> settle)
///   2 = settle for [+0xD] frames -> back to 0
fn move_type8(state: &mut NativeGameState, b: usize) {
    // [asm 492D] entity_vx_scratch = 0
    state.ww(ENTITY_VX_SCRATCH, 0);

    let vel0 = state.rw(b + TE_FALL_VEL); // fall_vel (word at +0x0B)
    let y_mem = state.rw(b + TE_Y).wrapping_sub(vel0); // [asm 4936] Y -= vel
    state.ww(b + TE_Y, y_mem);

    let type_dir = state.rb(b + TE_TYPE_DIR);
    let state_byte = state.rb(b + TE_STATE);
    let mut s7 = state.rb(b + TE_SPEED_RAMP);
    let mut vel = vel0;

    match state_byte {
        0 => {
            // [asm 4939] brake: vel -= 8, floor at 0
            let nv = s16(vel0 as i32) - 8;
            if nv < 0 {
                vel = 0;
                s7 = 0;
                state.wb(b + TE_SPEED_RAMP, 0); // [asm 4949]
            } else {
                vel = nv as u16;
            }
            state.ww(b + TE_FALL_VEL, vel);
            if type_dir & 0x40 != 0 {
                // [asm 4950] hold flag set — count down settle_timer
                let mut settle = true;
                if s7 == 0 {
                    // [asm 4956] only when speed_ramp already 0
                    let t = state.rb(b + TE_SETTLE_TIMER).wrapping_sub(1);
                    state.wb(b + TE_SETTLE_TIMER, t);
                    if t != 0 {
                        settle = false;
                    }
                }
                if settle {
                    state.wb(b + TE_STATE, 1); // [asm 4961] -> falling
                    state.wb(b + TE_SPEED_RAMP, 0);
                }
            }
        }
        1 => {
            // [asm 496B] fall + accelerate
            if s7 < 0xC0 {
                // [asm 4976] ramp acceleration
                s7 = s7.wrapping_add(8);
                state.wb(b + TE_SPEED_RAMP, s7);
            }
            let accel = (s7 >> 4) as u16; // [asm 4983]
            state.ww(ENTITY_VY_SCRATCH, accel); // [asm 4985]
            vel = vel0.wrapping_add(accel); // [asm 4988]
            state.ww(b + TE_FALL_VEL, vel);

            let col = (sar16(state.rw(b + TE_X), 4) & 0xFF) as u16; // [asm 498F]
            let nrow = sar16(y_mem.wrapping_add(vel), 4); // [asm 4993]
            let map_rows = state.rb(MAP_ROWS) as u16;
            let bound = map_rows.wrapping_sub(1); // [asm 499B-49A0]
            let mut settle = false;
            if bound >= nrow {
                // [asm 49A3] jae — in bounds: test the landing tile
                let tile_addr = col | ((nrow & 0xFF) << 8);
                let tile = read_tile(state, tile_addr); // [asm 49B0]
                let prop = floor_props(state, tile);
                if prop != 0 && prop != 6 {
                    // [asm 49B5-49C1] solid
                    settle = true;
                }
            } else if nrow.wrapping_sub(bound) >= 3 {
                // [asm 49A5-49AC] 3+ rows past bottom
                settle = true;
            }
            if settle {
                state.wb(b + TE_STATE, 2); // [asm 49C3] -> settle
                state.wb(b + TE_SETTLE_TIMER, 0x16);
            }
        }
        2 => {
            // [asm 49CF] settle
            if type_dir & 0x40 == 0 {
                // [asm 49D5] only when hold flag clear
                let t = state.rb(b + TE_SETTLE_TIMER).wrapping_sub(1);
                state.wb(b + TE_SETTLE_TIMER, t);
                if t == 0 {
                    state.wb(b + TE_STATE, 0); // [asm 49E0] -> wait
                    state.wb(b + TE_SETTLE_TIMER, state.rb(b + TE_SETTLE_RELOAD));
                }
            }
        }
        _ => {}
    }
    state.ww(b + TE_Y, y_mem.wrapping_add(vel)); // [asm 49EA] Y += vel
}

// --- move_default (asm 49F3..4A72) ----------------------------------------------------

/// 8-direction moving-platform movement -> source-slot + scratch writes.
///
/// Speed at [+0xE] ramps toward target at [+7] (±1/frame). Velocity = s8(speed) * dir8.
/// When speed reaches target, enter dwell/oscillate: count up [+0xC] (word) toward
/// [+0xA] (word); on match, negate the WORD at [+7] (reverse direction) and reset count.
fn move_default(state: &mut NativeGameState, b: usize) {
    // [asm 49F5/49F8] entity_vx_scratch = 0, entity_vy_scratch = 0
    state.ww(ENTITY_VX_SCRATCH, 0);
    state.ww(ENTITY_VY_SCRATCH, 0);

    let spd = state.rb(b + TE_SPEED); // +0x0E — current speed
    let s7 = state.rb(b + TE_SPEED_RAMP); // +0x07 — target speed
    let type_dir = state.rb(b + TE_TYPE_DIR);

    // [asm 49FB-4A0B] stationary early-out
    if spd == 0 && s8(s7) >= 0 && type_dir & 0xC0 == 0 {
        return;
    }

    // [asm 4A0D] ramp speed toward [+7]
    let mut al = spd;
    if s7 != al {
        let step = if s8(s7) >= s8(al) { 1 } else { -1 };
        al = (al as i32 + step) as u8;
        state.wb(b + TE_SPEED, al);
    }

    let dir = (type_dir & 7) as usize;
    let (sx, sy) = DIR8[dir];
    let vx = ((s8(al) * sx) & 0xFFFF) as u16;
    let vy = ((s8(al) * sy) & 0xFFFF) as u16;
    state.ww(ENTITY_VX_SCRATCH, vx); // [asm 4A64]
    state.ww(b + TE_X, state.rw(b + TE_X).wrapping_add(vx)); // [asm 4A67] X += vx
    state.ww(ENTITY_VY_SCRATCH, vy); // [asm 4A6C]
    state.ww(b + TE_Y, state.rw(b + TE_Y).wrapping_add(vy)); // [asm 4A6F] Y += vy

    // [asm 4A72] at target -> dwell/oscillate
    // The dwell tail reads/writes [+7]/[+0xA]/[+0xC] as full WORDS — a third
    // interpretation of the same storage (speed_ramp/state/osc as bytes vs.
    // speed_word/dwell_target/dwell_count as words).
    if s7 == al {
        let cnt = state.rw(b + 0x0C).wrapping_add(1); // dwell_count (word at +0xC)
        let target = state.rw(b + 0x0A); // dwell_target (word at +0xA)
        if target != cnt {
            // [asm 4A7E] not yet at target — increment count
            state.ww(b + 0x0C, cnt);
        } else {
            // [asm 4A83] negate WORD [si+7] (reverse direction) and reset count
            let speed_word = state.rw(b + 0x07); // speed_word (word at +7)
            state.ww(b + 0x07, (0u16).wrapping_sub(speed_word));
            state.ww(b + 0x0C, 0);
        }
    }
}

// --- collision_4b05 (asm 4B05) --------------------------------------------------------

/// Player-ride collision. Writes player state on contact; returns true if the player rode.
///
/// The hitbox probe uses a SIMULATED player Y (real Y + entity_vy if entity moving down)
/// and a fixed sprite id of 7 — a hypothetical position test, not a real state write.
/// We temporarily write these to state, call hitbox_overlap, then restore the real values.
/// The player's real yvel (at 0x4F2A) is NOT overridden — hitbox_overlap reads it for the
/// vertical-detail gate.
fn collision_4b05(state: &mut NativeGameState, di: usize) -> bool {
    // [asm 4B08] gate: already rode this frame
    if state.rb(UNK_6BFE) != 0 {
        return false;
    }

    let ride_y = state.rw(di + RS_Y);
    let py = state.rw(PLAYER_Y);
    // [asm 4B0F-4B15] entity must be below the player (Y larger = further down)
    if s16(ride_y as i32) <= s16(py as i32) {
        return false;
    }

    let saved_y = py;
    let yadj = state.rw(ENTITY_VY_SCRATCH);
    // [asm 4B1E-4B25] simulate player Y if entity is moving down
    let temp_y = if s16(yadj as i32) >= 0 {
        saved_y.wrapping_add(yadj)
    } else {
        saved_y
    };

    // [asm 4B29] override player Y=temp_y, sprite=7 for the hitbox probe
    // Save originals, write simulated values, call hitbox, restore.
    let orig_y = state.rw(PLAYER_Y);
    let orig_sprite = state.rw(PLAYER_SPRITE);
    state.ww(PLAYER_Y, temp_y);
    state.ww(PLAYER_SPRITE, 7);

    // [asm 4B31] 8D7B — the simulated Y flows through the hitbox test
    let hit = hitbox_overlap(state, PLAYER_BASE, di);

    // Restore the real player Y and sprite (the probe was hypothetical)
    state.ww(PLAYER_Y, orig_y);
    state.ww(PLAYER_SPRITE, orig_sprite);

    if !hit {
        // [asm 4B3B] no contact
        return false;
    }

    // --- contact: snap the player onto the entity ---

    state.wb(UNK_6BFE, 1); // [asm 4B3D]
    state.wb(PLAYER_MOTION_MODE, 0); // [asm 4B42] motion_mode = 0
    let entity_vx = state.rw(ENTITY_VX_SCRATCH);
    let px = state.rw(PLAYER_X);
    state.ww(PLAYER_X, px.wrapping_add(entity_vx)); // [asm 4B47] push player X

    // 64DF landing reset
    let fall_grace = state.rb(FALL_GRACE);
    state.wb(
        FALL_GRACE,
        if fall_grace > 0 { fall_grace - 1 } else { 0 },
    ); // [asm 64DF] saturating dec
    state.wb(FALL_LATCH, 0);
    state.wb(AIRBORNE, 2);
    state.ww(LAST_LAND_Y, state.rw(PLAYER_Y)); // real player Y (before snap)

    // [asm 4B51-4B61] snap onto the entity top or inherit its velocity
    let ride_y2 = state.rw(di + RS_Y);
    let ride_sprite = state.rw(di + RS_SPRITE);
    let half_h = sprite_half_h(state, ride_sprite) as u16;
    let top = ride_y2.wrapping_sub(half_h);
    let py2 = state.rw(PLAYER_Y); // real player Y
    if s16(py2 as i32) <= s16(top as i32) {
        // [asm 4B76] player above or at entity top — inherit entity velocity
        let entity_vy = state.rw(ENTITY_VY_SCRATCH);
        state.ww(PLAYER_YVEL, ((entity_vy as u32) << 4) as u16);
    } else {
        // [asm 4B6A] player below top — snap to top + 1
        state.ww(PLAYER_Y, top.wrapping_add(1));
        state.ww(PLAYER_YVEL, 1);
    }

    true
}

// --- tick_terrain_entities (asm 4907) ------------------------------------------------

/// Per-frame tick of the 16-slot terrain-entity list 0x9107 (source stride 0xF):
/// per live slot move (type 8 = falling/settling 3-state object; else 8-direction moving
/// platform), project on-screen slots into the render array 0x5570 (stride 0x12, budget 7,
/// vs camera [0x2DE4]/[0x2DE6]), and run the player-ride collision 4B05 (hitbox 8D7B;
/// snap player onto the entity top, inherit velocity).
pub fn tick_terrain_entities(state: &mut NativeGameState) {
    // [asm 4913] unk_6BFE = 0
    state.wb(UNK_6BFE, 0);

    let mut si = ENTITY_LO;
    let mut di = RENDER_LO;
    let mut budget = RENDER_BUDGET;

    for _ in 0..ENTITY_N {
        let sprite = state.rw(si + TE_SPRITE);
        if sprite == 0xFFFF {
            // [asm 4918] inactive -> di NOT advanced
            si += SRC_STRIDE;
            continue;
        }

        let type_dir = state.rb(si + TE_TYPE_DIR);
        if type_dir & 0xF == 8 {
            // [asm 4921] dispatch
            move_type8(state, si);
        } else {
            move_default(state, si);
        }

        // Re-read entity position after move (the move functions modified X/Y)
        let ent_x = state.rw(si + TE_X);
        let ent_y = state.rw(si + TE_Y);

        // [asm 4A8B] screen X = ent.x - (cam_col_word << 4)
        let cam_col = state.rw(CAM_COL_WORD);
        let cam_px_x = ((cam_col as u32) << 4) as u16;
        let sx = s16(ent_x.wrapping_sub(cam_px_x) as i32);
        // [asm 4A9D-4AA6] on-screen X: not (>= 0x160 or <= -0x20)
        if !(sx >= 0x160 || sx <= -0x20) {
            // [asm 4AA8] write worldX (even if Y is off-screen)
            state.ww(di + RS_X, ent_x);

            // [asm 4AAA] screen Y = ent.y - (cam_row_word << 4)
            let cam_row = state.rw(CAM_ROW_WORD);
            let cam_px_y = ((cam_row as u32) << 4) as u16;
            let sy = s16(ent_y.wrapping_sub(cam_px_y) as i32);
            // [asm 4ABD-4AC6] on-screen Y: not (>= 0xD0 or <= -0x20)
            if !(sy >= 0xD0 || sy <= -0x20) {
                state.ww(di + RS_Y, ent_y); // [asm 4AC8]
                state.ww(di + RS_SPRITE, sprite); // [asm 4ACB]
                state.ww(di + RS_SOURCE, si as u16); // [asm 4AD1] back-ref to source slot

                let mut collided = false;
                let pyvel = s16(state.rw(PLAYER_YVEL) as i32);
                if pyvel > -0x10 {
                    // [asm 4AD4] jle skips the collision
                    // [asm 4ADB] set hold flag
                    state.wb(si + TE_TYPE_DIR, state.rb(si + TE_TYPE_DIR) | 0x40);
                    collided = collision_4b05(state, di); // [asm 4ADF]
                }
                if !collided {
                    // [asm 4AE2] jb skips this
                    // [asm 4AE4] clear hold flag
                    state.wb(si + TE_TYPE_DIR, state.rb(si + TE_TYPE_DIR) & 0xBF);
                    // [asm 4AE8] nudge render Y up 2px
                    let dy = state.rw(di + RS_Y);
                    state.ww(di + RS_Y, dy.wrapping_sub(2));
                }
                di += RENDER_STRIDE; // [asm 4AEC]
                budget -= 1;
            }
        }
        si += SRC_STRIDE;
    }

    // [asm 4AF9] terminate the remaining render slots
    while budget > 0 {
        state.ww(di + RS_SPRITE, 0xFFFF); // dead marker
        di += RENDER_STRIDE;
        budget -= 1;
    }
}
