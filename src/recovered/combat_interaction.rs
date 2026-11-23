// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 combat / pickup interaction (1030:88D7 + 899E + 8C21).
//!
//! Sprite-hitbox overlap test, score/effect burst emitter, debris element spawner,
//! death handler, projectile-vs-enemies scan, pickup sparkle, bonus cell collect/hit/flood.
//!
//! All functions use the direct NativeGameState API (state.rb/rw/wb/ww) — no dict-based
//! interface. All operations mutate `NativeGameState` directly in place
//! so subsequent reads immediately observe earlier writes.

use crate::engine::state::NativeGameState;
use crate::recovered::prng;

// --- globals this island reads/writes ---
const SPAWN_X: usize = 0xA336;       // effect-spawn world X (cell << 4)
const SPAWN_Y: usize = 0xA338;       // effect-spawn world Y (cell << 4)
const RNG_STATE: usize = 0x2CEC;     // four bytes [0x2CEC..0x2CEF] = rng_lcg state
const HIT_FLAG: usize = 0xA330;      // byte: 1 when a vertical-detail hit was registered
const HIT_DETAIL: usize = 0xA331;    // word: the vertical penetration depth when HIT_FLAG set
const PASS_FLAG: usize = 0xA312;    // set across the projectile/player pass -> full tolerance
const PLAYER_YVEL: usize = 0x4F2A;
const PLAYER_REC: usize = 0x4F1C;    // the player struct base (excluded from the vertical-detail set)

// sprite-hitbox half-extent tables (DS), indexed by (sprite-id & 0x1FFF) << 1
const HALF_LO: usize = 0x7190;      // [+idx] X half-extent (hw2)
const HALF_HI: usize = 0x7191;      // [+idx] Y half-extent (= HALF_LO + 1 per stride-2 entry)
const HALF_WX: usize = 0x752A;      // [+idx] X half-width for the left-edge test

// effect/score-burst object slots (the free pool after the 12 main objects at 0x4FD0)
const BURST_SLOT_LO: usize = 0x50A8;
const BURST_SLOT_HI: usize = 0x52E8;
const BURST_STRIDE: usize = 0x12;
const BURST_SPRITE: usize = 0xA33A; // [0xA33A] the sprite id to spawn

// debris-element object pool (16 slots, stride 0x12)
const DEBRIS_POOL_LO: usize = 0x5450;
const DEBRIS_POOL_N: usize = 0x10;
const SCORE_LO: usize = 0x6C0E;     // 32-bit score accumulator, low word
const SCORE_HI: usize = 0x6C10;      // 32-bit score accumulator, high word
const SCORE_TABLE: usize = 0xA353;   // per-sprite score table (read as rw(0xA353 + (id-0x4A)*2))
const SPAWNED_PTR: usize = 0xA33E;   // [0xA33E] = the just-spawned element pointer

// death handler
const DEATH_ANIM_MARKER: u16 = 0x7D00;
const DEBRIS_COUNT_TABLE: usize = 0xA3F1; // death-debris count, read as rb(0xA3F1 + idx)
const DAMAGE: usize = 0x7B19;             // [0x7B19] current weapon damage
const DEATH_BONUS_SPRITE: u16 = 0x2046;    // the death-bonus sprite spawned on the non-launch path

// enemy slots
const ENEMY_SLOTS_LO: usize = 0x4FD0;
const ENEMY_SLOTS_N: usize = 0x0C;
const DEF_NONCOLLIDE: u8 = 0x10;    // [def+4] bit4 -> the object ignores collision
const KILL_SFX: u16 = 2;            // play_sfx index on a kill

// pickup sparkle
const SPARKLE_RING_PTR: usize = 0x6BBE;
const SPARKLE_RING_LO: usize = 0x4F76;
const SPARKLE_RING_TOP: usize = 0x4FBE;
const SPARKLE_SPRITE: u16 = 0x35;

// bonus collect
const COLLECTED_COUNTER: usize = 0x2A76;
const MAP_SEG_PTR: usize = 0x2DDA;
const PAGE_DIRTY: usize = 0x6BBD;
const GRID_DIRTY: usize = 0x2DF4;
const GRID_DIRTY_TOKEN: usize = 0x2DE0;

// bonus cell list
const BONUS_CELL_LIST: usize = 0x8C8D;
const BONUS_CELL_N: usize = 0x50;
const DEDUP_BUF: usize = 0xA2A8;

// bonus handler
const BONUS_DEBOUNCE: usize = 0xA33C;
const LEVEL_ID: usize = 0x2D8A;
const FRAME_STAMP: usize = 0x6BD5;

// --- helpers --------------------------------------------------------------------------

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
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

/// The ASM ``jns ; neg`` absolute value of a 16-bit subtraction (0x8000 stays 0x8000).
#[inline]
fn abs16(d: u16) -> u16 {
    (d as i16).unsigned_abs()
}

/// The ASM absolute value of an 8-bit subtraction (0x80 stays 0x80).
#[inline]
fn abs8(d: u8) -> u8 {
    (d as i8).unsigned_abs()
}

// --- sprite hitbox tables (read from DGROUP) ------------------------------------------

#[inline]
fn sprite_half_w(state: &NativeGameState, sprite: u16) -> u8 {
    let idx = ((sprite & 0x1FFF) as usize) << 1;
    state.rb(HALF_LO + idx)
}

#[inline]
fn sprite_half_h(state: &NativeGameState, sprite: u16) -> u8 {
    let idx = ((sprite & 0x1FFF) as usize) << 1;
    state.rb(HALF_HI + idx)
}

#[inline]
fn sprite_left_hw(state: &NativeGameState, sprite: u16) -> u8 {
    let idx = ((sprite & 0x1FFF) as usize) << 1;
    state.rb(HALF_WX + idx)
}

// --- pack-spawn-position (asm 8BF6) ---------------------------------------------------

/// From a bonus/source entry's packed cell coords, set [0xA336]=x<<4 and [0xA338]=y<<4.
/// Returns (spawn_x, spawn_y).
fn pack_spawn_pos(entry_xy_word: u16) -> (u16, u16) {
    let x_cell = entry_xy_word & 0xFF;
    let y_cell = (entry_xy_word >> 8) & 0xFF;
    ((x_cell << 4) & 0xFFFF, (y_cell << 4) & 0xFFFF)
}

// --- RNG advance (1030:39DF via RngView.roll) -----------------------------------------

/// Advance the LCG over the state, write back, return ``b`` (AL).
fn ov_rng(state: &mut NativeGameState) -> u8 {
    let a = state.rb(RNG_STATE);
    let b = state.rb(RNG_STATE + 1);
    let c = state.rb(RNG_STATE + 2);
    let d = state.rw(RNG_STATE + 3);
    let (na, nb, nc, nd, ret) = prng::rng_lcg(a, b, c, d);
    state.wb(RNG_STATE, na);
    state.wb(RNG_STATE + 1, nb);
    state.wb(RNG_STATE + 2, nc);
    state.ww(RNG_STATE + 3, nd);
    ret
}

// --- bonus helpers --------------------------------------------------------------------

/// [asm 8ADF-8AF4 / 8B19-8B2E] the level-dependent score-popup sprite id.
fn bonus_popup_cx(lvl: u8) -> u16 {
    if lvl == 3 { 0x132 }
    else if lvl == 6 || lvl == 7 { 0x12C }
    else { 0x134 }
}

/// [asm 8B46-8B5C] popup Xvel = 0x30, negated unless the source's facing value is negative.
fn bonus_facing_xvel(state: &NativeGameState, src_si: usize) -> u16 {
    let v: i16 = if src_si >= PLAYER_REC {
        state.rw(src_si + 6) as i16
    } else {
        state.rw(PLAYER_REC + 0x09) as i16  // PlayerView.facing at 0x4F25
    };
    if v < 0 { 0x30 } else { 0xFFD0 }  // (-0x30) & 0xFFFF = 0xFFD0
}

// --- hitbox_overlap (asm 8D7B) --------------------------------------------------------

/// Sprite-hitbox overlap test. ``si``/``di`` are the source/target sprite-record offsets.
/// Returns true if the sprites overlap. Writes HIT_FLAG/HIT_DETAIL to state.
pub fn hitbox_overlap(state: &mut NativeGameState, si: usize, di: usize) -> bool {
    // [asm 8D81] cleared
    state.wb(HIT_FLAG, 0);

    let src_x = state.rw(si);
    let src_y = state.rw(si + 2);
    let src_sprite = state.rw(si + 4);
    let tgt_x = state.rw(di);
    let tgt_y = state.rw(di + 2);
    let tgt_sprite = state.rw(di + 4);

    // [asm 8D86/8D96] coarse box gates
    if abs16(src_x.wrapping_sub(tgt_x)) >= 0x40 { return false; }
    if abs16(src_y.wrapping_sub(tgt_y)) >= 0x46 { return false; }

    // [asm 8DA8] Y axis — orient so (ax, si) is the larger-Y object, (dx, di) the smaller
    let (mut ax, mut dx, mut bx) = (src_y, tgt_y, src_sprite);
    let mut si_var = si;
    let (mut src_x_var, mut src_sprite_var) = (src_x, src_sprite);
    let (mut tgt_x_var, mut tgt_sprite_var) = (tgt_x, tgt_sprite);

    if s16(ax as i32) < s16(dx as i32) {
        bx = tgt_sprite;
        std::mem::swap(&mut ax, &mut dx);
        // After swap: si_var holds the larger-Y object's offset
        std::mem::swap(&mut src_x_var, &mut tgt_x_var);
        std::mem::swap(&mut src_sprite_var, &mut tgt_sprite_var);
        // si_var now holds the larger-Y object's offset
        si_var = di;  // the larger-Y object is the original target
    }
    // Note: if no swap, si_var = si (original source is larger-Y)

    let half_h = sprite_half_h(state, bx);  // bl = [bx + 0x7191]
    ax = ax.wrapping_sub(half_h as u16);
    if s16(ax as i32) >= s16(dx as i32) { return false; }  // [asm 8DCA] jge -> no overlap

    // [asm 8DD1] vertical detail
    let a312 = state.rb(PASS_FLAG);
    if a312 == 0 {
        let depth = dx.wrapping_sub(ax);  // sub dx,ax
        let pyvel = s16(state.rw(PLAYER_YVEL) as i32);
        let do_set = if pyvel >= 0x80 {  // [asm 8DDB] jge -> set
            true
        } else if (depth as u32) > ((half_h as u32) >> 1) {  // [asm 8DDF] ja -> skip (unsigned)
            false
        } else if si_var != PLAYER_REC {  // [asm 8DE7] je -> skip
            true
        } else {
            false
        };
        if do_set {
            state.wb(HIT_FLAG, 1);       // inc byte [0xA330]
            state.ww(HIT_DETAIL, depth); // mov word [0xA331],dx
        }
    }

    // [asm 8DF1] X axis — left edges = pos - X half-width; overlap if min_left + hw2 > max_left
    let src_left = src_x_var.wrapping_sub(sprite_left_hw(state, src_sprite_var) as u16);  // bp
    let tgt_left = tgt_x_var.wrapping_sub(sprite_left_hw(state, tgt_sprite_var) as u16);
    let mut hw2 = sprite_half_w(state, tgt_sprite_var);  // bl = [bx + 0x7190]
    let (ax2, dx2) = if s16(tgt_left as i32) < s16(src_left as i32) {
        // [asm 8E1C] jl keeps; (ax=tgt_left, dx=src_left)
        (tgt_left, src_left)
    } else {
        // else swap to src's hw2
        hw2 = sprite_half_w(state, src_sprite_var);
        (src_left, tgt_left)
    };
    if a312 == 0 {  // [asm 8E2B] jne skips the halving
        hw2 >>= 1;  // sar bx,1 (hw2 >= 0)
    }
    let ax2_val = ax2.wrapping_add(hw2 as u16);
    let hit = s16(ax2_val as i32) > s16(dx2 as i32);  // [asm 8E33] jle -> no hit
    hit
}

// --- spawn_effect_burst (asm 8D1B) ----------------------------------------------------

/// Score/effect-burst emitter: spawn `cx` sprites into the free object slots
/// 0x50A8..0x52E8 (stride 0x12); each gets sprite id [0xA33A], pos [0xA336]/[0xA338],
/// Xvel=ax, Yvel/state=dx, [+0xC]=0xC6, [+0x11]=0, [+9]=0xFFFF.
/// After each spawn ax is negated (alternating spread) and on every even spawn ax,dx step
/// down 0x10 (ax zeroed past the 12th).
pub fn spawn_effect_burst(state: &mut NativeGameState, mut ax: u16, mut dx: u16, mut cx: u16) {
    let sprite = state.rw(BURST_SPRITE);
    let px = state.rw(SPAWN_X);
    let py = state.rw(SPAWN_Y);
    ax &= 0xFFFF;
    dx &= 0xFFFF;

    let mut spawned: u16 = 0;
    let mut bx = BURST_SLOT_LO;
    while bx < BURST_SLOT_HI {  // [asm 8D6F] cmp bx,0x52E8 ; jb
        if state.rw(bx + 4) == 0xFFFF {  // [asm 8D25] free slot
            state.ww(bx + 4, sprite);
            state.wb(bx + 0x11, 0);
            state.ww(bx + 0x0C, 0xC6);
            state.ww(bx, px);
            state.ww(bx + 2, py);
            state.ww(bx + 6, ax);
            state.ww(bx + 0x0E, dx);
            state.ww(bx + 9, 0xFFFF);
            ax = (0u16).wrapping_sub(ax);  // [asm 8D53] neg ax
            spawned += 1;
            if spawned & 1 == 0 {  // [asm 8D56] even spawn
                if spawned > 0xC {  // [asm 8D5C] jbe skips
                    ax = 0;  // [asm 8D61] xor ax,ax
                }
                ax = ax.wrapping_sub(0x10);  // [asm 8D63]
                dx = dx.wrapping_sub(0x10);  // [asm 8D66]
            }
            cx -= 1;
            if cx == 0 {  // [asm 8D6A] je -> done
                break;
            }
        }
        bx += BURST_STRIDE;
    }
}

// --- spawn_debris_element (asm 8875) --------------------------------------------------

/// Spawn one debris element (sprite `ax`, position from `si`) into the 0x5450 pool
/// (16 slots, stride 0x12): set [+4]=sprite, [+0]/[+2]=pos, [+0xC]=0x2C, [0xA33E]=slot;
/// bump the 32-bit score by the per-sprite value when sprite-0x4A in [0,0x10];
/// if `si` is an effect slot (>=0x50A8) free its back-referenced slot ([si+9]).
/// Returns the spawned slot offset (or None if the pool was full).
fn spawn_debris_element(state: &mut NativeGameState, ax: u16, si: usize) -> Option<usize> {
    let ax = ax & 0xFFFF;
    let si = si & 0xFFFF;

    // [asm 8879] score bump for sprite ids 0x4A..0x5A
    let bx = ax.wrapping_sub(0x4A);
    if bx & 0x8000 == 0 && bx <= 0x10 {  // jb (negative) / ja (>0x10) skip
        let val = state.rw(SCORE_TABLE + ((bx as usize) << 1)) as u32;
        let score_lo = state.rw(SCORE_LO) as u32;
        let score_hi = state.rw(SCORE_HI) as u32;
        let total = (score_lo | (score_hi << 16)).wrapping_add(val);
        state.ww(SCORE_LO, (total & 0xFFFF) as u16);
        state.ww(SCORE_HI, ((total >> 16) & 0xFFFF) as u16);
    }

    // [asm 8894] find a free debris slot
    let mut slot: Option<usize> = None;
    for k in 0..DEBRIS_POOL_N {
        let b = DEBRIS_POOL_LO + k * 0x12;
        if state.rw(b + 4) == 0xFFFF {
            slot = Some(b);
            break;
        }
    }

    if let Some(s) = slot {  // [asm 88A7] fill it
        let src_x = state.rw(si);
        let src_y = state.rw(si + 2);
        state.ww(s + 4, ax);
        state.ww(s, src_x);
        state.ww(s + 2, src_y);
        state.ww(s + 0x0C, 0x2C);
        state.ww(SPAWNED_PTR, s as u16);  // [asm 88B9] [0xA33E]=di
        // [asm 88BD] if the pos source is an effect slot, free its back-referenced slot
        if si >= BURST_SLOT_LO {
            let ref_off = state.rw(si + 9) as usize;
            if ref_off != 0xFFFF {
                state.ww(ref_off + 4, 0xFFFF);
            }
        }
    }

    slot
}

// --- advance_death_anim (asm 80CB) ----------------------------------------------------

/// Scan the enemy's anim script forward from [di+0xC] by 2 bytes at a time until the
/// word 0x7D00 (the death-sequence marker), then step 2 past it.
/// Returns the new [di+0xC] value. (Read-only; caller writes it back.)
fn advance_death_anim(state: &NativeGameState, di: usize) -> u16 {
    let mut si = state.rw(di + 0x0C) as usize;
    for _ in 0..0x4000 {  // guard (the script always has a 0x7D00)
        si = (si.wrapping_add(2)) & 0xFFFF;
        if state.rw(si) == DEATH_ANIM_MARKER {
            return (si.wrapping_add(2)) as u16 & 0xFFFF;
        }
    }
    panic!("80CB: no 0x7D00 death marker in anim script");
}

// --- death_handler (asm 8C72) --------------------------------------------------------

/// Enemy-death handler: spawn `count` debris elements (8875) scattering the death point
/// (pos +=9/+7, [elem+0xC] staggered by remaining*4), restore the enemy pos, mark it dead
/// ([di+0xE]=0xFF); then if [def+4]&1 the launch path (80CB death-anim, conditional [def+4]
/// bit3 clear, knockback [di+0xA]=-min(dmg,0x19)*8 / [di+8]=+/-half), else spawn 6 death-bonus
/// sprites (8D1B, id 0x2046) at the enemy pos.
pub fn death_handler(state: &mut NativeGameState, bx: usize, di: usize, src_si: usize) {
    // [asm 8C72] [def+8] signed + 0x4A
    let sprite = ((s8(state.rb(bx + 8)) + 0x4A) as u16) & 0xFFFF;
    // [asm 8C7A] cnt_idx = (enemy.hits >> 3) & 7
    let cnt_idx = ((state.rb(di + 0x10) >> 3) & 7) as usize;
    let count = state.rb(DEBRIS_COUNT_TABLE + cnt_idx);

    // [asm 8C90/8C92] saved pos (restored later)
    let orig_x = state.rw(di);
    let orig_y = state.rw(di + 2);

    let mut rem = count as u16;
    while rem != 0 {  // [asm 8C96] debris loop
        let slot = spawn_debris_element(state, sprite, di);
        if let Some(_s) = slot {
            let spawned_ptr = state.rw(SPAWNED_PTR) as usize;  // [asm 8C99] di = [0xA33E]
            let cur = state.rw(spawned_ptr + 0x0C);
            // [asm 8CA2] [elem+0xC] -= rem*4
            state.ww(spawned_ptr + 0x0C, cur.wrapping_sub(rem << 2));
        }
        // [asm 8CA6/8CAA] scatter
        let ey = state.rw(di + 2);
        state.ww(di + 2, ey.wrapping_add(7));
        let ex = state.rw(di);
        state.ww(di, ex.wrapping_add(9));
        rem = rem.wrapping_sub(1);
    }

    // [asm 8CB1/8CB4] restore enemy pos
    state.ww(di + 2, orig_y);
    state.ww(di, orig_x);
    state.wb(di + 0x0E, 0xFF);  // [asm 8CB7] mark dead

    let def_flags = state.rb(bx + 4);
    if def_flags & 1 != 0 {  // [asm 8CBB] jne -> launch path
        let new_anim = advance_death_anim(state, di);  // [asm 8CE1] 80CB
        state.ww(di + 0x0C, new_anim);
        // [asm 8CE4-8CF2] conditional bit3 clear
        if (def_flags & 0xC8) != 0x88 {
            state.wb(bx + 4, def_flags & 0xF7);
        }
        let mut dmg = state.rb(DAMAGE) as i32;  // [asm 8CF5]
        if dmg > 0x19 { dmg = 0x19; }
        let yvel = (((-dmg) << 3) & 0xFFFF) as u16;  // [asm 8D02-8D08] -min(dmg,0x19)*8
        state.ww(di + 0x0A, yvel);  // enemy.yvel
        let mut xvel = sar16(yvel, 1);  // [asm 8D0D] ax sar 1
        // [asm 8D0F] test attacker [si+5],0x80 ; jne keep
        if state.rb(src_si + 5) & 0x80 == 0 {
            xvel = (0u16).wrapping_sub(xvel);  // negate
        }
        state.ww(di + 8, xvel);  // enemy.xvel
    } else {  // [asm 8CC1] bonus path
        state.ww(SPAWN_X, state.rw(di));      // [0xA336] = enemy X
        state.ww(SPAWN_Y, state.rw(di + 2));  // [0xA338] = enemy Y
        state.ww(BURST_SPRITE, DEATH_BONUS_SPRITE);  // [0xA33A] = 0x2046
        spawn_effect_burst(state, 0x30, 0xFF80, 6);  // [asm 8CDC] 8D1B
    }
}

// --- projectile_vs_enemies (asm 8C21) ------------------------------------------------

/// Scan the 12 enemy slots (0x4FD0) for the first the source sprite `si` (a projectile or
/// the player) overlaps; on a hit mark + damage it, kill (SFX + death_handler) or knock it
/// back, and consume the source.
/// Returns (hit, enemy_slot).
pub fn projectile_vs_enemies(state: &mut NativeGameState, si: usize) -> (bool, Option<usize>) {
    let damage = state.rb(DAMAGE);

    let mut di = ENEMY_SLOTS_LO;
    for _ in 0..ENEMY_SLOTS_N {
        let enemy_sprite = state.rw(di + 4);
        let enemy_state = state.rb(di + 0x0E);
        if enemy_sprite != 0xFFFF && enemy_state != 0xFF {  // [asm 8C27/8C2D]
            let bx = state.rw(di + 6) as usize;  // [di+6] the type-def ptr
            if state.rb(bx + 4) & DEF_NONCOLLIDE == 0 {  // [asm 8C36] collidable
                let hit = hitbox_overlap(state, si, di);  // [asm 8C3C] 8D7B
                if hit {  // [asm 8C3F] jae skips
                    // [asm 8C41] mark hit
                    let flags = state.rb(di + 5);
                    state.wb(di + 5, flags | 0x40);
                    let hp = state.rb(di + 0x0F);
                    state.wb(di + 0x0F, hp.wrapping_sub(damage));  // [asm 8C48] HP -= damage
                    if hp < damage {  // [asm 8C4B] borrow -> kill
                        let sx = crate::engine::audio::sfx_screen_x(state, state.rw(di) as i32);
                        crate::engine::audio::native_play_sfx(state, KILL_SFX as u8, Some(sx));  // [asm 8C50] play_sfx(2)
                        death_handler(state, bx, di, si);
                    } else {  // [asm 8C58] knockback
                        let kb = (state.rw(di + 8) as i16) >> 2;  // ax = [di+8] >> 2 (arith)
                        let nx = state.rw(di).wrapping_sub(kb as u16);
                        state.ww(di, nx);
                    }
                    // [asm 8C61] consume source
                    state.ww(si + 4, 0xFFFF);
                    return (true, Some(di));
                }
            }
        }
        di += 0x12;
    }

    (false, None)
}

// --- spawn_pickup_sparkle (asm 5E41) -------------------------------------------------

/// Spawn a pickup-sparkle effect (sprite 0x35) at (ax, dx) into the downward-growing ring
/// at [0x4F76..0x4FBE] (stride 0x12): write [ptr]=X, [ptr+2]=Y, [ptr+4]=0x35, then advance
/// the pointer [0x6BBE] down 0x12, wrapping to 0x4FBE when it would drop below 0x4F76.
fn spawn_pickup_sparkle(state: &mut NativeGameState, ax: u16, dx: u16) {
    let mut bx = state.rw(SPARKLE_RING_PTR) as usize;
    state.ww(bx, ax & 0xFFFF);           // [asm 5E46] [bx]   = X
    state.ww(bx + 2, dx & 0xFFFF);       // [asm 5E48] [bx+2] = Y
    state.ww(bx + 4, SPARKLE_SPRITE);    // [asm 5E4B] [bx+4] = sprite id
    bx = bx.wrapping_sub(0x12) & 0xFFFF;  // [asm 5E50]
    if bx < SPARKLE_RING_LO {  // [asm 5E53] jae keeps; else wrap
        bx = SPARKLE_RING_TOP;
    }
    state.ww(SPARKLE_RING_PTR, bx as u16);  // [asm 5E5C] new pointer
}

// --- bonus_collect_tail (asm 8B6E/8B77) ----------------------------------------------

/// Bump the collected counter [0x2A76], clear the cell ([di+3]=0xFFFF), restore the
/// underlying tile id [di+1] into the level map (es=[0x2DDA]) at the cell's map offset
/// (the old [di+3]); if that cell is on-screen, set the redraw-dirty flags.
/// Returns (onscreen, old_cell).
fn bonus_collect_tail(state: &mut NativeGameState, di: usize) -> (bool, u16) {
    let collected = state.rw(COLLECTED_COUNTER);
    state.ww(COLLECTED_COUNTER, collected.wrapping_add(1));  // [asm 8B77] inc [0x2A76]

    let old = state.rw(di + 3);  // [asm 8B7E] old [di+3] = the map offset
    state.ww(di + 3, 0xFFFF);    // clear the cell
    let tile = state.rb(di + 1); // [asm 8B81] tile id to restore

    // [asm 8B8A] level_map[old] = tile — write to the level-map segment
    let map_seg = state.rw(MAP_SEG_PTR) as usize;
    state.wb_abs((map_seg << 4) + (old as usize), tile);

    let al = (old & 0xFF) as u8;          // map X cell
    let ah = ((old >> 8) & 0xFF) as u8;   // map Y cell
    let cam_x = state.rb(0x2DE4);         // cam_col
    let cam_y = state.rb(0x2DE6);         // cam_row
    let sx = al.wrapping_sub(cam_x);      // [asm 8B8D] sub al,[0x2DE4]
    let sy = ah.wrapping_sub(cam_y);      // [asm 8B97] sub ah,[0x2DE6]
    let onscreen = al >= cam_x && sx < 0x14 && ah >= cam_y && sy < 0x0C;
    if onscreen {  // [asm 8BD8-8BE2] redraw-dirty flags
        state.wb(PAGE_DIRTY, 1);
        state.wb(GRID_DIRTY, 1);
        state.ww(GRID_DIRTY_TOKEN, 0x55AA);
    }
    (onscreen, old)
}

// --- bonus_finish (asm 8B66) ---------------------------------------------------------

/// Decrement the cell counter; collect (8B77) on underflow.
/// Returns (onscreen, collected).
fn bonus_finish(state: &mut NativeGameState, di: usize) -> (bool, bool) {
    let cnt = state.rb(di + 2);
    state.wb(di + 2, cnt.wrapping_sub(1));
    if cnt == 0 {  // sub borrow -> collect
        let (onscreen, _) = bonus_collect_tail(state, di);
        return (onscreen, true);
    }
    (false, false)
}

// --- bonus_hit_handler (asm 8A5A) ----------------------------------------------------

/// Bonus hit handler: spawn a pickup sparkle (5E41) at the cell; then a counter (bit7) path
/// (dec [cell+2]; on underflow collect, else a random-id sparkle + collect), or the normal
/// path with a frame debounce, level-dependent score-popup bursts keyed on [cell+2]&0x40,
/// then decrement+collect (8B6E) on underflow.
/// Returns (onscreen, collected).
pub fn bonus_hit_handler(state: &mut NativeGameState, di: usize, src_si: usize) -> (bool, bool) {
    // [8A64] sparkle at the cell centre (x*16+8, y*16+0xC)
    let cell = state.rw(di + 3);
    let sparkle_x = (((cell & 0xFF) << 4) + 8) & 0xFFFF;
    let sparkle_y = ((((cell >> 8) & 0xFF) << 4) + 0xC) & 0xFFFF;
    spawn_pickup_sparkle(state, sparkle_x, sparkle_y);

    let t = state.rb(di + 2);
    if t & 0x80 != 0 {  // [8A83] counter bonus
        let newt = t.wrapping_sub(1);
        state.wb(di + 2, newt);
        if newt & 0x80 != 0 {  // underflow -> stc, no collect
            return (false, true);
        }
        let (px, py) = pack_spawn_pos(cell);  // [8A8D] 8BF6
        state.ww(SPAWN_X, px);
        state.ww(SPAWN_Y, py);
        let mut r = ov_rng(state) & 7;  // [8A90] reroll until nonzero
        while r == 0 {
            r = ov_rng(state) & 7;
        }
        state.ww(BURST_SPRITE, (r as u16 + 0x6E) & 0xFFFF);
        let by = state.rw(SPAWN_Y);
        state.ww(SPAWN_Y, by.wrapping_sub(0x70));
        spawn_effect_burst(state, 0, 0, 1);  // [8AA9] 8D1B
        let (onscreen, _) = bonus_collect_tail(state, di);  // [8AAE] collect
        return (onscreen, true);
    }

    // [8AB1] normal bonus: frame debounce
    let frame = state.rw(FRAME_STAMP);
    let debounce = state.rw(BONUS_DEBOUNCE);
    if abs16(frame.wrapping_sub(debounce)) < 6 {
        return (false, false);  // recently collected -> ignore
    }
    state.ww(BONUS_DEBOUNCE, frame);
    let (px, py) = pack_spawn_pos(cell);  // [8ACC] 8BF6
    state.ww(SPAWN_X, px);
    state.ww(SPAWN_Y, py);
    let lvl = state.rb(LEVEL_ID);
    let t2 = state.rb(di + 2);  // re-read counter

    if t2 & 0x40 == 0 {  // [8B40] regular bonus: random sprite (8C13)
        let mut r = ov_rng(state) & 0x7F;
        while r >= 0x5F {
            r = ov_rng(state) & 0x7F;
        }
        state.ww(BURST_SPRITE, 0x2080 + r as u16);
        spawn_effect_burst(state, bonus_facing_xvel(state, src_si), 0xFF90, 1);
    } else if t2 == 0x40 {  // [8AD5] two-burst bonus
        state.wb(di + 2, 0);
        state.ww(BURST_SPRITE, bonus_popup_cx(lvl));
        spawn_effect_burst(state, 0x20, 0xFFD0, 2);  // [8B04]
        state.ww(BURST_SPRITE, if lvl == 8 { 0x136 } else { 0xE5 });  // [8B07]
        spawn_effect_burst(state, bonus_facing_xvel(state, src_si),
                           0xFF90, if lvl == 8 { 2 } else { 4 });  // [8B61]
    } else {  // [8B19] bit6 set, single burst, no facing
        state.ww(BURST_SPRITE, bonus_popup_cx(lvl));
        spawn_effect_burst(state, 0x30, 0xFFA0, 4);
    }

    let (onscreen, collected) = bonus_finish(state, di);
    (onscreen, collected)
}

// --- flood_collect (asm 89F0-8A4B) ----------------------------------------------------

/// Flood-fill collect of cells connected (8-adjacency in cell coords) to `center`,
/// deduped by cell index in the 0xA2A8 buffer. Returns the list of on-screen cell map
/// offsets whose tile must be re-blitted.
fn flood_collect(state: &mut NativeGameState, center: u16) -> Vec<u16> {
    // [asm 89F4] clear dedup buffer (0xA2A8, 80 bytes)
    for k in 0..BONUS_CELL_N {
        state.wb(DEDUP_BUF + k, 0);
    }
    let mut redraws: Vec<u16> = Vec::new();
    let mut stack: Vec<u16> = vec![center];
    while let Some(cur) = stack.pop() {  // [asm 8A44] pop a center, re-scan
        let mut si = BONUS_CELL_LIST;
        for idx in 0..BONUS_CELL_N {  // [asm 8A06] scan all cells
            let ax = state.rw(si + 3);
            if ax != 0xFFFF && ax != cur
                && abs8((ax as u8).wrapping_sub(cur as u8)) <= 1
                && abs8(((ax >> 8) as u8).wrapping_sub((cur >> 8) as u8)) <= 1
                && state.rb(DEDUP_BUF + idx) == 0  // [asm 8A27] dedup
            {
                state.wb(DEDUP_BUF + idx, 1);
                stack.push(ax);  // [asm 8A37] push as a new center
                let (onscreen, _) = bonus_collect_tail(state, si);  // [asm 8A3C] 8B6E
                if onscreen {
                    redraws.push(ax);
                }
            }
            si += 5;
        }
    }
    redraws
}

// --- bonus_pickup_scan (asm 899E) ----------------------------------------------------

/// Scan the 80-cell bonus list (0x8C8D) for cells near the source sprite `si`; call
/// bonus_hit_handler for each in range; on the first that collects, flood-fill-collect all
/// connected cells (8-adjacency in cell coords, deduped via 0xA2A8).
/// Returns (redraws, hit) — the list of on-screen map offsets to re-blit, and the ASM's CF.
pub fn bonus_pickup_scan(state: &mut NativeGameState, si: usize) -> (Vec<u16>, bool) {
    let src_x = state.rw(si);
    let src_x_lo = ((src_x as i16) >> 4) as u8;  // [asm 89A4] (ax = [si]>>4); only the low byte
    let src_y = state.rw(si + 2);
    let bp_y = src_y.wrapping_sub(0x10);  // [asm 89A8] bp = [si+2] - 0x10

    let mut redraws: Vec<u16> = Vec::new();

    let mut di = BONUS_CELL_LIST;
    for _ in 0..BONUS_CELL_N {
        let cell = state.rw(di + 3);
        if cell != 0xFFFF
            && abs8((cell as u8).wrapping_sub(src_x_lo)) <= 1
            && abs16((((cell >> 8) & 0xFF) as u16).wrapping_mul(16).wrapping_sub(bp_y)) < 0x10
        {  // [asm 89BC/89CC] in range
            state.ww(si + 4, 0xFFFF);  // [asm 89E3] consume the source
            let (onscr_h, collected) = bonus_hit_handler(state, di, si);  // [asm 89EB] 8A5A
            if onscr_h {
                redraws.push(cell);  // the original cell map offset
            }
            if collected {  // [asm 89EE] CF=1 -> flood-fill the rest
                let self_redraws = flood_collect(state, cell);
                redraws.extend(self_redraws);
                return (redraws, true);
            }
        }
        di += 5;
    }

    (redraws, false)  // [asm 8A57] clc
}

// --- roll_bonus_sprite_id (asm 8C13) -------------------------------------------------

/// Rejection-sample rng_lcg (1030:39DF) -> v = ret & 0x7F, reroll while v >= 0x5F,
/// return 0x2080 + v (a score/bonus-popup sprite id in [0x2080,0x20DE]).
/// Advances the [0x2CEC..0x2CEF] generator state once per draw.
pub fn roll_bonus_sprite_id(state: &mut NativeGameState) -> u16 {
    let mut a = state.rb(RNG_STATE);
    let mut b = state.rb(RNG_STATE + 1);
    let mut c = state.rb(RNG_STATE + 2);
    let mut d = state.rw(RNG_STATE + 3);
    loop {
        let (na, nb, nc, nd, ret) = prng::rng_lcg(a, b, c, d);
        a = na; b = nb; c = nc; d = nd;
        let v = ret & 0x7F;
        if v < 0x5F {
            state.wb(RNG_STATE, a);
            state.wb(RNG_STATE + 1, b);
            state.wb(RNG_STATE + 2, c);
            state.ww(RNG_STATE + 3, d);
            return 0x2080 + v as u16;
        }
    }
}
