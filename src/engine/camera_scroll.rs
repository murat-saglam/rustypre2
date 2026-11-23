// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 camera-follow controller — 1030:5643.
//!
//! Keeps the camera tracking the player each frame.

use crate::engine::state::NativeGameState;
use crate::recovered::frame_renderer::calc_scroll_source;

const CS_BASE: usize = 0x1030 << 4;
pub const SCROLL_DONE_FLAG: usize = 0x6771;
const SPEED_TABLE: usize = 0x78C6;

#[inline]
fn s16(v: i32) -> i32 { let v = v & 0xFFFF; if v & 0x8000 != 0 { v - 0x10000 } else { v } }

#[inline]
fn sar16(v: u16, n: u32) -> i32 {
    (s16(v as i32) >> n) & 0xFFFF
}

fn rb_cs(state: &NativeGameState, off: usize) -> u8 {
    state.data[(CS_BASE + (off & 0xFFFF)) & 0xFFFFF]
}

fn wb_cs(state: &mut NativeGameState, off: usize, val: u8) {
    let addr = (CS_BASE + (off & 0xFFFF)) & 0xFFFFF;
    state.data[addr] = val;
}

/// Vertical scroll down (1030:33AD). Returns true if scrolled, false at bottom.
pub fn v_scroll_down(state: &mut NativeGameState, dl: u8) -> bool {
    let map_rows = state.rb(0x2CF5) as i32;
    let maxy = (map_rows - 0xB) & 0xFFFF;
    if s16(state.rw(0x2DE6) as i32) >= s16(maxy as i32) {
        return false;
    }
    // scroll_anim_ctr += 1
    let ctr = state.rb(0x2DF5);
    state.wb(0x2DF5, ctr.wrapping_add(1));

    // sub-tile accumulator += dl
    let acc = state.rb(0x6BC4).wrapping_add(dl);
    if acc < 0x10 {
        state.wb(0x6BC4, acc);
        return true;
    }
    // carry: camera cell += 1
    let acc = acc - 0x10;
    let cy = state.rw(0x2DE6).wrapping_add(1);
    state.ww(0x2DE6, cy);
    let acc = if cy as i32 >= s16(maxy as i32) { 0u8 } else { acc };
    state.wb(0x6BC4, acc);

    // row_ring wrap
    let mut dea = state.rw(0x2DEA).wrapping_add(1);
    if dea >= 0xC { dea = 0; }
    state.ww(0x2DEA, dea);

    // recompute scroll_copy_src
    let col = state.rb(0x2DE8) as usize;
    let scroll_src = calc_scroll_source(col, dea as usize);
    state.ww(0x2DBA, scroll_src as u16);
    true
}

/// Vertical scroll up (1030:3363). Returns true if scrolled, false at top.
pub fn v_scroll_up(state: &mut NativeGameState, dl: u8) -> bool {
    if state.rw(0x2DE6) == 0 {
        return false;
    }

    let ctr = state.rb(0x2DF5);
    state.wb(0x2DF5, ctr.wrapping_add(1));

    let fine = state.rb(0x6BC4) as i32;
    let raw = fine - (dl as i32);
    if raw >= 0 {
        state.wb(0x6BC4, (raw & 0xFF) as u8);
        return true;
    }
    // borrow: camera cell -= 1
    state.wb(0x6BC4, ((raw + 0x10).max(0) & 0xFF) as u8);
    let cy = state.rw(0x2DE6).wrapping_sub(1);
    state.ww(0x2DE6, cy);

    let mut dea = (state.rw(0x2DEA) as i16 as i32) - 1;
    if dea < 0 { dea = 0xB; }
    state.ww(0x2DEA, dea as u16);

    let col = state.rb(0x2DE8) as usize;
    let scroll_src = calc_scroll_source(col, dea as usize);
    state.ww(0x2DBA, scroll_src as u16);
    true
}

/// Horizontal scroll direction init (1030:57F6).
fn h_init(state: &mut NativeGameState, dxs: i32) {
    let xvel = state.rw(0x4F22);
    let move_flag = state.rb(0x4F23);

    let right = if xvel != 0 && move_flag & 0x80 != 0 {
        false
    } else if xvel != 0 {
        true
    } else if dxs >= 0xA {
        true
    } else {
        false
    };

    if right {
        if dxs >= 0x10 { state.wb(0x6BED, 1); }
    } else {
        if dxs <= 4 { state.wb(0x6BED, 2); }
    }
}

/// Apply camera pan right (1030:3435). Returns true if panned, false at right edge.
pub fn apply_camera_pan_right(state: &mut NativeGameState) -> bool {
    crate::views::camera_pan::apply_camera_pan(
        state,
        crate::views::camera_pan::PanDirection::Right,
    )
}

/// Apply camera pan left (1030:3414). Returns true if panned, false at left edge.
pub fn apply_camera_pan_left(state: &mut NativeGameState) -> bool {
    crate::views::camera_pan::apply_camera_pan(
        state,
        crate::views::camera_pan::PanDirection::Left,
    )
}

/// Horizontal camera follow (1030:57A8).
fn h_follow(state: &mut NativeGameState) {
    let px = state.rw(0x4F1C) as i32;
    let cam_col = state.rw(0x2DE4) as i32;
    let dx = (sar16(px as u16, 4) - cam_col) & 0xFFFF;
    let dxs = s16(dx);

    let unk_6bfe = state.rb(0x6BFE);
    let xvel = state.rw(0x4F22);

    if dx < 0x14 && unk_6bfe == 0 && xvel == 0 {
        state.wb(0x6BED, 0);
        return;
    }
    let bed = state.rb(0x6BED);
    if bed == 0 {
        h_init(state, dxs);
    } else if bed == 1 {
        if dxs <= 5 || !apply_camera_pan_right(state) {
            state.wb(0x6BED, 0);
        }
    } else {
        if dxs >= 0xF || !apply_camera_pan_left(state) {
            state.wb(0x6BED, 0);
        }
    }
}

/// Vertical scroll speed from the [0x78C6] curve (1030:5738).
fn v_speed(state: &mut NativeGameState, down: bool) -> u8 {
    if rb_cs(state, SCROLL_DONE_FLAG) != 0 {
        return 0x10;
    }
    let py = state.rw(0x4F1E) as i32;
    let fine = state.rb(0x6BC4) as i32;
    let cam_row = state.rw(0x2DE6) as i32;
    let target = state.rw(0x6BF1) as i32;
    let ay = (py - fine) & 0xFFFF;
    let cell = ((cam_row + target) << 4) & 0xFFFF;
    let raw_bx = if down {
        (ay - cell) & 0xFFFF
    } else {
        (cell - ay) & 0xFFFF
    };
    let grid_dirty = state.rb(0x2DF4);
    if grid_dirty == 0 {
        let bx = if raw_bx >= 0x8000 { 0usize } else { (raw_bx as usize).min(263) };
        state.rb((SPEED_TABLE + bx) & 0xFFFF)
    } else {
        let ptr = state.rw(0x78C4) as usize;
        let max_idx = if ptr == 0x794A { 131usize } else { 263usize };
        let bx = if raw_bx >= 0x8000 { 0usize } else { (raw_bx as usize).min(max_idx) };
        state.rb((ptr + bx) & 0xFFFF)
    }
}

/// Scroll the camera one step toward the target row (1030:56EA).
fn v_scroll_apply(state: &mut NativeGameState, dxs: i32) {
    let py_u16 = state.rw(0x4F1E);
    let prop_header = state.rw(0x815E);
    let top_px = prop_header.wrapping_add(0xB).wrapping_shl(4);
    let cam_row_u16 = state.rw(0x2DE6);

    // [56EA-5701] level-edge pre-check: unsigned `jbe` on `py_u16 <= top_px`
    if py_u16 <= top_px && s16(prop_header as i32) < s16((cam_row_u16.wrapping_sub(1)) as i32) {
        let speed = v_speed(state, false);
        if !v_scroll_up(state, speed) {
            state.wb(0x6BEE, 0);
        }
        return;
    }

    if state.rb(0x6BEE) == 0 { return; }

    let tgt = state.rw(0x6BF1) as i32;
    if tgt == (dxs & 0xFFFF) {
        state.wb(0x6BEE, 0);
        return;
    }
    if s16(tgt) > dxs {
        let speed = v_speed(state, false);
        if !v_scroll_up(state, speed) {
            state.wb(0x6BEE, 0);
        }
        return;
    }
    // [5718] scroll down toward the target (`jbe` unsigned on `py_u16 <= top_px`, `ja` unsigned on `cam_row_u16 > prop_header`)
    if py_u16 <= top_px && cam_row_u16 > prop_header {
        state.wb(0x6BEE, 0);
        return;
    }
    let speed = v_speed(state, true);
    if !v_scroll_down(state, speed) {
        state.wb(0x6BEE, 0);
    }
}

/// Vertical camera follow (1030:5663).
fn v_follow(state: &mut NativeGameState) {
    if rb_cs(state, SCROLL_DONE_FLAG) == 0 {
        let level_flags = state.rb(0x8166);
        if level_flags & 4 != 0 {
            v_scroll_down(state, 1);
            return;
        }
    }

    let py = state.rw(0x4F1E) as i32;
    let yvel = state.rw(0x4F2A) as i32;
    let cam_row = state.rw(0x2DE6) as i32;
    let dxs = s16((sar16(py as u16, 4) - cam_row) & 0xFFFF);

    let yvel_zero = state.rw(0x4F2A) == 0;
    let level_flags = state.rb(0x8166);
    let is_demo = state.rb(0x2879) != 0;

    if is_demo {
        if yvel_zero {
            state.wb(0x6BEE, 0);
        }
    } else if yvel_zero {
        // In live play, only clear the active vertical scroll flag if we are NOT in the middle
        // of re-centering the camera upward (target > dxs) or downward (target < dxs) after a jump/fall.
        let cur_tgt = s16(state.rw(0x6BF1) as i32);
        if state.rb(0x6BEE) == 0 || cur_tgt == dxs || (dxs >= 6 && dxs <= 8) {
            state.wb(0x6BEE, 0);
        }
    }

    let mut target: Option<i32> = None;
    if yvel != 0 {
        if dxs >= 9 { target = Some(if is_demo { 3 } else { 5 }); }
        else if dxs <= 2 { target = Some(8); }
    } else if level_flags & 1 != 0 {
        if dxs >= 8 { target = Some(7); }
        else if dxs <= 5 { target = Some(6); }
    } else if is_demo {
        if dxs >= 0xA { target = Some(9); }
        else if dxs <= 3 { target = Some(8); }
    } else {
        // Live gameplay: keep the player comfortably framed in rows 6..8 so the camera
        // never lags below the character when climbing platforms or landing from jumps.
        if dxs >= 9 { target = Some(8); }
        else if dxs <= 5 { target = Some(7); }
    }

    if let Some(t) = target {
        state.ww(0x6BF1, t as u16);
        let idle = state.rb(0x6BEE).wrapping_add(1);
        state.wb(0x6BEE, idle);
    }

    if state.rb(0x6BEE) == 0 {
        return;
    }

    v_scroll_apply(state, dxs);
}

/// The whole per-frame camera-follow (1030:5643).
pub fn native_camera_follow(state: &mut NativeGameState) {
    wb_cs(state, SCROLL_DONE_FLAG, 0);

    let unk_6bd9 = state.rb(0x6BD9);
    if unk_6bd9 != 0 { return; }

    let level_flags = state.rb(0x8166);
    if level_flags & 2 == 0 {
        h_follow(state);
    }
    v_follow(state);
}

/// [asm 3af2] Level-start camera-init: reset camera cells, then snap-scroll to player.
/// Initialises the background scroll ring (`1030:3AF2`).
pub fn native_3af2(state: &mut NativeGameState) {
    // [asm 3afe-3b07] reset camera cells
    state.ww(0x2DE4, 0);  // cam_col_word
    state.ww(0x2DE6, 0);  // cam_row_word
    state.ww(0x2DE8, 0);  // col_ring
    state.ww(0x2DEA, 0);  // row_ring

    // [asm 3b35] 5634: snap-scroll to convergence (max 8000 iterations)
    for _ in 0..8000 {
        wb_cs(state, SCROLL_DONE_FLAG, 1);  // [asm 5634] set snap flag

        let unk_6bd9 = state.rb(0x6BD9);
        let level_flags = state.rb(0x8166);
        if unk_6bd9 == 0 && level_flags & 2 == 0 {
            h_follow(state);  // [asm 565c] 57a8 horizontal follow
        }
        v_follow(state);  // [asm 565f] 5663 vertical follow

        wb_cs(state, SCROLL_DONE_FLAG, 0);  // [asm 563d] clear snap flag

        // [asm 3b38] both axes idle -> converged
        let scroll_dir = state.rb(0x6BED);
        let cam_scroll_idle = state.rb(0x6BEE);
        if scroll_dir == 0 && cam_scroll_idle == 0 {
            break;
        }
    }

    // [asm 3b41-3b5b] still > 0xc cols right -> centre it (pan right x10)
    let level_flags = state.rb(0x8166);
    if level_flags & 2 == 0 {
        let px = state.rw(0x4F1C) as i32;
        let px_tile = (s16(px) >> 4) & 0xFFFF;
        let cam_x = state.rw(0x2DE4) as i32;
        if ((px_tile - cam_x) & 0xFFFF) >= 0xC {
            for _ in 0..0xA {
                apply_camera_pan_right(state);
            }
        }
    }

    // [asm 3b60-3b65] grid_dirty + sentinel
    state.wb(0x2DF4, 1);  // grid_dirty
    state.ww(0x2DE0, 0x55AA);  // grid_dirty_token
}
