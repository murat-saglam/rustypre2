// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 object/effect spawner pre-amble (1030:6822) + camera engine.
//!
//! Dispatches: camera_engine (70D7), level-6 boss (6D34), mode-9 boss (6ADD).
//! This is the spawner head that falls through into object_tick (684E).
//!
//! All functions use the direct NativeGameState API (state.rb/rw/wb/ww) — no dict/overlay.
//! All operations mutate `NativeGameState` directly in place mutation,
//! since later reads naturally see earlier writes in a mutable borrow.

use crate::engine::state::NativeGameState;
use crate::recovered::combat_interaction;
use crate::recovered::prng;

// ── helpers ─────────────────────────────────────────────────────

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

#[inline]
fn sar16(v: u16, n: u32) -> u16 {
    ((v as i16) >> n) as u16
}

#[inline]
fn abs16(v: u16) -> u16 {
    (v as i16).unsigned_abs()
}

/// `al -> ax` sign-extend (cbw / cwde).
#[inline]
fn cbw(v: u8) -> u16 {
    (v as i8) as i16 as u16
}

// ── named DGROUP offsets ────────────────────────────────────────

// Camera/script pointers (values stored into the 0xA4xx camera-script region)
const CAM_SCRIPT_INTRO: u16 = 0xA427;
const CAM_SCRIPT_PAN: u16 = 0xA434;
const CAM_SCRIPT_SPAWN: u16 = 0xA43B;
const CAM_SCRIPT_IDLE: u16 = 0xA45E;
const CAM_SCRIPT_IDLE_HI: u16 = 0xA460;
const CAM_SCRIPT_IDLE_LO: u16 = 0xA462;
const CAM_SCRIPT_73A4: u16 = 0xA464;
const CAM_SCRIPT_73D1: u16 = 0xA46B;
const CAM_SCRIPT_73DA: u16 = 0xA470;
const CAM_SCRIPT_743D: u16 = 0xA475;
const CAM_SCRIPT_7471: u16 = 0xA47E;
const CAM_SCRIPT_7481: u16 = 0xA480;
const CAM_CMD_TABLE: usize = 0xA571;

const BOSS_SCRIPT_HIT: u16 = 0xA534;
const BOSS_SCRIPT_CYCLE: u16 = 0xA540;
const BOSS_SCRIPT_M9: u16 = 0xA4F9;

const L6_SUB_B_TABLE: usize = 0xA4B5;
const L6_SUB_A_TABLE: usize = 0xA485;
const L6_ANIM_TABLE: usize = 0xA4E5;

// Camera-target param/position offsets
const CAM_PARAM_1: usize = 0xA407;
const CAM_PARAM_2: usize = 0xA40D;
const CAM_T0_X: usize = 0xA409;
const CAM_T0_Y: usize = 0xA40B;
const CAM_T1_PARAM: usize = 0xA413;
const CAM_T1_X: usize = 0xA40F;
const CAM_T1_Y: usize = 0xA411;
const CAM_T2_PARAM: usize = 0xA419;
const CAM_T2_X: usize = 0xA415;
const CAM_T2_Y: usize = 0xA417;
const CAM_T3_PARAM: usize = 0xA41F;
const CAM_T3_X: usize = 0xA41B;
const CAM_T3_Y: usize = 0xA41D;

// Effect row (7585)
const EFFECT_ROW_LO: usize = 0x56A2;
const EFFECT_ROW_N: usize = 8;
const EFFECT_ROW_STRIDE: usize = 0x12;
const EFFECT_SPRITE: u16 = 0x135;
const EFFECT_X0: u16 = 0xD;
const EFFECT_DX: u16 = 5;
const EFFECT_Y: u16 = 0xAA;
const BOSS_SONG_INDEX: u16 = 0xD;

// Scroll phase
const SCROLL_PHASE: usize = 0x6C05;

// Camera-target collision scan (80DE)
const SCAN_PLAYER: usize = 0x4F0A;
const SCAN_PROJ_LO: usize = 0x4F2E;
const SCAN_PROJ_N: usize = 4;
const TARGET_A: usize = 0xA423;
const TARGET_B: usize = 0xA425;

// Spawn gate / scroll cursor (70D7 head)
const SPAWN_COUNT: usize = 0x91FC;
const SPAWN_GATE_PTR: usize = 0x564C;
const CURSOR_X: usize = 0x91FF;
const CURSOR_Y: usize = 0x9201;
const CURSOR_X_LO: usize = 0x91F7;
const CURSOR_X_HI: usize = 0x91F9;
const SCROLL_VX: usize = 0x6C06;
const SCROLL_VY: usize = 0x6C08;
const SCROLL_STOP_FLAG: usize = 0x6BEA;
const CAM_STATE: usize = 0x91FE;
const SCROLL_GRAVITY: u16 = 0x10;
const SCROLL_VY_GRAV_CAP: u16 = 0xE0;

// Player-cursor distance (7172)
const PLAYER_X: usize = 0x4F1C;
const PLAYER_Y: usize = 0x4F1E;
const DIST_DIR: usize = 0xA3FA;
const DIST_X: usize = 0xA3FB;
const CULL_X: u16 = 0x190;
const CULL_Y: u16 = 0xFA;

// Hurt effect (824D)
const HURT_COOLDOWN: usize = 0x6BC9;
const ENERGY: usize = 0x27D6;
const HURT_FX_X: usize = 0xA336;
const HURT_FX_Y: usize = 0xA338;
const HURT_FX_SPRITE: usize = 0xA33A;

// Camera target bounce (81F3)
const SCROLL_PUSH: usize = 0x91FB;
const CAM_TARGET_A: usize = 0xA421;
const CAM_TARGET_B: usize = 0xA423;
const CAM_TARGET_C: usize = 0xA425;

// Camera state machine
const CAM_TIMER: usize = 0xA3F7;
const SCRIPT_PTR: usize = 0xA401;
const HIT_DEBOUNCE: usize = 0xA3FD;
const FRAME_STAMP: usize = 0x6BD5;
const ATTACK_V19: usize = 0x7B19;

// Camera offset table (94DC)
const CAM_OFFSET_TABLE: usize = 0xA6ED;

// Camera target geometry (93F6)
const CURSOR_SNAP_X: usize = 0xA403;
const CURSOR_SNAP_Y: usize = 0xA405;
const CMD_BYTE: usize = 0xA3F9;

// Camera script (7534)
const SCRIPT_CURSOR: usize = 0xA3FF;
const SCRIPT_LAST: usize = 0x6C0A;

// Camera script command (93B2)
const TARGET_RECORDS: usize = 0x5648;
const TARGET_STRIDE: usize = 0x12;

// Mode-9 boss (6ADD)
const M9_PTR: usize = 0xA4F7;
const M9_COUNT: usize = 0xA519;
const BOSS_SCRIPT_PTR: usize = 0xA517;
const BOSS_DWELL: usize = 0xA515;
const BOSS_CYCLE: usize = 0xA516;

// Boss projectile pool (6CA7/6CF1)
const BOSS_POOL_LO: usize = 0x50A8;
const BOSS_POOL_N: usize = 0x20;
const BOSS_POOL_STRIDE: usize = 0x12;

// Boss zone (6B1C)
const BOSS_ZONE_X_LO: u16 = 0xB9;
const BOSS_ZONE_X_SZ: u16 = 0x32;
const BOSS_ZONE_Y_LO: u16 = 0x1E;
const BOSS_ZONE_Y_SZ: u16 = 0x32;

// L6 boss (6D34)
const L6_PROJ_LIST: usize = 0x7DAF;
const L6_PROJ_RENDER: usize = 0x55EE;
const L6_STUN: usize = 0xA324;
const L6_HITS: usize = 0xA325;
const L6_PHASE: usize = 0xA326;
const L6_ANIM: usize = 0xA328;
const L6_SUB_B: usize = 0xA329;
const L6_SUB_A: usize = 0xA32A;
const L6_TIMER: usize = 0xA32B;
const L6_RESEED: usize = 0xA32C;
const BOSS_HITBOX: usize = 0x5690;
const BOSS_HITBOX_ALT: usize = 0x567E;

// Player struct (PlayerView at 0x4F1C)
const PLAYER_BASE: usize = 0x4F1C;
const PLAYER_XVEL: usize = PLAYER_BASE + 0x06;
const PLAYER_MOTION_MODE: usize = PLAYER_BASE + 0x08;
const PLAYER_YVEL: usize = PLAYER_BASE + 0x0E;
const PLAYER_DEATH_STATE: usize = PLAYER_BASE + 0x11;
const PLAYER_SLOT0: usize = 0x4F0A;

// Misc globals
const ANIM_GATE: usize = 0x6BD0;
const LOW_GRAVITY: usize = 0x6BC7;
const HIT_VDETAIL: usize = 0xA330;
const HIT_PASS_FULL: usize = 0xA312;
const RESPAWN_STATE: usize = 0x6BE4;
const LIVES: usize = 0x27D8;
const END_SIGNAL: usize = 0x6BE5;
const FRAME_BLINK: usize = 0x6BD5;
const IN_UP: usize = 0x27EA;
const GLIDER: usize = 0x6BC5;
const BOSS_X: usize = 0x5648;
const BOSS_Y: usize = 0x564A;
const RNG_STATE: usize = 0x2CEC;
const MAP_SEG_PTR: usize = 0x2DDA;

// ── tile map helpers ─────────────────────────────────────────────

/// Read a byte from the level map (es:[off]).
fn read_tile(state: &NativeGameState, off: u16) -> u8 {
    let map_seg = state.rw(MAP_SEG_PTR) as usize;
    state.rb_abs((map_seg << 4) + (off as usize))
}

/// Floor property byte for a tile (table at 0x7F5E).
fn floor_props(state: &NativeGameState, tile: u8) -> u8 {
    state.rb(0x7F5E + tile as usize)
}

// ── 7585: init_effect_row ───────────────────────────────────────

/// [asm 7585] Spawn `min(cx,8)` sprite-0x135 effects into the 8-slot list at 0x56A2
/// (stride 0x12): `[+4]=0x135, [+0]=X (0xD, step 5), [+2]=Y (0xAA)`, then fill the
/// remaining slots with the 0xFFFF terminator. When `cx>0` it first loads song 0xD
/// (MONSTER.TRK) via 0x2CC.
pub fn init_effect_row(state: &mut NativeGameState, cx: u16) {
    let n = if cx < EFFECT_ROW_N as u16 { cx } else { EFFECT_ROW_N as u16 };
    let n = n as usize;
    let mut x = EFFECT_X0;
    if n > 0 {
        state.song_request = Some(BOSS_SONG_INDEX);
    }
    for i in 0..n {
        let slot = EFFECT_ROW_LO + i * EFFECT_ROW_STRIDE;
        state.ww(slot + 4, EFFECT_SPRITE);
        state.ww(slot, x);
        state.ww(slot + 2, EFFECT_Y);
        x = x.wrapping_add(EFFECT_DX);
    }
    for i in n..EFFECT_ROW_N {
        let slot = EFFECT_ROW_LO + i * EFFECT_ROW_STRIDE;
        state.ww(slot + 4, 0xFFFF);
    }
}

// ── 757A: inc_scroll_phase ──────────────────────────────────────

/// [asm 757A] Saturating increment of the scroll-phase counter [0x6C05]:
/// add 1, then clamp at 0xFF.
fn inc_scroll_phase(state: &mut NativeGameState) {
    let v = state.rb(SCROLL_PHASE);
    state.wb(SCROLL_PHASE, if v == 0xFF { 0xFF } else { v + 1 });
}

// ── 80DE: scan_camera_targets ───────────────────────────────────

/// [asm 8182] Test one sprite `si` against the camera targets.
/// Returns the ASM's CF at the RET (True = a "response" hit from target B).
fn target_collision(state: &mut NativeGameState, si: usize) -> bool {
    let slot_sprite = state.rw(si + 4);
    if slot_sprite == 0xFFFF {
        return false;
    }
    let di = state.rw(TARGET_A) as usize;
    let target_a_sprite = state.rw(di + 4);
    if (target_a_sprite & 0x1FFF) == 0x19C || (target_a_sprite & 0x1FFF) == 0x19D {
        let hit = combat_interaction::hitbox_overlap(state, si, di);
        if hit {
            state.ww(si + 4, 0xFFFF);
            return false;
        }
    }
    let target_b = state.rw(TARGET_B) as usize;
    combat_interaction::hitbox_overlap(state, si, target_b)
}

/// [asm 80DE..8181] Camera-target collision scan + response (run every frame from
/// the 70D7 tail): test the player (0x4F0A) then the 4 projectile slots (0x4F2E)
/// against the camera targets [0xA423]/[0xA425] via 8D7B. On a hit, and unless
/// debounced, run the response: latch [0xA3FD], sfx 8, set flash bit6 on all 5 target
/// records, [0x6BC5]=0, inc_scroll_phase, then decrement the level param [0x91FC] by
/// [0x7B19] -> state 6 + dive on underflow, else a state-5 scroll kick when
/// [0x7B19]>0x14; [0xA3F7]=0 and free the hitting slot.
pub fn scan_camera_targets(state: &mut NativeGameState) {
    let mut si_hit: Option<usize> = None;

    if target_collision(state, SCAN_PLAYER) {
        si_hit = Some(SCAN_PLAYER);
    }
    if si_hit.is_none() {
        for i in 0..SCAN_PROJ_N {
            let si = SCAN_PROJ_LO + i * 0x12;
            if target_collision(state, si) {
                si_hit = Some(si);
                break;
            }
        }
    }

    let si_hit = match si_hit {
        Some(si) => si,
        None => return,
    };

    let bd5 = state.rw(FRAME_STAMP);
    let debounce = state.rw(HIT_DEBOUNCE);
    if abs16(bd5.wrapping_sub(debounce)) <= 0x16 {
        return;
    }
    state.ww(HIT_DEBOUNCE, bd5);

    // sfx 8 (audio seam)
    crate::engine::audio::native_play_sfx(state, 8, None);

    // Flash all 5 boss target records (bit6 on the flags byte = high byte of sprite word)
    for k in 0..5 {
        let off = TARGET_RECORDS + k * TARGET_STRIDE + 5;
        state.wb(off, state.rb(off) | 0x40);
    }

    state.wb(GLIDER, 0); // [asm 812C]
    inc_scroll_phase(state); // [asm 8131] 757A

    let al = state.rb(ATTACK_V19); // [asm 8134]
    let nfc = state.rw(SPAWN_COUNT).wrapping_sub(al as u16); // [asm 8139]
    state.ww(SPAWN_COUNT, nfc);

    if nfc & 0x8000 != 0 {
        // [asm 813D] underflow -> finale approach
        state.wb(CAM_STATE, 6); // [asm 813F]
        state.ww(SCROLL_VY, 0xFF80); // [asm 8144]
        return;
    }

    if al > 0x14 {
        // [asm 814C] a scroll kick
        let dx = if s16(state.rw(CURSOR_X) as i32) >= s16(state.rw(PLAYER_X) as i32) {
            0x30u16
        } else {
            (0u16).wrapping_sub(0x30)
        };
        state.ww(SCROLL_VX, dx); // [asm 815E]
        let svy = state.rw(SCROLL_VY);
        state.ww(SCROLL_VY, if s16(svy as i32) > 0 { 0xFF80 } else { 0xFFC0 });
        state.wb(CAM_STATE, 5); // [asm 8171]
    }

    state.ww(CAM_TIMER, 0); // [asm 8176]

    // [asm 817C] free the hitting slot
    state.ww(si_hit + 4, 0xFFFF);
}

// ── 70D7 head: tick_scroll_cursor ───────────────────────────────

/// [asm 70D7..7172] The spawn gate + scroll-cursor advance (the every-frame core).
/// `read_tile` reads the level map (es=[0x2DDA]).
fn tick_scroll_cursor(state: &mut NativeGameState) {
    // Spawn gate: boss_targets[0].sprite (UNION: the sprite word at 0x5648+4 = 0x564C
    // doubles as the spawn gate ptr).
    let di = state.rw(SPAWN_GATE_PTR);
    let mut cx = (state.rw(SPAWN_COUNT) >> 3) & 0xFFFF;
    if di == 0xFFFF || state.rb(CAM_STATE) == 6 || (di & 0x2000) == 0 {
        cx = 0;
    }
    init_effect_row(state, cx); // [asm 70F9] 7585

    // [asm 70FE-7103] advance cursor X
    let scroll_vx = state.rw(SCROLL_VX);
    let cursor_x = state.rw(CURSOR_X);
    let ax = sar16(scroll_vx, 4).wrapping_add(cursor_x);
    let cur_x;
    if (ax & 0x8000) == 0
        && s16(state.rw(CURSOR_X_LO) as i32) <= s16(ax as i32)
        && s16(state.rw(CURSOR_X_HI) as i32) >= s16(ax as i32)
    {
        state.ww(CURSOR_X, ax); // [asm 7115] within bounds
        cur_x = ax;
    } else {
        cur_x = cursor_x;
    }

    // [asm 7118] advance cursor Y
    let scroll_vy = state.rw(SCROLL_VY);
    let cursor_y = state.rw(CURSOR_Y);
    let cur_y = cursor_y.wrapping_add(sar16(scroll_vy, 4));
    state.ww(CURSOR_Y, cur_y);

    let vy = scroll_vy;
    let mut landed = false;

    if s16(vy as i32) >= 0 {
        // [asm 7121-7126] scrolling down or still -> check the floor
        let col = (sar16(cur_x, 4) & 0xFF) as u16;
        let row = (sar16(cur_y, 4) & 0xFF) as u16;
        let tile = read_tile(state, (col | (row << 8)) & 0xFFFF); // [asm 7128-713B]
        if floor_props(state, tile) != 0 {
            // [asm 7142-7144] solid floor -> land
            state.ww(CURSOR_Y, cur_y & 0xFFF0); // [asm 7146] snap
            if s16(vy as i32) != 0 {
                state.wb(SCROLL_STOP_FLAG, 7); // [asm 714B]
            }
            state.ww(SCROLL_VY, 0); // [asm 7157]
            state.ww(SCROLL_VX, 0);
            landed = true;
        }
    }

    if !landed && (s16(vy as i32) < SCROLL_VY_GRAV_CAP as i32) {
        // [asm 7165] fall faster toward the floor (cap 0xE0)
        state.ww(SCROLL_VY, vy.wrapping_add(SCROLL_GRAVITY));
    }
}

// ── 7172: player_cursor_dist ────────────────────────────────────

/// [asm 7172..71AB] Player-vs-cursor distance. Sets [0xA3FA] (dir) and [0xA3FB]
/// (dist_x). Returns the cull flag: the camera state machine is skipped when
/// |player_X-cursor_X| > 0x190 or |player_Y-cursor_Y| > 0xFA.
fn player_cursor_dist(state: &mut NativeGameState) -> bool {
    let px = state.rw(PLAYER_X);
    let cur_x = state.rw(CURSOR_X);
    let dir_flag = if cur_x >= px { 0u8 } else { 1u8 }; // [asm 7177] jae (unsigned)
    let xd = abs16(px.wrapping_sub(cur_x)); // [asm 7185-718B]
    state.wb(DIST_DIR, dir_flag);
    state.ww(DIST_X, xd);
    if xd > CULL_X {
        // [asm 7190] jbe
        return true;
    }
    let py = state.rw(PLAYER_Y);
    let cur_y = state.rw(CURSOR_Y);
    let yd = abs16(py.wrapping_sub(cur_y)); // [asm 7198-71A1]
    yd > CULL_Y // [asm 71A3] jbe
}

// ── 65B3: player_death ─────────────────────────────────────────

/// [asm 65B3] Off-camera death/respawn trigger. If not already triggered
/// (respawn_state == 0): consume a life, reset energy, arm respawn (state=2);
/// if no lives remain, set game-over flag.
fn player_death(state: &mut NativeGameState) {
    if state.rb(RESPAWN_STATE) != 0 {
        return;
    }
    let lives = state.rb(LIVES);
    if lives == 0 {
        state.wb(END_SIGNAL, 1); // game over
        return;
    }
    state.wb(LIVES, lives - 1);
    state.wb(ENERGY, 0);
    state.wb(RESPAWN_STATE, 2);
}

// ── 824D: hurt_effect ───────────────────────────────────────────

/// [asm 824D] Player-hurt effect when the scroll boundary crushes the player.
/// Tick the damage cooldown [0x6BC9] (reload 5 + spend energy on underflow -> player
/// death 65B3 when lives deplete), record the hurt-burst origin, then spawn one hurt
/// sprite (8D1B, id 0x2046, Xvel +/-0x30 by [0x6BC9] parity, Yvel 0xFF80).
fn hurt_effect(state: &mut NativeGameState) {
    let cd_val = state.rb(HURT_COOLDOWN).wrapping_sub(1);
    let cd;
    if cd_val & 0x80 != 0 {
        // [asm 8254] dec underflowed -> reload + lose a life
        state.wb(HURT_COOLDOWN, 5);
        cd = 5u8;
        let energy = state.rb(ENERGY).wrapping_sub(1);
        state.wb(ENERGY, energy);
        if energy & 0x80 != 0 {
            // [asm 825F] energy underflow -> 65B3 player death
            player_death(state);
        }
    } else {
        state.wb(HURT_COOLDOWN, cd_val);
        cd = cd_val;
    }

    let pv_x = state.rw(PLAYER_X);
    let pv_y = state.rw(PLAYER_Y);
    state.ww(HURT_FX_X, pv_x); // [asm 8264]
    state.ww(HURT_FX_Y, pv_y.wrapping_sub(0x30)); // [asm 826A]
    let ax = if cd & 1 != 0 {
        (0u16).wrapping_sub(0x30)
    } else {
        0x30
    }; // [asm 8276-8280] sign by final [0x6BC9] parity
    state.ww(HURT_FX_SPRITE, 0x2046); // [asm 8285]
    combat_interaction::spawn_effect_burst(state, ax, 0xFF80, 1); // [asm 828B] 8D1B
}

// ── 81F3: camera_target_bounce ──────────────────────────────────

/// [asm 81F3] One camera-boundary target `di` vs the player (the per-target half of
/// the 81B4 crush): skip unless the target is active and a boundary; test the player
/// hitbox (8D7B). On overlap, knock the scroll phase down, hurt the player (824D),
/// and bounce.
fn camera_target_bounce(state: &mut NativeGameState, di: usize) {
    let di = di & 0xFFFF;
    let tgt_sprite = state.rw(di + 4);
    if tgt_sprite == 0xFFFF {
        return; // [asm 81F3] inactive target
    }
    let tgt_flags = state.rb(di + 5);
    if (tgt_flags & 0x20) == 0 {
        return; // [asm 81F9] not a boundary target
    }
    let hit = combat_interaction::hitbox_overlap(state, PLAYER_X, di); // [asm 8201] 8D7B
    if !hit {
        return; // [asm 8204] jae
    }

    let al = (4u8.wrapping_sub(state.rb(SCROLL_PUSH))) & 0xFF; // [asm 8206]
    let r = state.rb(SCROLL_PHASE).wrapping_sub(al); // [asm 820C]
    state.wb(SCROLL_PHASE, if r & 0x80 != 0 { 0 } else { r }); // [asm 8210] jns -> clamp to 0

    hurt_effect(state); // [asm 8217] 824D

    state.wb(PLAYER_DEATH_STATE, 0x2C); // [asm 8220]
    state.wb(ANIM_GATE, 0); // [asm 8225]
    state.ww(PLAYER_YVEL, 0xFF80); // [asm 822A]
    state.wb(PLAYER_MOTION_MODE, 3); // [asm 8230]
    let bvx = if s16(state.rw(PLAYER_X) as i32) >= s16(state.rw(CURSOR_X) as i32) {
        0x80u16
    } else {
        0xFF80
    }; // [asm 8235-8242] toward the player
    state.ww(PLAYER_XVEL, bvx); // [asm 8244]
    state.wb(LOW_GRAVITY, 0); // [asm 8247]
}

// ── 81B4: camera_boundary_collision ─────────────────────────────

/// [asm 81B4] The whole camera-boundary crush (gated off when [0x6BE4]!=0): bounce
/// the player off two boundary targets [0xA421]/[0xA423] (81F3 each), then test the
/// third [0xA425] (8D7B); on that overlap with a set [0xA330] detail, drive the player
/// downward.
fn camera_boundary_collision(state: &mut NativeGameState) {
    if state.rb(RESPAWN_STATE) != 0 {
        return; // [asm 81B4] crush disabled this frame
    }

    let cam_target_ptr = state.rw(CAM_TARGET_A) as usize; // [asm 81BE] 0xA421
    camera_target_bounce(state, cam_target_ptr);

    let target_a = state.rw(CAM_TARGET_B) as usize; // [asm 81C5] 0xA423
    camera_target_bounce(state, target_a);

    let target_b = state.rw(CAM_TARGET_C) as usize; // [asm 81D0] 0xA425
    let hit = combat_interaction::hitbox_overlap(state, PLAYER_X, target_b);

    if hit && state.rb(HIT_VDETAIL) != 0 {
        // [asm 81D3 jae / 81D5 cmp [0xA330],0]
        let ax = if state.rb(IN_UP) != 0 { 0xFF80u16 } else { 0xFFC0 }; // [asm 81DC-81E6]
        state.ww(PLAYER_YVEL, ax); // [asm 81EF]
    }
}

// ── 7301: _cam_scroll_velocity ──────────────────────────────────

/// [asm 7301-7346] Ramp the scroll velocity [0x6C08]/[0x6C06] from the cursor-player
/// gap, capped by [0x91FB]*0x50; direction is toward the player.
fn cam_scroll_velocity(state: &mut NativeGameState) {
    let bx = (state.rb(SCROLL_PUSH) as u16).wrapping_mul(0x50); // [asm 7301-7308]
    state.ww(SCRIPT_PTR, CAM_SCRIPT_IDLE); // [asm 730A]
    let diff = state.rw(CURSOR_X).wrapping_sub(state.rw(PLAYER_X)); // [asm 7310-7313]
    let adist = abs16(diff); // [asm 7318-731A]
    let (axv, dxv);
    if adist <= bx {
        // [asm 731C] jbe 7328
        let q = (adist / 0xE) & 0xFF; // [asm 7328-732C] 8-bit div
        let v = (q << 4) as u16; // [asm 7330]
        axv = (0u16).wrapping_sub(v); // [asm 7336]
        dxv = v >> 1; // [asm 7332-7334]
    } else {
        // [asm 7320-7323]
        axv = 0xFFA0;
        dxv = 0x30;
    }
    state.ww(SCROLL_VY, axv); // [asm 7338]
    let vx = if s16(diff as i32) < 0 {
        dxv
    } else {
        (0u16).wrapping_sub(dxv)
    }; // [asm 733C-7342]
    state.ww(SCROLL_VX, vx);
}

// ── 94F3: boss_death_burst_94f3 ─────────────────────────────────

/// [asm 94F3..951E] The boss-death diamond burst: spawn 4 sprites at the
/// [0xA336]/[0xA338] origin, each a RANDOM bonus-popup sprite id from
/// roll_bonus_sprite_id (8C13), Xvel cycling +/-0x20/+/-0x10 and Yvel 0xFF60 stepping
/// down 0x10.
fn boss_death_burst_94f3(state: &mut NativeGameState) {
    let mut ax: u16 = 0x20; // [asm 94F9-94FC]
    let mut dx: u16 = 0xFF60;
    for _ in 0..4 {
        // [asm 94F6 cx=4]
        let sid = combat_interaction::roll_bonus_sprite_id(state); // [asm 9504] 8C13
        state.ww(HURT_FX_SPRITE, sid); // [asm 9507]
        combat_interaction::spawn_effect_burst(state, ax, dx, 1); // [asm 950B] 8D1B
        ax = (0u16).wrapping_sub(ax); // [asm 950E] neg ax
        if (ax & 0x8000) == 0 {
            // [asm 9510] js -> skip the step
            ax = ax.wrapping_sub(0x10); // [asm 9512]
            dx = dx.wrapping_sub(0x10); // [asm 9515]
        }
    }
}

// ── 7491: camera_state6_finale ───────────────────────────────────

/// [asm 7491..74E9] The camera state-6 boss-REACH finale (the gorilla defeat):
/// disable the camera SM ([0x91FE]=0xFF), clear the 5 boss target render records,
/// then spawn 4 death-bursts (94F3) in a diamond around the boss origin plus one
/// final sprite (id 0x63, no velocity).
fn camera_state6_finale(state: &mut NativeGameState) {
    state.wb(CAM_STATE, 0xFF); // [asm 7491] disable the camera state machine

    // [asm 7496-74A5] clear the 5 target render records
    for k in 0..5 {
        let off = TARGET_RECORDS + k * TARGET_STRIDE;
        state.ww(off + 4, 0xFFFF);
    }

    let boss_x = state.rw(BOSS_X); // [asm 74A8-74B1] the boss origin
    let boss_y = state.rw(BOSS_Y);
    state.ww(HURT_FX_X, boss_x);
    state.ww(HURT_FX_Y, boss_y);
    boss_death_burst_94f3(state); // [asm 74B4] burst 1 at (X, Y)

    state.ww(HURT_FX_X, state.rw(HURT_FX_X).wrapping_add(0x20)); // [asm 74B7-74BC]
    state.ww(HURT_FX_Y, state.rw(HURT_FX_Y).wrapping_add(0x20));
    boss_death_burst_94f3(state); // [asm 74C1] burst 2 at (X+0x20, Y+0x20)

    state.ww(HURT_FX_X, state.rw(HURT_FX_X).wrapping_sub(0x40)); // [asm 74C4]
    boss_death_burst_94f3(state); // [asm 74C9] burst 3 at (X-0x20, Y+0x20)

    state.ww(HURT_FX_X, state.rw(HURT_FX_X).wrapping_sub(0x20)); // [asm 74CC-74D1]
    state.ww(HURT_FX_Y, state.rw(HURT_FX_Y).wrapping_sub(0x40));
    boss_death_burst_94f3(state); // [asm 74D6] burst 4 at (X-0x40, Y-0x20)

    state.ww(HURT_FX_SPRITE, 0x63); // [asm 74E0] final sprite id
    combat_interaction::spawn_effect_burst(state, 0, 0, 1); // [asm 74D9-74E6] 8D1B
}

// ── 71AB: camera_state_machine ──────────────────────────────────

/// [asm 71AB..7534] The camera SEQUENCER: an 8-state machine on [0x91FE] driving the
/// level-intro/boss camera pan. Each state advances the timer [0xA3F7], gates on the
/// player-cursor distance / level param, sets the active camera-script pointer
/// [0xA401], computes the scroll velocity, and transitions [0x91FE]. State 6 is the
/// boss-reach FINALE (camera_state6_finale).
fn camera_state_machine(state: &mut NativeGameState) {
    let cam_state = state.rb(CAM_STATE);
    let dist = state.rw(DIST_X);

    match cam_state {
        0 => {
            // [asm 71AB]
            if dist >= 0xFA {
                state.wb(SCROLL_PHASE, 0); // [asm 71BA]
                state.ww(SCRIPT_PTR, CAM_SCRIPT_INTRO); // [asm 71BF]
            } else {
                state.ww(CAM_TIMER, 0); // [asm 71C8]
                state.wb(CAM_STATE, 1); // [asm 71CE]
            }
        }
        1 => {
            // [asm 71D6]
            if dist >= 0xFA {
                state.wb(CAM_STATE, 0); // [asm 71E8]
                return;
            }
            let t = state.rw(CAM_TIMER).wrapping_add(1); // [asm 71F0]
            state.ww(CAM_TIMER, t);
            if state.rw(SPAWN_COUNT) < 0x3C {
                // [asm 71F4] jae 7203
                state.wb(CAM_STATE, 2); // [asm 71FB]
                return;
            }
            state.ww(SCRIPT_PTR, CAM_SCRIPT_PAN); // [asm 7203]
            if t >= 0x6E {
                // [asm 7209] jb 7221
                state.wb(CAM_STATE, 2); // [asm 7210]
                inc_scroll_phase(state); // [asm 7215] 757A
                state.ww(CAM_TIMER, 0); // [asm 7218]
                return;
            }
            if state.rb(ANIM_GATE) != 0 && (state.rb(FRAME_BLINK) & 7) == 0 {
                // [asm 7221/7228]
                inc_scroll_phase(state); // [asm 722F] 757A
                return;
            }
            let c = state.rb(SCROLL_PHASE); // [asm 7235]
            if c >= 0xA {
                // jb 724A
                state.wb(CAM_STATE, 3); // [asm 723C]
                state.ww(CAM_TIMER, 0); // [asm 7241]
            } else if c > 3 {
                // [asm 724A] ja 7254
                state.wb(CAM_STATE, 2); // [asm 7254]
                state.ww(CAM_TIMER, 0); // [asm 7259]
            }
        }
        2 => {
            // [asm 7262]
            let t = state.rw(CAM_TIMER).wrapping_add(1); // [asm 726C]
            state.ww(CAM_TIMER, t);
            if t < 0x2C {
                // [asm 7270/7277]
                camera_boundary_collision(state); // [asm 7279] 81B4
                if dist >= 0x4B {
                    // [asm 727C] jae 7290
                    state.ww(SCRIPT_PTR, CAM_SCRIPT_SPAWN); // [asm 7290]
                } else {
                    state.wb(CAM_STATE, 3); // [asm 7283]
                    state.ww(CAM_TIMER, 0);
                }
                return;
            }
            if t == 0x2C {
                // [asm 7275] je 7299
                camera_boundary_collision(state); // [asm 7299] 81B4
                state.ww(SCROLL_VY, 0xFF20); // [asm 729C]
                if state.rw(SPAWN_COUNT) < 0x28 && dist < 0x50 {
                    // [asm 72A2/72A9]
                    state.wb(CAM_STATE, 4); // [asm 72B0]
                    state.ww(CAM_TIMER, 0); // [asm 72B5]
                }
                state.ww(SCRIPT_PTR, CAM_SCRIPT_IDLE); // [asm 72BB]
                return;
            }
            // t > 0x2C  [asm 72C4]
            let scroll_vy = state.rw(SCROLL_VY);
            if s16(scroll_vy as i32) > 0 {
                // [asm 72C4] jle 72DB
                let script = if state.rw(SPAWN_COUNT) >= 0x64 {
                    CAM_SCRIPT_IDLE_HI
                } else {
                    CAM_SCRIPT_IDLE_LO
                }; // [asm 72CB-72D8]
                state.ww(SCRIPT_PTR, script);
            }
            if dist > 0x50 {
                // [asm 72DB] ja 72F5
                if t < 0x58 {
                    return; // [asm 72F5] jae 72FF
                }
                if t == 0x58 {
                    // [asm 72FF] jne 7349
                    cam_scroll_velocity(state); // [asm 7301-7346]
                    return;
                }
                // t > 0x58
                if scroll_vy == 0 {
                    // [asm 7349] je 7353
                    state.wb(SCROLL_PHASE, 3); // [asm 7353]
                    state.ww(CAM_TIMER, 0); // [asm 7356]
                    state.wb(CAM_STATE, 1); // [asm 735E]
                }
                return;
            }
            // dist <= 0x50
            state.ww(CAM_TIMER, 0); // [asm 72E2]
            state.wb(CAM_STATE, 3); // [asm 72E6]
            state.wb(SCROLL_PHASE, 0xB); // [asm 72ED]
        }
        3 => {
            // [asm 7366]
            camera_boundary_collision(state); // [asm 7370] 81B4
            let t = state.rw(CAM_TIMER).wrapping_add(1); // [asm 7373]
            state.ww(CAM_TIMER, t);
            let spawn_count = state.rw(SPAWN_COUNT);
            let limit = if abs16(spawn_count.wrapping_sub(0x32)) <= 0x19 {
                0x9Au16
            } else {
                0x42u16
            }; // [asm 7377-7389]
            if t > limit {
                // [asm 738C] ja 740C
                state.ww(CAM_TIMER, 0); // [asm 740C]
                state.wb(CAM_STATE, 2);
                return;
            }
            let vy = s16(state.rw(SCROLL_VY) as i32); // [asm 7392]
            if vy < 0 {
                // [asm 7397 je / 7399 jg]
                state.ww(SCRIPT_PTR, CAM_SCRIPT_IDLE); // [asm 739B]
                return;
            }
            if vy > 0 {
                state.ww(SCRIPT_PTR, CAM_SCRIPT_73A4); // [asm 73A4]
                return;
            }
            // vy == 0
            let a = state.rw(DIST_X); // [asm 73AD]
            if a >= 0x64 {
                // [asm 73B0] jae 73F1
                state.ww(SCROLL_VY, 0xFFAF); // [asm 73F1]
                let dx = if s16(state.rw(PLAYER_X) as i32) > s16(state.rw(CURSOR_X) as i32) {
                    0x50u16
                } else {
                    (0u16).wrapping_sub(0x50)
                }; // [asm 73FA-7403]
                state.ww(SCROLL_VX, dx); // [asm 7405]
            } else if a <= 0x19 {
                // [asm 73B5] jbe 73DA
                state.ww(SCRIPT_PTR, CAM_SCRIPT_73DA); // [asm 73DA]
            } else if a <= 0x23 {
                // [asm 73BA] jbe 73E3
                state.wb(CAM_STATE, 4); // [asm 73E3]
                state.ww(CAM_TIMER, 0);
            } else {
                // [asm 73BF] 0x24..0x63
                // 81B4 may have knocked SCROLL_PHASE down — read the current value
                let sp = state.rb(SCROLL_PHASE);
                if sp == 0 {
                    // [asm 73BF] jne 73D1
                    state.wb(CAM_STATE, 7); // [asm 73C6]
                    state.ww(CAM_TIMER, 0);
                }
                state.ww(SCRIPT_PTR, CAM_SCRIPT_73D1); // [asm 73D1]
            }
        }
        4 => {
            // [asm 741A]
            camera_boundary_collision(state); // [asm 7421] 81B4
            let t = state.rw(CAM_TIMER).wrapping_add(1); // [asm 7424]
            state.ww(CAM_TIMER, t);
            if t > 0x42 {
                // [asm 7428] jbe 743D
                state.wb(CAM_STATE, 3); // [asm 742F]
                state.ww(CAM_TIMER, 0);
                return;
            }
            state.ww(SCRIPT_PTR, CAM_SCRIPT_743D); // [asm 743D]
            if state.rw(SCROLL_VY) == 0 {
                // [asm 7443] jne 744F
                state.wb(SCROLL_STOP_FLAG, 4); // [asm 744A]
            }
        }
        5 => {
            // [asm 7452]
            let t = state.rw(CAM_TIMER).wrapping_add(1); // [asm 7459]
            state.ww(CAM_TIMER, t);
            if t > 0x13 {
                // [asm 745D] jbe 7471
                state.ww(CAM_TIMER, 0); // [asm 7464]
                state.wb(CAM_STATE, 1); // [asm 7469]
            } else {
                state.ww(SCRIPT_PTR, CAM_SCRIPT_7471); // [asm 7471]
            }
        }
        6 => {
            // [asm 747A]
            state.ww(SCRIPT_PTR, CAM_SCRIPT_7481); // [asm 7481]
            if s16(state.rw(SCROLL_VY) as i32) < 0 {
                // [asm 7487] jge 7491
                return;
            }
            camera_state6_finale(state); // [asm 7491-74E9] the boss-reach death finale
        }
        7 => {
            // [asm 74EA]
            let t = state.rw(CAM_TIMER).wrapping_add(1); // [asm 74F1]
            state.ww(CAM_TIMER, t);
            if t > 0x2C {
                // [asm 74F5] jbe 7503
                state.wb(CAM_STATE, 1); // [asm 74FC]
                return;
            }
            state.ww(SCRIPT_PTR, CAM_SCRIPT_IDLE); // [asm 7503]
            let vy = state.rw(SCROLL_VY);
            if vy == 0 {
                // [asm 7509] je 751A
                state.ww(SCROLL_VY, 0xFF9F); // [asm 751A]
                let dx = if s16(state.rw(PLAYER_X) as i32) > s16(state.rw(CURSOR_X) as i32) {
                    0xFFD0u16
                } else {
                    0x30
                }; // [asm 7523-752C]
                state.ww(SCROLL_VX, dx); // [asm 752E]
            } else if s16(vy as i32) > 0 {
                // [asm 7510] jl 7534
                state.ww(SCRIPT_PTR, CAM_SCRIPT_IDLE_HI); // [asm 7512]
            }
        }
        _ => {
            // [asm 74EF jne 7534] states >= 8 / 0xFF: no-op
        }
    }
}

// ── 94DC: camera_offset_lookup ──────────────────────────────────

/// [asm 94DC] Precomputed camera-offset lookup: scan the [0xA6ED] table (stride 6:
/// dx-key, ax-key, value) for the (dx, ax) pair and return the value word.
fn camera_offset_lookup(state: &NativeGameState, key_dx: u16, key_ax: u16) -> u16 {
    let mut bx = CAM_OFFSET_TABLE;
    for _ in 0..4096 {
        // [asm 94E0-94F1] scan to the matching entry
        let dx = state.rw(bx);
        let ax = state.rw(bx + 2);
        if dx == key_dx && ax == key_ax {
            return state.rw(bx + 4); // [asm 94E9]
        }
        bx = (bx + 6) & 0xFFFF;
    }
    panic!("94DC camera-offset (dx, ax) not found in the [0xA6ED] table");
}

// ── 93F6: camera_target_geometry ────────────────────────────────

/// [asm 93F6] Camera-target GEOMETRY chain: read 5 script param words via lodsw,
/// look up each target's offset (94DC), and lay out 4 targets — target 0 = cursor
/// +/- offset, targets 1/2/3 = target 0 +/- offset — mirroring X by player facing
/// and nudging Y when the command [0xA3F9]&0x40 is set. Returns the new `si` advanced
/// past the 5 consumed param words.
fn camera_target_geometry(state: &mut NativeGameState, mut si: u16) -> u16 {
    let face = state.rb(DIST_DIR) != 0; // pg.dist_dir
    let flip = |p: u16| -> u16 {
        if face { p ^ 0x8000 } else { p }
    };
    let cx = state.rw(CURSOR_SNAP_X); // cursor_latch_x
    let cy = state.rw(CURSOR_SNAP_Y); // cursor_latch_y

    let p1 = state.rw(si as usize);
    si = si.wrapping_add(2); // [asm 93F9] lodsw
    let p2 = state.rw(si as usize);
    si = si.wrapping_add(2); // [asm 93FF] lodsw
    let off = camera_offset_lookup(state, p1, p2); // [asm 9403] 94DC
    let off_lo = cbw(off as u8);
    let off_hi = cbw((off >> 8) as u8);
    let t0x = if face {
        cx.wrapping_sub(off_lo)
    } else {
        cx.wrapping_add(off_lo)
    }; // [asm 9406-9412]
    let t0y = cy.wrapping_add(off_hi); // [asm 9419-941C]
    state.ww(CAM_PARAM_1, flip(p1));
    state.ww(CAM_PARAM_2, flip(p2));
    state.ww(CAM_T0_X, t0x);
    state.ww(CAM_T0_Y, t0y);

    // Targets 1, 2, 3 (each relative to target 0, shared first key p2)
    let targets: [(usize, usize, usize); 3] = [
        (CAM_T1_PARAM, CAM_T1_X, CAM_T1_Y),
        (CAM_T2_PARAM, CAM_T2_X, CAM_T2_Y),
        (CAM_T3_PARAM, CAM_T3_X, CAM_T3_Y),
    ];
    for &(poff, txo, tyo) in &targets {
        let p = state.rw(si as usize);
        si = si.wrapping_add(2); // [asm 9427/944F/9477] lodsw
        let off = camera_offset_lookup(state, p2, p); // [asm 942B/9453/947B] 94DC
        let off_lo = cbw(off as u8);
        let off_hi = cbw((off >> 8) as u8);
        let tx = if face {
            t0x.wrapping_sub(off_lo)
        } else {
            t0x.wrapping_add(off_lo)
        };
        let ty = t0y.wrapping_add(off_hi);
        state.ww(poff, flip(p));
        state.ww(txo, tx);
        state.ww(tyo, ty);
    }

    if state.rb(CMD_BYTE) & 0x40 != 0 {
        // [asm 949B-94AF] vertical nudge
        state.ww(CAM_T0_Y, state.rw(CAM_T0_Y).wrapping_add(2));
        state.ww(CAM_T1_Y, state.rw(CAM_T1_Y).wrapping_add(1));
        state.ww(CAM_T2_Y, state.rw(CAM_T2_Y).wrapping_add(1));
        state.ww(CAM_T3_Y, state.rw(CAM_T3_Y).wrapping_add(1));
    }
    si
}

// ── 93B2: camera_script_command ─────────────────────────────────

/// [asm 93B2] Build the 5 camera-target records: compute the geometry (93F6), then
/// for each of the command's 5 source pointers copy 3 words from the geometry into
/// the records @0x5648 (stride 0x12), and set the collision refs. Returns the new
/// `si`.
fn camera_script_command(state: &mut NativeGameState, mut si: u16) -> u16 {
    si = camera_target_geometry(state, si); // [asm 93B9] 93F6
    let mut di: u16 = TARGET_RECORDS as u16; // 0x5648
    for _ in 0..5 {
        // [asm 93BF dx=5]
        let bx = state.rw(si as usize); // [asm 93C2] source pointer
        si = si.wrapping_add(2); // [asm 93E2]
        if bx as usize == CAM_T2_X {
            // [asm 93C4]
            state.ww(CAM_TARGET_B, di); // 0xA423
        }
        if bx as usize == CAM_T3_X {
            // [asm 93CE]
            state.ww(CAM_TARGET_A, di); // 0xA421
        }
        if bx as usize == CAM_T1_X {
            // [asm 93D8]
            state.ww(CAM_TARGET_C, di); // 0xA425
        }
        // Copy 3 words: CamTarget(bx) -> RenderSlot(di)
        state.ww(di as usize, state.rw(bx as usize)); // dst.x = src.param
        state.ww(di as usize + 2, state.rw(bx as usize + 2)); // dst.y = src.x
        state.ww(di as usize + 4, state.rw(bx as usize + 4)); // dst.sprite = src.y
        di = di.wrapping_add(TARGET_STRIDE as u16); // [asm 93EF] +0xC (stosw x3)
    }
    si
}

// ── 7534: camera_script_interp ──────────────────────────────────

/// [asm 7534..7579] The camera-script bytecode interpreter (the 70D7 tail): when the
/// state machine's script pointer [0xA401] changes, reset the cursor [0xA3FF]; skip
/// JUMP opcodes (negative bytes advance the cursor by the signed offset); then
/// dispatch the command byte, latch the cursor pos, run the command (93B2), and scan
/// the camera targets (80DE).
fn camera_script_interp(state: &mut NativeGameState) {
    let ax = state.rw(SCRIPT_PTR); // [asm 7534]
    if state.rw(SCRIPT_LAST) != ax {
        // [asm 7537]
        state.ww(SCRIPT_LAST, ax); // [asm 753D]
        state.ww(SCRIPT_CURSOR, ax); // [asm 7540] reset the cursor
    }

    // [asm 7543] skip JUMP opcodes
    let mut al;
    let mut count = 0;
    loop {
        count += 1;
        if count > 4096 {
            panic!("7534 camera-script JUMP loop did not terminate");
        }
        let cursor = state.rw(SCRIPT_CURSOR);
        al = state.rb(cursor as usize); // [asm 7549]
        if (al & 0x80) == 0 {
            break; // a command byte
        }
        state.ww(SCRIPT_CURSOR, cursor.wrapping_add(cbw(al))); // [asm 754D-754E] jump
    }

    state.wb(CMD_BYTE, al); // [asm 7554]
    let cmd = (al & 0xBF) as u16; // [asm 7557]
    let cursor = state.rw(SCRIPT_CURSOR);
    state.ww(SCRIPT_CURSOR, cursor.wrapping_add(1)); // [asm 7559]
    let si = cmd.wrapping_mul(0x14).wrapping_add(CAM_CMD_TABLE as u16) & 0xFFFF; // [asm 755D-7563]
    state.ww(CURSOR_SNAP_X, state.rw(CURSOR_X)); // [asm 7567]
    state.ww(CURSOR_SNAP_Y, state.rw(CURSOR_Y)); // [asm 7570]
    camera_script_command(state, si); // [asm 7573] 93B2
    scan_camera_targets(state); // [asm 7576] 80DE
}

// ── 70D7..7579: camera_engine ───────────────────────────────────

/// [asm 70D7..7579] The whole per-frame level-camera/scroll engine (called from 6822
/// when [0x91FE]!=0xFF): advance the spawn cursor + scroll (tick_scroll_cursor), follow
/// the player (player_cursor_dist), and unless the player is too far (the cull), run
/// the camera sequencer (camera_state_machine) and its bytecode script
/// (camera_script_interp).
pub fn camera_engine(state: &mut NativeGameState) {
    tick_scroll_cursor(state); // [asm 70D7..7172] cursor + scroll + spawn gate
    let cull = player_cursor_dist(state); // [asm 7172..71AB] player-follow distance
    if cull {
        return; // [asm 7195/71A8] too far -> jmp 7579
    }
    // [asm 747A-748C] the state-6 boss-reach finale (state 6 & SCROLL_VY>=0) RETs at
    // 74E9 -> skips 7534
    let cam_state = state.rb(CAM_STATE);
    let scroll_vy = state.rw(SCROLL_VY);
    let finale = cam_state == 6 && s16(scroll_vy as i32) >= 0;
    camera_state_machine(state); // [asm 71AB..7534] the camera sequencer
    if !finale {
        camera_script_interp(state); // [asm 7534..7579] its bytecode script
    }
}

// ── 6ADD head: tick_mode9_spawn ─────────────────────────────────

/// [asm 6ADD..6B0C] The mode-9 boss-engine head: on first entry ([0xA517]==-1) seed
/// the boss state, then spawn the per-frame effect row of [0xA519]>>2 slots (7585).
fn tick_mode9_spawn(state: &mut NativeGameState) {
    let count;
    if state.rw(BOSS_SCRIPT_PTR) == 0xFFFF {
        // [asm 6AE7] first frame -> seed the state
        state.ww(M9_PTR, BOSS_SCRIPT_M9); // [asm 6AEE]
        state.ww(M9_COUNT, 0x18); // [asm 6AF4]
        state.wb(BOSS_DWELL, 0); // [asm 6AFA]
        state.wb(BOSS_CYCLE, 3); // [asm 6AFF]
        count = 0x18u16;
    } else {
        count = state.rw(M9_COUNT); // g.boss_health
    }
    init_effect_row(state, sar16(count, 2)); // [asm 6B04-6B0C] 7585, cx = [0xA519]>>2
}

// ── 6BDB: boss_hit_burst ────────────────────────────────────────

/// [asm 6BDB..6C0C] The boss-hit effect burst: spawn 4 sprites (id 0x2137) at the
/// boss origin (0xB9, 0x1E) in a diamond — Xvel cycles +/-0x20, +/-0x10 and Yvel steps
/// 0xFF60 -> 0xFF50.
fn boss_hit_burst(state: &mut NativeGameState) {
    state.ww(HURT_FX_X, 0xB9); // [asm 6BDB-6BF5] origin + sprite
    state.ww(HURT_FX_Y, 0x1E);
    state.ww(HURT_FX_SPRITE, 0x2137);
    let mut ax: u16 = 0x20; // [asm 6BEA-6BED]
    let mut dx: u16 = 0xFF60;
    for _ in 0..4 {
        // [asm 6BE7 cx=4]
        combat_interaction::spawn_effect_burst(state, ax, dx, 1); // [asm 6BFC] 8D1B
        ax = (0u16).wrapping_sub(ax); // [asm 6BFF] neg ax
        if (ax & 0x8000) == 0 {
            // [asm 6C01] js -> skip the step
            ax = ax.wrapping_sub(0x10); // [asm 6C03]
            dx = dx.wrapping_sub(0x10); // [asm 6C06]
        }
    }
}

// ── 6CA9: free_boss_slot ────────────────────────────────────────

/// [asm 6CA9-6CB8] The first free 0x50A8 slot ([+4]==0xFFFF), or None when the pool
/// is full.
fn free_boss_slot(state: &NativeGameState) -> Option<usize> {
    let mut si = BOSS_POOL_LO;
    for _ in 0..BOSS_POOL_N {
        if state.rw(si + 4) == 0xFFFF {
            return Some(si);
        }
        si += BOSS_POOL_STRIDE;
    }
    None
}

// ── rng_lcg_next ────────────────────────────────────────────────

/// [asm 39DF via call] Advance rng_lcg over [0x2CEC..0x2CEF], write back, return AL.
fn rng_lcg_next(state: &mut NativeGameState) -> u8 {
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

// ── 6CA7: spawn_boss_bolt_1ca ──────────────────────────────────

/// [asm 6CA7..6CF0] Boss-script opcode 0xFF: spawn a boss projectile (sprite 0x1CA)
/// into the first free 0x50A8 slot at (0xC8, 0x58), anim [+0xC]=0x84, with a random
/// upward-left Xvel. No-op when the pool is full.
fn spawn_boss_bolt_1ca(state: &mut NativeGameState) {
    let si = match free_boss_slot(state) {
        Some(si) => si,
        None => return, // [asm 6CBA] pool full -> no spawn
    };
    state.ww(si + 4, 0x1CA); // [asm 6CBC] sprite
    state.ww(si, 0xC8); // [asm 6CC0] x
    state.ww(si + 2, 0x58); // [asm 6CC5] y
    state.wb(si + 0x11, 0); // [asm 6CCA] substate
    state.ww(si + 0x0C, 0x84); // [asm 6CCE] lifetime (anim selector)
    state.ww(si + 9, 0xFFFF); // [asm 6CD3] source
    let r = rng_lcg_next(state); // [asm 6CD8] 39DF
    let xvel = (0u16).wrapping_sub(((r & 0xF) as u16) << 3); // [asm 6CDB-6CE6] random upward-left
    state.ww(si + 6, xvel); // xvel
    state.ww(si + 0x0E, 0); // [asm 6CE9] yvel
}

// ── 6CF1: spawn_boss_bolt_1cb ──────────────────────────────────

/// [asm 6CF1..6D33] Boss-script opcode 0xFE: spawn a boss projectile (sprite 0x1CB)
/// into the first free 0x50A8 slot at (random X = (rng_lcg & 0x7F) - 0x10, Y=0), anim
/// [+0xC]=0x42, no velocity. No-op when the pool is full.
fn spawn_boss_bolt_1cb(state: &mut NativeGameState) {
    let si = match free_boss_slot(state) {
        Some(si) => si,
        None => return, // [asm 6D04] pool full -> no spawn
    };
    state.ww(si + 4, 0x1CB); // [asm 6D06] sprite
    let r = rng_lcg_next(state); // [asm 6D0B] 39DF
    state.ww(si, ((r & 0x7F) as u16).wrapping_sub(0x10)); // [asm 6D0E-6D14] random X
    state.ww(si + 2, 0); // [asm 6D16] y
    state.wb(si + 0x11, 0); // [asm 6D1B] substate
    state.ww(si + 0x0C, 0x42); // [asm 6D1F] lifetime (anim selector)
    state.ww(si + 9, 0xFFFF); // [asm 6D24] source
    state.ww(si + 6, 0); // [asm 6D2B] xvel
    state.ww(si + 0x0E, 0); // [asm 6D2E] yvel
}

// ── 6B91: boss_script_interp ───────────────────────────────────

/// [asm 6B91..6BDA] The boss glyph-script interpreter: from the cursor [0xA517],
/// process control opcodes — 0xFF spawn 0x1CA (6CA7), 0xFE spawn 0x1CB (6CF1), 0xFD
/// play sfx 2, a negative byte = a relative jump that also decrements the dwell
/// [0xA515] (saturating) — until a glyph byte (al>=0): advance [0xA517] by 1, render
/// it (6C0D), and return.
fn boss_script_interp(state: &mut NativeGameState) {
    let mut bx = state.rw(BOSS_SCRIPT_PTR); // [asm 6B91]
    let mut al;
    let mut count = 0;
    loop {
        count += 1;
        if count > 4096 {
            panic!("6B91 boss-script interpreter ran 4096 opcodes without reaching a glyph");
        }
        al = state.rb(bx as usize); // [asm 6B95]
        if al == 0xFF {
            // [asm 6B97] spawn 0x1CA
            spawn_boss_bolt_1ca(state);
            bx = bx.wrapping_add(1);
        } else if al == 0xFE {
            // [asm 6BA1] spawn 0x1CB
            spawn_boss_bolt_1cb(state);
            bx = bx.wrapping_add(1);
        } else if al == 0xFD {
            // [asm 6BAB] play sfx 2 (audio seam)
            bx = bx.wrapping_add(1);
        } else if al & 0x80 != 0 {
            // [asm 6BB8] negative -> relative jump + dwell
            let d = state.rb(BOSS_DWELL); // [asm 6BC0-6BC5] sub 1 ; adc 0 (saturate at 0)
            state.wb(BOSS_DWELL, if d != 0 { d - 1 } else { 0 });
            bx = bx.wrapping_add(cbw(al)); // [asm 6BCB] bx += signed al
            state.ww(BOSS_SCRIPT_PTR, bx); // [asm 6BCD]
        } else {
            // [asm 6BD3] glyph -> advance [0xA517] (inc word [0xA517], NOT bx+1!), render (6C0D), return
            let cur = state.rw(BOSS_SCRIPT_PTR);
            state.ww(BOSS_SCRIPT_PTR, cur.wrapping_add(1));
            state.boss_glyph = Some(al as u16); // [asm 6BD7] the glyph AL handed to the 6C0D render
            break;
        }
    }
}

// ── 6B1C: boss_pre_interp ──────────────────────────────────────

/// [asm 6B1C..6B90] The boss script-advance + projectile-hit detection: when the
/// dwell [0xA515] hits 0, load the next attack-script entry from the table [0xA4F7]
/// (0xFFFF = a relative wrap) into [0xA517]/[0xA515]; then scan the 4 projectile slots
/// and, on the first one inside the boss zone, switch to the hit script, drop the boss
/// health (saturating; ->6BDB death-burst at 0), and cycle [0xA516]&3.
fn boss_pre_interp(state: &mut NativeGameState) {
    if state.rb(BOSS_DWELL) == 0 {
        // [asm 6B1C] dwell expired -> next script entry
        let mut bx = state.rw(M9_PTR); // [asm 6B23]
        let mut script_ptr = state.rw(bx as usize);
        if script_ptr == 0xFFFF {
            // [asm 6B29] wrap marker
            let wrap_delta = state.rw(bx as usize + 2);
            bx = bx.wrapping_add(wrap_delta); // [asm 6B2E]
            script_ptr = state.rw(bx as usize);
        }
        let dwell = state.rb(bx as usize + 2); // [asm 6B36-6B39]
        state.ww(BOSS_SCRIPT_PTR, script_ptr); // [asm 6B33]
        state.wb(BOSS_DWELL, dwell);
        state.ww(M9_PTR, bx.wrapping_add(4)); // [asm 6B3C-6B3F]
    }

    // [asm 6B43-6B8C] the 4 projectile slots
    for i in 0..4 {
        let slot = SCAN_PROJ_LO + i * 0x12;
        let sprite = state.rw(slot + 4);
        let x = state.rw(slot);
        let y = state.rw(slot + 2);
        if sprite != 0xFFFF
            && x.wrapping_sub(BOSS_ZONE_X_LO) < BOSS_ZONE_X_SZ
            && y.wrapping_sub(BOSS_ZONE_Y_LO) < BOSS_ZONE_Y_SZ
        {
            // [asm 6B49] active + in zone
            state.ww(BOSS_SCRIPT_PTR, BOSS_SCRIPT_HIT); // [asm 6B64] switch to the hit script
            let h = state.rb(M9_COUNT); // [asm 6B6A-6B6F] boss health-- (saturating)
            let nh = if h != 0 { h - 1 } else { 0 };
            state.wb(M9_COUNT, nh);
            if nh == 0 {
                // [asm 6B74] health depleted -> 6BDB death-burst
                boss_hit_burst(state); // [asm 6B76]
            }
            let c = (state.rb(BOSS_CYCLE) + 1) & 3; // [asm 6B79-6B7D]
            state.wb(BOSS_CYCLE, c);
            if c == 0 {
                // [asm 6B82]
                state.ww(BOSS_SCRIPT_PTR, BOSS_SCRIPT_CYCLE); // [asm 6B84]
            }
            break; // [asm 6B8A] -> the glyph interpreter
        }
    }
}

// ── 6ADD..6BDA: tick_mode9_boss ─────────────────────────────────

/// [asm 6ADD..6BDA] The whole mode-9 last-boss engine: the head (seed + per-frame
/// effect row) -> while the boss is alive the script-advance + projectile-hit
/// detection (boss_pre_interp) -> the glyph-script interpreter (boss_script_interp).
pub fn tick_mode9_boss(state: &mut NativeGameState) {
    tick_mode9_spawn(state); // [asm 6ADD..6B0C] head (seed + spawn row)
    if state.rw(M9_COUNT) == 0 {
        // [asm 6B0F] boss dead -> 6C0D victory-glyph render (a pure page blit, no
        // DGROUP writes) then RET; the glyph render is the renderer's job, not a
        // gameplay-state write — so the contract ends at the head.
        return;
    }
    boss_pre_interp(state); // [asm 6B1C..6B90] script-advance + hit-detection
    boss_script_interp(state); // [asm 6B91..6BDA] glyph-script interpreter
}

// ── 6D34..70D6: level-6 boss helpers ────────────────────────────

/// [asm 39DF via RngView.roll] Advance the LCG over the state; return the AL byte.
fn l6_draw_rng(state: &mut NativeGameState) -> u8 {
    rng_lcg_next(state)
}

/// [asm 6E92..6F37] The [0xA32B]-timer state machine (reached only when [0xA324]==0
/// and [0xA32B]==0): advance the sub-indices, drop a random falling projectile (unless
/// the new [0xA32A]==1), reload [0xA32B], and re-seed from the rng when [0xA32C]
/// underflows.
fn l6_spawn_state_machine(state: &mut NativeGameState) {
    let l6_sub_b = state.rb(L6_SUB_B);
    state.wb(L6_SUB_B, (l6_sub_b.wrapping_add(1)) % 3); // [asm 6E92]
    let al = (state.rb(L6_SUB_A).wrapping_add(1)) % 3; // [asm 6EA0]
    state.wb(L6_SUB_A, al);

    let ah;
    if al != 1 {
        // [asm 6EB2 je 6F0F] (skip the spawn)
        ah = 3;
        state.wb(SCROLL_STOP_FLAG, 4); // [asm 6EB6] camera_shake
        let p_x = state.rw(PLAYER_X).wrapping_add(2); // [asm 6EBB] nudge the player X
        state.ww(PLAYER_X, p_x);
        let p_y = state.rw(PLAYER_Y);

        // [asm 6EC1-6ED0] a free projectile slot
        let mut found_slot: Option<usize> = None;
        for k in 0..5 {
            let slot = L6_PROJ_LIST + k * 0xB;
            if state.rw(slot + 4) == 0xFFFF {
                found_slot = Some(slot);
                break;
            }
        }
        if let Some(slot) = found_slot {
            // [asm 6ED4]
            state.ww(slot + 4, 0x1B3); // [asm 6ED4] sprite id
            state.ww(slot + 2, p_y.wrapping_sub(0x96)); // [asm 6ED9] Y = playerY - 0x96
            let ret = l6_draw_rng(state); // [asm 6EE2] 39DF
            let mut al2 = ret & 0x7C; // [asm 6EE7] and al,0x7c
            if al2 & 4 != 0 {
                // [asm 6EE9 test/6EED neg] randomize the X sign
                al2 = (al2 as i8).wrapping_neg() as u8;
            }
            state.ww(slot, p_x.wrapping_add(cbw(al2))); // [asm 6EEF-6EF4] X = playerX +/- offset
            state.ww(slot + 6, (((ret & 3) + 1) << 4) as u16); // [asm 6EF6-6F02] fall speed
            state.ww(slot + 8, 0); // [asm 6F05] drift_acc
            state.wb(slot + 0xA, 4); // [asm 6F0A] drift_step
        }
    } else {
        ah = 1;
    }
    state.wb(L6_TIMER, ah); // [asm 6F0F]

    let reseed = state.rb(L6_RESEED).wrapping_sub(1); // [asm 6F13] dec [0xA32C]
    state.wb(L6_RESEED, reseed);
    if reseed == 0 {
        // [asm 6F17 jne 6F3C]
        let r1 = l6_draw_rng(state);
        state.wb(L6_RESEED, ((r1 & 0xF) << 3) as u8); // [asm 6F19-6F24]
        let r2 = l6_draw_rng(state);
        state.wb(
            L6_TIMER,
            (((r2 & 0xF) << 3) + 0x40) as u8,
        ); // [asm 6F27-6F34]
        state.wb(L6_SUB_B, 0); // [asm 6F37]
    }
}

/// [asm 6F8F..6FBA] The wait-path player collision: on overlap with target 0x5690
/// (and [0xA330] set) push the player down. Returns True if it branched straight to
/// the [0xA32B]-dec (i.e. skip the boss-hit + boundary).
fn l6_tail_6f8f(state: &mut NativeGameState) -> bool {
    if state.rb(RESPAWN_STATE) != 0 {
        // [asm 6F94 je / 6F96 jmp 70CB]
        return true;
    }
    let hit = combat_interaction::hitbox_overlap(state, PLAYER_X, BOSS_HITBOX); // [asm 6F9F] 8D7B
    if hit && state.rb(HIT_VDETAIL) != 0 {
        // [asm 6FA2 jae / 6FA4 je 6FBB]
        let yvel = if state.rb(IN_UP) != 0 { 0xFF80u16 } else { 0xFFC0 }; // [asm 6FAB-6FB8]
        state.ww(PLAYER_YVEL, yvel);
    }
    false
}

/// [asm 6F59..6F8D] The player bounced off a boss target: pop up ([0x4F2A]=0xFF60);
/// above Y 0x7BB it is a full crush (knockback + 3x the 824D hurt effect).
fn l6_bounce_6f59(state: &mut NativeGameState) {
    let p_y = s16(state.rw(PLAYER_Y) as i32);
    if p_y > 0x7BB {
        // [asm 6F5F jg 6F69]
        state.ww(PLAYER_YVEL, 0xFF60); // [asm 6F69] full crush: knockback
        state.wb(PLAYER_DEATH_STATE, 0x2C); // [asm 6F6E]
        state.wb(ANIM_GATE, 0); // [asm 6F74]
        state.wb(PLAYER_MOTION_MODE, 3); // [asm 6F79]
        state.ww(PLAYER_XVEL, 0xFF80); // [asm 6F7E]
        for _ in 0..3 {
            // [asm 6F84-6F8A] 824D x3
            hurt_effect(state);
        }
    } else {
        state.ww(PLAYER_YVEL, 0xFF60); // [asm 6F61]
    }
}

/// [asm 7041..70A4] The level-6 tree phase-3 boss-death finale: 4 death-bursts (94F3)
/// in a diamond around the boss origin ([0x5648]-0xC8, [0x564A]) + a final sprite
/// (0x63, no velocity), then clear the 5 L6 projectile render slots and the 5 boss
/// target records.
fn l6_tree_death_finale(state: &mut NativeGameState) {
    let boss_x = state.rw(BOSS_X);
    let boss_y = state.rw(BOSS_Y);
    state.ww(HURT_FX_X, boss_x.wrapping_sub(0xC8)); // [asm 7041-7047] boss origin
    state.ww(HURT_FX_Y, boss_y); // [asm 704A-704D]
    boss_death_burst_94f3(state); // [asm 7050] burst 1

    state.ww(HURT_FX_X, state.rw(HURT_FX_X).wrapping_add(0x20)); // [asm 7053]
    state.ww(HURT_FX_Y, state.rw(HURT_FX_Y).wrapping_add(0x20)); // [asm 7058]
    boss_death_burst_94f3(state); // [asm 705D] burst 2

    state.ww(HURT_FX_X, state.rw(HURT_FX_X).wrapping_sub(0x40)); // [asm 7060]
    boss_death_burst_94f3(state); // [asm 7065] burst 3

    state.ww(HURT_FX_X, state.rw(HURT_FX_X).wrapping_sub(0x20)); // [asm 7068]
    state.ww(HURT_FX_Y, state.rw(HURT_FX_Y).wrapping_sub(0x40)); // [asm 706D]
    boss_death_burst_94f3(state); // [asm 7072] burst 4

    state.ww(HURT_FX_SPRITE, 0x63); // [asm 707C]
    combat_interaction::spawn_effect_burst(state, 0, 0, 1); // [asm 7075-7082] final sprite

    // [asm 7085-7093] clear the 5 L6 projectile render slots
    for k in 0..5 {
        let off = L6_PROJ_RENDER + k * 0x12;
        state.ww(off + 4, 0xFFFF);
    }
    // [asm 7095-70A3] clear the 5 L6 boss target records
    for k in 0..5 {
        let off = TARGET_RECORDS + k * TARGET_STRIDE;
        state.ww(off + 4, 0xFFFF);
    }
}

/// [asm 6FBB..70A4] Test the player's club (0x4F0A) then the 4 thrown projectiles
/// (0x4F2E) against the active target record set (0x5648 + 0x24*[0xA326]); on a hit,
/// kill the attacker, flash the target, and (every 7 hits) advance the phase
/// [0xA326]. Reaching phase 3 runs the boss-death finale.
fn l6_boss_hit(state: &mut NativeGameState) {
    let boss_phase = state.rw(L6_PHASE) as usize;
    let si = TARGET_RECORDS + 0x24 * boss_phase; // [asm 6FBB-6FC7] boss_targets[2*phase]

    let mut hit_slot: Option<usize> = None;

    // [asm 6FC9] the player club (0x4F0A+4)
    if state.rw(PLAYER_SLOT0 + 4) != 0xFFFF {
        state.wb(HIT_PASS_FULL, 1); // [asm 6FCF] full-tolerance hitbox
        let h = combat_interaction::hitbox_overlap(state, si, SCAN_PLAYER); // [asm 6FD4] 8D7B
        state.wb(HIT_PASS_FULL, 0); // [asm 6FD7]
        if h {
            hit_slot = Some(SCAN_PLAYER); // [asm 6FDC]
        }
    }

    if hit_slot.is_none() {
        // [asm 6FDE-6FFC] the 4 projectiles
        for i in 0..4 {
            let slot = SCAN_PROJ_LO + i * 0x12;
            if state.rw(slot + 4) != 0xFFFF {
                // [asm 6FE4] active slot
                state.wb(HIT_PASS_FULL, 1);
                let h = combat_interaction::hitbox_overlap(state, si, slot); // [asm 6FEF]
                state.wb(HIT_PASS_FULL, 0);
                if h {
                    hit_slot = Some(slot); // [asm 6FF7]
                    break;
                }
            }
        }
    }

    let hit_slot = match hit_slot {
        Some(si) => si,
        None => return, // [asm 6FFE jmp 70A5]
    };

    // [asm 7001] flash the target (toggle bit 0x40)
    let tgt_flags = state.rb(si + 5);
    state.wb(si + 5, tgt_flags ^ 0x40);

    if boss_phase != 0 {
        // [asm 7005 je 7010]
        let prev = TARGET_RECORDS + 0x24 * boss_phase - 0x12; // (si-0xD = previous record)
        let prev_flags = state.rb(prev + 5);
        state.wb(prev + 5, prev_flags ^ 0x40);
    }

    state.ww(hit_slot + 4, 0xFFFF); // [asm 7010] kill the attacker
    state.wb(L6_STUN, 6); // [asm 7015] hit-stun

    if boss_phase < 2 {
        // [asm 701A jae 702B]
        state.wb(L6_SUB_B, 3); // [asm 7021]
        state.wb(L6_SUB_A, 3); // [asm 7026]
    }

    let hits = state.rb(L6_HITS).wrapping_sub(1); // [asm 702B] dec [0xA325]
    state.wb(L6_HITS, hits);
    if hits != 0 {
        return; // [asm 702F jne 70A5]
    }

    state.wb(L6_HITS, 7); // [asm 7031]
    let new_phase = state.rw(L6_PHASE).wrapping_add(1); // [asm 7036] advance the phase
    state.ww(L6_PHASE, new_phase);
    if new_phase != 3 {
        return; // [asm 703A jne 70A5]
    }

    l6_tree_death_finale(state); // [asm 7041-70A4] the phase-3 boss-death finale
}

/// [asm 70CB..70D6] Saturating decrement of the spawn timer [0xA32B].
fn l6_finish(state: &mut NativeGameState) {
    let v = state.rb(L6_TIMER);
    state.wb(L6_TIMER, if v != 0 { v - 1 } else { 0 });
}

// ── 6D34..70D6: tick_level6_boss ───────────────────────────────

/// [asm 6D34..70D6] The whole level-6 (inside-a-tree) camera/boss state machine:
/// rewrite the effect row (7585), move the 5 falling projectiles (0x7DAF -> render
/// 0x55EE) with player collision (8D7B + 824D hurt), bake the target geometry from
/// the 0xA4B5/0xA485/0xA4E5 tables into the target records (0x5648..0x5694), run the
/// [0xA32B]-timer spawn state machine, the player<->boss collision tails, and the
/// club/projectile-vs-boss hit + phase advance ([0xA326]++ every 7 hits). Saturating-
/// decrements [0xA32B] each frame.
pub fn tick_level6_boss(state: &mut NativeGameState) {
    let boss_phase = state.rw(L6_PHASE);
    init_effect_row(state, (((2i32 - s16(boss_phase as i32)) << 1) & 0xFFFF) as u16); // [asm 6D34-6D3D] 7585

    // [asm 6D40..6DDD] the 5 falling projectiles: move + player collision, projected to render slots
    for k in 0..5 {
        let proj = L6_PROJ_LIST + k * 0xB;
        let rslot = L6_PROJ_RENDER + k * 0x12;
        let mut bp = state.rw(proj + 4);
        if bp != 0xFFFF {
            // [asm 6D4F] active slot
            let mut killed = false;
            if state.rb(RESPAWN_STATE) == 0 {
                // [asm 6D54] not the death freeze
                let hit = combat_interaction::hitbox_overlap(state, PLAYER_X, rslot); // [asm 6D5B] 8D7B
                if hit {
                    // [asm 6D5E jb -> knock the player back]
                    state.wb(PLAYER_DEATH_STATE, 0x2C); // [asm 6D60]
                    state.wb(ANIM_GATE, 0); // [asm 6D65]
                    state.ww(PLAYER_YVEL, 0xFF80); // [asm 6D6A]
                    state.wb(PLAYER_MOTION_MODE, 3); // [asm 6D72]
                    state.ww(PLAYER_XVEL, 0xFF80); // [asm 6D75]
                    hurt_effect(state); // [asm 6D7B] 824D
                    bp = 0xFFFF;
                    state.ww(proj + 4, bp); // [asm 6DC9-6DCC] kill source
                    state.ww(rslot + 4, bp); // [asm 6DCF]
                    killed = true;
                }
            }
            if !killed {
                // [asm 6D80] integrate: Y by fall_vel>>4, X by the oscillating drift accumulator
                let proj_y = state.rw(proj + 2);
                let fall_vel = state.rw(proj + 6);
                state.ww(proj + 2, proj_y.wrapping_add(sar16(fall_vel, 4)));

                let drift_acc = state.rw(proj + 8);
                let drift_step = state.rb(proj + 0xA);
                let new_drift_acc = drift_acc.wrapping_add(cbw(drift_step));
                state.ww(proj + 8, new_drift_acc);

                let d8 = s16(new_drift_acc as i32); // [asm 6D95-6DA2] reverse at the +/-0x20 extents
                if d8 >= 0x20 || d8 < -0x20 {
                    state.wb(proj + 0xA, (drift_step as i8).wrapping_neg() as u8);
                }

                let dx4 = sar16(new_drift_acc, 4); // [asm 6DA5]
                if dx4 != 0 {
                    // [asm 6DAD je 6DB7]
                    bp = bp.wrapping_add(if s16(dx4 as i32) > 0 { 1 } else { 2 }); // [asm 6DAF-6DB5]
                }
                let proj_x = state.rw(proj);
                let new_x = proj_x.wrapping_add(dx4);
                state.ww(proj, new_x); // [asm 6DB7] X += drift
                state.ww(rslot, new_x); // [asm 6DB9-6DBB] project X
                let y = state.rw(proj + 2);
                state.ww(rslot + 2, y); // [asm 6DBD-6DC0] project Y
                if s16(y as i32) > 0x7D8 {
                    // [asm 6DC3 jle keeps]
                    bp = 0xFFFF;
                    state.ww(proj + 4, bp); // [asm 6DC9-6DCC] kill off the bottom
                }
                state.ww(rslot + 4, bp); // [asm 6DCF]
            }
        }
    }

    // [asm 6DDE..6E57] bake the target geometry from the three tables into the boss target records
    let t0 = TARGET_RECORDS; // 0x5648
    let t1 = TARGET_RECORDS + 0x12; // 0x565A
    let t2 = TARGET_RECORDS + 0x24; // 0x566C
    let t3 = TARGET_RECORDS + 0x36; // 0x567E
    let t4 = TARGET_RECORDS + 0x48; // 0x5690

    state.ww(t2 + 4, 0xFFFF); // [asm 6DDE-6DE4]
    state.ww(t1 + 4, 0xFFFF);

    let boss_phase_u16 = state.rw(L6_PHASE);
    if boss_phase_u16 < 2 {
        // [asm 6DE7 jae 6E12]
        let l6_sub_b = state.rb(L6_SUB_B) as u16;
        let si = (0xC * l6_sub_b + L6_SUB_B_TABLE as u16) & 0xFFFF; // [asm 6DEE-6DF6]
        let src2 = si as usize;
        let src1 = (si.wrapping_add(6)) as usize;
        // t2 = src2
        state.ww(t2, state.rw(src2)); // [asm 6DFA-6E02]
        state.ww(t2 + 2, state.rw(src2 + 2));
        state.ww(t2 + 4, state.rw(src2 + 4));
        // t1 = src1
        state.ww(t1, state.rw(src1)); // [asm 6E05-6E0F]
        state.ww(t1 + 2, state.rw(src1 + 2));
        state.ww(t1 + 4, state.rw(src1 + 4));
    }

    let l6_sub_a = state.rb(L6_SUB_A) as u16;
    let si = (0xC * l6_sub_a + L6_SUB_A_TABLE as u16) & 0xFFFF; // [asm 6E12-6E18]
    let src4 = si as usize;
    let src3 = (si.wrapping_add(6)) as usize;
    state.ww(t4, state.rw(src4)); // [asm 6E1E-6E26]
    state.ww(t4 + 2, state.rw(src4 + 2));
    state.ww(t4 + 4, state.rw(src4 + 4));
    state.ww(t3, state.rw(src3)); // [asm 6E29-6E33]
    state.ww(t3 + 2, state.rw(src3 + 2));
    state.ww(t3 + 4, state.rw(src3 + 4));

    let l6_anim = state.rb(L6_ANIM) as u16;
    let si = (6 * l6_anim + L6_ANIM_TABLE as u16) & 0xFFFF; // [asm 6E36-6E3C] the main target record
    let src0 = si as usize;
    state.ww(t0, state.rw(src0)); // [asm 6E42-6E47] (= boss_x / boss_y)
    state.ww(t0 + 2, state.rw(src0 + 2));
    if boss_phase_u16 != 0 {
        // [asm 6E4A-6E55]
        state.ww(t0 + 4, 0xFFFF);
    } else {
        state.ww(t0 + 4, state.rw(src0 + 4));
    }

    // [asm 6E58] cycle the anim index every 4th frame
    if state.rb(FRAME_BLINK) & 3 == 0 {
        let l6_anim = state.rb(L6_ANIM); // [asm 6E5F-6E6A]
        state.wb(L6_ANIM, (l6_anim + 1) % 3);
    }

    // [asm 6E6D..] hit-stun countdown vs the spawn state machine, then the shared collision + boss-hit tails
    let stun = state.rb(L6_STUN);
    let mut run_boss_hit = true;
    if stun != 0 {
        // [asm 6E6D-6E88]
        if stun == 1 {
            // [asm 6E74-6E7F]
            state.wb(L6_SUB_B, 2);
            state.wb(L6_SUB_A, state.rb(L6_SUB_A).wrapping_sub(1));
            state.wb(L6_TIMER, state.rb(L6_TIMER).wrapping_add(5));
        }
        state.wb(L6_STUN, stun.wrapping_sub(1)); // [asm 6E84]
        run_boss_hit = !l6_tail_6f8f(state); // [asm 6E88 jmp 6F8F]
    } else if state.rb(L6_TIMER) != 0 {
        // [asm 6E8B-6E90] still counting down -> wait
        run_boss_hit = !l6_tail_6f8f(state);
    } else {
        l6_spawn_state_machine(state); // [asm 6E92-6F37]
        if state.rb(RESPAWN_STATE) != 0 {
            // [asm 6F3C-6F43]
            l6_finish(state);
            return; // [asm 6F43 jmp 70CB]
        }
        let hit = combat_interaction::hitbox_overlap(state, PLAYER_X, BOSS_HITBOX); // [asm 6F4C]
        let mut bounced = hit;
        if !hit {
            let hit2 = combat_interaction::hitbox_overlap(state, PLAYER_X, BOSS_HITBOX_ALT); // [asm 6F54]
            bounced = hit2;
        }
        if bounced {
            // [asm 6F4F jb / 6F57 jae 6F8F]
            l6_bounce_6f59(state); // [asm 6F59]
        } else {
            run_boss_hit = !l6_tail_6f8f(state); // [asm 6F57 jae 6F8F]
        }
    }

    if run_boss_hit {
        l6_boss_hit(state); // [asm 6FBB-70A4]
        if s16(state.rw(PLAYER_X) as i32) > 0x3D8 {
            // [asm 70A5 jle 70CB] the right-boundary crush
            state.wb(PLAYER_DEATH_STATE, 0x2C); // [asm 70AD]
            state.wb(ANIM_GATE, 0); // [asm 70B2]
            state.ww(PLAYER_YVEL, 0xFF70); // [asm 70B7]
            state.wb(PLAYER_MOTION_MODE, 3); // [asm 70BD]
            state.ww(PLAYER_XVEL, 0xFF60); // [asm 70C2]
            hurt_effect(state); // [asm 70C8] 824D
        }
    }

    l6_finish(state); // [asm 70CB-70D6]
}

// ── 6822 dispatch: native_object_spawn ──────────────────────────

/// The 6822 dispatch (the object/effect spawner pre-amble): dispatches three
/// game-mode-gated event/spawn routines, all of which call the shared list-initialiser
/// 7585:
///   * 70D7  if [0x91FE]!=0xFF   — the terrain-entity spawner (camera/scroll engine)
///   * 6D34  if [0x2D8A]==5      — the level-6 (inside-a-tree) boss state machine
///   * 6ADD  if [0x2D8A]==9      — the mode-9 (last-boss) spawn event
pub fn native_object_spawn(state: &mut NativeGameState) {
    // Camera engine (70D7)
    if state.rb(CAM_STATE) != 0xFF {
        camera_engine(state);
    }

    // Level 6 tree boss (6D34)
    if state.rb(0x2D8A) == 5 && state.rb(0x8166) & 0xFE == 0 && state.rw(L6_PHASE) != 3 {
        tick_level6_boss(state);
    }

    // Mode 9 last boss (6ADD)
    if state.rb(0x2D8A) == 9 {
        tick_mode9_boss(state);
    }
}
