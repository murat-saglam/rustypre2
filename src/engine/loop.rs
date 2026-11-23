// SPDX-License-Identifier: GPL-3.0-only
//! The per-frame main-loop spine and the VM-less gameplay step.
//!
//! `MAIN_LOOP_SPINE` is the roadmap: every per-frame `call` the original main loop
//! (1030:0214..026D) issues, tagged `native` (a recovered gameplay system, wired below),
//! `render` (a draw routine — the faithful renderer's job, not the gameplay step), or
//! `gap` (not yet recovered). Coverage grows as gaps are recovered; nothing is ever
//! silently skipped.
//!
//! The wired steps run the *same* recovered functions the hybrid hooks call, over a
//! `NativeGameState` instead of VM memory — the migration's adapter swap.

use crate::engine::state::NativeGameState;
use crate::engine::camera_scroll;
use crate::engine::input;
use crate::recovered::player;
use crate::recovered::effects_update;
use crate::recovered::object_tick;
use crate::recovered::object_inject;
use crate::recovered::player_interaction;
use crate::recovered::terrain_entities;
use crate::recovered::combat_interaction;
use crate::recovered::camera_shake;
use crate::recovered::particles;
use crate::recovered::firefly_sim;
use crate::recovered::scroll_script;
use crate::recovered::object_spawn;
use crate::recovered::object_render;
use crate::recovered::object_particles;
use crate::recovered::input_decode;

// ── DGROUP offsets (the named fields this island reads/writes) ──────────────────────

const DS_BASE: usize = 0x1A0F << 4;
const CS_BASE: usize = 0x1030 << 4;

// Player struct
const PLAYER_BASE: usize = 0x4F1C;
const PLAYER_X: usize = PLAYER_BASE;       // 0x4F1C
const PLAYER_Y: usize = PLAYER_BASE + 2;   // 0x4F1E
const P_XVEL: usize = PLAYER_BASE + 6;     // 0x4F22
const P_YVEL: usize = PLAYER_BASE + 0xE;   // 0x4F2A
const P_SPRITE: usize = PLAYER_BASE + 4;   // 0x4F20 packed anim-frame word

// Render slot layout
const RENDER_SLOTS_BASE: usize = 0x4F0A;
const SLOT0_SPRITE: usize = RENDER_SLOTS_BASE + 4; // 0x4F0E
const PROJECTILE_SLOTS: usize = 0x4F2E;
const PROJECTILE_STRIDE: usize = 0x12;
const PROJECTILE_COUNT: usize = 4;
const RECORD_BYTES: usize = 0x12;
const LIST_BASE: usize = 0x4F0A;
const LIST_TOP: usize = 0x5720;

// Camera / scroll
const CAM_COL: usize = 0x2DE4;
const CAM_ROW: usize = 0x2DE6;
const FINE_SCROLL: usize = 0x6BC4;
const SCROLL_ACCUM: usize = 0x8164;
const SCROLL_GATE_6BD9: usize = 0x6BD9;
const ROW_FACTOR: usize = 0x6BF8;
const ROW_STRIDE: usize = 0x2DB0;
const DEST_PAGE: usize = 0x2DD8;

// Frame counter
const FRAME_TIMER: usize = 0x6BD5;

// Player globals
const DROP_GATE: usize = 0x6BE1;    // PENDING_PICKUP
const GLIDER: usize = 0x6BC5;
const RESPAWN_STATE: usize = 0x6BE4;
const END_SIGNAL: usize = 0x6BE5;
const LEVEL_END_MODE: usize = 0x6BE6;
const LEVEL_ID: usize = 0x2D8A;
const LEVEL_FLAGS: usize = 0x8166;
const MAP_SEG: usize = 0x2DDA;
const IDLE_CLOCK: usize = 0x27F0;
const IDLE_CLOCK_HI: usize = 0x27F2;

// Timer ISR
const TIMER_PHASE_ADDR: usize = CS_BASE + 0x1D6B; // cs:[0x1d6b]
const TICKS_PER_FRAME: usize = 3;

// Combat
const COMBAT_FLAG: usize = 0xA312;

// Special event / bonus
const BONUS_LETTERS_MASK: usize = 0x6CA7;
const UTENSILS_MASK: usize = 0x6CA8;
const BURST_POS_X: usize = 0xA336;
const BURST_POS_Y: usize = 0xA338;
const BURST_SPRITE: usize = 0xA33A;
const REWARD_ARM_LO: usize = 0x6BFF;
const REWARD_ARM_HI: usize = 0x6C00;
const COMBO_COMPLETE: usize = 0x6BE2;
const SHAKE_MAGNITUDE: usize = 0x6BEA;
const CAM_H_FOLLOW_GATE: usize = 0x8166;
const TERRAIN_ENTITY_BASE: usize = 0x5570;

// Light fade
const LIGHT_FADE_TO_DARK: usize = 0x6C01;
const LIGHT_FADE_TO_LIGHT: usize = 0x6C02;
const LIGHT_FADE_STEP: usize = 0x6C03;
const PALETTE_PTR_TABLE: usize = 0x2D00;
const DARK_PALETTE: usize = 0xACB7;

// Cave trigger table
const CAVE_TRIGGER_BASE: usize = 0x8367;
const CAVE_TRIGGER_STRIDE: usize = 7;
const CAVE_TRIGGER_COUNT: usize = 20;

// Proximity trigger table
const PROX_TRIGGER_BASE: usize = 0x83F3;
const PROX_TRIGGER_STRIDE: usize = 0xA;
const PROX_TRIGGER_COUNT: usize = 15;
const PROX_FIRED: u16 = 0xFFFE;
const PROX_INACTIVE: u16 = 0xFFFF;

// Sprite attribute tables
const TBL_WIDTH_HEIGHT: usize = 0x7190;
const TBL_XY_OFFSET: usize = 0x752A;
const TBL_SRC_SEG: usize = 0x62E8;
const TBL_SRC_OFF: usize = 0x5F48;

// Particle array
const PARTICLE_BASE: usize = 0x7DE6;
const PARTICLE_COUNT: usize = 0x14;
const PARTICLE_STRIDE: usize = 6;

// ── Frame transition events ────────────────────────────────────────────────────────

/// A transition the gameplay frame signals to the caller (the runtime/flow driver).
/// Transition events raised during a gameplay frame step;
/// in Rust they are a `Result` return so the caller can drive the multi-frame transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTransition {
    /// [asm 5326] The cave/teleport-entrance transition — `si` is the matched table entry offset.
    CaveTeleport(usize),
    /// [asm 4CBA/4C74] The level-end transition (normal end or warp).
    LevelEnd,
    /// [asm 4F6C] The respawn-to-checkpoint transition (multi-frame animation).
    Respawn,
    /// [asm 5063] Death -> game-over restart.
    GameOver,
    /// [asm 5034] THE END (level 0xE cleared).
    GameComplete,
}

// ── helpers ────────────────────────────────────────────────────────────────────────

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

/// The packed player TILE coordinate `(sar(y,4)&0xFF)<<8 | (sar(x,4)&0xFF)` — what the
/// trigger tables match against [asm 549A]. Arithmetic (sign-preserving) shifts exactly
/// like the ASM `sar`.
fn player_tile_coords(state: &NativeGameState) -> u16 {
    let y = state.rw(PLAYER_Y);
    let x = state.rw(PLAYER_X);
    let sy = ((y as i16) >> 4) as u8;
    let sx = ((x as i16) >> 4) as u8;
    ((sy as u16) << 8) | (sx as u16)
}

/// Read sprite attributes for a given sprite_id (the [0x7190]/[0x752A]/[0x62E8]/[0x5F48] tables).
fn read_sprite_attr(state: &NativeGameState, sprite_id: u16) -> object_render::SpriteAttr {
    let bx = ((sprite_id & 0x1FFF) << 1) as usize;
    let wh = state.rw(TBL_WIDTH_HEIGHT + bx);
    object_render::SpriteAttr {
        width: (wh & 0xFF) as i32,
        height: ((wh >> 8) & 0xFF) as i32,
        x_off: state.rb(TBL_XY_OFFSET + bx) as i32,
        y_off: state.rb(TBL_XY_OFFSET + bx + 1) as i32,
        src_seg: state.rw(TBL_SRC_SEG + bx) as usize,
        src_off: state.rw(TBL_SRC_OFF + bx) as usize,
    }
}

// ── native_object_spawn_step ───────────────────────────────────────────────────────

/// Run the recovered 6822 spawner branches over the `NativeGameState` — the first
/// native-driven main-loop call, no VM. Mirrors the 6822 dispatch: `camera_engine`
/// when the camera is active ([0x91FE]!=0xFF), `tick_level6_boss` (6D34) in the level-6
/// inside-a-tree level, and `tick_mode9_boss` in the mode-9 last-boss level.
pub fn native_object_spawn_step(state: &mut NativeGameState) {
    // The Rust object_spawn::native_object_spawn already does the camera/boss dispatch:
    //   camera_engine when CAM_STATE != 0xFF,
    //   tick_level6_boss when level==5 && level_flags&0xFE==0 && boss_phase!=3,
    //   tick_mode9_boss when level==9.
    object_spawn::native_object_spawn(state);
}

// ── native_object_system_step ──────────────────────────────────────────────────────

/// The whole 6822 object system over NativeGameState (no VM): the spawner branches
/// (camera/boss), then object_tick (684E, in place), then the second pass (6913).
/// Each sub-pass reads the previous one's writes in place — exactly the ASM's
/// fall-through 6822 -> 684E -> 6913.
pub fn native_object_system_step(state: &mut NativeGameState) {
    native_object_spawn_step(state);        // [asm 6822..6B..] camera_engine / tick_mode9_boss
    object_tick::object_tick(state);        // [asm 684E..6912] per-slot walker, in place
    object_inject::second_pass_tick(state); // [asm 6913..698B] the second pass
}

// ── native_trigger_scan ────────────────────────────────────────────────────────────

/// [asm 52FE..5325] The per-frame position-trigger scan (the cave/teleport-entrance scan).
/// When armed ([0x6BE1]!=0 and momentum dormant [0x6BC5]==0) it matches the player's
/// tile coords (549A) against the 20-entry table at [0x8367] (stride 7, `[+0]` = source
/// tile); a NO match touches nothing (byte-exact no-op). A match returns
/// `FrameTransition::CaveTeleport(entry_offset)` (BEFORE mutating anything) — the
/// caller drives the multi-frame 5326 transition via `native_cave_teleport`.
pub fn native_trigger_scan(state: &mut NativeGameState) -> Result<(), FrameTransition> {
    let drop_gate = state.rb(DROP_GATE);
    let glider = state.rb(GLIDER);
    if drop_gate != 0 && glider == 0 {
        let dx = player_tile_coords(state);
        for i in 0..CAVE_TRIGGER_COUNT {
            let entry_offset = CAVE_TRIGGER_BASE + i * CAVE_TRIGGER_STRIDE;
            let source = state.rw(entry_offset);
            if source == dx {
                return Err(FrameTransition::CaveTeleport(entry_offset));
            }
        }
        // [asm 5325] no match -> ret (a byte-exact no-op)
    }
    Ok(())
}

// ── native_cave_teleport ────────────────────────────────────────────────────────────

/// [asm 5326..53F5] The whole cave/teleport-entrance TRANSITION. The matched 7-byte-stride
/// entry `si` gives the destination: `[si+2]` = camera (lo=X, hi=Y), `[si+4]` = player tile,
/// `[si+6]` = the `[0x6BD9]` flag.
///
/// Faithful sequence (verified against the VM):
///   1. zero the velocities; 30C6 VERTICAL FADE-OUT curtain (VRAM-only; no DGROUP state).
///   2. reset the scroll accumulator, snap the player to the destination tile.
///   3. the HIDDEN camera pan: Y steps first (3363 up / 33AD down, dl=0x10), then X
///      (3414 left / 3435 right) — 1 unit per step behind the black curtain.
///   4. restore [0x8164]; [0x6BD9] = [si+6]; disarm [0x6BE1]. Level-6 boss re-init when
///      [0x2D8A]==5.
///   5. the arrival MINI-PASS: 4907 (terrain), 8922 (projector), 6822 (object system),
///      26FA ([0x6BD5]++ + record mutation), 54AB (fireflies), 3922 (scroll counter).
///      3054 is the CENTER-OUT REVEAL curtain (VRAM-only).
///   6. The interrupted frame's REMAINDER — `frame_tail_after_trigger`.
///
/// Advances all gameplay subsystems for a single frame tick;
/// just drains it, so this Rust function runs all state mutations to completion.
pub fn native_cave_teleport(state: &mut NativeGameState, si: usize) -> Result<(), FrameTransition> {
    let dest_cam = state.rw(si + 2);       // [si+2] destination camera (packed lo=X, hi=Y)
    let dest_x = (dest_cam & 0xFF) as u8;
    let dest_y = ((dest_cam >> 8) & 0xFF) as u8;
    let dest_tile = state.rw(si + 4);      // [si+4] destination player tile
    let flag = state.rb(si + 6);           // [si+6] -> [0x6BD9]

    // [asm 5326-532C] zero the velocities
    state.ww(P_XVEL, 0);
    state.ww(P_YVEL, 0);

    // [asm 5332] 30C6 vertical fade-out (VRAM-only — no DGROUP state, 9 phases)
    // for k in range(1, 10): yield ("fade", k)  -- drained by state-only consumer

    // [asm 5335] zero the vertical sub-tile accumulator
    state.wb(FINE_SCROLL, 0);

    // [asm 533A] push [0x8164]; [asm 533F] pan clamp -> max (don't block the pan)
    let saved_8164 = state.rw(SCROLL_ACCUM);
    state.ww(SCROLL_ACCUM, 0xEC);

    // [asm 5350] player X = tile.lo << 4; [asm 535A] player Y = tile.hi << 4
    state.ww(PLAYER_X, ((dest_tile & 0xFF) as u16) << 4);
    state.ww(PLAYER_Y, (((dest_tile >> 8) & 0xFF) as u16) << 4);

    // [asm 5361-5375] vertical pan first
    loop {
        let cam_row = state.rb(CAM_ROW);
        if cam_row == dest_y { break; }
        let ok = if cam_row > dest_y {
            camera_scroll::v_scroll_up(state, 0x10)
        } else {
            camera_scroll::v_scroll_down(state, 0x10)
        };
        if !ok {
            // The ASM would spin forever; we fail loud
            log::warn!(
                "cave pan blocked vertically at cam_row={} dest={}",
                cam_row, dest_y
            );
            break;
        }
        // yield ("pan",) — drained by state-only consumer
    }

    // [asm 5377-5387] then horizontal
    loop {
        let cam_col = state.rb(CAM_COL);
        if cam_col == dest_x { break; }
        let ok = if cam_col > dest_x {
            camera_scroll::apply_camera_pan_left(state)
        } else {
            camera_scroll::apply_camera_pan_right(state)
        };
        if !ok {
            log::warn!(
                "cave pan blocked horizontally at cam_col={} dest={}",
                cam_col, dest_x
            );
            break;
        }
        // yield ("pan",) — drained by state-only consumer
    }

    // [asm 538A] pop [0x8164]
    state.ww(SCROLL_ACCUM, saved_8164);
    // [asm 5391] [0x6BD9] = flag
    state.wb(SCROLL_GATE_6BD9, flag);
    // [asm 5394] disarm the trigger
    state.wb(DROP_GATE, 0);

    // [asm 5399] level-6 (inside-a-tree) boss re-init when [0x2D8A]==5
    if state.rb(LEVEL_ID) == 5 {
        state.wb(CAM_H_FOLLOW_GATE, state.rb(LEVEL_FLAGS) & 1); // [asm 53A0]
        // [asm 53A5-53CD] the boss block presets
        let presets: [(usize, u8); 8] = [
            (0xA324, 0), (0xA325, 5), (0xA326, 0), (0xA328, 0),
            (0xA32A, 1), (0xA329, 0), (0xA32B, 0x6E), (0xA32C, 8),
        ];
        for (off, val) in presets {
            state.wb(off, val);
        }
        // [asm 53CD-53D5] fill [0x5570..] with 0xFF (0x69 bytes)
        for k in 0..0x69 {
            state.wb(TERRAIN_ENTITY_BASE + k, 0xFF);
        }
    }

    // [asm 53D7-53F2] the arrival mini-pass (35A1/3A27/3721 = render; the gameplay calls run natively):
    terrain_entities::tick_terrain_entities(state);    // [asm 53DD] 4907
    object_particles::project_particles(state);         // [asm 53E0] 8922
    native_object_system_step(state);                   // [asm 53E3] 6822
    let frame_stamp = state.rw(FRAME_TIMER);
    state.ww(FRAME_TIMER, frame_stamp.wrapping_add(1) & 0xFFFF); // [asm 53E6: 26FA]
    native_object_render_state(state);                   // [asm 53E6: 26FA] record mutation
    native_firefly_step(state);                          // [asm 53EC] 54AB
    native_scroll_script(state);                         // [asm 53EF] 3922
    // [asm 53F2] 3054 center-out reveal (VRAM-only) — 10 phases, drained

    // The interrupted frame's remainder (023B..026D)
    frame_tail_after_trigger(state)
}

// ── native_proximity_trigger ────────────────────────────────────────────────────────

/// [asm 53F6..5497] The per-frame proximity-trigger scan (breakable-wall / opening-passage
/// triggers): 15 entries. When the player comes within 8 (packed) of an armed entry it
/// FIRES (`trigger_pos` = FIRED, camera shake = 7); a fired entry re-applies its map
/// modification (`native_proximity_mapmod`) each frame until its countdown disarms it.
/// No trigger in range and none fired -> a byte-exact no-op.
pub fn native_proximity_trigger(state: &mut NativeGameState) {
    let player = player_tile_coords(state);
    for i in 0..PROX_TRIGGER_COUNT {
        let base = PROX_TRIGGER_BASE + i * PROX_TRIGGER_STRIDE;
        let pos = state.rw(base + 4); // trigger_pos
        if pos == PROX_FIRED {
            // [asm 5407 je 5427] fired -> the map mod
            native_proximity_mapmod(state, base);
        } else if pos != PROX_INACTIVE && player.wrapping_sub(pos) <= 8 {
            // [asm 540C-541B] in range -> FIRST fire
            state.ww(base + 4, PROX_FIRED);
            state.wb(SHAKE_MAGNITUDE, 7); // [asm 5420] camera shake
        }
    }
}

// ── native_proximity_mapmod ──────────────────────────────────────────────────────────

/// [asm 5427..5497] The fired-trigger map modification (a wall rising / passage opening):
/// every 4th frame (`tick & 3 == 0`), shift the trigger's `height x width` tile block UP
/// one row in the level map, move `block_top` up a row, reveal a fresh bottom row from
/// the 41CA-saved pristine rows in the bank (walking `reveal_cursor` backward one `width`
/// per fire), and count down — disarming at 0. The per-tile 653D re-blit is a render
/// side-effect (the faithful renderer redraws the changed tiles).
fn native_proximity_mapmod(state: &mut NativeGameState, trig_base: usize) {
    // [asm 5429] camera shake (even on gated frames)
    state.wb(SHAKE_MAGNITUDE, 7);

    // [asm 542E] only acts every 4th frame
    let tick = state.rb(FRAME_TIMER);
    if tick & 3 != 0 {
        return;
    }

    // [asm 5435] es = the level map; [asm 5471] ds = the pristine-row bank
    let map_seg = state.rw(MAP_SEG) as usize;
    let bank_seg = state.rw(0x2875) as usize;
    let map_base = (map_seg << 4) & 0xFFFFF;
    let bank_base = (bank_seg << 4) & 0xFFFFF;

    let width = state.rb(trig_base + 2) as usize;  // [asm 5439]
    let height = state.rb(trig_base + 3) as usize;  // [asm 543B]
    let block_top = state.rw(trig_base);            // [asm 543F]
    let mut row = (block_top.wrapping_sub(0x100)) & 0xFFFF; // [asm 5445] one row up

    // [asm 5449-5463] shift the block up one row
    for _ in 0..height {
        for col in 0..width {
            let dst = (map_base + ((row as usize + col) & 0xFFFF)) & 0xFFFFF;
            let src = (map_base + ((row as usize + col + 0x100) & 0xFFFF)) & 0xFFFFF;
            state.data[dst] = state.data[src];
        }
        row = (row + 0x100) & 0xFFFF; // [asm 545B-545D] next row
    }

    // [asm 5465] the block rose one row
    state.ww(trig_base, block_top.wrapping_sub(0x100) & 0xFFFF);

    // [asm 5469-5483] reveal a fresh bottom row from the bank
    let src_cursor = state.rw(trig_base + 6) as usize; // reveal_cursor
    for col in 0..width {
        let dst = (map_base + ((row as usize + col) & 0xFFFF)) & 0xFFFFF;
        let src = (bank_base + ((src_cursor + col) & 0xFFFF)) & 0xFFFFF;
        state.data[dst] = state.data[src];
    }

    // [asm 5488] next pristine row (backward)
    state.ww(trig_base + 6, (src_cursor.wrapping_sub(width) & 0xFFFF) as u16);

    // [asm 548B] countdown
    let cnt = state.rb(trig_base + 8).wrapping_sub(1);
    state.wb(trig_base + 8, cnt);
    if cnt == 0 {
        // [asm 5490] done -> disarm
        state.ww(trig_base + 4, PROX_INACTIVE);
    }
}

// ── native_idle_timer_tick ──────────────────────────────────────────────────────────

/// [asm 0264 -> 17C0] Advance the wall-clock idle counter `[0x27F0]` the way the timer
/// ISR does. `[0x27F0]` (a free-running 32-bit counter at `[0x27F0]`/`[0x27F2]`) is
/// bumped by the timer ISR every 4th tick — i.e. whenever the mod-4 phase `cs:[0x1d6b]`
/// wraps to 0. `ticks` = how many timer ticks fire this frame.
pub fn native_idle_timer_tick(state: &mut NativeGameState, ticks: usize) {
    let timer_addr = TIMER_PHASE_ADDR & 0xFFFFF;
    let mut phase = state.data[timer_addr];
    let mut lo = state.rw(IDLE_CLOCK);
    let mut hi = state.rw(IDLE_CLOCK_HI);
    for _ in 0..ticks {
        phase = phase.wrapping_add(1) & 3; // [asm 07B2/07B7] inc [0x1d6b]; and 3
        if phase == 0 {
            // [asm 07BD je 17C0]
            lo = lo.wrapping_add(1); // [asm 17C9] add word [0x27f0], 1
            if lo == 0 {
                hi = hi.wrapping_add(1); // [asm 17CE] adc word [0x27f2], 0
            }
        }
    }
    state.data[timer_addr] = phase;
    state.ww(IDLE_CLOCK, lo);
    state.ww(IDLE_CLOCK_HI, hi);
}

// ── native_firefly_step ─────────────────────────────────────────────────────────────

/// [asm 0253: 54AB] Step the firefly swarm (animation + both RNGs) in place — the
/// per-frame sim. The VRAM draw is the renderer's job; only the state contract (slots
/// + RNG seeds + scratch) is applied here.
pub fn native_firefly_step(state: &mut NativeGameState) {
    firefly_sim::step_fireflies(state);
}

// ── native_scroll_script ───────────────────────────────────────────────────────────

/// [asm 0256: 3922] The scripted-camera-scroll STATE update. Advances the frame counter
/// and, for a level with an active script, ramps the vertical scroll amount. The render
/// half (the LEVELG falling snow) is reproduced by the renderer; the state half runs
/// here.
pub fn native_scroll_script(state: &mut NativeGameState) {
    scroll_script::scroll_script_state(state);
    let plots = scroll_script::scroll_script_snow(state);
    state.snow_plots = if plots.is_empty() { None } else { Some(plots) };
}

// ── native_level_state ─────────────────────────────────────────────────────────────

/// [asm 0259: 4C69] The per-frame level/death state dispatcher. Idle (mode, respawn,
/// death all 0) it returns `Ok(())` and the loop continues. Armed:
///   * `[0x6BE6]!=0` -> `LevelEnd` transition;
///   * `[0x6BE4]==1` -> `Respawn` transition (multi-frame animation);
///   * `[0x6BE4]!=0` (i.e. ==2) -> idle this frame (counter winding down);
///   * `[0x6BE5]==1` -> `GameOver` (death -> game-over restart);
///   * `[0x6BE5]==0xFF` -> `GameComplete` (THE END, level 0xE cleared).
pub fn native_level_state(state: &mut NativeGameState) -> Result<(), FrameTransition> {
    let level_end_mode = state.rb(LEVEL_END_MODE);
    if level_end_mode != 0 {
        return Err(FrameTransition::LevelEnd);
    }
    let respawn_state = state.rb(RESPAWN_STATE);
    if respawn_state == 1 {
        return Err(FrameTransition::Respawn);
    }
    if respawn_state != 0 {
        return Ok(()); // [0x6BE4]==2: 4C69 idle (counter winding down)
    }
    let end_signal = state.rb(END_SIGNAL);
    if end_signal == 1 {
        return Err(FrameTransition::GameOver);
    }
    if end_signal == 0xFF {
        return Err(FrameTransition::GameComplete);
    }
    Ok(())
}

// ── native_respawn_gate ────────────────────────────────────────────────────────────

/// [asm 0261: 45AF] The respawn-animation pass: when respawning it DRAWS the
/// death/respawn sequence — pure render, no gameplay-state writes — so it is a no-op
/// for the gameplay step (the faithful renderer draws it).
pub fn native_respawn_gate(_state: &mut NativeGameState) {
    // [verified render: the whole-loop verify stays DIV=0]
}

// ── native_special_event ───────────────────────────────────────────────────────────

/// [asm 026A: 67D7] The BONUS-letters event. When all 5 letters are collected
/// ([0x6CA7]==0x1F), spawn the reward sprite (0x6E) at the player (Y-0x70) via 8D1B,
/// reset the mask, and arm [0x6BFF]/[0x6C00]. Otherwise, if the [0x6CA8] 0x38-bit group
/// is complete, clear it and set [0x6BE2]=0x294. Neither armed -> a byte-exact no-op.
pub fn native_special_event(state: &mut NativeGameState) {
    let bonus_letters = state.rb(BONUS_LETTERS_MASK);
    if bonus_letters == 0x1F {
        // [asm 67D7] all 5 BONUS letters -> reward burst
        let px = state.rw(PLAYER_X);
        let py = state.rw(PLAYER_Y);
        state.ww(BURST_POS_X, px);                              // [asm 67DE] burst pos X = player X
        state.ww(BURST_POS_Y, py.wrapping_sub(0x70) & 0xFFFF);  // [asm 67E4] pos Y = player Y - 0x70
        state.ww(BURST_SPRITE, 0x6E);                           // [asm 67ED] reward sprite id
        combat_interaction::spawn_effect_burst(state, 0, 0, 1); // [asm 67FA] 8D1B: spawn 1
        state.wb(BONUS_LETTERS_MASK, 0);                        // [asm 67FD] reset the letters mask
        state.wb(REWARD_ARM_LO, 1);                             // [asm 6802]
        state.wb(REWARD_ARM_HI, 0x2C);                          // [asm 6807]
    } else {
        let utensils = state.rb(UTENSILS_MASK);
        if (utensils & 0x38) == 0x38 {
            // [asm 680D-6814] the [0x6CA8] 0x38-group is complete
            state.wb(UTENSILS_MASK, utensils & 0xC7);  // [asm 6816] clear those bits
            state.ww(COMBO_COMPLETE, 0x294);            // [asm 681B]
        }
    }
}

// ── native_camera_shake ────────────────────────────────────────────────────────────

/// [asm 026D: 4C30] One frame's screen-shake apply: from magnitude [0x6BEA] + parity
/// [0x6BD5]&1, write the renderer row-bias [0x6BF8], the jittered magnitude, and the
/// odd-frame horizontal nudge. Magnitude 0 -> no change (no shake).
pub fn native_camera_shake(state: &mut NativeGameState) {
    camera_shake::apply_camera_shake(state);
}

// ── combat pass ────────────────────────────────────────────────────────────────────

/// [asm 8C21 then 899E] Resolve one source sprite (a projectile or the player at `si`)
/// vs enemies then bonus tiles, applying each result to the live state IN PLACE so the
/// bonus scan sees the projectile's writes (the ASM's fall-through). `bounce` (player
/// only) fires the [0x4F2A] Yvel bounce on an enemy hit OR a collect.
fn combat_source_pass(state: &mut NativeGameState, si: usize, bounce: bool) {
    // [asm 8C21] source-vs-ENEMY — Rust projectile_vs_enemies applies writes directly
    let (hit, _slot) = combat_interaction::projectile_vs_enemies(state, si);

    let mut did = hit; // [asm 88EB/8908] jb -> skip the bonus scan

    if !hit {
        // [asm 899E] source-vs-BONUS pickup — Rust bonus_pickup_scan handles map writes
        let (_redraws, collected) = combat_interaction::bonus_pickup_scan(state, si);
        did = collected; // [asm 890D] jae -> skip the bounce if no collect
    }

    if bounce && did {
        // [asm 890F/8914] airborne only
        let yvel = state.rw(P_YVEL);
        if yvel != 0 {
            state.ww(P_YVEL, 0xFFB0); // [asm 8916] bounce up (-0x50 as u16)
        }
    }
}

/// [asm 88D7] The per-frame COMBAT / pickup pass: the 4 thrown-weapon slots ([0x4F2E])
/// then the player ([0x4F0A]), each resolved vs enemies (8C21) then bonus tiles (899E).
/// Fills the effect/debris burst pools, damages/kills enemies, consumes hit projectiles,
/// collects secret/bonus tiles, and bounces the player ([0x4F2A]) on a hit/collect.
pub fn native_combat_pass(state: &mut NativeGameState) {
    state.wb(COMBAT_FLAG, 1); // [asm 88DD] [0xA312] = 1 (relax the bounce test)

    // [asm 88D7/88DA] the 4 projectile slots
    for i in 0..PROJECTILE_COUNT {
        let off = PROJECTILE_SLOTS + i * PROJECTILE_STRIDE;
        let sprite = state.rw(off + 4); // [asm 88E2] slot occupied?
        if sprite != 0xFFFF {
            combat_source_pass(state, off, false);
        }
    }

    // [asm 88F5] not a scripted pose (shares PlayerGlobals.glider's byte, 0x6BC5)
    let glider = state.rb(GLIDER);
    if glider == 0 {
        // [asm 88FF] the player sprite present?
        let player_sprite = state.rw(SLOT0_SPRITE);
        if player_sprite != 0xFFFF {
            combat_source_pass(state, RENDER_SLOTS_BASE, true);
        }
    }

    state.wb(COMBAT_FLAG, 0); // [asm 891C] [0xA312] = 0
}

// ── native_particle_consume ─────────────────────────────────────────────────────────

/// [asm 4B8E state half] The point-particle pass (spider-threads / fireflies / sparkles
/// at [0x7DE6], 20 slots of 6 bytes) advances each active slot by its angle/speed, plots
/// one pixel, then KILLS the slot — so the array is one-shot, gone by the next frame.
/// The plot is render; the STATE half — write the advanced Y back and the kill sentinel
/// — is gameplay-coupled (the effect-spawn handlers reuse a freed slot).
pub fn native_particle_consume(state: &mut NativeGameState) {
    particles::consume_particles(state);
}

// ── native_object_render_state ─────────────────────────────────────────────────────

/// [asm 26FA state half] The moving-sprite renderer's RECORD MUTATION: for every active
/// slot (sprite_id [+4] != 0xFFFF) in the list 0x4F0A..0x5720 it decrements the life
/// countdown [+0x11] (saturating) and updates the drawn flag [+5]. This is the
/// GAMEPLAY-COUPLED half of object_render (the pixel half is native_render's).
/// [0x6BD5] was already incremented by the caller, so read the camera with
/// frame_pre_inc=False.
pub fn native_object_render_state(state: &mut NativeGameState) {
    // Read the camera (frame_pre_inc=False: frame = current value, NOT +1)
    let frame = state.rw(FRAME_TIMER) as u32; // NOT +1 — the caller already incremented
    let cam = object_render::Camera {
        cam_x: s16(state.rw(CAM_COL) as i32),
        cam_y: s16(state.rw(CAM_ROW) as i32),
        fine_scroll: state.rb(FINE_SCROLL) as i32,
        row_factor: s16(state.rw(ROW_FACTOR) as i32),
        dest_page: state.rw(DEST_PAGE) as usize,
        row_stride: state.rw(ROW_STRIDE) as usize,
        global_shift: 3,  // [cs:0] = 3 — pixel-to-byte shift divisor
        frame,
    };

    let mut flash: Vec<u16> = Vec::new();

    // Walk the active list top->down (LIST_TOP-RECORD_BYTES down to LIST_BASE)
    let mut off = LIST_TOP;
    while off > LIST_BASE {
        off -= RECORD_BYTES;

        let sprite_id = state.rw(off + 4);
        if sprite_id == 0xFFFF {
            continue; // [asm 2713] empty slot
        }

        // [asm 2757/28BA] the OPAQUE/flash flag (id bit14 = 0x4000)
        if sprite_id & 0x4000 != 0 {
            flash.push(off as u16);
        }

        let spr = object_render::Sprite {
            x: s16(state.rw(off) as i32),
            y: s16(state.rw(off + 2) as i32),
            sprite_id,
            flags: state.rb(off + 5),
            life: state.rb(off + 0x11),
        };

        let attr = read_sprite_attr(state, sprite_id);

        // The draw/visibility decision
        let draw = object_render::plan_sprite(&spr, &attr, &cam);

        // [asm 2732/2742/28B6] write the record mutation (life + flags)
        let update = object_render::plan_record_update(&spr, draw.is_some());
        state.wb(off + 0x11, update.new_life);
        state.wb(off + 5, update.new_flags);
    }

    state.flash_slots = if flash.is_empty() { None } else { Some(flash) };
}

// ── native_light_fade_step ──────────────────────────────────────────────────────────

/// [asm 6772 STATE half, called at 0267 each main-loop tick] The light-fade (dark-cave
/// lamp) progress: while a fade is active ([0x6C01]=to-dark | [0x6C02]=to-level-palette,
/// set by the light pickups in player_interaction 876C/8790) the ASM increments the step
/// [0x6C03] and, when every one of the 0x30 DAC channels is within one step of its
/// target, clears both flags — fade complete. The DAC ramp itself is render. Inactive
/// fade -> byte-exact no-op.
pub fn native_light_fade_step(state: &mut NativeGameState) {
    let to_dark = state.rb(LIGHT_FADE_TO_DARK);
    let to_light = state.rb(LIGHT_FADE_TO_LIGHT);
    if to_dark == 0 && to_light == 0 {
        return; // [asm 6771/6775 je 67D6] no active fade
    }

    // [asm 677B] inc byte [0x6C03]
    let step = state.rb(LIGHT_FADE_STEP).wrapping_add(1);
    state.wb(LIGHT_FADE_STEP, step);

    // [asm 6787/6791] level -> dark
    let level = state.rb(LEVEL_ID) as usize;
    let level_palette = state.rw(PALETTE_PTR_TABLE + level * 2) as usize;
    let mut src = level_palette;
    let mut dst = DARK_PALETTE;
    if to_light != 0 {
        // [asm 6799-67A0] fading BACK -> swap
        std::mem::swap(&mut src, &mut dst);
    }

    // [asm 67A2-67C6] 16 colours x RGB (0x30 bytes)
    let mut still_ramping = 0usize;
    for k in 0..0x30 {
        let src_byte = state.rb((src + k) & 0xFFFF);
        let dst_byte = state.rb((dst + k) & 0xFFFF);
        let diff = (src_byte as i16 - dst_byte as i16).abs();
        if diff > step as i16 {
            still_ramping += 1; // [asm 67B3 ja]
        }
    }

    if still_ramping == 0 {
        // [asm 67C8-67D1] complete -> clear flags
        state.wb(LIGHT_FADE_TO_DARK, 0);
        state.wb(LIGHT_FADE_TO_LIGHT, 0);
    }
}

// ── native_player_step ──────────────────────────────────────────────────────────────

/// [asm 022F: 5850] The WHOLE player update over NativeGameState — the VM-less player
/// step. Suppresses the raw player draw (slot 0 = 0xFFFF), decodes input (DC1), runs
/// the FSM, integrates kinematics, resolves collision, and ticks timers. SFX are
/// emitted via the FSM's sfx_queue pushes.
pub fn native_player_step(state: &mut NativeGameState) {
    // [asm 585E] suppress the raw player draw (slot 0 = 0x4F0A)
    state.ww(SLOT0_SPRITE, 0xFFFF);

    // [asm 5864-5879] F1 debug kill-self
    if state.rb(RESPAWN_STATE) == 0 && state.rb(0x282E) != 0 {
        player::player_f1_suicide(state);
        return;
    }
    // [asm 587C-588F] F2 debug abort -> game over
    if state.rb(0x282F) != 0 {
        state.wb(END_SIGNAL, 1);
        return;
    }

    // [asm 58A4] DC1 input decode
    input_decode::decode_input(state);

    // [asm 58A7] FSM (jump/land/... sfx) — the Rust player_fsm_step applies writes + sfx
    if let Some(scroll) = player::player_fsm_step(state) {
        let dir = match scroll {
            player::ScrollDir::Left => crate::views::camera_pan::PanDirection::Left,
            player::ScrollDir::Right => crate::views::camera_pan::PanDirection::Right,
        };
        crate::views::camera_pan::apply_camera_pan(state, dir);
    }

    // [asm 5A0F/5A36] X/Y kinematics integrate
    player::player_integrate(state);

    // [asm 5A96] ground/tile collision
    crate::recovered::player_collision::collision(state);

    // [asm 5A44 -> 484E] the glider/flying update (0x6BC5, dormant when 0)
    if state.rb(GLIDER) != 0 {
        player::player_flying_484e(state);
    }

    // [asm 5A4A-5A87] player timer decrements
    player::player_timers(state);
}

// ── native_gameplay_frame ──────────────────────────────────────────────────────────

/// Drive the WHOLE per-frame main loop (0214..0270) over NativeGameState — VM-less, in
/// spine order. The recovered gameplay systems run; the render calls are the faithful
/// renderer's job and are not run here. The event-driven paths a normal frame doesn't
/// take — death/respawn + the level-state machine, the scripted camera, the 67D7
/// one-shot — are byte-exact no-ops when idle and signal a `FrameTransition` when armed,
/// never a silent skip.
pub fn native_gameplay_frame(state: &mut NativeGameState) -> Result<(), FrameTransition> {
    // [asm 021A] 581E — tick_popup_ring (effects-update popup)
    effects_update::tick_popup_ring(state);

    // [asm 021D] 88D7 — combat/pickup pass
    native_combat_pass(state);

    // [asm 0220] 6822 — whole object system (spawn + tick + 2nd pass)
    native_object_system_step(state);

    // [asm 0223] 6210 — tick_projectiles (effects-update)
    effects_update::tick_projectiles(state);

    // [asm 0226] 60FE — tick_particles (effects-update)
    effects_update::tick_particles(state);

    // [asm 0229] 60DF — tick_debris_pool (effects-update)
    effects_update::tick_debris_pool(state);

    // [asm 022C] 4907 — tick_terrain_entities
    terrain_entities::tick_terrain_entities(state);

    // [asm 022F] 5850 — the WHOLE player update (DC1 input + FSM + X/Y integrate +
    // collision + timers)
    native_player_step(state);

    // [asm 0232] 8295 — player<->world pass (stomp/hurt/pickup)
    player_interaction::player_interaction_tick(state);

    // [asm 0235] 8922 — effect-sprite projector
    object_particles::project_particles(state);

    // [asm 0238] 52FE — position-trigger scan (returns CaveTeleport on a match)
    native_trigger_scan(state)?;

    // [asm 023B..026D] the frame's remainder
    frame_tail_after_trigger(state)
}

// ── frame_tail_after_trigger ────────────────────────────────────────────────────────

/// The main-loop frame REMAINDER after the position-trigger scan ([asm 023B..026D]) —
/// shared by the normal frame and the cave-teleport transition (whose 5326 runs mid-scan,
/// then the frame continues here).
fn frame_tail_after_trigger(state: &mut NativeGameState) -> Result<(), FrameTransition> {
    // [asm 023B] 53F6 — proximity trigger (no-op unfired)
    native_proximity_trigger(state);

    // [asm 023E] 5643 — H+V camera follow/scroll
    camera_scroll::native_camera_follow(state);

    // [asm 0241..0250] 3668/35A1/3A27/4B8E/26FA/3721 render cluster — the faithful
    // renderer's job. Two gameplay side effects are extracted:

    // 4B8E: snapshot [0x7DE6] at the ENTRY (spider-threads/sparkles) so native_render
    // can DRAW them — the consume below KILLS the one-shot slots
    let particle_base = DS_BASE + PARTICLE_BASE;
    let particle_len = PARTICLE_COUNT * PARTICLE_STRIDE;
    state.particle_capture = Some(state.data[particle_base..particle_base + particle_len].to_vec());

    // [asm 0247: 4B8E state] advance-Y + kill [0x7DE6]
    native_particle_consume(state);

    // [asm 024D: 2708] inc word [0x6bd5] — the free-running frame counter
    let frame_stamp = state.rw(FRAME_TIMER);
    state.ww(FRAME_TIMER, frame_stamp.wrapping_add(1) & 0xFFFF);

    // [asm 024D: 26FA] record mutation (life/flags)
    native_object_render_state(state);

    // [asm 0253] 54AB — firefly step
    native_firefly_step(state);

    // [asm 0256] 3922 — scroll script
    native_scroll_script(state);

    // [asm 0259] 4C69 — level/death state dispatcher (carry -> level change @ 0x12f)
    native_level_state(state)?;

    // [asm 0261] 45AF — respawn gate (render-only, no-op for gameplay)
    native_respawn_gate(state);

    // [asm 0264: 44FB wait -> 17C0 timer] advance the [0x27F0] idle counter (fidget anim)
    native_idle_timer_tick(state, TICKS_PER_FRAME);

    // [asm 0267: 6772 state] [0x6C03]++ / flag clear — the light-fade step
    native_light_fade_step(state);

    // [asm 026A] 67D7 — special event (BONUS letters)
    native_special_event(state);

    // [asm 026D] 4C30 — camera shake
    native_camera_shake(state);

    // [asm 0270] jmp 0214 — loop back
    Ok(())
}

// ── native_death_bounce_509d ────────────────────────────────────────────────────────

/// [asm 509D] The death-bounce animation — the player's death-jump (called first by the
/// respawn 4F6C and the death 5063). Sets the player flying (Yvel=+0xF, anim 0x21)
/// drifting toward the screen centre (Xvel=+/-5), then runs 0x3C=60 frames of the
/// entity-update SUBSET (581E/6822/6210/60FE/60DF + the 26FA frame-counter tick + 54AB/
/// 3922) while integrating the player ballistically with gravity (NO collision — the
/// corpse arcs up then falls through the level). The render cluster is the renderer's job.
///
/// `render_frame` is called once per frame (mirroring the ASM's 0x50de loop-top
/// yield) so the caller renders each of the 60 frames — the whole arc animates
/// instead of teleporting to the end.
pub fn native_death_bounce_509d<F: FnMut(&mut NativeGameState)>(
    state: &mut NativeGameState,
    mut render_frame: F,
) {
    // [asm 50a6-50a9] the death SCREAM (play_sfx 7)
    let px = crate::engine::audio::player_sfx_x(state);
    crate::engine::audio::native_play_sfx(state, 7, Some(px));

    // [asm 50ac] player.death_state = 0
    state.wb(PLAYER_BASE + 0x11, 0);

    // [asm 50b1] player.sprite = 0x21 (the death anim frame)
    state.ww(P_SPRITE, 0x21);

    // [asm 50b7] player.yvel = 0x0F (the upward kick)
    state.ww(P_YVEL, 0x0F);

    // [asm 50c0-50cd] centre = ((cam_col_word + 0xA) << 4) & 0xFFFF
    let cam_col_word = state.rw(CAM_COL);
    let centre = ((cam_col_word.wrapping_add(0xA)) << 4) & 0xFFFF;

    // [asm 50cf-50d8] player.xvel = 5 if player.x < centre else -5 (drift toward screen centre)
    let player_x = state.rw(PLAYER_X);
    let xvel: i16 = if player_x < centre { 5 } else { -5 };
    state.ww(P_XVEL, xvel as u16);

    // [asm 50db cx=0x3c] 60 frames
    for _ in 0..0x3C {
        // yield — render THIS bounce frame (mirrors the ASM loop-top 0x50de)
        render_frame(state);

        // [asm 50df] player.slot0.sprite = 0xFFFF (suppress the normal player draw)
        state.ww(SLOT0_SPRITE, 0xFFFF);

        // [asm 50e5] 581E tick_popup_ring
        effects_update::tick_popup_ring(state);

        // [asm 50e8] 6822 native_object_system_step (spawn + tick + 2nd pass)
        native_object_system_step(state);

        // [asm 50eb] 6210 tick_projectiles
        effects_update::tick_projectiles(state);

        // [asm 50ee] 60FE tick_particles
        effects_update::tick_particles(state);

        // [asm 50f1] 60DF tick_debris_pool
        effects_update::tick_debris_pool(state);

        // [asm 50fx] 4B8E state (advance-Y + kill [0x7DE6])
        native_particle_consume(state);

        // [asm 26fa:2708] frame counter increment
        let frame_stamp = state.rw(FRAME_TIMER);
        state.ww(FRAME_TIMER, frame_stamp.wrapping_add(1) & 0xFFFF);

        // [asm 5106] 54AB firefly step (swarm sim + RNG)
        native_firefly_step(state);

        // [asm 5109] 3922 scroll script
        native_scroll_script(state);

        // [asm 510f] X += Xvel
        let px = state.rw(PLAYER_X) as i32;
        let xv = state.rw(P_XVEL) as i16 as i32;
        state.ww(PLAYER_X, ((px + xv) & 0xFFFF) as u16);

        // [asm 5116] Yvel -= 1 (gravity)
        let yv = state.rw(P_YVEL) as i16;
        let yv = yv - 1;
        // [asm 511a] clamp at terminal -0x10
        let yv = if yv < -0x10 { -0x10i16 } else { yv };
        // [asm 5122]
        state.ww(P_YVEL, yv as u16);

        // [asm 5125] Y -= Yvel
        let py = state.rw(PLAYER_Y) as i32;
        state.ww(PLAYER_Y, ((py - yv as i32) & 0xFFFF) as u16);
    }
    // [asm 512c] call 30c6 — camera/scroll fixup (the renderer's job; no gameplay DGROUP state)
}

// ── native_init_input ──────────────────────────────────────────────────────────────

/// Initialize keyboard input for native play.
/// Call once after level start, before the first gameplay frame.
pub fn native_init_input(state: &mut NativeGameState) {
    input::init_keyboard_input(state);
}
