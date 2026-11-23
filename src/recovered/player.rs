// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 player FSM — recovered native logic.
//!
//! Pure leaf primitives + FSM frontend + all state handlers operating on NativeGameState.

use crate::engine::state::NativeGameState;

// ════════════════════════════════════════════════════════════════════
// Constants
// ════════════════════════════════════════════════════════════════════

pub const PLAYER_BASE: usize = 0x4F1C;
pub const RENDER_SLOTS_BASE: usize = 0x4F0A;

// Player struct fields (relative to PLAYER_BASE = 0x4F1C)
pub const P_SPRITE: usize       = PLAYER_BASE + 0x04; // 0x4F20 packed anim-frame word
pub const P_FLAGS: usize        = P_SPRITE + 1;       // 0x4F21 sprite high byte (flags)
pub const P_XVEL: usize         = PLAYER_BASE + 0x06; // 0x4F22 X velocity (12.4 fixed)
pub const P_MOTION_MODE: usize  = PLAYER_BASE + 0x08; // 0x4F24 kinematics mode/shift
pub const P_FACING: usize       = PLAYER_BASE + 0x09; // 0x4F25 facing +1/-1
pub const P_ANIM_B: usize       = PLAYER_BASE + 0x0B; // 0x4F27 anim B-state
pub const P_ANIM_PTR: usize     = PLAYER_BASE + 0x0C; // 0x4F28 anim-script pointer
pub const P_YVEL: usize         = PLAYER_BASE + 0x0E; // 0x4F2A Y velocity (12.4 fixed)
pub const P_RUN: usize          = PLAYER_BASE + 0x10; // 0x4F2C run-state flag
pub const P_DEATH_STATE: usize  = PLAYER_BASE + 0x11; // 0x4F2D death/hurt state

// Render slot 0 (below the player slot)
pub const SLOT0_X: usize     = RENDER_SLOTS_BASE;       // 0x4F0A
pub const SLOT0_Y: usize     = RENDER_SLOTS_BASE + 2;   // 0x4F0C
pub const SLOT0_SPRITE: usize = RENDER_SLOTS_BASE + 4;   // 0x4F0E

// Player globals
pub const SLOT0_DEATH: usize    = RENDER_SLOTS_BASE + 0x11; // 0x4F1B slot-0 life/death byte
pub const POPUP_RING_HEAD: usize = 0x6BBE;   // popup/effect ring write cursor / trail ring ptr
pub const RUN_COUNT: usize      = 0x6BEB;    // run-frames counter (inc-wrap-to-1)
pub const FIDGET_RANGE_TABLE: usize = 0x79E0; // idle-fidget anim threshold ranges [lo,hi)
pub const IDLE_TIMER: usize     = 0x6BD3;    // idle/fidget clock gate
pub const ANIM_GATE: usize      = 0x6BD0;    // hold-current-anim / FSM-route gate
pub const ANIM_HI: usize        = 0x6BCF;    // advance_anim raw frame high byte (bcf)
pub const FRAME_BLINK: usize    = 0x6BD5;    // frame counter gating trail emit to every 4th frame
pub const FRICTION: usize       = 0x6BF6;    // per-level directional-friction constant
pub const CAM_LEFT: usize       = 0x8164;    // camera-left tile (X-integrate right bound)
pub const CAM_COL_WORD: usize   = 0x2DE4;    // camera tile column (word read for range check)
pub const IDLE_CLOCK: usize     = 0x27F0;    // PIT-fed idle counter (fidget selector reads &0x1FF)
pub const UNK_6BD9: usize       = 0x6BD9;    // nonzero suppresses idle camera-pan

// Per-frame countdown timers
pub const CHARGE: usize         = 0x6BCE;    // +2-while-<=0x30 charge counter
pub const INPUT_SUPPRESS: usize = 0x6BCD;    // nonzero forces input bitmask to 0
pub const SHAKE_MAGNITUDE: usize = 0x6BEA;   // camera-shake kick
pub const TIMER_6BE8: usize    = 0x6BE8;    // countdown timer
pub const RESPAWN_STATE: usize = 0x6BE4;    // 0=none, 2=armed by off-camera death
pub const PENDING_PICKUP: usize = 0x6BE1;   // drop_gate: nonzero=fall through ground
pub const REWARD_ARM_HI: usize = 0x6C00;    // bonus-collect flash timer
pub const TIMER_WORD: usize    = 0x6BE2;    // word countdown timer (also scale_level)

pub const TIMER_BYTES: [usize; 7] = [
    CHARGE, INPUT_SUPPRESS, SHAKE_MAGNITUDE, TIMER_6BE8,
    RESPAWN_STATE, PENDING_PICKUP, REWARD_ARM_HI,
];

// Flying / glider
pub const GLIDER_GATE: usize    = 0x6BC5;    // nonzero => glider/flying mode active
const LOW_GRAVITY: usize     = 0x6BC7;    // glide/descent flag (bit0 armed, bit1 or'd)
const FLY_TIMER: usize      = 0x6BC8;    // glider hold/flight counter
pub const FLY_HOLD: usize       = 0x6BC6;    // glider hold budget
pub const GLIDER_TILT: usize    = 0x7B1A;    // tilt/pitch 0..6, neutral=3
pub const GLIDER_ANIM_TABLE: usize = 0x7B29; // {frame_key, wing_anim, xoff, yoff} stride 6
pub const GLIDER_TILT_TABLE: usize = 0x7B1B; // 7-word tilt -> wing-anim table

// Fall / landing (FALL_FRAMES/FALL_LATCH/FALL_GRACE also in player_collision;
// kept private here to avoid glob-import ambiguity)
const FALL_FRAMES: usize    = 0x6BD2;    // fall counter (classifies landing impact)
const FALL_LATCH: usize     = 0x6BD1;    // jump arc frame counter / cleared on landing
const FALL_GRACE: usize      = 0x6BE0;    // =6 while falling; routes jump to idle
pub const UNK_6BFE: usize       = 0x6BFE;    // post-worker: nonzero -> soft-land tail

// Input flags (DC1 outputs)
pub const INPUT_LR: usize   = 0x6BDB;        // left|right held
pub const INPUT_UD: usize   = 0x6BDC;        // up|down held
pub const IN_FIRE: usize    = 0x27E8;        // fire/jump held
pub const IN_AUX: usize     = 0x27E9;        // sixth flag (idle-gate input)
pub const IN_UP: usize      = 0x27EA;        // up held
pub const IN_DOWN: usize    = 0x27EB;        // down held
pub const IN_RIGHT: usize   = 0x27EC;        // right held
pub const IN_LEFT: usize    = 0x27ED;        // left held

// Level / game state
pub const LEVEL_FLAGS: usize = 0x8166;       // bit1 = no idle camera-pan
pub const ATTACK_PHASE: usize = 0x7B18;     // index into 5-byte attack phase records
pub const ATTACK_V19: usize   = 0x7B19;      // loaded from phase.v19 (x4 when charged)
const LIVES: usize        = 0x27D8;      // remaining player lives
const ENERGY: usize        = 0x27D6;      // hit-points within a life
const END_SIGNAL: usize   = 0x6BE5;      // 1=game over, 0xFF=game complete

// Tables
pub const ANIM_SEQ_TABLE: usize   = 0x7CDF;  // per-state animation-sequence pointer table
pub const ANIM_ID_TABLE: usize    = 0x7B7F;  // input-bitmask -> anim_id table

// Trail
pub const TRAIL_RING_LO: usize   = 0x4F76;   // lowest trail-ring slot
pub const TRAIL_RING_HI: usize   = 0x4FBE;    // wrap target (highest trail-ring slot)
pub const TRAIL_STRIDE: usize    = 0x12;     // trail-ring slot stride
pub const TRAIL_SPRITE: u16      = 0x35;     // trail sprite id

// Jump / attack
pub const RUN_ACCEL_LIMIT: u16    = 0x50;     // run state horizontal speed cap
pub const JUMP_IMPULSE_TABLE: usize = 0x79CE; // 9 words of per-frame Yvel impulse
pub const JUMP_FRAMES: u8        = 9;        // frames driven by impulse table
pub const ATTACK_PHASE_TABLE: usize = 0x7B04; // 5-byte per-phase records
pub const ATTACK_SPAWN_LIST: usize  = 0x4F2E; // projectile list (4 slots, stride 0x12)

// World bounds
pub const X_MIN: i32 = 0x0008;
pub const X_MAX: i32 = 0x0FF8;
pub const VIEW_TILES: i32 = 0x14;
pub const XVEL_FLOOR: i32 = -0x60;

// ════════════════════════════════════════════════════════════════════
// Scroll request (idle look-around camera pan)
// ════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDir {
    Left,
    Right,
}

// ════════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════════

#[inline]
fn s16(v: i32) -> i32 { let v = v & 0xFFFF; if v & 0x8000 != 0 { v - 0x10000 } else { v } }

#[inline]
fn s8(v: i32) -> i32 { let v = v & 0xFF; if v & 0x80 != 0 { v - 0x100 } else { v } }

/// Saturating decrement (sub v,1 ; adc v,0) for a byte counter — clamps at 0.
#[inline]
fn dec_floor(v: u8) -> u8 { if v > 0 { v - 1 } else { 0 } }

/// Saturating increment (add v,1 ; sbb v,0) for a byte counter — caps at 0xFF.
#[inline]
fn sat_inc(v: u8) -> u8 { if v == 0xFF { 0xFF } else { v + 1 } }

/// Increment-wrap word (add v,1 ; adc v,0) — wraps 0xFFFF to 1.
#[inline]
fn inc_wrap_word(v: u16) -> u16 {
    if v == 0xFFFF { 1 } else { v + 1 }
}

/// Grow [0x6BCE] by 2 while <= 0x30 (1030:5EB7).
#[inline]
fn player_charge_6bce(v: u8) -> u8 {
    if v <= 0x30 { v.wrapping_add(2) } else { v }
}

// ════════════════════════════════════════════════════════════════════
// Pure leaf primitives
// ════════════════════════════════════════════════════════════════════

/// Player horizontal kinematics (1030:5A0F..5A33).
/// new_x = x + sar(xvel, 4); commits only if inside world bounds.
pub fn player_x_integrate(x: u16, xvel: u16, cam_left: u16) -> u16 {
    let new_x = (s16(x as i32) + (s16(xvel as i32) >> 4)) & 0xFFFF;
    let bound = ((s16(cam_left as i32) + VIEW_TILES) << 4) & 0xFFFF;
    if s16(bound) > s16(new_x) && s16(new_x) >= X_MIN && s16(new_x) < X_MAX {
        new_x as u16
    } else {
        x
    }
}

/// Player vertical kinematics (1030:5A36..5A3D).
/// new_y = y + sar(yvel, 4); unconditional.
pub fn player_y_integrate(y: u16, yvel: u16) -> u16 {
    ((s16(y as i32) + (s16(yvel as i32) >> 4)) & 0xFFFF) as u16
}

/// Player horizontal accelerator (1030:62B1).
pub fn player_accel(xvel: u16, facing: u16, shift: u32, input_held: bool, limit: u16) -> u16 {
    let step = if input_held { s16(((facing as i32) << 4) & 0xFFFF) >> shift } else { 0 };
    let dx = s16((s16(xvel as i32) + step) & 0xFFFF);
    let lim = s16(limit as i32);
    let clamped = if dx >= lim { lim } else if dx <= -lim { -lim } else { dx };
    (clamped & 0xFFFF) as u16
}

/// Player directional friction (1030:62EC).
pub fn player_friction_dir(xvel: u16, force: u16) -> u16 {
    let nv = s16((s16(xvel as i32) - (force as i32 >> 3)) & 0xFFFF);
    let clamped = if nv < XVEL_FLOOR { XVEL_FLOOR } else { nv };
    (clamped & 0xFFFF) as u16
}

/// Player symmetric friction (1030:6333).
pub fn player_friction_sym(xvel: u16, shift: u32) -> u16 {
    let a = s16(xvel as i32);
    let neg = a < 0;
    let mut a = if neg { -a } else { a };
    a -= (0xC >> shift) as i32;
    if a < 0 { a = 0; }
    (if neg { -a } else { a } & 0xFFFF) as u16
}

/// Player gravity (1030:6309).
pub fn player_gravity(yvel: u16, low_gravity: u8, limit: u16) -> u16 {
    let (grav, term) = if low_gravity == 1 {
        (4i32, s16(limit as i32) >> 3)
    } else {
        (0x10i32, s16(limit as i32))
    };
    let nv = s16((s16(yvel as i32) + grav) & 0xFFFF);
    let clamped = if nv >= term { term } else { nv };
    (clamped & 0xFFFF) as u16
}

// ════════════════════════════════════════════════════════════════════
// Timer tail (1030:5A4A-5A87)
// ════════════════════════════════════════════════════════════════════

/// Decrement each of the 7 byte timers + 1 word timer, clamped at 0.
/// Returns (new_byte_values, new_word_value).
pub fn player_tick_timers(timers: &[(usize, u8)], timer_word: u16) -> (Vec<(usize, u8)>, u16) {
    let mut out = Vec::with_capacity(timers.len());
    for &(off, v) in timers {
        out.push((off, dec_floor(v)));
    }
    let new_word = if timer_word > 0 { timer_word - 1 } else { 0 };
    (out, new_word)
}

// ════════════════════════════════════════════════════════════════════
// Animation helpers
// ════════════════════════════════════════════════════════════════════

/// Animation-sequence selector (1030:635D / 6374).
/// If anim_id differs from cur_state, load a new sequence pointer.
/// Returns (new_state, new_ptr).
pub fn player_set_anim(state: &NativeGameState, anim_id: u8, seq_index: usize, cur_state: u8, cur_ptr: u16) -> (u8, u16) {
    if cur_state != anim_id {
        let ptr = state.rw(ANIM_SEQ_TABLE + seq_index);
        (anim_id, ptr)
    } else {
        (cur_state, cur_ptr)
    }
}

/// Animation stepper (1030:638B).
/// Returns (frame_0x4F20, new_ptr_0x4F28, bcf_0x6BCF).
pub fn player_advance_anim(state: &NativeGameState, mut anim_ptr: u16, facing: u8) -> (u16, u16, u8) {
    let mut ax = state.rw(anim_ptr as usize);
    if ax & 0x8000 != 0 {
        // negative = loop marker: rewind and re-read
        let delta = s16(ax as i32);
        anim_ptr = ((anim_ptr as i32 + delta) & 0xFFFF) as u16;
        ax = state.rw(anim_ptr as usize);
    }
    let bcf = ((ax >> 8) & 0xFF) as u8;
    let ah = (bcf & 0x1F) | (facing & 0x80);
    let frame = ((ah as u16) << 8) | (ax & 0xFF);
    let new_ptr = anim_ptr.wrapping_add(2);
    (frame, new_ptr, bcf)
}

// ════════════════════════════════════════════════════════════════════
// Trail emitter (1030:5E11)
// ════════════════════════════════════════════════════════════════════

/// Emit a trail sprite into the ring buffer at [0x6BBE].
/// Gated to every 4th frame (blink & 3 == 0). Returns true if emitted.
pub fn player_emit_trail(state: &mut NativeGameState, blink: u8) -> bool {
    if blink & 3 != 0 { return false; }
    let player_x = state.rw(PLAYER_BASE);
    let player_y = state.rw(PLAYER_BASE + 2);
    let ring_ptr = state.rw(POPUP_RING_HEAD);
    let base = ring_ptr as usize;
    state.ww(base, player_x);           // slot.x
    state.ww(base + 2, player_y);       // slot.y
    state.ww(base + 4, TRAIL_SPRITE);   // slot.sprite (word write!)
    let bx = ring_ptr.wrapping_sub(TRAIL_STRIDE as u16);
    let bx = if (bx as usize) < TRAIL_RING_LO { TRAIL_RING_HI as u16 } else { bx };
    state.ww(POPUP_RING_HEAD, bx);
    true
}

// ════════════════════════════════════════════════════════════════════
// Idle helpers
// ════════════════════════════════════════════════════════════════════

/// Idle helper: set_anim_a (635D, tracks run_flag) then advance_anim (638B).
fn idle_set_advance(state: &mut NativeGameState, anim_id: u8, seq: usize, facing: u8) {
    let run_flag = state.rb(P_RUN);
    let anim_ptr = state.rw(P_ANIM_PTR);
    let (new_state, ptr) = player_set_anim(state, anim_id, seq, run_flag, anim_ptr);
    state.wb(P_RUN, new_state);
    let (frame, new_ptr, bcf) = player_advance_anim(state, ptr, facing);
    state.ww(P_ANIM_PTR, new_ptr);
    state.ww(P_SPRITE, frame);
    state.wb(ANIM_HI, bcf);
}

/// Idle "default" anim path (1030:5DED) — load sequence for entry_bx, write frame 0
/// WITHOUT advancing and WITHOUT the 0x1F mask (only facing bit merged); resets run_flag.
fn idle_default_anim(state: &mut NativeGameState, entry_bx: usize, facing: u8) {
    let ptr = state.rw(ANIM_SEQ_TABLE + entry_bx);
    state.ww(P_ANIM_PTR, ptr);
    let ax = state.rw(ptr as usize);
    let ah = ((ax >> 8) as u8) | (facing & 0x80);
    let frame = ((ah as u16) << 8) | (ax & 0xFF);
    state.ww(P_SPRITE, frame);
    state.wb(P_RUN, 0);
}

// ════════════════════════════════════════════════════════════════════
// State handlers
// ════════════════════════════════════════════════════════════════════

/// Player state: run (anim_id=1, 1030:5EC4).
pub fn player_state_run(state: &mut NativeGameState) {
    // [5ED8-5EF8] FLYING-state block: while gliding at speed (|Xvel|>=0x40),
    // count flying timer up (saturating), drop ONE trail sprite when it hits 0x17.
    let glider = state.rb(GLIDER_GATE);
    let xvel_signed = s16(state.rw(P_XVEL) as i32);
    if glider != 0 && xvel_signed.abs() >= 0x40 {
        let bc8 = sat_inc(state.rb(FLY_TIMER));
        state.wb(FLY_TIMER, bc8);
        if bc8 == 0x17 {
            player_emit_trail(state, 0); // blink=0 = ungated emit
        }
    }

    // [5EF9] idle_timer = sat_inc
    state.wb(IDLE_TIMER, sat_inc(state.rb(IDLE_TIMER)));

    // [5F03-5F06] accel + friction_dir
    let xvel = state.rw(P_XVEL);
    let facing = state.rw(P_FACING);
    let motion_mode = state.rb(P_MOTION_MODE);
    let input_lr = state.rb(INPUT_LR);
    let friction = state.rw(FRICTION);
    let xvel = player_accel(xvel, facing, motion_mode as u32, input_lr != 0, RUN_ACCEL_LIMIT);
    let xvel = player_friction_dir(xvel, friction);
    state.ww(P_XVEL, xvel);

    // [5F0C] set_anim_b: seq is 0x42 if gliding, else 2
    let seq = if glider != 0 { 0x42 } else { 2 };
    let anim_b = state.rb(P_ANIM_B);
    let anim_ptr = state.rw(P_ANIM_PTR);
    let (new_state, new_ptr) = player_set_anim(state, 1, seq, anim_b, anim_ptr);
    state.wb(P_ANIM_B, new_state);

    // [5F0F] advance_anim
    let facing_lo = state.rb(P_FACING);
    let (frame, new_ptr, bcf) = player_advance_anim(state, new_ptr, facing_lo);
    state.ww(P_ANIM_PTR, new_ptr);
    state.ww(P_SPRITE, frame);
    state.wb(ANIM_HI, bcf);
}

/// Player state: idle (anim_id=0, 1030:5CDB).
/// entry_bx is the dispatch bx (anim_id*2) — 0 for direct idle, but other handlers
/// fall through here with their own bx (e.g. jump with 4, anim4 with 8).
pub fn player_state_idle(state: &mut NativeGameState, entry_bx: usize) -> Option<ScrollDir> {
    state.wb(FLY_TIMER, 0); // [5CE8]

    // [5CED-5CF0] friction_dir + friction_sym
    let xvel = state.rw(P_XVEL);
    let friction = state.rw(FRICTION);
    let motion_mode = state.rb(P_MOTION_MODE);
    let mut xvel = player_friction_dir(xvel, friction);
    xvel = player_friction_sym(xvel, motion_mode as u32);
    state.ww(P_XVEL, xvel);

    // [5CF3-5CFF] airborne check
    let unk_6bfe = state.rb(UNK_6BFE);
    let yvel = state.rw(P_YVEL);
    if unk_6bfe == 0 && yvel != 0 {
        // [5D01] fall_latch > 4 -> extra friction
        let fall_latch = state.rb(FALL_LATCH);
        if fall_latch > 4 {
            state.ww(P_XVEL, player_friction_dir(xvel, friction)); // [5D08]
        }
        return None; // [5D0B] no anim_b reset
    }

    let facing = state.rb(P_FACING); // facing_lo
    let xv = xvel;
    let ax = s16(xv as i32).abs(); // [5D0E-5D15] |Xvel| (post-friction)

    if ax >= 8 {
        // [5D17] moving
        let flags = state.rb(P_FLAGS);
        let flags_bit = (flags >> 7) & 1;
        let xv_bit = ((xv >> 15) & 1) as u8;
        if flags_bit == xv_bit {
            // [5D31-5D39] facing == vel sign -> anim 0x12 + trail
            idle_set_advance(state, 0x12, 0x24, facing);
            player_emit_trail(state, state.rb(FRAME_BLINK));
        } else {
            // [5D2E] facing != vel sign -> default anim (turn around)
            idle_default_anim(state, entry_bx, facing);
        }
        state.wb(P_ANIM_B, 0); // [5E08]
        return None;
    }

    if s16(xv as i32) != 0 {
        // [5D42-5D46] 0 < |Xvel| < 8 -> default
        idle_default_anim(state, entry_bx, facing);
        state.wb(P_ANIM_B, 0);
        return None;
    }

    // Xvel == 0 [5D49]
    let timer = state.rb(IDLE_TIMER);
    let e9 = state.rb(IN_AUX);
    let ec = state.rb(IN_RIGHT);
    let ed = state.rb(IN_LEFT);
    let eced = ec & ed;

    let reach_5d83;
    if timer >= 0x1E {
        // [5D49] long idle threshold
        if e9 == 0 && eced == 0 {
            // [5D50-5D5E] no input -> long idle
            state.wb(IDLE_TIMER, timer.wrapping_sub(3)); // [5D60]
            idle_set_advance(state, 0x10, 0x20, facing);  // [5D65-5D6D] anim 0x10
            state.wb(P_ANIM_B, 0);
            return None;
        }
        reach_5d83 = true; // input present -> 5D83
    } else {
        // [5D73]
        if eced != 0 {
            reach_5d83 = true;
        } else if e9 == 0 {
            reach_5d83 = false; // [5D7C] -> fidget (5DC9)
        } else {
            reach_5d83 = true;
        }
    }

    if reach_5d83 && unk_6bfe == 0 {
        // [5D8A] look-around: set_anim_a 0x13 + advance
        idle_set_advance(state, 0x13, 0x26, facing);

        // [5D95-5DA1] camera-pan gates
        let level_flags = state.rb(LEVEL_FLAGS);
        let unk_6bd9 = state.rb(UNK_6BD9);
        if (level_flags & 2) == 0 && unk_6bd9 == 0 {
            // [5DA3-5DAA] screen_x = player tile X - camera X
            let px = s16(state.rw(PLAYER_BASE) as i32);
            let cam_col = s16(state.rw(CAM_COL_WORD) as i32);
            let screen_x = ((px >> 4) - cam_col) & 0xFFFF;
            let flags = state.rb(P_FLAGS);
            if flags & 0x80 != 0 {
                // [5DAE] facing left -> 3414
                if screen_x < 0x11 {
                    // [5DC4] scroll left
                    state.wb(P_ANIM_B, 0);
                    return Some(ScrollDir::Left);
                }
            } else if screen_x > 2 {
                // [5DBA] facing right -> 3435
                state.wb(P_ANIM_B, 0);
                return Some(ScrollDir::Right);
            }
        }
        state.wb(P_ANIM_B, 0); // [5E08]
        return None;
    }

    // [5DC9] fidget: find the 0x79E0 range [lo,hi) containing key=idle_clock&0x1FF
    let key = state.rw(IDLE_CLOCK) & 0x1FF;
    let mut si = FIDGET_RANGE_TABLE;
    loop {
        let lo = state.rw(si);
        let hi = state.rw(si + 2);
        if key < lo {
            // [5DD2] below lo -> default
            idle_default_anim(state, entry_bx, facing);
            state.wb(P_ANIM_B, 0);
            return None;
        }
        if key < hi {
            // [5DD6] in range -> anim 0x11
            idle_set_advance(state, 0x11, 0x22, facing);
            state.wb(P_ANIM_B, 0);
            return None;
        }
        si = (si + 4) & 0xFFFF; // [5DDB] next entry
    }
}

/// Player state: jump (anim_id=2, 1030:5F30).
/// Falls through to idle when [0x6BE0]!=0 (entering with bx==4).
pub fn player_state_jump(state: &mut NativeGameState) -> Option<ScrollDir> {
    let fall_grace = state.rb(FALL_GRACE);
    if fall_grace != 0 {
        // [5F37-5F3E] jmp 5CDB (idle with entry_bx=4)
        return player_state_idle(state, 4);
    }
    jump_body(state);
    None
}

/// Jump-arc body (1030:5F41) — shared by player_state_jump and flying_jump.
fn jump_body(state: &mut NativeGameState) {
    state.wb(UNK_6BFE, 0); // [5F41]
    let counter = state.rb(FALL_LATCH); // [5F46]
    state.wb(FALL_LATCH, counter.wrapping_add(1)); // [5F4C] inc

    let yvel = state.rw(P_YVEL);
    if counter < JUMP_FRAMES {
        // [5F50] first 9 frames: add decaying impulse
        let impulse = state.rw(JUMP_IMPULSE_TABLE + counter as usize * 2); // [5F55-5F57]
        let glider = state.rb(GLIDER_GATE);
        let impulse = if glider != 0 {
            // [5F5B-5F62] flying -> halve the impulse
            ((s16(impulse as i32) >> 1) & 0xFFFF) as u16
        } else {
            impulse
        };
        state.ww(P_YVEL, yvel.wrapping_add(impulse)); // [5F64] Yvel += impulse
    } else {
        // [5F6A-5F6D] gravity
        let low_grav = state.rb(LOW_GRAVITY);
        state.ww(P_YVEL, player_gravity(yvel, low_grav, 0xC0));
    }

    // [5F73] horizontal control
    let xvel = state.rw(P_XVEL);
    let facing = state.rw(P_FACING);
    let motion_mode = state.rb(P_MOTION_MODE);
    let input_lr = state.rb(INPUT_LR);
    let friction = state.rw(FRICTION);
    let xvel = if xvel < 0x30 {
        // [5F7E] accel toward 0x30
        player_accel(xvel, facing, motion_mode as u32, input_lr != 0, 0x30)
    } else {
        // [5F79] symmetric friction
        player_friction_sym(xvel, motion_mode as u32)
    };

    // [5F81-5F86] set_anim_b(2, seq=4)
    let anim_b = state.rb(P_ANIM_B);
    let anim_ptr = state.rw(P_ANIM_PTR);
    let (new_state, new_ptr) = player_set_anim(state, 2, 4, anim_b, anim_ptr);
    state.wb(P_ANIM_B, new_state);

    // [5F89] advance_anim
    let facing_lo = state.rb(P_FACING);
    let (frame, new_ptr, bcf) = player_advance_anim(state, new_ptr, facing_lo);
    state.ww(P_ANIM_PTR, new_ptr);
    state.ww(P_SPRITE, frame);
    state.wb(ANIM_HI, bcf);

    // [5F8C/5F8F] two directional frictions
    let xvel = player_friction_dir(xvel, friction);
    let xvel = player_friction_dir(xvel, friction);
    state.ww(P_XVEL, xvel);
}

/// Player state: anim4 (anim_id=4, 1030:5E62).
pub fn player_state_anim4(state: &mut NativeGameState) -> Option<ScrollDir> {
    state.wb(IDLE_TIMER, 0);       // [5E6C]
    state.wb(PENDING_PICKUP, 4);   // [5E71] drop_gate = 4
    let charge = state.rb(CHARGE);
    state.wb(CHARGE, player_charge_6bce(charge)); // [5E76]

    let xvel = state.rw(P_XVEL);
    let mag = s16(xvel as i32).abs(); // [5E79] |Xvel|
    if mag <= 0x20 {
        // [5E85-5E87] accel (limit 0x20)
        let facing = state.rw(P_FACING);
        let motion_mode = state.rb(P_MOTION_MODE);
        let input_lr = state.rb(INPUT_LR);
        state.ww(P_XVEL, player_accel(xvel, facing, motion_mode as u32, input_lr != 0, 0x20)); // [5E8C]

        // [5E8F] set_anim(al = clobbered |Xvel| low byte, seq=8)
        let anim_b = state.rb(P_ANIM_B);
        let anim_ptr = state.rw(P_ANIM_PTR);
        let al = (mag & 0xFF) as u8;
        let (new_state, new_ptr) = player_set_anim(state, al, 8, anim_b, anim_ptr);
        state.wb(P_ANIM_B, new_state);

        // [5E92] advance_anim
        let facing_lo = state.rb(P_FACING);
        let (frame, new_ptr, bcf) = player_advance_anim(state, new_ptr, facing_lo);
        state.ww(P_ANIM_PTR, new_ptr);
        state.ww(P_SPRITE, frame);
        state.wb(ANIM_HI, bcf);
        return None;
    }

    // [5E89] jmp 5CDB — idle sees idle_timer==0 (just written) and bx==8
    player_state_idle(state, 8)
}

/// Player state: anim5 (anim_id=5, 1030:5E96).
/// Also the FLYING dispatch's catch-all (anim_id 0/3/4/6/7 sent here).
pub fn player_state_anim5(state: &mut NativeGameState, anim_id: u8) {
    state.wb(FLY_TIMER, 0);      // [5EA0]
    state.wb(PENDING_PICKUP, 4); // [5EA5] drop_gate = 4

    // seq = anim_id*2 (+0x40 if gliding)
    let glider = state.rb(GLIDER_GATE);
    let seq = (anim_id as usize * 2 + if glider != 0 { 0x40 } else { 0 }) & 0xFF;

    // [5EAA] set_anim_b
    let anim_b = state.rb(P_ANIM_B);
    let anim_ptr = state.rw(P_ANIM_PTR);
    let (new_state, new_ptr) = player_set_anim(state, anim_id, seq, anim_b, anim_ptr);
    state.wb(P_ANIM_B, new_state);

    // [5EAD] advance_anim
    let facing_lo = state.rb(P_FACING);
    let (frame, new_ptr, bcf) = player_advance_anim(state, new_ptr, facing_lo);
    state.ww(P_ANIM_PTR, new_ptr);
    state.ww(P_SPRITE, frame);
    state.wb(ANIM_HI, bcf);

    // [5EB0] friction_sym
    let xvel = state.rw(P_XVEL);
    let motion_mode = state.rb(P_MOTION_MODE);
    state.ww(P_XVEL, player_friction_sym(xvel, motion_mode as u32));

    // [5EB3] charge_6bce
    let charge = state.rb(CHARGE);
    state.wb(CHARGE, player_charge_6bce(charge));
}

/// Player state: anim8 (anim_id=8, 1030:5CCE).
/// The depth-override state; no [0x6BD0] gate.
pub fn player_state_anim8(state: &mut NativeGameState) {
    let friction = state.rw(FRICTION);
    let motion_mode = state.rb(P_MOTION_MODE);
    let xvel = state.rw(P_XVEL);

    // [5CCE] friction_dir
    let xvel = player_friction_dir(xvel, friction);
    // [5CD1] friction_sym -> ax = xv
    let xvel = player_friction_sym(xvel, motion_mode as u32);
    state.ww(P_XVEL, xvel);

    // [5CD4] set_anim(al = xv low byte, bx=0x10)
    let anim_b = state.rb(P_ANIM_B);
    let anim_ptr = state.rw(P_ANIM_PTR);
    let al = (xvel & 0xFF) as u8;
    let (new_state, new_ptr) = player_set_anim(state, al, 0x10, anim_b, anim_ptr);
    state.wb(P_ANIM_B, new_state);

    // [5CD7] advance_anim
    let facing_lo = state.rb(P_FACING);
    let (frame, new_ptr, bcf) = player_advance_anim(state, new_ptr, facing_lo);
    state.ww(P_ANIM_PTR, new_ptr);
    state.ww(P_SPRITE, frame);
    state.wb(ANIM_HI, bcf);
}

// ════════════════════════════════════════════════════════════════════
// Attack helpers
// ════════════════════════════════════════════════════════════════════

/// 1030:6081 — map the current anim frame to the player render sprite via the phase's
/// frame table (8-byte records {frame, sprite_id, x_off, y_off}, 0x55AA terminator).
fn attack_render_sprite(state: &mut NativeGameState, rec: usize, frame: u16) {
    let base = state.rw(rec) as usize; // [6081] frametbl_ptr
    let dh = ((frame >> 8) & 0x80) as u8; // [6088-60A5] facing bit
    let want = frame & 0x1FFF; // [6085-608A] frame masked to 0x1FFF

    let mut off = 0xFFF8usize;
    loop {
        off = (off + 8) & 0xFFFF; // [6090] stride 8
        let entry_addr = (off + base) & 0xFFFF;
        let cx = state.rw(entry_addr); // [6093] frame word
        if cx == 0x55AA { return; } // [6095] terminator
        if cx == want { break; } // [609B-609D] match
    }

    let entry_addr = (off + base) & 0xFFFF;
    let mut sprid = state.rw(entry_addr + 2); // [609F] sprite_id
    let mut cx = state.rw(entry_addr + 4); // [60A2] x_off (signed)
    let yoff = state.rw(entry_addr + 6); // [60D8] y_off (signed)

    if dh != 0 {
        // [60A5-60AC] facing flip
        sprid |= (dh as u16) << 8; // set bit 15
        cx = ((-s16(cx as i32)) & 0xFFFF) as u16;
    }

    // [60AE] slot0.sprite
    state.ww(SLOT0_SPRITE, sprid);
    // [60B1-60C4] slot0.x = player_x + sar(xvel,4) - x_off
    let px = state.rw(PLAYER_BASE) as i32;
    let xvel = s16(state.rw(P_XVEL) as i32);
    state.ww(SLOT0_X, ((px + (xvel >> 4) - s16(cx as i32)) & 0xFFFF) as u16);
    // [60C7-60DB] slot0.y = player_y + sar(yvel,4) - y_off
    let py = state.rw(PLAYER_BASE + 2) as i32;
    let yvel = s16(state.rw(P_YVEL) as i32);
    state.ww(SLOT0_Y, ((py + (yvel >> 4) - s16(yoff as i32)) & 0xFFFF) as u16);
}

/// 1030:6017-6070 — spawn a projectile into the first free 0x4F2E slot.
/// Returns true if a slot was taken.
fn attack_spawn(state: &mut NativeGameState, rec: usize) -> bool {
    let phase_flag = state.rb(rec + 4); // flag byte
    let frametbl_ptr = state.rw(rec) as usize; // frametbl_ptr

    // [627C-6293] find first free projectile slot (4 slots, stride 0x12)
    let mut slot_base: Option<usize> = None;
    for i in 0..4 {
        let base = ATTACK_SPAWN_LIST + i * 0x12;
        if state.rw(base + 4) == 0xFFFF {
            slot_base = Some(base);
            break;
        }
    }
    let slot_base = match slot_base {
        Some(b) => b,
        None => return false,
    };

    // [601C-601E] kind = (flag>>1)&3
    state.wb(slot_base + 8, ((phase_flag >> 1) & 3) as u8);

    // [6025-602B] walk to terminator (word-by-word)
    let mut bx = frametbl_ptr;
    while state.rw(bx) != 0x55AA {
        bx = (bx + 2) & 0xFFFF;
    }
    // [602D] past terminator
    bx = (bx + 6) & 0xFFFF;

    // [6030] spawn_ptr
    state.ww(slot_base + 0x0C, bx as u16);

    // Spawn descriptor at (bx - 4)
    let desc_base = (bx.wrapping_sub(4)) & 0xFFFF;
    let sprid = state.rw(desc_base + 4); // [6033] sprite_id
    let cx = state.rw(desc_base);        // [6035] x_off
    let yoff = state.rw(desc_base + 2);  // [6038] y_off

    // [603B] slot.yoff
    state.ww(slot_base + 0x0E, yoff);

    // [603E-6049] facing flip
    let facing_lo = state.rb(P_FACING);
    let (mut sprid, mut cx) = (sprid, cx);
    if facing_lo & 0x80 != 0 {
        sprid |= 0x8000;
        cx = ((-s16(cx as i32)) & 0xFFFF) as u16;
    }

    // [604B] slot.sprite
    state.ww(slot_base + 4, sprid);
    // [604E] slot.xoff
    state.ww(slot_base + 6, cx);

    // [6051-605E] slot.x = slot0.x + sar(xoff,4)
    let slot0_x = state.rw(SLOT0_X) as i32;
    state.ww(slot_base, ((slot0_x + (s16(cx as i32) >> 4)) & 0xFFFF) as u16);
    // [6060-606D] slot.y = slot0.y + sar(yoff,4)
    let slot0_y = state.rw(SLOT0_Y) as i32;
    state.ww(slot_base + 2, ((slot0_y + (s16(yoff as i32) >> 4)) & 0xFFFF) as u16);

    true
}

/// Player state: attack (anim_id 3/6/7, 1030:5F96) AND the shared override tail 5F93.
/// al = entry anim id / state, bx = anim_id*2 (the set_anim sequence index).
pub fn player_state_attack(state: &mut NativeGameState, al: u8, bx: usize) {
    // [5F96] set_anim_b
    let anim_b = state.rb(P_ANIM_B);
    let anim_ptr = state.rw(P_ANIM_PTR);
    let (new_state, ptr) = player_set_anim(state, al, bx, anim_b, anim_ptr);
    state.wb(P_ANIM_B, new_state);

    // [5F99] advance_anim
    let facing_lo = state.rb(P_FACING);
    let (frame, new_ptr, bcf) = player_advance_anim(state, ptr, facing_lo);
    state.ww(P_ANIM_PTR, new_ptr);
    state.ww(P_SPRITE, frame);
    state.wb(ANIM_HI, bcf);

    // [5F9C] friction_sym
    let xvel = state.rw(P_XVEL);
    let motion_mode = state.rb(P_MOTION_MODE);
    state.ww(P_XVEL, player_friction_sym(xvel, motion_mode as u32));

    // [5F9F] sat_inc idle_timer
    state.wb(IDLE_TIMER, sat_inc(state.rb(IDLE_TIMER)));

    // [5FA9-5FAF] attack phase
    let phase = state.rb(ATTACK_PHASE) as usize;
    let rec = (ATTACK_PHASE_TABLE + 5 * phase) & 0xFFFF;

    // [5FB1] phase.v19
    let mut v19 = state.rb(rec + 3);
    let charge = state.rb(CHARGE);
    if charge != 0 {
        // [5FB5-5FBE] x4 if charged
        v19 = (v19.wrapping_mul(4)) & 0xFF; // v19 << 2
    }
    state.wb(ATTACK_V19, v19); // [5FC0]

    // [5FC3-5FC8] bd0 = (~bcf) & 0x40 (bit6 of bcf complemented)
    let bd0 = (!bcf) & 0x40;
    state.wb(ANIM_GATE, bd0);

    if bd0 != 0 {
        // [5FCD-5FCF] -> 6081 render-sprite path (bit6 clear)
        attack_render_sprite(state, rec, frame);
        return;
    }

    // ── sound path [5FD2+] ──

    // [5FD2] input_suppress = phase.sfx
    let pe_sfx = state.rb(rec + 2);
    state.wb(INPUT_SUPPRESS, pe_sfx);

    // [5FD9-5FEB] play_sfx dl
    let sfx_dl: u8 = if phase == 0 { 5 } else if phase == 1 { 0 } else { 0x0A };
    let px = crate::engine::audio::player_sfx_x(state);
    crate::engine::audio::native_play_sfx(state, sfx_dl, Some(px));

    // [5FF0] a27 = post-set_anim state (== new anim_b value)
    let a27 = new_state;

    let dy: u16;
    if a27 == 6 {
        dy = 0;
    } else if a27 == 3 {
        // [5FF7/5FFA-5FFC] dx=0xFFE0
        dy = 0xFFE0; // (-0x20) & 0xFFFF
    } else {
        // [5FFE] dx=0xFFD0
        dy = 0xFFD0; // (-0x30) & 0xFFFF
        // [6001] emit trail
        player_emit_trail(state, state.rb(FRAME_BLINK));
    }

    // [6004-600B] Yvel nudge
    if state.rb(UNK_6BFE) == 0 {
        let yvel = state.rw(P_YVEL);
        state.ww(P_YVEL, yvel.wrapping_add(dy));
    }

    // [600F-6017] flag bit0 + free slot -> spawn
    let pe_flag = state.rb(rec + 4);
    if (pe_flag & 1) != 0 && attack_spawn(state, rec) {
        // [6070] suppress the player draw
        state.ww(SLOT0_SPRITE, 0xFFFF);
    } else if state.rb(FALL_FRAMES) == 0 {
        // [6075-607A] fall_frames==0 -> render sprite
        attack_render_sprite(state, rec, frame);
    } else {
        // [607C] suppress
        state.ww(SLOT0_SPRITE, 0xFFFF);
    }
}

// ════════════════════════════════════════════════════════════════════
// Flying / glider
// ════════════════════════════════════════════════════════════════════

/// The glider / flying-mode per-frame update (1030:484E).
/// Active only while [0x6BC5] is set. Computes the flight-animation frame
/// and places the glider WING sprite as render slot 0.
pub fn player_flying_484e(state: &mut NativeGameState) {
    if state.rb(GLIDER_GATE) == 0 { return; } // [484E] not flying

    let mut frame = state.rw(P_SPRITE);

    if state.rb(LOW_GRAVITY) != 0 {
        // [485D] descending -> recompute the flight frame
        let f = frame & 0x8000; // [485F] keep the facing bit
        let xvel = s16(state.rw(P_XVEL) as i32);
        let ax = xvel.abs(); // [4868-486F] |Xvel|
        let mut dx: u16 = 0x30; // [4865]
        if ax <= 0x40 {
            dx += 1; // [4871-487C]
            if ax <= 0x20 {
                dx += 1;
            }
        }
        let tilt = state.rb(GLIDER_TILT); // [487D]
        if tilt < 3 {
            // [4880-4886] banked below neutral
            dx = (0x33u16.wrapping_sub(tilt as u16)) & 0xFF;
        }
        frame = (f | dx) & 0xFFFF; // [4888]
        state.ww(P_SPRITE, frame);
    }

    let dl = ((frame >> 8) & 0xFF) as u8; // [4892] facing/high byte
    let key = frame & 0x1FFF; // [4894] masked frame
    let mut tilt = state.rb(GLIDER_TILT);

    let in_up = state.rb(IN_UP);
    let in_down = state.rb(IN_DOWN);
    if (in_up | in_down) == 0 {
        // [4897-489F] no up/down -> auto-return tilt toward neutral (3)
        if tilt != 3 {
            // [48A3-48AC] step toward neutral
            tilt = if tilt < 3 { tilt + 1 } else { tilt - 1 };
            state.wb(GLIDER_TILT, tilt);
        }
    }

    // [488C] scan the 0x7B29 anim table (stride 6, frame_key==0 = end)
    let mut si = GLIDER_ANIM_TABLE;
    loop {
        let frame_key = state.rw(si);
        if frame_key == 0 { return; } // [48B4] end of table
        if key == frame_key {
            // [48B6] matched frame
            let xoff = s8(state.rb(si + 4) as i32); // [48BA-48BE] signed X offset
            let mut wing = state.rw(si + 2); // [48C0] wing anim
            if wing >= 0x79 {
                // [48C3] -> tilt table (indexed by auto-returned tilt)
                wing = state.rw(GLIDER_TILT_TABLE + tilt as usize * 2); // [48C8-48D0]
            }
            let mut xoff = xoff;
            if dl & 0x80 != 0 {
                // [48D4-48DB] facing left
                wing = (wing | 0x8000) & 0xFFFF;
                xoff = -xoff;
            }
            if state.rb(FLY_TIMER) > 0x18 && (wing & 0xFF) < 0x79 {
                // [48DD-48E8] wing bump
                wing = wing.wrapping_add(1);
            }
            // [48E9] slot0.sprite = wing anim
            state.ww(SLOT0_SPRITE, wing);
            // [48EC-48F1] slot0.x = player_x + xoff
            let px = state.rw(PLAYER_BASE) as i32;
            state.ww(SLOT0_X, ((px + xoff) & 0xFFFF) as u16);
            // [48F4-48FC] slot0.y = player_y + yoff
            let yoff = s8(state.rb(si + 5) as i32);
            let py = state.rw(PLAYER_BASE + 2) as i32;
            state.ww(SLOT0_Y, ((py + yoff) & 0xFFFF) as u16);
            return;
        }
        si = (si + 6) & 0xFFFF; // [4901] next entry
    }
}

/// Flying-movement state machine (1030:596A).
/// Returns do_dispatch: when true, caller runs the flying handler table.
fn player_fsm_flying(state: &mut NativeGameState) -> bool {
    // [596D] bc7 = low_gravity & 1
    let mut bc7 = state.rb(LOW_GRAVITY) & 1;
    state.wb(LOW_GRAVITY, bc7);

    if bc7 == 0 {
        // [5972] not held -> check Yvel and dispatch
        let yvel = s16(state.rw(P_YVEL) as i32);
        if yvel > 0xA0 {
            // [59FE-5A04] jg -> arm descent
            state.wb(LOW_GRAVITY, 1); // [5A06]
        }
        return true; // dispatch via cs:[0x7D6F]
    }

    // [5977] flying hold is active (bc7 != 0)
    let ea = state.rb(IN_UP);
    let eb = state.rb(IN_DOWN);
    let mut bc6 = state.rb(FLY_HOLD);
    let mut b1a = state.rb(GLIDER_TILT);

    if ea != 0 {
        // [5977] held "up"
        if b1a == 6 {
            // [597E]
            let yvel = s16(state.rw(P_YVEL) as i32);
            if yvel > 0x10 {
                // [5985] jle 5997
                bc7 |= 2; // [598C]
            }
        } else {
            // [5993] inc the tilt
            b1a = b1a.wrapping_add(1);
        }
        if bc6 != 0 {
            // [5997] je 59B1
            bc6 = bc6.wrapping_sub(1); // [599E]
            state.wb(FLY_HOLD, bc6);
            state.wb(LOW_GRAVITY, bc7);
            state.wb(GLIDER_TILT, b1a);
            if b1a >= 4 {
                // [59A2] jb 5A0F
                state.ww(P_YVEL, 0xFFC0); // [59A9]
            }
            return false;
        }
        // bc6 == 0 -> fall to 59B1
        state.wb(LOW_GRAVITY, bc7);
        state.wb(GLIDER_TILT, b1a);
    }

    // [59B1] (ea==0, or held-up with fly_hold==0)
    if eb == 0 {
        // [59B1] not held "down"
        state.wb(FLY_HOLD, dec_floor(bc6)); // [59B8] saturating dec
        return false;
    }

    // [59C4] held "down"
    if b1a != 0 {
        b1a = b1a.wrapping_sub(1); // [59CB] dec the tilt
    }
    // [59CF] or (descend),2
    state.wb(LOW_GRAVITY, bc7 | 2);
    state.wb(GLIDER_TILT, b1a);

    if b1a > 1 {
        // [59D4] ja 5A0F
        return false;
    }

    // [59DC-59E9] |sar(Xvel,4)|
    let xvel = s16(state.rw(P_XVEL) as i32);
    let mag = ((xvel >> 4).abs() & 0xFF) as u8;
    // [59ED-59F5] cmp fly_hold, mag*5 ; jae
    let threshold = mag.wrapping_mul(5);
    if bc6 < threshold {
        // [59F7] add fly_hold, mag
        state.wb(FLY_HOLD, bc6.wrapping_add(mag));
    }
    false
}

/// The flying handler table cs:[0x7D6F] (= cs:[0x7D2F] + 0x40).
/// anim_id 1 -> run, 2 -> flying jump, 8 -> no-op, everything else -> anim5.
fn player_fsm_flying_dispatch(state: &mut NativeGameState, anim_id: u8) {
    match anim_id {
        1 => player_state_run(state),       // [5EC4]
        2 => flying_jump(state),            // [5F13]
        8 => { /* [454C] ret — no-op */ }
        _ => player_state_anim5(state, anim_id), // [5E96] idx 0/3/4/5/6/7
    }
}

/// The flying-mode jump (1030:5F13).
/// Once fly_timer reaches 0x18, the jump ends (arm descent, nudge Y-=3,
/// reset fly_timer, two directional frictions); otherwise runs jump_body.
fn flying_jump(state: &mut NativeGameState) {
    if state.rb(FLY_TIMER) >= 0x18 {
        // [5F13-5F18] end the jump
        state.wb(LOW_GRAVITY, 1);   // [5F1A-5F29] arm the descent
        state.wb(FLY_HOLD, 0x18);
        state.wb(FLY_TIMER, 0);
        let py = state.rw(PLAYER_BASE + 2);
        state.ww(PLAYER_BASE + 2, py.wrapping_sub(3)); // nudge Y -= 3

        // [5F2E jmp 5F8C] two directional frictions
        let xvel = state.rw(P_XVEL);
        let friction = state.rw(FRICTION);
        let xvel = player_friction_dir(xvel, friction);
        let xvel = player_friction_dir(xvel, friction);
        state.ww(P_XVEL, xvel);
        return;
    }
    jump_body(state); // [5F41]
}

// ════════════════════════════════════════════════════════════════════
// FSM frontend and step
// ════════════════════════════════════════════════════════════════════

/// The FSM front-end (1030:58A7..591F).
/// Combine the six decoded input flags, update facing, pack the 5-bit bitmask.
/// Returns the bitmask.
pub fn player_fsm_frontend(state: &mut NativeGameState) -> u8 {
    let ec = state.rb(IN_RIGHT); // 0x27EC
    let ed = state.rb(IN_LEFT);   // 0x27ED
    let ea = state.rb(IN_UP);    // 0x27EA
    let eb = state.rb(IN_DOWN);  // 0x27EB
    let e8 = state.rb(IN_FIRE);  // 0x27E8

    state.wb(INPUT_LR, ed | ec);  // [58A7-58B0]
    state.wb(INPUT_UD, ea | eb);  // [58B4-58BB]

    // [58BF-58FC] facing update (raw word compare)
    let facing = state.rw(P_FACING);
    if ec != 0 {
        if ed == 0 && facing != 1 {
            state.ww(P_FACING, 1);
            state.ww(RUN_COUNT, 0);
        }
    } else if ed != 0 {
        if facing != 0xFFFF {
            state.ww(P_FACING, 0xFFFF);
            state.ww(RUN_COUNT, 0);
        }
    }

    // [58FC-591F] pack bit0 of the 5 flags: bit4=right, bit3=left, bit2=up, bit1=down, bit0=fire
    let mut bitmask = 0u8;
    for &flag in &[ec, ed, ea, eb, e8] {
        bitmask = (bitmask << 1) | (flag & 1);
    }
    bitmask
}

/// FSM state selection (1030:5921..595C). Returns anim_id.
pub fn player_select_anim_id(state: &mut NativeGameState, bitmask: u8) -> u8 {
    let suppress = state.rb(INPUT_SUPPRESS);
    let bm = if suppress != 0 { 0u8 } else { bitmask }; // [5921-592C]
    let mut anim_id = state.rb(ANIM_ID_TABLE + bm as usize); // [592E]

    let depth = state.rb(P_DEATH_STATE); // [5932-5936] [0x4F2D]
    state.wb(SLOT0_DEATH, depth); // [0x4F1B] = [0x4F2D]

    if depth >= 0x16 {
        // [593A-593F] depth override
        anim_id = 8;
    }

    let anim_b = state.rb(P_ANIM_B);
    if anim_b != anim_id {
        // [5941] anim changed -> reset run state
        state.wb(P_RUN, 0);     // [594D]
        state.ww(RUN_COUNT, 1);  // [5952-5957] inc_wrap_word(0) = 1
    } else {
        let rc = state.rw(RUN_COUNT);
        state.ww(RUN_COUNT, inc_wrap_word(rc)); // [5952-5957]
    }
    anim_id
}

/// Dispatch the player FSM to the per-state handler (the cs:[anim_id*2 + 0x7D2F] table).
fn player_dispatch_handler(state: &mut NativeGameState, anim_id: u8) -> Option<ScrollDir> {
    match anim_id {
        0 => player_state_idle(state, 0),
        1 => { player_state_run(state); None }
        2 => player_state_jump(state),
        3 | 6 | 7 => {
            player_state_attack(state, anim_id, anim_id as usize * 2);
            None
        }
        4 => player_state_anim4(state),
        5 => { player_state_anim5(state, 5); None }
        8 => { player_state_anim8(state); None }
        _ => panic!("player FSM handler for anim_id={} not recovered", anim_id),
    }
}

/// Full per-frame player FSM step (1030:58A7..5A0B).
/// Front-end -> select_anim_id -> dispatch to handler.
/// Returns the scroll request (idle look-around camera pan), if any.
pub fn player_fsm_step(state: &mut NativeGameState) -> Option<ScrollDir> {
    let bitmask = player_fsm_frontend(state);
    let anim_id = player_select_anim_id(state, bitmask);

    // [5960] flying gate
    let glider = state.rb(GLIDER_GATE);
    if glider != 0 {
        // [596A] flying state machine
        let do_dispatch = player_fsm_flying(state);
        if do_dispatch {
            // [-> 5A0B] dispatch via cs:[0x7D6F]
            // (the flying handlers' [0x6BD0] override is unwitnessed)
            player_fsm_flying_dispatch(state, anim_id);
        }
        return None;
    }

    // [5F35-etc] override gate: when anim_gate is set and anim_id is not 3/6/7/8,
    // run the shared override tail 0x5F93 (attack with al=[0x4F27], bx=anim_id*2)
    let anim_gate = state.rb(ANIM_GATE);
    if anim_gate != 0 && anim_id != 3 && anim_id != 6 && anim_id != 7 && anim_id != 8 {
        let anim_b = state.rb(P_ANIM_B); // [0x4F27] untouched by frontend/selector
        player_state_attack(state, anim_b, anim_id as usize * 2);
        return None;
    }

    // [5A0B] normal dispatch
    player_dispatch_handler(state, anim_id)
}

// ════════════════════════════════════════════════════════════════════
// F1 debug kill-self (1030:5872-5879, 65AF-65D0)
// ════════════════════════════════════════════════════════════════════

/// F1 debug 'kill-self' branch. Active when respawn timer is idle ([0x6BE4]==0)
/// and F1 key is held ([0x282F]!=0).
pub fn player_f1_suicide(state: &mut NativeGameState) {
    let lives = state.rb(LIVES);
    if lives == 0 {
        // [65BA/65BF -> 65D0] game over
        state.wb(END_SIGNAL, 1);
        let timer = state.rb(RESPAWN_STATE); // 65D0 leaves timer untouched (==0)
        state.wb(RESPAWN_STATE, timer.wrapping_sub(1)); // [5875] dec after the call
    } else {
        // [65C1-65CA] spend a life
        state.wb(LIVES, lives - 1);
        state.wb(ENERGY, 0);
        // timer = 2, then [5875] dec: (2-1)&0xFF = 1
        state.wb(RESPAWN_STATE, 1);
    }
}

// ════════════════════════════════════════════════════════════════════
// State accessors and integrate
// ════════════════════════════════════════════════════════════════════

/// Read the player's X position.
pub fn player_x(state: &NativeGameState) -> u16 { state.rw(PLAYER_BASE) }
/// Write the player's X position.
pub fn set_player_x(state: &mut NativeGameState, x: u16) { state.ww(PLAYER_BASE, x); }
/// Read the player's Y position.
pub fn player_y(state: &NativeGameState) -> u16 { state.rw(PLAYER_BASE + 2) }
/// Write the player's Y position.
pub fn set_player_y(state: &mut NativeGameState, y: u16) { state.ww(PLAYER_BASE + 2, y); }
/// Read the player's X velocity.
pub fn player_xvel(state: &NativeGameState) -> u16 { state.rw(P_XVEL) }
/// Read the player's Y velocity.
pub fn player_yvel(state: &NativeGameState) -> u16 { state.rw(P_YVEL) }

/// Run the X/Y integrate over NativeGameState.
pub fn player_integrate(state: &mut NativeGameState) {
    let x = player_x(state);
    let xvel = player_xvel(state);
    let cam_left = state.rw(CAM_LEFT);
    let new_x = player_x_integrate(x, xvel, cam_left);
    set_player_x(state, new_x);

    let y = player_y(state);
    let yvel = player_yvel(state);
    let new_y = player_y_integrate(y, yvel);
    set_player_y(state, new_y);
}

/// Run the per-frame timer decrements over NativeGameState.
pub fn player_timers(state: &mut NativeGameState) {
    let timers: Vec<(usize, u8)> = TIMER_BYTES.iter().map(|&off| (off, state.rb(off))).collect();
    let tw = state.rw(TIMER_WORD);
    let (out, new_word) = player_tick_timers(&timers, tw);
    for (off, v) in out {
        state.wb(off, v);
    }
    state.ww(TIMER_WORD, new_word);
}
