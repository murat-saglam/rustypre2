// SPDX-License-Identifier: GPL-3.0-only
//! End-of-level transition drivers (`native_iris_close` and `native_exit_anim`).
//!
//! Implements the iris-close transition (`1030:316F..32DD`) and the animated
//! food-into-cauldron tally cutscene (`1030:4CEA..4F53`).

use crate::engine::state::NativeGameState;
use crate::engine::vga::NativeVGA;
use crate::engine::render::{self, RenderOptions};
use crate::recovered::object_render::{self, Sprite, SpriteAttr, Camera};
use crate::recovered::player::player_advance_anim;
use crate::recovered::tally_panel::{render_tally_panel, compute_percent};
use crate::views::tables::{ByteTable, WordTable};
use std::collections::HashMap;

// ── constants ──────────────────────────────────────────────────────────────────────

const FOOD_INDEX_TABLE: usize = 0x5C8B; // food-type id -> score-table index
const FOOD_SCORE_TABLE: usize = 0x5CAD; // index -> score value

const PLAYER_BASE: usize = 0x4F1C;
const RENDER_SLOTS_BASE: usize = 0x4F0A;
const SLOT0_SPRITE: usize = RENDER_SLOTS_BASE + 4; // 0x4F0E
const PROJECTILE_SLOTS: usize = 0x4F2E;
const EFFECT_SLOTS: usize = 0x52E8;
const SLOT_STRIDE: usize = 0x12;

const CAM_COL: usize = 0x2DE4;
const CAM_ROW: usize = 0x2DE6;
const FINE_SCROLL: usize = 0x6BC4;
const FRAME_TIMER: usize = 0x6BD5;
const GLIDER: usize = 0x6BC5;
const ROW_FACTOR: usize = 0x6BF8;
const DEST_PAGE: usize = 0x2DD8;
const ROW_STRIDE: usize = 0x2DB0;
const ANIM_HI: usize = 0x6BCF;

const SCORE_LO: usize = 0x6C0E;
const SCORE_HI: usize = 0x6C10;
const ITEM_TOTAL: usize = 0x6C9E;
const ITEM_QUEUE: usize = 0x6C12;

// Sprite attribute tables
const TBL_WIDTH_HEIGHT: usize = 0x7190;
const TBL_XY_OFFSET: usize = 0x752A;
const TBL_SRC_SEG: usize = 0x62E8;
const TBL_SRC_OFF: usize = 0x5F48;

// EGA plane constants
const PLANE_LEN: usize = 0x10000;

#[inline]
fn s16(v: u16) -> i32 {
    let v = v as i32;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

// ── read helpers ───────────────────────────────────────────────────────────────────

fn read_camera(state: &NativeGameState, frame_pre_inc: bool) -> Camera {
    let frame = state.rw(FRAME_TIMER) as u32;
    let frame = if frame_pre_inc { frame.wrapping_add(1) & 0xFFFF } else { frame };
    Camera {
        cam_x: s16(state.rw(CAM_COL)),
        cam_y: s16(state.rw(CAM_ROW)),
        fine_scroll: state.rb(FINE_SCROLL) as i32,
        row_factor: s16(state.rw(ROW_FACTOR)),
        dest_page: state.rw(DEST_PAGE) as usize,
        row_stride: state.rw(ROW_STRIDE) as usize,
        global_shift: 3, // [cs:0] = 3 — pixel-to-byte shift divisor
        frame,
    }
}

fn read_sprite(state: &NativeGameState, off: usize) -> Sprite {
    Sprite {
        x: s16(state.rw(off)),
        y: s16(state.rw(off + 2)),
        sprite_id: state.rw(off + 4),
        flags: state.rb(off + 5),
        life: state.rb(off + 0x11),
    }
}

fn read_attr(state: &NativeGameState, sprite_id: u16) -> SpriteAttr {
    let bx = ((sprite_id & 0x1FFF) << 1) as usize;
    let wh = state.rw(TBL_WIDTH_HEIGHT + bx);
    SpriteAttr {
        width: (wh & 0xFF) as i32,
        height: ((wh >> 8) & 0xFF) as i32,
        x_off: state.rb(TBL_XY_OFFSET + bx) as i32,
        y_off: state.rb(TBL_XY_OFFSET + bx + 1) as i32,
        src_seg: state.rw(TBL_SRC_SEG + bx) as usize,
        src_off: state.rw(TBL_SRC_OFF + bx) as usize,
    }
}

fn read_source(state: &NativeGameState, seg: usize, off: usize, length: usize) -> Vec<u8> {
    let base = ((seg << 4) + off) & 0xFFFFF;
    let end = (base + length).min(state.data.len());
    state.data[base..end].to_vec()
}

// ── _paint_player_over_iris ────────────────────────────────────────────────────────

/// Draw the player sprite (slot 0x4F1C) ON TOP of a composed iris frame, so it stays
/// visible as the iris fades everything else to black.
fn paint_player_over_iris(state: &mut NativeGameState, planes: &mut [Vec<u8>; 4], page: usize) {
    let spr = read_sprite(state, 0x4F1C);  // the player render record
    if spr.sprite_id == 0xFFFF {
        return;
    }
    let mut cam = read_camera(state, false);
    cam.dest_page = page & 0xFFFF;
    let attr = read_attr(state, spr.sprite_id);
    if let Some(draw) = object_render::plan_sprite(&spr, &attr, &cam) {
        let src_len = (draw.src_bw as usize) * (draw.full_rows as usize) * 6 + 64;
        let src = read_source(state, draw.src_seg, draw.src_off, src_len);
        object_render::paint_sprite(planes, &draw, &src, cam.row_stride);
    }
}

// ── native_iris_close ──────────────────────────────────────────────────────────────

/// The level-end IRIS CLOSE (316F setup + the 31F4..32DD shrink loop): a circle centred
/// on the player shrinks to black over the frozen level frame. A callback-based
/// generator: `on_frame` is called once per shrink step with `(planes, page)`.
pub fn native_iris_close<F: FnMut(&[Vec<u8>; 4], usize)>(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    display_page: usize,
    game_root: &str,
    mut on_frame: F,
) {
    // [asm 31AC-31B9] player screen X
    let pv_x = state.rw(PLAYER_BASE);
    let cam_col = state.rw(CAM_COL);
    let x_off = (pv_x.wrapping_sub(cam_col << 4)) & 0xFFFF;

    // [asm 31BC-31D1] player screen Y
    let pv_y = state.rw(PLAYER_BASE + 2);
    let cam_row = state.rw(CAM_ROW);
    let fine = state.rb(FINE_SCROLL);
    let y_off = (pv_y.wrapping_sub(cam_row << 4).wrapping_sub(0x10).wrapping_sub(fine as u16)) & 0xFFFF;

    // [asm 31D4-31DF] max(2*Y, 0xF0)
    let mut clamp = ((2 * s16(y_off)) & 0xFFFF) as u16;
    if s16(clamp) < 0xF0 {
        clamp = 0xF0;
    }

    // [asm 31E2-31EE] radius + counters
    state.ww(0x2DC8, x_off);   // iris.center_x
    state.ww(0x2DC6, y_off);   // iris.center_y
    state.ww(0x2DC4, clamp);   // iris.clamp
    state.ww(0x2DD0, 0xE6);    // iris.radius
    state.ww(0x2DC0, 4);       // iris.step
    state.ww(0x2DC2, 0);       // iris.accel_ctr

    // [asm 31F4..32DD] shrink loop
    while s16(state.rw(0x2DD0)) > 0 {
        // native_render sees [0x2DD0]!=0 -> SceneKind::IRIS (compose_iris)
        let (planes, page) = match render::native_render(
            state, dos, display_page, game_root, RenderOptions::default(),
        ) {
            (Some(mut p), pg) => {
                paint_player_over_iris(state, &mut p, pg);  // keep the player ON TOP of the fade
                (Some(p), pg)
            }
            (None, pg) => (None, pg),
        };
        if let Some(ref p) = planes {
            on_frame(p, page);
        }

        // [asm 32B0-32C1] accel every 0x14
        let c2 = state.rw(0x2DC2).wrapping_add(1) & 0xFFFF;
        let c2 = if c2 >= 0x14 { 0 } else { c2 };
        let mut step = state.rw(0x2DC0);
        if c2 == 0 {
            step = step.wrapping_add(1) & 0xFFFF;
            state.ww(0x2DC0, step);
        }
        state.ww(0x2DC2, c2);

        // [asm 32D1] radius -= step
        let radius = state.rw(0x2DD0).wrapping_sub(step) & 0xFFFF;
        state.ww(0x2DD0, radius);
    }
    state.ww(0x2DD0, 0);  // iris closed -> off
}

// ── _emit_sfx ──────────────────────────────────────────────────────────────────────

fn emit_sfx(state: &mut NativeGameState, idx: u8) {
    // audio is not gameplay — queue ONE sfx index, panned to the player
    let px = crate::engine::audio::player_sfx_x(state);
    crate::engine::audio::native_play_sfx(state, idx, Some(px));
}

// ── _exit_anim_refill ───────────────────────────────────────────────────────────────

/// [asm 4EBA-4F04] Spawn one queued food item into a free effect slot at (0x9B, 0).
/// Returns true iff a queued item was found (whether or not a free slot was available).
fn exit_anim_refill(state: &mut NativeGameState) -> bool {
    // the [0x6C12] collected-item queue (byte counts)
    let mut di: Option<usize> = None;
    for i in 0..0x71 {
        if state.rb(ITEM_QUEUE + i) != 0 {
            di = Some(i);
            break;
        }
    }
    let di = match di {
        None => return false,  // the queue is empty
        Some(d) => d,
    };
    for k in 0..0x14 {
        let base = EFFECT_SLOTS + k * SLOT_STRIDE;
        if state.rw(base + 4) == 0xFFFF {  // a free slot (sprite id 0xFFFF)
            state.ww(base, 0x9B);           // x = 0x9B
            state.ww(base + 2, 0);          // y = 0
            state.ww(base + 4, ((di + 0x6E) & 0xFFFF) as u16);  // sprite = di + 0x6E
            state.ww(base + 0x0E, 0);       // yvel = 0
            // [asm 4EFF] dequeue
            let q = state.rb(ITEM_QUEUE + di).wrapping_sub(1);
            state.wb(ITEM_QUEUE + di, q);
            return true;
        }
    }
    true  // found an item but the pool is full
}

// ── tally scene ────────────────────────────────────────────────────────────────────

const DIGIT_FONT_OFF: usize = 0x1C70;
const DIGIT_SZ: usize = 0x58;
const DIR_OFFSETS: usize = 0x5F48;
const DIR_SEGMENTS: usize = 0x62E8;
const GLYPH_HDR: usize = 0x16;
const GLYPH_LEN: usize = 0x58;
const LABEL_CHARS: &str = "SCORELVMP TD";
const PCT_GLYPH_IDX: usize = 0x1A;

fn r16(state: &NativeGameState, seg: usize, off: usize) -> u16 {
    let a = ((seg << 4) + (off & 0xFFFF)) & 0xFFFFF;
    (state.data[a] as u16) | ((state.data[(a + 1) & 0xFFFFF] as u16) << 8)
}

fn letter_glyph(state: &NativeGameState, dir_index: usize) -> Vec<u8> {
    let off = r16(state, 0x1A0F, DIR_OFFSETS + dir_index * 2) as usize;
    let seg = r16(state, 0x1A0F, DIR_SEGMENTS + dir_index * 2) as usize;
    let base = ((seg << 4) + ((off + GLYPH_HDR) & 0xFFFF)) & 0xFFFFF;
    let end = (base + GLYPH_LEN).min(state.data.len());
    state.data[base..end].to_vec()
}

/// Read the tally panel inputs (score, completion counters) + resolve the glyph fonts.
pub fn read_tally_panel(state: &NativeGameState) -> (u32, u32, Vec<u8>, HashMap<char, Vec<u8>>, Vec<u8>) {
    let score = r16(state, 0x1A0F, SCORE_LO) as u32 | ((r16(state, 0x1A0F, SCORE_HI) as u32) << 16);
    let percent = compute_percent(
        r16(state, 0x1A0F, 0x2A74),
        r16(state, 0x1A0F, 0x2A76),
        r16(state, 0x1A0F, 0x2A78),
        r16(state, 0x1A0F, 0x2A7A),
    );
    let font_seg = r16(state, 0x1A0F, 0x3D) as usize;
    let fbase = ((font_seg << 4) + DIGIT_FONT_OFF) & 0xFFFFF;
    let fend = (fbase + 10 * DIGIT_SZ).min(state.data.len());
    let digit_font = state.data[fbase..fend].to_vec();

    let mut letters = HashMap::new();
    for ch in LABEL_CHARS.chars() {
        if ch != ' ' {
            let idx = ((ch as usize) - 0x41 + 0xF1) & 0xFFFF;
            letters.insert(ch, letter_glyph(state, idx));
        }
    }
    let pct_glyph = letter_glyph(state, (PCT_GLYPH_IDX + 0xF1) & 0xFFFF);
    (score, percent, digit_font, letters, pct_glyph)
}

/// Compose the tally scene: black background + object overlay + the recovered text panel.
fn build_tally_scene(
    state: &mut NativeGameState,
    _dos: &mut NativeVGA,
    _display_page: usize,
    _game_root: &str,
    page: usize,
) -> ([Vec<u8>; 4], usize) {
    let mut planes: [Vec<u8>; 4] = [
        vec![0u8; PLANE_LEN],
        vec![0u8; PLANE_LEN],
        vec![0u8; PLANE_LEN],
        vec![0u8; PLANE_LEN],
    ];

    // ── object overlay: walk the active sprite list and draw each ──────────────
    let mut cam = read_camera(state, false);
    cam.dest_page = page & 0xFFFF;
    let mut off = object_render::LIST_TOP;
    while off > object_render::LIST_BASE {
        off -= SLOT_STRIDE;
        let sprite_id = state.rw(off + 4);
        if sprite_id != 0xFFFF {
            let spr = read_sprite(state, off);
            let attr = read_attr(state, sprite_id);
            if let Some(draw) = object_render::plan_sprite(&spr, &attr, &cam) {
                let src_len = (draw.src_bw as usize) * (draw.full_rows as usize) * 6 + 64;
                let src = read_source(state, draw.src_seg, draw.src_off, src_len);
                object_render::paint_sprite(&mut planes, &draw, &src, cam.row_stride);
            }
        }
    }

    // ── tally panel: SCORE + LEVEL COMPLETED % ─────────────────────────────────
    let (score, percent, digit_font, letters, pct_glyph) = read_tally_panel(state);
    render_tally_panel(&mut planes, score, percent, page, &digit_font, &letters, &pct_glyph);

    (planes, page)
}

// ── native_exit_anim ───────────────────────────────────────────────────────────────

/// The animated level-end TALLY cutscene [asm 4CEA..4F53] — runs AFTER native_iris_close,
/// on black. A callback-based generator: `on_frame` is called once per frame with
/// `(Option<planes>, page)`. When `state_only` is true, the tally BLIT is skipped
/// (the tick-demo verifier drains state mutations only).
pub fn native_exit_anim<F: FnMut(Option<&[Vec<u8>; 4]>, usize)>(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    display_page: usize,
    game_root: &str,
    state_only: bool,
    mut on_frame: F,
) {
    let page = display_page & 0xFFFF;

    // [asm 316F->318B-31AA] clear the object/effect slots for the tally (0x73 slots from 0x4F2E).
    for _k in 0..0x73 {
        let off = (0x4F2E + _k * SLOT_STRIDE) & 0xFFFF;
        state.ww(off + 4, 0xFFFF);  // the effect-slot arena: free the id
    }

    // [asm 4CEA-4D1A] recenter the player to screen space, zero the camera + velocities
    let cam_row = state.rw(CAM_ROW);
    let pv_y = state.rw(PLAYER_BASE + 2);
    state.ww(PLAYER_BASE + 2, pv_y.wrapping_sub(cam_row << 4) & 0xFFFF);

    let cam_col = state.rw(CAM_COL);
    let pv_x = state.rw(PLAYER_BASE);
    state.ww(PLAYER_BASE, pv_x.wrapping_sub(cam_col << 4) & 0xFFFF);

    state.ww(CAM_COL, 0);
    state.ww(CAM_ROW, 0);
    state.ww(PLAYER_BASE + 9, 0);   // pv.facing = 0
    state.ww(PLAYER_BASE + 6, 0);   // pv.xvel = 0
    state.wb(GLIDER, 0);            // g.glider = 0
    state.ww(SLOT0_SPRITE, 0xFFFF); // free weapon slot
    state.ww(PLAYER_BASE + 0x0C, 0x7BA7);  // pv.anim_ptr = walk anim

    // The inner frame function — steps the player walk animation + the flame + frame counter
    let mut frame_step = |state: &mut NativeGameState| -> (Option<[Vec<u8>; 4]>, usize) {
        // [asm 638B] step the player walk animation
        let ap = state.rw(PLAYER_BASE + 0x0C);
        let facing_lo = state.rb(PLAYER_BASE + 9);
        let (fr, nptr, bcf) = player_advance_anim(state, ap, facing_lo);
        state.ww(PLAYER_BASE + 4, fr);       // pv.sprite = fr
        state.ww(PLAYER_BASE + 0x0C, nptr);  // pv.anim_ptr = nptr
        state.wb(ANIM_HI, bcf);              // g.anim_hi = bcf

        // [asm 51F0] the pot-FLAME animation — every 4th frame advances the flame sprite id
        let blink = state.rb(FRAME_TIMER);
        if blink & 3 == 0 {
            let fid = state.rw(0x4F56) & 0x1FFF;  // [0x4F56] the pot-flame sprite_id (projectiles[2].sprite)
            if (0x68..0x6E).contains(&fid) {
                let next = if fid + 1 >= 0x6E { 0x68 } else { fid + 1 };
                state.ww(0x4F56, next);  // flame.sprite = next
            }
        }

        // [asm 26FA:2708] inc word [0x6bd5] — the free-running frame counter
        let frame_stamp = state.rw(FRAME_TIMER);
        state.ww(FRAME_TIMER, frame_stamp.wrapping_add(1) & 0xFFFF);

        // native_sync_render_state — re-derive the tile-ring indices
        render::native_sync_render_state(state);
        crate::engine::front_end::poll_fe_input(state);

        if state_only {
            return (None, page);  // skip the tally BLIT
        }
        let (planes, pg) = build_tally_scene(state, dos, display_page, game_root, page);
        (Some(planes), pg)
    };

    // [asm 4D3E-4D8E] the player walks in to (0x3C, 0xAF)
    loop {
        let mut moved = false;
        // X
        let cur_x = state.rw(PLAYER_BASE);
        let diff_x = s16(cur_x.wrapping_sub(0x3C));
        if diff_x.abs() >= 2 {
            let new_x = if diff_x > 0 { cur_x.wrapping_sub(2) } else { cur_x.wrapping_add(2) };
            state.ww(PLAYER_BASE, new_x);
            moved = true;
        }
        // Y
        let cur_y = state.rw(PLAYER_BASE + 2);
        let diff_y = s16(cur_y.wrapping_sub(0xAF));
        if diff_y.abs() >= 2 {
            let new_y = if diff_y > 0 { cur_y.wrapping_sub(2) } else { cur_y.wrapping_add(2) };
            state.ww(PLAYER_BASE + 2, new_y);
            moved = true;
        }
        if !moved {
            break;
        }
        let (planes, pg) = frame_step(state);
        on_frame(planes.as_ref(), pg);
    }

    // [asm 4D8E-4DE9] 3 food sprites slide in from the right (X=0x168) to X<=0x9B
    let foods: [(u16, u16); 3] = [(0xAF, 0x64), (0x94, 0x62), (0x9B, 0x68)];
    for (i, (y, sid)) in foods.iter().enumerate() {
        let base = PROJECTILE_SLOTS + i * SLOT_STRIDE;
        state.ww(base, 0x168);     // x = 0x168
        state.ww(base + 2, *y);   // y
        state.ww(base + 4, *sid); // sprite
    }
    while s16(state.rw(PROJECTILE_SLOTS)) > 0x9B {
        for i in 0..3 {
            let base = PROJECTILE_SLOTS + i * SLOT_STRIDE;
            let x = state.rw(base);
            state.ww(base, x.wrapping_sub(3) & 0xFFFF);
        }
        let (planes, pg) = frame_step(state);
        on_frame(planes.as_ref(), pg);
    }

    // [asm 4DF5-4F0B] the food-throw score COUNT-UP
    let count_up_recovered = true;
    if count_up_recovered && state.rw(ITEM_TOTAL) != 0 {
        // [asm 4DF5-4E30] the player throws; recoil velocity decays
        state.ww(PLAYER_BASE + 0x0C, 0x7C6B);  // pv.anim_ptr = throw anim
        state.ww(PLAYER_BASE + 6, 0x40);      // pv.xvel = 0x40
        state.wb(PLAYER_BASE + 8, 2);         // pv.motion_mode = 2

        while state.rw(PLAYER_BASE + 6) != 0 {
            // [asm 4E1F-4E2A] X += Xvel>>4
            let px = state.rw(PLAYER_BASE) as i32;
            let xv = state.rw(PLAYER_BASE + 6) as i16 as i32;
            state.ww(PLAYER_BASE, ((px + (xv >> 4)) & 0xFFFF) as u16);

            // [asm 6333] friction
            let mode = state.rb(PLAYER_BASE + 8);
            let friction_val = 0xCu32 >> mode;
            let xv_s = state.rw(PLAYER_BASE + 6) as i16 as i32;
            let v = xv_s.abs() - friction_val as i32;
            let v = if v < 0 { 0 } else if xv_s < 0 { -v } else { v };
            state.ww(PLAYER_BASE + 6, (v & 0xFFFF) as u16);

            let (planes, pg) = frame_step(state);
            on_frame(planes.as_ref(), pg);
        }

        // [asm 4E32-4F0B] the food falls into the pot; each item adds its value
        state.ww(PLAYER_BASE + 0x0C, 0x7CAF);  // pv.anim_ptr = throw/count-up anim
        loop {
            let (planes, pg) = frame_step(state);  // [asm 4E3A-4E50] render
            on_frame(planes.as_ref(), pg);

            let mut alive = false;
            for k in 0..0x14 {  // [asm 4E53-4EAE] the 20 effect slots [0x52E8]
                let base = (EFFECT_SLOTS + k * SLOT_STRIDE) & 0xFFFF;
                let sprite = state.rw(base + 4);
                if sprite == 0xFFFF {
                    continue;
                }
                alive = true;  // [asm 4E7A] any non-empty slot
                let mut vel = state.rw(base + 0x0E);  // yvel
                if (vel as i16) < 0x80 {
                    vel = (vel.wrapping_add(8)) & 0xFFFF;
                    state.ww(base + 0x0E, vel);  // accelerate the fall
                }
                // [asm 4E6F-4E77] Y += vel>>4
                let py = state.rw(base + 2) as i32;
                let vel_s = vel as i16 as i32;
                state.ww(base + 2, ((py + (vel_s >> 4)) & 0xFFFF) as u16);

                if s16(state.rw(base + 2)) < 0x91 {
                    continue;  // not at the pot line yet -> keep falling
                }
                // [asm 4E82-4E93] value = tables[food type]
                let sprite_id = state.rw(base + 4) & 0x1FFF;
                let t = (sprite_id.wrapping_sub(0x6E)) & 0xFFFF;
                let vi = ByteTable::read(state, FOOD_INDEX_TABLE.wrapping_neg(), t as usize);
                let val = WordTable::read(state, FOOD_SCORE_TABLE.wrapping_neg(), 2 * vi as usize);

                // [asm 4E97-4E9B] score += value
                let sc = state.rw(SCORE_LO) as u32 | ((state.rw(SCORE_HI) as u32) << 16);
                let sc = sc.wrapping_add(val as u32);
                state.ww(SCORE_LO, (sc & 0xFFFF) as u16);
                state.ww(SCORE_HI, ((sc >> 16) & 0xFFFF) as u16);

                state.ww(base + 4, 0xFFFF);  // free the slot
                emit_sfx(state, 8);          // [asm 4EA5-4EA8] sfx 8
            }

            // [asm 4EB0] a refill frame (every 8)
            if state.rb(FRAME_TIMER) & 7 == 0 {
                if exit_anim_refill(state) {  // spawned a queued item -> continue
                    continue;
                }
                if !alive {  // queue empty AND no slot falling -> done
                    break;
                }
            }
        }
    }

    // [asm 4F0E-4F52] the player + the food walk off to the left
    state.ww(PLAYER_BASE + 0x0C, 0x7BA7);  // pv.anim_ptr = walk anim
    while s16(state.rw(PROJECTILE_SLOTS)) > -0x34 {
        for i in 0..3 {
            let base = PROJECTILE_SLOTS + i * SLOT_STRIDE;
            let x = state.rw(base);
            state.ww(base, x.wrapping_sub(2) & 0xFFFF);
        }
        let proj_x = s16(state.rw(PROJECTILE_SLOTS));
        let pv_x = state.rw(PLAYER_BASE);
        let new_x = if proj_x < 0 { pv_x.wrapping_add(3) } else { pv_x.wrapping_add(2) } & 0xFFFF;
        state.ww(PLAYER_BASE, new_x);

        let (planes, pg) = frame_step(state);
        on_frame(planes.as_ref(), pg);
    }
}
