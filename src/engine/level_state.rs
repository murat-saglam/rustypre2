// SPDX-License-Identifier: GPL-3.0-only
//! The PRE2 death / respawn / level-state handlers — the 4C69 dispatch targets, VM-independent native source.
//!
//! `native_4f6c` is the respawn-to-checkpoint handler (`[0x6be4]==1`): it plays the death-bounce, then restores
//! the level to its checkpoint and continues the gameplay loop. The death (5063) / game-over (5034) / level-end
//! (4F65) handlers — which return carry to main's level-change at 0x12f — are the remaining gaps.
//!
//! Each multi-frame sequence (`native_5063`, `native_4f6c`) emits one state snapshot per death-bounce frame so the caller
//! renders the whole arc. In Rust this is modelled as a `render_frame: &mut impl FnMut(&mut NativeGameState)`
//! callback invoked once per frame — the caller passes its render closure and the function drives all 60
//! frames to completion before proceeding to the reset/restore.

use crate::engine::state::{NativeGameState, DS_BASE};
use crate::engine::dgroup_offsets;
use crate::engine::level_init;
use crate::engine::r#loop;
use crate::views::dgroup_view::PLAYER_BASE;
use crate::views::tables::ByteTable;

/// DGROUP paragraph base (DATA_SEG << 4 = 0x1A0F0).
const _DS: usize = DS_BASE;

/// [asm 4fe1-4fff] effect-sprite types (relative to 0x35) the checkpoint restore must NOT overwrite — the
/// re-init's freshly-spawned transient sprites are kept rather than rolled back to the snapshot.
const _TRANSIENT_TYPES: [u8; 7] = [0xD, 0x2C, 0x41, 0xA9, 0xAA, 0xB6, 0xE0];

// ---- struct-array layout constants (from pre2/views/dgroup_view.py) -----------------------------
/// Effect-source list base (0x8F1D), stride 7, 0x46 entries; `.sprite` at offset +4.
const EFFECT_SRC_BASE: usize = 0x8F1D;
const EFFECT_SRC_STRIDE: usize = 7;
const EFFECT_SRC_SPRITE_OFF: usize = 4;
const EFFECT_SRC_COUNT: usize = 0x46;

/// Bonus-cell list base (0x8C8D), stride 5, 0x50 entries; `.cell` (u16) at offset +3.
const BONUS_CELL_BASE: usize = 0x8C8D;
const BONUS_CELL_STRIDE: usize = 5;
const BONUS_CELL_CELL_OFF: usize = 3;
const BONUS_CELL_COUNT: usize = 0x50;

/// item_snapshot: _U16Array(0x6C12, 0x47) — 0x46 entries + 1 end marker.
const ITEM_SNAPSHOT_BASE: usize = dgroup_offsets::ITEM_QUEUE; // 0x6C12

/// active_flag_snapshot: _U8Array(0xA2A8, 0x50).
const ACTIVE_FLAG_SNAPSHOT_BASE: usize = dgroup_offsets::ACTIVE_FLAG_SNAPSHOT; // 0xA2A8

// ---- DGROUP offset shortcuts --------------------------------------------------------------------
const CAM_COL_WORD: usize = 0x2DE4;
const CAM_ROW_WORD: usize = 0x2DE6;
const ROW_FACTOR: usize = 0x6BF8;
const FINE_SCROLL: usize = 0x6BC4;
const LEVEL_INDEX: usize = 0x2D8A;
const SCORE_MID: usize = 0x6C0C;
const SCORE_LO: usize = 0x6C0E;
const SCORE_HI: usize = 0x6C10;
const ITEM_TOTAL: usize = dgroup_offsets::ITEM_TOTAL; // 0x6C9E
const INPUT_SOURCE: usize = 0x2879;
const END_SIGNAL: usize = 0x6BE5;
const LEVEL_END_MODE: usize = 0x6BE6;
const CHECKPOINT_X: usize = 0x6BAD;
const CHECKPOINT_Y: usize = 0x6BAF;
const LIVES: usize = 0x27D8;
const BONUS_LETTERS_MASK: usize = 0x6CA7;
const UTENSILS_MASK: usize = 0x6CA8;
const ATTACK_V19: usize = 0x7B19;
const ATTACK_PHASE: usize = 0x7B18;
const MODE_BYTE: usize = 0xB197;
const RENDER_SLOTS_BASE: usize = 0x4F0A;

// ---- the gap / transition signals (pre2/gaps.py) -----------------------------------------------

/// Level-state transition signals returned to the flow driver.
///
/// Represents game-over, expert-wall, or unrecovered state transitions.
/// In Rust they are returned as `Err(LevelStateSignal)` from the functions that can raise them.
#[derive(Debug)]
pub enum LevelStateSignal {
    /// `Pre2HybridGap` — the hybrid runtime reached something not yet recovered. Fail loud.
    HybridGap(String),
    /// `Pre2GameOverTransition` — death → game-over-restart: the bounce is done, reset to level 1.
    GameOverTransition,
    /// `Pre2ExpertEater` — a BEGINNER advanced into level 8/9 (the penguin is expert-only).
    ExpertEater,
}

impl std::fmt::Display for LevelStateSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LevelStateSignal::HybridGap(msg) => write!(f, "Pre2HybridGap: {}", msg),
            LevelStateSignal::GameOverTransition => write!(f, "Pre2GameOverTransition"),
            LevelStateSignal::ExpertEater => write!(f, "Pre2ExpertEater"),
        }
    }
}

impl std::error::Error for LevelStateSignal {}

// ---- the handlers ------------------------------------------------------------------------------

/// [asm 51DF] Respawn cleanup: zero the object-backup scratch `[0x6c12..+0x71]` + `[0x6c9e]`.
pub fn native_51df(state: &mut NativeGameState) {
    // [asm 51e2-51e7] the item-count table
    let base = _DS + dgroup_offsets::ITEM_QUEUE;
    for b in base..base + 0x71 {
        state.data[b] = 0;
    }
    // [asm 51e9] item_total = 0
    state.ww(ITEM_TOTAL, 0);
}

/// [asm 506c tail] The game-over DGROUP reset shared by `native_5063` (the `[0x6be5]==1` game-over) and
/// the `native_4f6c` real-death branch — `5063` restores `[0x2875]` then does the bounce and FALLS INTO
/// `506c`, and `4f6c`'s real-death path `jmp`s to the same `506c`. The reset: the `9b23` camera reset,
/// the death pose, restart-at-level-1, a zero score, and the `51df` cleanup. (The bounce + the `9b23/25c7`
/// game-over presentation + music are the flow driver's; this is the gameplay contract only.)
fn _game_over_reset(state: &mut NativeGameState) {
    // [asm 9b35-9b3e] camera reset (9b23)
    state.ww(CAM_COL_WORD, 0);   // g.cam_col_word = 0
    state.ww(CAM_ROW_WORD, 0);   // g.cam_row_word = 0
    state.ww(ROW_FACTOR, 0);     // g.row_factor = 0
    state.wb(FINE_SCROLL, 0);   // g.fine_scroll = 0
    // [asm 5078] the death pose
    state.ww(PLAYER_BASE + 4, 0x0D);   // pv.sprite = 0x0D
    // [asm 507e] restart at level 1
    state.wb(LEVEL_INDEX, 0);    // g.level = 0
    // [asm 5083-508f] score = 0
    state.ww(SCORE_LO, 0);       // g.score_lo = 0
    state.ww(SCORE_HI, 0);       // g.score_hi = 0
    state.ww(SCORE_MID, 0);      // g.score_mid = 0
    // [asm 5095] cleanup
    native_51df(state);
}

/// [asm 5063] The DEATH -> GAME-OVER-RESTART handler (4C69's `[0x6be5]==1`). Plays the death-bounce (509d),
/// then resets to level 1 with a zero score and returns carry to main's 0x12f, which RELOADS level 1 (the
/// game restarts).
///
/// `render_frame` is called once per death-bounce frame (60 frames) so the caller renders the whole arc;
/// after it drains, the game-over reset is applied (instantaneous). Drive to completion to apply the reset.
pub fn native_5063<F: FnMut(&mut NativeGameState)>(
    state: &mut NativeGameState,
    render_frame: &mut F,
) {
    // [asm 5069] 509d death-bounce (60 frames)
    r#loop::native_death_bounce_509d(state, render_frame);
    // [asm 506c tail]
    _game_over_reset(state);
}

/// [asm 4F6C] The respawn-to-checkpoint handler (4C69's `[0x6be4]==1` target). Plays the death-bounce
/// (509d), then — unless a real death is pending (`[0x2879]` or `[0x6be5]`) — snapshots the live
/// effect-sprite-source + active-flag tables, re-inits the level (5237), drops the player at the checkpoint
/// `[0x6bad]`/`[0x6baf]`, re-inits the camera (3af2), restores the snapshot (keeping the re-init's transient
/// effect sprites + re-freeing the slots that were dead), and cleans up (51df). Returns with NO level
/// change — the gameplay loop continues.
///
/// A pending REAL death instead takes the shared 506c game-over tail: the reset is applied
/// (`_game_over_reset`) and `LevelStateSignal::GameOverTransition` propagates to the flow driver.
///
/// `render_frame` is called once per death-bounce frame (60 frames) so the caller renders the whole arc,
/// THEN runs the checkpoint restore (instantaneous — the screen jumps to the checkpoint on the next
/// rendered frame). Drive to completion to apply the respawn.
pub fn native_4f6c<F: FnMut(&mut NativeGameState)>(
    state: &mut NativeGameState,
    render_frame: &mut F,
) -> Result<(), LevelStateSignal> {
    // [asm 4f6c] 509d death-bounce (per-frame)
    r#loop::native_death_bounce_509d(state, render_frame);

    // [asm 4f6f-4f80] a real death is pending
    let input_source = state.rb(INPUT_SOURCE);
    let end_signal = state.rb(END_SIGNAL);
    if input_source != 0 || end_signal != 0 {
        // [asm 4f76/4f80 jmp 506c] the bounce is done; this is the SAME game-over tail as native_5063 (5063
        // falls into 506c). Apply the shared reset, then hand off to the flow driver's game-over (scene +
        // restart) — exactly the [0x6be5]==1 path.
        _game_over_reset(state);
        return Err(LevelStateSignal::GameOverTransition);
    }

    // [asm 4f91-4fa6] snapshot the 0x46 effect-sprite source values [0x8f1d]+4 (stride 7) -> [0x6c12] (stride 2)
    for i in 0..EFFECT_SRC_COUNT {
        let sprite = state.rw(EFFECT_SRC_BASE + i * EFFECT_SRC_STRIDE + EFFECT_SRC_SPRITE_OFF);
        state.ww(ITEM_SNAPSHOT_BASE + i * 2, sprite);
    }
    // [asm 4fa3] end marker
    state.ww(ITEM_SNAPSHOT_BASE + EFFECT_SRC_COUNT * 2, 0x55AA);

    // [asm 4fa7-4fbd] snapshot the 0x50 active flags [0x8c8d]+3 (stride 5) -> [0xa2a8] (1 = live, 0 = free)
    for i in 0..BONUS_CELL_COUNT {
        let cell = state.rw(BONUS_CELL_BASE + i * BONUS_CELL_STRIDE + BONUS_CELL_CELL_OFF);
        state.wb(ACTIVE_FLAG_SNAPSHOT_BASE + i, if cell == 0xFFFF { 0 } else { 1 });
    }

    // [asm 4fbf] 5237 level re-init
    level_init::native_5237(state);

    // [asm 4fc2-4fcb] player -> the checkpoint
    let cp_x = state.rw(CHECKPOINT_X);
    let cp_y = state.rw(CHECKPOINT_Y);
    state.ww(PLAYER_BASE, cp_x);       // pv.x = g.checkpoint_x
    state.ww(PLAYER_BASE + 2, cp_y);  // pv.y = g.checkpoint_y

    // [asm 4fce] 3af2 camera-init
    level_init::native_3af2(state);

    // [asm 4fd1-500a] restore the effect-sprite sources, skipping slots whose re-init type is transient
    for i in 0..EFFECT_SRC_COUNT {
        let ax = state.rw(ITEM_SNAPSHOT_BASE + i * 2);
        let src_sprite = state.rw(EFFECT_SRC_BASE + i * EFFECT_SRC_STRIDE + EFFECT_SRC_SPRITE_OFF);
        let diff = (src_sprite.wrapping_sub(0x35)) as u8;
        if !_TRANSIENT_TYPES.contains(&diff) {
            state.ww(EFFECT_SRC_BASE + i * EFFECT_SRC_STRIDE + EFFECT_SRC_SPRITE_OFF, ax);
        }
    }
    // [asm 500c-5022] re-free any slot the snapshot recorded as dead -> [0x8c8d]+3 = 0xffff
    for i in 0..BONUS_CELL_COUNT {
        let snap = state.rb(ACTIVE_FLAG_SNAPSHOT_BASE + i);
        if snap == 0 {
            state.ww(BONUS_CELL_BASE + i * BONUS_CELL_STRIDE + BONUS_CELL_CELL_OFF, 0xFFFF);
        }
    }

    // [asm 5024] cleanup
    native_51df(state);
    // [asm 5030-5033] ax=0; clc; ret — no level change, the gameplay loop continues
    Ok(())
}

/// [asm 4C69] Whether this level-end runs the TALLY flow (4CCB exit-anim/BRAVO/carte) or the cave-style
/// CURTAIN (`call 30C6` + `jmp 4F65` — no tally, no carte).
///
/// The dispatch: a normal end (`mode==1`) on a MAIN level (<0xA) falls into 4CCB = TALLY; a normal end on a
/// BONUS level (>=0xA, 4CBA/4CC1) closes the curtain and reloads. A warp (`mode>1`) FROM a main level (4C8F)
/// jumps INTO its bonus level curtain-style; a warp FROM a bonus level (4C7E reverse lookup) re-enters the
/// 4CBA normal path = TALLY. So: tally iff `(mode==1) == (level<0xA)`.
pub fn level_end_takes_tally(mode: u8, level: u8) -> bool {
    (mode == 1) == ((level as usize) < 0xA)
}

/// [asm 4C69 `[0x6be6]==1 -> 4cba -> 4F65 -> main 0x12f`] The LEVEL-END transition: advance to the next
/// level.
///
/// The VM path runs the exit anim (4ccb, a visual multi-frame scene whose player moves are overwritten by the
/// load), increments `[0x2d8a]`, calls 4F65 (5237 re-init + carry), and re-enters main at 0x12f which clears
/// [0x6be6], DAC-fades, runs the level-init (447d -> 3ed6 loader) for the new level, then sets the
/// level-change flags. The recovered GAMEPLAY transition keeps the byte-exact end state (the next level
/// loaded + ready) and leaves the exit-anim / fade to the renderer / flow driver: increment the level,
/// `native_level_init` the new one (load + 5237 + player-init + camera, all individually byte-exact), then
/// apply the 0x12f flag writes.
///
/// The next level is the 4C74 table lookup: `[0x6be6]==1` normal end -> `+1`; `[0x6be6]>1` WARP -> a main
/// level (<0xA) jumps to its BONUS level `[0x2cf6+level]` (no +1, the ASM jmp 4f65 skips 4cba), and a bonus
/// level (>=0xA) reverse-looks-up its source main level in `[0x2cf6]` then advances `+1` (jmp 4cba -> 4cc4).
///
/// Returns `Err(LevelStateSignal)` on the expert-eater wall (beginner reaching level 8/9) or a hybrid gap
/// (bonus level not in the warp table).
pub fn native_level_end(
    state: &mut NativeGameState,
    game_root: &std::path::Path,
) -> Result<(), LevelStateSignal> {
    let level = state.rb(LEVEL_INDEX);
    let level_end_mode = state.rb(LEVEL_END_MODE);

    if level_end_mode > 1 {
        // [asm 4C74] a WARP (not the normal +1 end)
        if (level as usize) < 0xA {
            // a main level -> its bonus level [0x2cf6+level]
            // [asm 4c8f-4c90] (jmp 4f65: NO +1) — warp table
            let warp = ByteTable::read(state, dgroup_offsets::WARP_TABLE, level as usize);
            state.wb(LEVEL_INDEX, warp);
        } else {
            // a bonus level -> the source main level, then +1
            // [asm 4c7e-4c85] reverse-lookup: scan [0x2cf6] for the entry whose value == this bonus level.
            // The table only maps the FIVE table-warp bonuses (0x00/0x0A/0x0B/0x0C/0x0E, reached by a
            // `[0x6be6]=0xff` id-0x137 tile on a main level); those always resolve here. The OTHER bonuses
            // (0x0D/0x0F=LEVELG/0x10) are NOT in the table and never reach this branch: they exit via the
            // id-0x117 end-of-level tile (8859->882A) which REMAPS [0x2d8a] (LEVELG 0x0F->6) and sets
            // [0x6be6]=1, so they take the normal-end path above (recovered in player_interaction
            // loop2_handler num==0xE2 — that is LEVELG's real exit). If one ever did land here the ORIGINAL
            // scans off the end of the 10-entry table into garbage and jumps to a non-existent level
            // (0x0F -> level 0x94) — i.e. the real game crashes too. Fail loud rather than reproduce that
            // crash.
            let mut src: Option<usize> = None;
            for i in 0..0xA {
                if ByteTable::read(state, dgroup_offsets::WARP_TABLE, i) == level {
                    src = Some(i);
                    break;
                }
            }
            match src {
                None => {
                    return Err(LevelStateSignal::HybridGap(format!(
                        "native level-warp: bonus level {:#x} not a [0x2cf6] table-warp bonus \
                         (the original scans past the table into garbage here — an unreachable state; \
                         LEVELG/0x0D/0x10 exit via the id-0x117 end-of-level remap, not this warp)",
                        level
                    )));
                }
                Some(src_val) => {
                    // [asm 4c89 index -> 4cba -> 4cc4 +1]
                    state.wb(LEVEL_INDEX, ((src_val + 1) & 0xFF) as u8);
                }
            }
        }
    } else {
        // [asm 4cba/4cc4] the normal end -> +1
        state.wb(LEVEL_INDEX, level.wrapping_add(1));
    }

    // [asm 0137 or ax,ax / je 0163] main's 0x141-0155 level-start block (lives [0x27D8]=2, BONUS-letters
    // mask [0x6CA7]=0, utensils mask [0x6CA8]=0, damage [0x7B19]/[0x7B18]) runs ONLY when ax!=0 — a FRESH
    // game start. The between-levels path enters via 4F65 (`call 5237; xor ax,ax; stc; ret`), so ax=0 and
    // the block is SKIPPED: that persistent state carries into the next level (user: collected BONUS
    // letters were resetting each level). native_level_start runs the block unconditionally, so preserve
    // those five offsets across the load.
    // [asm 4F62] cleanup — clear [0x6c9e] ITEM_TOTAL + the object-backup scratch [0x6c12..+0x71] — runs
    // ONLY on the TALLY path (4ccb -> 4F5E `inc; call 51df; call 5237`). The BONUS/WARP path
    // (4cba/4cc1/4cc4 -> `jmp 4F65`) jumps PAST 4F62, so it SKIPS the cleanup and CARRIES the
    // collected-food count into the bonus level (verified: the LEVEL7->LEVELG warp preserves
    // [0x6c9e]=0xc/[0x6c12]/[0x6c5b] unchanged). Gate it on the same tally split.
    if level_end_takes_tally(level_end_mode, level) {
        native_51df(state);
    } else {
        // The bonus/warp path also skips the multi-frame exit-anim/reveal whose object_render erase FREES
        // the player's own sprite record [0x4F0A..0x4F1B] to 0xFFFF (the 3ED6 loader clears from 0x4F1C,
        // NOT this slot). It's render-only garbage while the sprite is suppressed ([0x4F0E]==0xFFFF at
        // level entry), so native collapses that erase to its end state here — matching the VM's LEVELG
        // entry.
        let base = _DS + RENDER_SLOTS_BASE; // 0x4F0A
        for b in base..base + (0x4F1C - RENDER_SLOTS_BASE) {
            state.data[b] = 0xFF;
        }
    }

    // [asm 0163] the EXPERT-EATER gate — main checks it AFTER 4F65 advanced [0x2d8a] (above) but BEFORE the
    // loader (447d, below): a BEGINNER ([0xB197]==0) who advanced into level 8/9 (the penguin is
    // expert-only) is shown CASTLE.SQZ and sent back to the menu, so the next level is NEVER loaded. Raise
    // here (post-advance, pre-load) so the flow driver drives the wall + re-enters the menu; the 3ED6 load
    // is skipped (matches the VM — no load).
    let new_level = state.rb(LEVEL_INDEX);
    let mode = state.rb(MODE_BYTE);
    if (new_level == 8 || new_level == 9) && mode == 0 {
        return Err(LevelStateSignal::ExpertEater);
    }

    // the level-start block resets these, but the VM carries them across a between-levels load — preserve
    // them
    // [0x27D8/6CA7/6CA8/7B19/7B18]
    let keep_lives = state.rb(LIVES);
    let keep_bonus_letters = state.rb(BONUS_LETTERS_MASK);
    let keep_utensils_mask = state.rb(UTENSILS_MASK);
    let keep_attack_v19 = state.rb(ATTACK_V19);
    let keep_attack_phase = state.rb(ATTACK_PHASE);

    // [asm 0x13e..0155] load + re-init + level-start flags
    level_init::native_level_start(state, game_root);

    // restore the preserved persistent state
    state.wb(LIVES, keep_lives);
    state.wb(BONUS_LETTERS_MASK, keep_bonus_letters);
    state.wb(UTENSILS_MASK, keep_utensils_mask);
    state.wb(ATTACK_V19, keep_attack_v19);
    state.wb(ATTACK_PHASE, keep_attack_phase);

    Ok(())
}
