// SPDX-License-Identifier: GPL-3.0-only
//! Effect/particle projection island — 1030:8922.
//!
//! Once per frame the main loop (call site ~1030:0235) walks a fixed 70-entry
//! effect/particle list at DS:0x8F1D (7 bytes each) and projects every entry that is
//! currently on-screen into the render-slot array at DS:0x52E8 (up to 20 slots,
//! 0x12 bytes each). Each projected entry also gets a small vertical "bounce"
//! animation: a per-entry counter ([+6]) ping-pongs 0,1,2,3 -> -3,-2,-1,0 and is
//! added to the world Y so the effect floats up and down.
//!
//! Source entry (DS:0x8F1D, stride 7):
//!     [+0] word  world X
//!     [+2] word  world Y      (advanced in place by the bounce each frame)
//!     [+4] word  sprite id    (0xFFFF = empty slot)
//!     [+6] byte  bounce counter (signed, ping-pong)
//!
//! Render slot (DS:0x52E8, stride 0x12) — only these fields are written here:
//!     [+0] word  screen-space X (= source X, raw)
//!     [+2] word  Y (after bounce)
//!     [+4] word  sprite id  (0xFFFF = slot unused)
//!     [+9] word  back-reference to the source entry offset
//!
//! Pure: reads/writes happen through the NativeGameState DS accessors; no VM, no planes.
//! The animation is gated off when [0x6BD5] & 1 (a global "freeze effects" flag, e.g.
//! during pause/scripted pose).

use crate::engine::state::NativeGameState;
use std::sync::atomic::{AtomicI32, Ordering};

const SRC_LIST: usize = 0x8F1D;
const SRC_STRIDE: usize = 7;
const SRC_COUNT: usize = 0x46; // 70

const DST_SLOTS: usize = 0x52E8;
const DST_STRIDE: usize = 0x12;
const DST_COUNT: usize = 0x14; // 20

// screen-window culling bounds (inclusive), in >>4 (tile) units after camera subtract
const WIN_X: i32 = 0x16;
const WIN_Y: i32 = 0x2B;

const CAM_COL: usize = 0x2DE4; // [0x2DE4] tile camera column (word)
const CAM_ROW: usize = 0x2DE6; // [0x2DE6] tile camera row (word)
const FRAME_BLINK: usize = 0x6BD5; // [0x6BD5] frame counter; bit0 gates the bounce

const DEAD: u16 = 0xFFFF; // empty source entry / unused render slot

static ITEM_ZONE_X_LEFT: AtomicI32 = AtomicI32::new(0);
static ITEM_ZONE_X_RIGHT: AtomicI32 = AtomicI32::new(0);
static ITEM_ZONE_Y_TOP: AtomicI32 = AtomicI32::new(0);
static ITEM_ZONE_Y_BOTTOM: AtomicI32 = AtomicI32::new(0);

/// Sign-extend a word to i32.
#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 {
        v - 0x10000
    } else {
        v
    }
}

/// Sign-extend a byte to i32.
#[inline]
fn s8(v: i32) -> i32 {
    let v = v & 0xFF;
    if v & 0x80 != 0 {
        v - 0x100
    } else {
        v
    }
}

/// Mutable projection loop state shared across the two passes.
struct ProjState {
    di: usize,     // current render-slot DS offset
    bx: i32,       // remaining slot budget
    filled_all: bool,
}

/// [asm 8955..8989] Project one source entry into the current render slot and advance the
/// slot cursor. Animates the bounce counter, writes the new Y back to the source, and fills
/// the render-slot fields (X / Y / sprite / back-ref).
fn project_one(state: &mut NativeGameState, ps: &mut ProjState, si: usize, no_anim: bool) {
    // [asm 8955] on-screen: copy raw X into the render slot
    state.ww(ps.di, state.rw(si));

    // [asm 8959..8974] bounce animation -> ax (added to Y)
    let mut ax: i32 = 0;
    if !no_anim {
        let al = state.rb(si + 6) as i32; // bounce counter (signed byte)
        ax = (s8(al) + 1) & 0xFFFF; // cbw ; inc ax
        state.wb(si + 6, (ax & 0xFF) as u8);
        if s8(ax & 0xFF) >= 4 {
            // cmp al,4 ; jl skips
            ax = (-ax + 1) & 0xFFFF; // neg ax ; inc ax
            state.wb(si + 6, (ax & 0xFF) as u8);
        }
    }

    // [asm 8974] new Y written back to the source AND into the slot
    let new_y = ((ax + s16(state.rw(si + 2) as i32)) & 0xFFFF) as u16;
    state.ww(si + 2, new_y);
    state.ww(ps.di + 2, new_y);
    state.ww(ps.di + 4, state.rw(si + 4)); // [asm 897D] sprite id
    state.ww(ps.di + 9, si as u16); // ... + back-reference to the source entry

    ps.di += DST_STRIDE;
    ps.bx -= 1;
    if ps.bx == 0 {
        ps.filled_all = true; // [asm 8989] all slots filled -> done, no clearing
    }
}

/// One sweep over the source list; `faithful` selects [0,WIN_X] vs margin-only. Returns
/// `true` if the slot budget ran out (caller should stop).
fn run_pass(
    state: &mut NativeGameState,
    ps: &mut ProjState,
    no_anim: bool,
    cam_x: i32,
    cam_y: i32,
    faithful: bool,
    zxl: i32,
    zxr: i32,
    zyt: i32,
    zyb: i32,
) -> bool {
    for k in 0..SRC_COUNT {
        if ps.filled_all {
            return true;
        }
        let si = SRC_LIST + k * SRC_STRIDE;

        if state.rw(si + 4) == DEAD {
            // [asm 8930] empty source entry
            continue;
        }

        // [asm 8936] screen-X cull: ax = (X >> 4) - cam_x ; jb / jg out of window (widened
        // each side by the widescreen item zone; m=0 is byte-exact with the ASM: sxt<0 ==
        // the jb 'axb<cam_x', sxt>WIN_X == the jg)
        let axb = (s16(state.rw(si) as i32) >> 4) & 0xFFFF;
        let sxt = s16((axb - cam_x) & 0xFFFF);
        if faithful {
            if sxt < 0 || sxt > WIN_X {
                continue;
            }
        } else {
            if (0 <= sxt && sxt <= WIN_X) || sxt < -zxl || sxt > WIN_X + zxr {
                continue;
            }
        }

        // [asm 8945] screen-Y cull (top/bottom widened by the smooth-camera vertical item
        // zone; [0,0] is byte-exact: sy<0 == the 'jb ayb<cam_y', sy>WIN_Y == the jg)
        let ayb = (s16(state.rw(si + 2) as i32) >> 4) & 0xFFFF;
        let sy = s16((ayb - cam_y) & 0xFFFF);
        if sy < -zyt || sy > WIN_Y + zyb {
            continue;
        }

        project_one(state, ps, si, no_anim);
    }
    ps.filled_all
}

/// [asm 8922] Animate + project the on-screen effect list into the render slots.
///
/// Walks the 70-entry list at DS:0x8F1D (X/Y/sprite/bounce, stride 7), culls each vs camera
/// `[0x2DE4]`/`[0x2DE6]` to the on-screen window (<=0x16 x, <=0x2B y), animates the per-entry
/// bounce counter [+6] (cbw+1 ping-pong 0..3 -> -3..0, gated off by `[0x6BD5]`&1) into the
/// world Y [+2], and projects the on-screen ones into the render slots at DS:0x52E8
/// (X/Y/sprite/back-ref, stride 0x12, max 20); unused tail slots get sprite-id 0xFFFF.
///
/// Two X-culled PASSES, not one: items within the FAITHFUL window (`0 <= sxt <= WIN_X` —
/// what the ORIGINAL game already shows, at most 20 of them by the original's own design)
/// always get a slot first; only once those are all placed does a second pass spend any
/// REMAINING slots on widescreen-MARGIN-only items (`sxt` outside [0, WIN_X] but inside the
/// zone widened by `set_item_zone_margins`). At the faithful zone ([0, 0]) the margin-only
/// set is empty by construction, so this is a no-op — byte-identical single pass, in list
/// order, exactly as the ASM's own one-pass loop. But once a dense item cluster sits in the
/// WIDESCREEN margin, a single list-order pass could let it exhaust the 20-slot budget
/// before reaching a later, faithfully-visible item further down the list — e.g. LEVEL1's
/// exit semaphore (list index 69, the very last entry) losing its slot to margin-only
/// clutter near the player. Prioritizing the faithful set first means an item the ORIGINAL
/// game would show is never starved by one only the widescreen enhancement added.
pub fn project_particles(state: &mut NativeGameState) {
    let cam_x = state.rw(CAM_COL) as i32;
    let cam_y = state.rw(CAM_ROW) as i32;
    let no_anim = (state.rb(FRAME_BLINK) as i32 & 1) != 0;

    let zxl = ITEM_ZONE_X_LEFT.load(Ordering::Relaxed);
    let zxr = ITEM_ZONE_X_RIGHT.load(Ordering::Relaxed);
    let zyt = ITEM_ZONE_Y_TOP.load(Ordering::Relaxed);
    let zyb = ITEM_ZONE_Y_BOTTOM.load(Ordering::Relaxed);

    let mut ps = ProjState {
        di: DST_SLOTS,
        bx: DST_COUNT as i32,
        filled_all: false,
    };

    if !run_pass(state, &mut ps, no_anim, cam_x, cam_y, true, zxl, zxr, zyt, zyb) {
        run_pass(state, &mut ps, no_anim, cam_x, cam_y, false, zxl, zxr, zyt, zyb);
    }

    // [asm 8992] clear the unused tail slots
    if !ps.filled_all {
        while ps.bx > 0 {
            state.ww(ps.di + 4, DEAD);
            ps.di += DST_STRIDE;
            ps.bx -= 1;
        }
    }
}
