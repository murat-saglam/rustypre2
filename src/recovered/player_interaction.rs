// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 player<->world interaction pass (1030:8295..8617).
//!
//! Loop1: player-vs-enemy (stomp/hurt/die). Loop2: player-vs-entity pickups.
//! All effects written directly to NativeGameState — no overlay/contract system.

use crate::engine::state::NativeGameState;
use crate::recovered::combat_interaction;
use crate::recovered::prng;
use crate::audio::native::native_play_sfx;

// --- constants ([asm] proven) ---
const OBJ_BASE: usize = 0x4FD0;       // 12-slot object/enemy list
const OBJ_STRIDE: usize = 0x12;
const OBJ_COUNT: usize = 12;
const PLAYER: usize = 0x4F1C;         // player record base
const EFFECT_LIST: usize = 0x5450;    // 16-slot debris/effect pool
const EFFECT_LIST_COUNT: usize = 0x10;
const ENTITY2: usize = 0x50A8;        // 52-entry pickup/entity list
const ENTITY_COUNT: usize = 0x34;     // 52
const WALL_MARKER_TABLE: usize = 0x6EA9; // per-column wall marker table (stride 8)
const EFFECT_SOURCES: usize = 0x8F1D; // 70-entry effect source list (stride 7)
const EFFECT_SOURCES_COUNT: usize = 0x46; // 70
const ANIM_SECTION_MARKER: u16 = 0x7D00;

// PlayerGlobals offsets
const SCORE_LO: usize = 0x6C0E;
const SCORE_HI: usize = 0x6C10;
const ITEM_TOTAL: usize = 0x6C9E;
const ITEM_QUEUE: usize = 0x6C12;
const COLLECTED_LINKED: usize = 0x2A7A;
const SCALE_LEVEL: usize = 0x6BE2;
const HURT_COOLDOWN: usize = 0x6BC9;
const LOW_GRAVITY: usize = 0x6BC7;
const GLIDER: usize = 0x6BC5;
const ANIM_GATE: usize = 0x6BD0;
const FALL_FRAMES: usize = 0x6BD2;
const LIVES: usize = 0x27D8;
const ENERGY: usize = 0x27D6;
const LEVEL_END_MODE: usize = 0x6BE6;
const LEVEL: usize = 0x2D8A;
const ATTACK_PHASE: usize = 0x7B18;
const BONUS_LETTERS: usize = 0x6CA7;
const UTENSILS_MASK: usize = 0x6CA8;
const CAMERA_SHAKE: usize = 0x6BEA;
const BURST_X: usize = 0xA336;
const BURST_Y: usize = 0xA338;
const BURST_SPRITE: usize = 0xA33A;
const SPAWNED_PTR: usize = 0xA33E;
const CHECKPOINT_X: usize = 0x6BAD;
const CHECKPOINT_Y: usize = 0x6BAF;
const HIT_FLAG: usize = 0xA330;
const HIT_DETAIL: usize = 0xA331;
// (HIT_PASS_FULL not needed — hitbox_overlap handles this internally)
const IN_UP: usize = 0x27EA;
const RESPAWN_STATE: usize = 0x6BE4;
const END_SIGNAL: usize = 0x6BE5;

// LightFadeView offsets
const LIGHT_TO_DARK: usize = 0x6C01;
const LIGHT_TO_LIGHT: usize = 0x6C02;
const LIGHT_STEP: usize = 0x6C03;
const LIGHTS_OFF: usize = 0x6C04;

// Table bases
const SCORE_TABLE: usize = 0xA353;
const SCORE_SPR_LUT: usize = 0xA375;
const HURT_SFX_TABLE: usize = 0xA3E5;

// Early-skip ids (no consume, no effect)
const EARLY_SKIP: [u16; 5] = [0xE5, 0x12C, 0x132, 0x134, 0x136];

// RenderSlot field offsets (stride 0x12)
const RS_X: usize = 0;
const RS_Y: usize = 2;
const RS_SPRITE: usize = 4;
const RS_FLAGS: usize = 5;
const RS_SOURCE: usize = 9;
// (RS_LIFE unused — player death_state uses PV_DEATH_STATE directly)

// ObjectSlot field offsets (same layout, different names)
const OS_DEF_PTR: usize = 6;
const OS_XVEL: usize = 8;
const OS_YVEL: usize = 0x0A;
const OS_ANIM_PTR: usize = 0x0C;
const OS_STATE: usize = 0x0E;
// (OS_HP not directly accessed in player_interaction)
const OS_HITS: usize = 0x10;

// EffectParticle field offsets (same layout, different names)
const EP_XVEL: usize = 6;
const EP_LIFETIME: usize = 0x0C;
const EP_YVEL: usize = 0x0E;

// PlayerView field offsets (extends RenderSlot)
const PV_XVEL: usize = 6;
const PV_ANIM_B: usize = 0x0B;
// (PV_ANIM_PTR handled by advance_anim_script via OS_ANIM_PTR)
const PV_YVEL: usize = 0x0E;
const PV_RUN_FLAG: usize = 0x10;
const PV_DEATH_STATE: usize = 0x11;

#[inline]
fn _s16(v: i32) -> i32 { let v = v & 0xFFFF; if v & 0x8000 != 0 { v - 0x10000 } else { v } }

#[inline]
fn s16u(v: u16) -> i32 {
    let v = v as i32;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

// ========================================================================
// offcamera_trigger (65B3) — death/respawn trigger
// ========================================================================
fn offcamera_trigger(state: &mut NativeGameState) {
    let respawn = state.rb(RESPAWN_STATE);
    if respawn != 0 { return; }
    let lives = state.rb(LIVES);
    if lives == 0 {
        state.wb(END_SIGNAL, 1); // game over
        return;
    }
    state.wb(LIVES, lives - 1);
    state.wb(ENERGY, 0);
    state.wb(RESPAWN_STATE, 2);
}

// ========================================================================
// spawn_pickup_effect (8875) — add score + spawn popup at source position
// ========================================================================
fn spawn_pickup_effect(state: &mut NativeGameState, eff_id: u16, src_si: usize) {
    // [887B] score-add for collectible ids 0x4A..0x5A
    let bx = eff_id.wrapping_sub(0x4A);
    if bx <= 0x10 {
        let val = state.rw(SCORE_TABLE + (bx as usize) * 2); // [8887] table word
        let score_lo = state.rw(SCORE_LO) as u32;
        let score_hi = state.rw(SCORE_HI) as u32;
        let score = (score_lo | (score_hi << 16)) + val as u32; // [888B/888F] 32-bit add
        state.ww(SCORE_LO, (score & 0xFFFF) as u16);
        state.ww(SCORE_HI, (score >> 16) as u16);
    }
    // [8894] allocate a free effect slot ([+4]==0xFFFF)
    let src_x = state.rw(src_si + RS_X);
    let src_y = state.rw(src_si + RS_Y);
    for k in 0..EFFECT_LIST_COUNT {
        let slot = EFFECT_LIST + k * 0x12;
        if state.rw(slot + RS_SPRITE) == 0xFFFF {
            state.ww(slot + RS_SPRITE, eff_id);     // [88A7] effect id
            state.ww(slot + RS_X, src_x);            // [88AA] X = [src]
            state.ww(slot + RS_Y, src_y);            // [88AE] Y = [src+2]
            state.ww(slot + 0x0C, 0x2C);             // [88B4] lifetime
            state.ww(SPAWNED_PTR, slot as u16);     // [88B9] spawned_ptr
            // [88BD] if src_si is a loop2 entity (>= 0x50A8) -> consume the link
            if src_si >= ENTITY2 {
                let link = state.rw(src_si + RS_SOURCE);
                if link != 0xFFFF {                  // [88C7]
                    state.ww(link as usize + RS_SPRITE, 0xFFFF); // [88CC]
                }
            }
            return;
        }
    }
    // [88A5] no slot -> only the score landed
}

// ========================================================================
// advance_anim_script (80CB) — advance object's anim-script past next 0x7D00
// ========================================================================
fn advance_anim_script(state: &mut NativeGameState, di: usize) {
    let mut si = state.rw(di + OS_ANIM_PTR);
    loop {                                           // [80CF] si += 2 until [si]==0x7D00
        si = si.wrapping_add(2);
        if state.rw(si as usize) == ANIM_SECTION_MARKER {
            break;
        }
    }
    state.ww(di + OS_ANIM_PTR, si.wrapping_add(2));  // [80D7] past the marker
}

// ========================================================================
// _knockback (837A) — knock the player up
// ========================================================================
fn knockback(state: &mut NativeGameState, yvel: u16) {
    state.ww(PLAYER + PV_YVEL, yvel);                 // [837A] Yvel = yvel
    state.wb(FALL_FRAMES, 0);                         // [837E] clear [0x6BD2]
    let hit_detail = state.rw(HIT_DETAIL);            // [0xA331]
    state.ww(PLAYER + RS_Y, state.rw(PLAYER + RS_Y).wrapping_sub(hit_detail)); // player Y -= [0xA331]
}

// ========================================================================
// _hurt (8348) — player hurt by object di
// ========================================================================
fn hurt(state: &mut NativeGameState, di: usize) {
    native_play_sfx(state, 3);                        // [834B] play_sfx(3)
    let v10 = state.rb(di + OS_HITS);
    let hc = v10 >> 2;                                // [834E] hit count
    if hc != 0xB {                                    // [8355] cap
        state.wb(di + OS_HITS, v10.wrapping_add(4)); // [8359]
        let cnt = hc.wrapping_add(1);                 // [835D] inc
        if cnt & 1 == 0 {                             // [8360] only on even counts
            let eff = state.rw(HURT_SFX_TABLE + cnt as usize); // [8366]
            spawn_pickup_effect(state, eff, PLAYER);  // [836A]
        }
    }
    let yvel = if state.rb(IN_UP) != 0 { 0xFF20u16 } else { 0xFFC0 }; // [836D/8377]
    knockback(state, yvel);
}

// ========================================================================
// _death (838A) — player killed by object di
// ========================================================================
fn death(state: &mut NativeGameState, di: usize) {
    native_play_sfx(state, 9);                        // [838D] play_sfx(9)
    let defp = state.rw(di + OS_DEF_PTR) as usize;    // [8390] di = [di+6] (the type def)
    let d4 = state.rb(defp + 4);
    state.wb(defp + 4, d4 & 0xFE);                    // [8393] defv.d4 &= 0xFE
    state.wb(PLAYER + PV_DEATH_STATE, 0x2C);          // [8397] death_state
    state.wb(ANIM_GATE, 0);                            // [839C]
    state.ww(PLAYER + PV_YVEL, 0xFF80);               // [83A1]
    let xvel = state.rw(PLAYER + PV_XVEL) as i32;
    state.ww(PLAYER + PV_XVEL, (((-xvel) << 2) & 0xFFFF) as u16); // [83A7] = -(Xvel<<2)
    state.wb(LOW_GRAVITY, 0);                          // [83B3]
    let old = state.rb(GLIDER);                        // [83B8] cmp before the clear
    state.wb(GLIDER, 0);                               // [83BD]
    if old == 0 {                                      // [83C2] jne skip
        let n = state.rb(ENERGY).wrapping_sub(1);     // [83C4] dec
        state.wb(ENERGY, n);
        if n & 0x80 != 0 {                             // [83C8] jns skip -> call only if went negative
            offcamera_trigger(state);                  // [83CA] 65B3
        }
    }
}

// ========================================================================
// _stomp (82F7) — player stomps object di
// ========================================================================
fn stomp(state: &mut NativeGameState, di: usize) {
    let v10 = state.rb(di + OS_HITS);
    let dl = v10 & 3;
    spawn_pickup_effect(state, ((dl as u16) << 1).wrapping_add(0x52), PLAYER); // [82F7..8304]
    let flags = state.rb(di + RS_FLAGS);
    state.wb(di + RS_FLAGS, flags | 0x40);            // [8307] mark stomped
    if dl == 2 {                                       // [830B] kill
        state.wb(di + OS_STATE, 0xFF);                 // [8310]
        advance_anim_script(state, di);                // [8314] 80CB
        let defp = state.rw(di + OS_DEF_PTR) as usize;
        let d4 = state.rb(defp + 4);
        state.wb(defp + 4, d4 & 0xF7);                  // [831A]
        state.ww(di + OS_YVEL, 0xFF38);                 // [831E] object Yvel up
        let pv_yvel = s16u(state.rw(PLAYER + PV_YVEL));
        let ax = pv_yvel.abs();                         // [8323] |player Yvel|
        let obj_x = s16u(state.rw(di + RS_X));
        let px = s16u(state.rw(PLAYER + RS_X));
        let push = if obj_x > px { ax } else { -ax };  // [8330] obj left of player -> push left
        state.ww(di + OS_XVEL, ((push * 3) & 0xFFFF) as u16); // [8336]
        return;
    }
    state.wb(di + OS_HITS, v10.wrapping_add(1));      // [8340]
    knockback(state, 0xFFA0);                           // [8343] ax=0xffa0 -> 837A
}

// ========================================================================
// _loop1_hit_outcome (82C8..82F7) — dispatch a player-vs-object hit
// ========================================================================
fn loop1_hit_outcome(state: &mut NativeGameState, di: usize) {
    let hit_flag = state.rb(HIT_FLAG);
    let pyvel = s16u(state.rw(PLAYER + PV_YVEL));
    if hit_flag == 0 || pyvel < 0 {                     // [82D5 je / 82DF]
        death(state, di);                               // die
        return;
    }
    if state.rb(LOW_GRAVITY) == 0 {                    // [82E9] hurt
        hurt(state, di);
        return;
    }
    if pyvel <= 0x20 {                                  // [82F0] not falling -> bump
        knockback(state, 0xFFA0);
        return;
    }
    stomp(state, di);                                   // [82F7] stomp
}

// ========================================================================
// loop1 (829F..83D4) — walk 12 object slots vs the player
// Returns true if an interaction occurred (loop2 should be skipped).
// ========================================================================
pub fn loop1(state: &mut NativeGameState) -> bool {
    let death_state = state.rb(PLAYER + PV_DEATH_STATE);
    if death_state != 0 { return false; }               // [8295] already dying -> straight to loop2

    let mut di = OBJ_BASE;
    for _ in 0..OBJ_COUNT {                             // [82A5] cx=0xC
        let sprite = state.rw(di + RS_SPRITE);
        let flags = state.rb(di + RS_FLAGS);
        let obj_state = state.rb(di + OS_STATE);
        let def_ptr = state.rw(di + OS_DEF_PTR) as usize;
        let d4 = state.rb(def_ptr + 4);

        if sprite != 0xFFFF && (flags & 0x20) != 0      // [82A8..82B2]
            && (d4 & 0x10) == 0                          // [82B7]
            && obj_state != 0xFF                         // [82BD]
        {
            let hit = combat_interaction::hitbox_overlap(state, PLAYER, di); // [82C3] 8D7B
            if hit {
                let scale = state.rw(SCALE_LEVEL);
                if scale != 0 {                           // [82C8] scale/zoom active -> instant death, keep walking
                    combat_interaction::death_handler(state, def_ptr, di, PLAYER); // [82CF] 8C72
                } else {
                    loop1_hit_outcome(state, di);
                    return true;
                }
            }
        }
        di += OBJ_STRIDE;                                // [83CE]
    }
    false
}

// ========================================================================
// _consume_link (853F) — consume [si+9] linked entity
// ========================================================================
fn consume_link(state: &mut NativeGameState, si: usize) {
    let bx = state.rw(si + RS_SOURCE);                   // [si+9]
    if bx != 0xFFFF {
        state.ww(bx as usize + RS_SPRITE, 0xFFFF);       // [bx+4] = 0xFFFF
    }
}

// ========================================================================
// _count_and_score (85B6) — shared food/collectible tail
// ========================================================================
fn count_and_score(state: &mut NativeGameState, si: usize, num: u16) {
    let idx = num.wrapping_sub(0x39) as usize;           // num-57
    let old_q = state.rb(ITEM_QUEUE + idx);
    state.wb(ITEM_QUEUE + idx, old_q.wrapping_add(1));  // item_queue[idx]++
    let total = state.rw(ITEM_TOTAL);
    state.ww(ITEM_TOTAL, total.wrapping_add(1));         // item_total++
    let lut_val = state.rb(SCORE_SPR_LUT + idx) as u16;
    let eff = lut_val.wrapping_add(0x4A);                // score_spr_lut[idx] + 0x4A
    if state.rw(si + RS_SOURCE) != 0xFFFF {              // [85CC] linked -> bump the linked count
        let cl = state.rw(COLLECTED_LINKED);
        state.ww(COLLECTED_LINKED, cl.wrapping_add(1));
    }
    spawn_pickup_effect(state, eff, si);                 // [860B] spawn at eff id
}

// ========================================================================
// _food_fountain (94F3) — erupt 4 food sprites with alternating-spread
// ========================================================================
fn food_fountain(state: &mut NativeGameState) {
    let mut ax: u16 = 0x20;                              // [94F9]
    let mut dx: u16 = 0xFF60;                            // [94FC]
    for _ in 0..4 {                                      // [94F6] cx=4
        let sid = combat_interaction::roll_bonus_sprite_id(state); // [9504] 8C13
        state.ww(BURST_SPRITE, sid);                     // [9507] burst sprite id
        combat_interaction::spawn_effect_burst(state, ax, dx, 1); // [950B] 8D1B, one sprite
        ax = 0u16.wrapping_sub(ax);                      // [950E] neg ax (alternating spread)
        if ax & 0x8000 == 0 {                             // [9510] js not taken (positive) -> step down
            ax = ax.wrapping_sub(0x10);                  // [9512]
            dx = dx.wrapping_sub(0x10);                  // [9515]
        }
    }
}

// ========================================================================
// _bone_burst (867E) — scatter bone sprites at (playerX, playerY-0x30)
// ========================================================================
fn bone_burst(state: &mut NativeGameState) {
    let px = state.rw(PLAYER + RS_X);
    let py = state.rw(PLAYER + RS_Y);
    state.ww(BURST_X, px);                                // [867F/8682] [0xA336] = player X
    state.ww(BURST_Y, py.wrapping_sub(0x30));            // [8685/8688/868B] [0xA338] = player Y - 0x30
    let energy = state.rb(ENERGY);
    let hc = state.rb(HURT_COOLDOWN);
    let cnt = ((6 * energy as u16) + hc as u16) & 0xFF;  // [868E mul 6] + [8696 add]
    if cnt == 0 {                                        // [869A] je -> nothing to scatter
        return;
    }
    state.wb(ENERGY, 0);                                 // [86A2] [0x27D6] = 0
    state.wb(HURT_COOLDOWN, 0);                           // [86A7] [0x6BC9] = 0
    state.ww(BURST_SPRITE, 0x2046);                      // [86AC] burst sprite id
    combat_interaction::spawn_effect_burst(state, 0x30, 0xFF80, cnt); // [86B2] 8D1B
}

// ========================================================================
// _kill_all_screen (86B7/870A) — walk 12 object slots, run per_enemy on each
// active enemy, then consume the linked entity [si+9].
// ========================================================================
fn kill_all_screen_grenade(state: &mut NativeGameState, si: usize) {
    // [86E1] each enemy dies via the recovered 8C72 death_handler
    let mut di = OBJ_BASE;
    for _ in 0..OBJ_COUNT {
        let sprite = state.rw(di + RS_SPRITE);
        if sprite != 0xFFFF {                             // [86CC] slot active
            let def_ptr = state.rw(di + OS_DEF_PTR) as usize;
            let d4 = state.rb(def_ptr + 4);
            let flags = state.rb(di + RS_FLAGS);
            if (d4 & 0x10) == 0 && (flags & 0x20) != 0 { // [86D5/86DB]
                combat_interaction::death_handler(state, def_ptr, di, si); // 8C72
            }
        }
        di += OBJ_STRIDE;                                // [86E4]
    }
    // [86F7] consume the linked entity
    let link = state.rw(si + RS_SOURCE);
    if link != 0xFFFF {
        state.ww(link as usize + RS_SPRITE, 0xFFFF);
    }
}

fn kill_all_screen_bomb(state: &mut NativeGameState, si: usize) {
    // [8734] erase the enemy + erupt a fountain at its pos
    let mut di = OBJ_BASE;
    for _ in 0..OBJ_COUNT {
        let sprite = state.rw(di + RS_SPRITE);
        if sprite != 0xFFFF {                             // [871F] slot active
            let def_ptr = state.rw(di + OS_DEF_PTR) as usize;
            let d4 = state.rb(def_ptr + 4);
            let flags = state.rb(di + RS_FLAGS);
            if (d4 & 0x10) == 0 && (flags & 0x20) != 0 { // [86D5/86DB]
                // _bomb per-enemy: erase + food fountain
                state.ww(BURST_X, state.rw(di + RS_X));  // [8736]
                state.ww(BURST_Y, state.rw(di + RS_Y));  // [873C]
                state.wb(di + OS_STATE, 0xFF);           // [873F] mark dead
                state.ww(di + RS_SPRITE, 0xFFFF);        // [8743] free the slot
                food_fountain(state);                     // [8748] 94F3
            }
        }
        di += OBJ_STRIDE;                                // [874B]
    }
    // [8759] consume the linked entity
    let link = state.rw(si + RS_SOURCE);
    if link != 0xFFFF {
        state.ww(link as usize + RS_SPRITE, 0xFFFF);
    }
}

// ========================================================================
// rng_roll — advance the LCG (39DF), write state back, return AL
// (advance the shared 4-byte PRNG)
// ========================================================================
fn rng_roll(state: &mut NativeGameState) -> u8 {
    let a = state.rb(0x2CEC);
    let b = state.rb(0x2CED);
    let c = state.rb(0x2CEE);
    let d = state.rw(0x2CEF);
    let (na, nb, nc, nd, ret) = prng::rng_lcg(a, b, c, d);
    state.wb(0x2CEC, na);
    state.wb(0x2CED, nb);
    state.wb(0x2CEE, nc);
    state.ww(0x2CEF, nd);
    ret
}

// ========================================================================
// loop2_handler — dispatch a pickup hit to its effect
// num = (spr_num & 0x1FFF) - 0x35
// ========================================================================
fn loop2_handler(state: &mut NativeGameState, num: u16, si: usize) {
    let pv_x = state.rw(PLAYER + RS_X);
    let pv_y = state.rw(PLAYER + RS_Y);

    if num == 0x91 {                                     // id 0xc6 [885F] "tap"
        // clear fly timers, then count
        for k in 0..0x14 {                                // [8861] table 0x6EA9, 0x14 * 8
            state.wb(WALL_MARKER_TABLE + k * 8 + 7, 7); // WallMarker.b7 = 7
        }
        count_and_score(state, si, num);
        native_play_sfx(state, 8);
        return;
    }
    if num == 0xE2 {                                     // id 0x117 [882A] end-of-level
        let lvl = state.rb(LEVEL);
        let nxt = match lvl { 2 => Some(0xCu8), 0xD => Some(2), 6 => Some(0xE), 0xF => Some(6), _ => None };
        if let Some(n) = nxt { state.wb(LEVEL, n); }
        state.wb(LEVEL_END_MODE, 1);
        return;
    }
    if num == 0x102 {                                    // id 0x137 [8859] game complete
        state.wb(LEVEL_END_MODE, 0xFF);
        return;
    }
    if num == 0xE4 {                                     // id 0x119 [87FD] checkpoint
        state.ww(CHECKPOINT_X, pv_x);
        state.ww(CHECKPOINT_Y, pv_y);
        for k in 0..EFFECT_SOURCES_COUNT {                // [8809] reveal item 0x118 in the 0x8F1D table
            let src_off = EFFECT_SOURCES + k * 7;
            if state.rw(src_off + 4) == 0x118 {           // src.sprite == 0x118
                state.ww(src_off + 4, 0x119);             // src.sprite = 0x119
            }
        }
        let bx = state.rw(si + RS_SOURCE);
        if bx != 0xFFFF {
            let old_spr = state.rw(bx as usize + RS_SPRITE);
            state.ww(bx as usize + RS_SPRITE, old_spr.wrapping_sub(1)); // link.sprite -= 1
        }
        return;
    }
    if num == 0xAE {                                     // id 0xe3 [87E6] extra life
        let lives = state.rb(LIVES);                      // [65D6] 65DA cmp [0x27D8],0x63
        if lives < 0x63 {                                 // [65DF je / 65E1 jb] <99 -> inc
            state.wb(LIVES, lives.wrapping_add(1));       // [65E3]
        }
        spawn_pickup_effect(state, 0xE3, PLAYER);         // [87F6] 0xe3 effect at the player pos
        consume_link(state, si);                           // [87FA] jmp 853F
        native_play_sfx(state, 4);
        return;
    }
    if num == 0x0D || num == 0xB6 || num == 0x2C || num == 0xE0 { // ids 0x42/0xeb/0x61/0x115 [87AE] club/weapon 0-3
        let ct = match num { 0x0D => 0u8, 0xB6 => 1, 0x2C => 2, 0xE0 => 3, _ => 0 };
        state.wb(ATTACK_PHASE, ct);                        // attack_phase = ct
        consume_link(state, si);
        native_play_sfx(state, 8);
        return;
    }
    if num <= 0x14 {                                     // ids 0x35-0x49 [85DA] small energy bonus
        consume_link(state, si);
        let ctr = state.rb(HURT_COOLDOWN).wrapping_add(1);
        state.wb(HURT_COOLDOWN, ctr);
        if ctr >= 6 && state.rb(ENERGY) != 3 {
            state.wb(ENERGY, state.rb(ENERGY).wrapping_add(1));
            state.wb(HURT_COOLDOWN, 0);
            spawn_pickup_effect(state, 0xE2, si);
        }
        native_play_sfx(state, 8);
        return;
    }
    if num <= 0x2C {                                     // ids 0x4a-0x60 [8524] BONUS letters
        let idx = num.wrapping_sub(0x27);
        if idx <= 4 {
            let mask = state.rb(BONUS_LETTERS) | (1 << idx);
            state.wb(BONUS_LETTERS, mask);
        }
        consume_link(state, si);
        native_play_sfx(state, 8);
        return;
    }
    if num <= 0x32 {                                     // ids 0x62-0x67 [854F] utensils/tools
        let idx = num.wrapping_sub(0x2D) as u8;
        let mask = state.rb(UTENSILS_MASK) | (1 << idx);
        state.wb(UTENSILS_MASK, mask);
        if idx == 1 {                                     // lighter -> reveal the 0x116 semaphore item
            for k in 0..EFFECT_SOURCES_COUNT {
                let src_off = EFFECT_SOURCES + k * 7;
                if state.rw(src_off + 4) == 0x116 {
                    state.ww(src_off + 4, 0x117);
                }
            }
        }
        consume_link(state, si);
        native_play_sfx(state, 8);
        return;
    }
    if num <= 0x40 {                                     // ids 0x68-0x75 [8582] food (bounce or score)
        let ydir = s16u(state.rw(si + EP_YVEL));
        if ydir < 0x80 {                                  // low -> count + score (shared 85B6)
            count_and_score(state, si, num);
            native_play_sfx(state, 4);
            return;
        }
        // bounce up
        state.ww(si + EP_YVEL, (0u16).wrapping_sub(ydir as u16)); // item2.yvel = -ydir
        let ret = rng_roll(state);                        // [asm call 39DF] advance + write back the LCG
        let xv: u16 = if ret & 1 != 0 {
            state.wb(CAMERA_SHAKE, 7);
            0x20u16.wrapping_neg()                        // -0x20
        } else {
            0x20
        };
        state.ww(si + EP_XVEL, xv);                       // item2.xvel = xv
        state.ww(si + RS_SPRITE, state.rw(BURST_SPRITE)); // item2.sprite = g.burst_sprite
        native_play_sfx(state, 4);
        return;
    }
    if num <= 0x4A {                                     // ids 0x76-0x7f [850A] flying power-up
        if state.rb(GLIDER) == 0 {
            state.wb(GLIDER, 1);                          // glider = 1
            state.wb(PLAYER + PV_ANIM_B, 0xFF);          // anim_b = 0xFF
            consume_link(state, si);
        }
        native_play_sfx(state, 8);
        return;
    }
    if num <= 0xA6 {                                     // ids 0x80-0xdb [85B0] collectibles -> score
        count_and_score(state, si, num);
        native_play_sfx(state, 8);
        return;
    }
    if num == 0xAD {                                     // id 0xe2 [84F6] energy refill (+1 if < 3)
        if state.rb(ENERGY) < 3 {
            state.wb(ENERGY, state.rb(ENERGY).wrapping_add(1));
            consume_link(state, si);
            native_play_sfx(state, 4);
            return;
        }
        return;                                          // full -> nothing (the 8509 ret)
    }
    if num == 0xA7 || num == 0xA8 {                       // ids 0xdc/0xdd [864F] trap hit (scatter bones)
        // VERIFIED byte-exact on the skull-trap witness 202721
        state.wb(PLAYER + PV_DEATH_STATE, 0x2C);         // [8655] enter hurt/death state
        state.wb(PLAYER + PV_RUN_FLAG, 0);               // [865A]
        state.wb(PLAYER + PV_ANIM_B, 8);                  // [865F] anim 8
        bone_burst(state);                                 // [8664] 867E scatter the player's bones
        state.wb(CAMERA_SHAKE, 7);                        // [8667]
        let link = state.rw(si + RS_SOURCE);              // [866C] consume the linked entity
        if link != 0xFFFF {
            state.ww(link as usize + RS_SPRITE, 0xFFFF);
        }
        spawn_pickup_effect(state, 0xE4, si);             // [8679] ax=0xe4 -> 860B
        native_play_sfx(state, 1);
        return;
    }
    if num == 0xA9 {                                      // id 0xde [86B7] grenade: kill every on-screen enemy
        kill_all_screen_grenade(state, si);
        state.wb(CAMERA_SHAKE, 9);                        // [86E9] screen shake
        spawn_pickup_effect(state, 0xE6, si);             // [8704] ax=0xe6 -> 860B
        native_play_sfx(state, 0);
        return;
    }
    if num == 0xAA {                                      // id 0xdf [870A] bomb: kill all + food fountains
        kill_all_screen_bomb(state, si);
        spawn_pickup_effect(state, 0xE7, si);             // [8766] ax=0xe7 -> 860B
        native_play_sfx(state, 0);
        return;
    }
    if num == 0xB5 {                                     // id 0xea [876C] light OFF
        if state.rb(LIGHTS_OFF) != 1 {
            state.wb(LIGHT_TO_LIGHT, 0);                  // to_light = 0
            state.wb(LIGHT_TO_DARK, 1);                   // to_dark = 1
            state.wb(LIGHT_STEP, 0);                      // step = 0
            state.wb(LIGHTS_OFF, 1);                      // lights_off = 1
            consume_link(state, si);
            native_play_sfx(state, 1);
            return;
        }
        consume_link(state, si);
        return;
    }
    if num == 0xB4 {                                     // id 0xe9 [8790] light ON
        if state.rb(LIGHTS_OFF) != 0 {
            state.wb(LIGHT_TO_DARK, 0);                   // to_dark = 0
            state.wb(LIGHT_TO_LIGHT, 1);                  // to_light = 1
            state.wb(LIGHT_STEP, 0);                      // step = 0
            state.wb(LIGHTS_OFF, 0);                      // lights_off = 0
        }
        consume_link(state, si);
        return;
    }
    // [asm 84F3 jmp 860E] an unmapped id is a NO-OP
}

// ========================================================================
// _boss_projectile (8618) — boss-projectile hit (loop2 ids 0x1CA/0x1CB)
// ========================================================================
fn boss_projectile(state: &mut NativeGameState) {
    if state.rb(ENERGY) < 1 {                            // [8618] cmp [0x27D6],1 ; jae
        offcamera_trigger(state);                         // [861F] 65B3 death/respawn
        return;
    }
    state.wb(PLAYER + PV_DEATH_STATE, 0x2C);             // [862A] hurt state
    state.wb(PLAYER + PV_RUN_FLAG, 0);                    // [862F]
    state.wb(PLAYER + PV_ANIM_B, 8);                     // [8634] anim 8
    let e = state.rb(ENERGY).wrapping_sub(1);            // [8639] dec [0x27D6]
    state.wb(ENERGY, 1);                                 // [8641] force ENERGY=1 so 867E scatters 6 bones
    bone_burst(state);                                    // [8646] 867E (also zeroes ENERGY + bonus)
    state.wb(ENERGY, e);                                  // [864A] restore the decremented energy
    native_play_sfx(state, 1);
}

// ========================================================================
// loop2 (83D7..8617) — walk the 52-entry pickup list vs the player
// ========================================================================
pub fn loop2(state: &mut NativeGameState) {
    let mut si = ENTITY2;
    for _ in 0..ENTITY_COUNT {                             // cx=0x34 (52)
        let sid = state.rw(si + RS_SPRITE);
        let lifetime = s16u(state.rw(si + EP_LIFETIME));
        let flags = state.rb(si + RS_FLAGS);

        if sid != 0xFFFF && lifetime <= 0xBC              // [83E0/83E9] live + not-yet-active
            && (flags & 0x20) != 0                         // [83F3] collectible flag
        {
            let hit = combat_interaction::hitbox_overlap(state, si, PLAYER); // [83FC] 8D7B
            if hit {
                let aid = sid & 0x1FFF;                    // [8404/8407]
                if !EARLY_SKIP.contains(&aid) {
                    // [8426] consume: [0xA33A] stores the FULL spr_num
                    state.ww(si + RS_SPRITE, 0xFFFF);     // RenderSlot.sprite = 0xFFFF
                    state.ww(BURST_SPRITE, sid);           // burst_sprite = sid (full sprite word)
                    if aid == 0x1CA || aid == 0x1CB {       // [8432] boss projectile (8618)
                        boss_projectile(state);
                    } else {
                        let num = aid.wrapping_sub(0x35);
                        loop2_handler(state, num, si as usize);
                    }
                }
            }
        }
        si += 0x12;                                       // [860E]
    }
}

// ========================================================================
// player_interaction_tick (8295..8617) — the whole player<->world interaction
// ========================================================================
pub fn player_interaction_tick(state: &mut NativeGameState) {
    if loop1(state) {
        return;
    }
    loop2(state);
}
