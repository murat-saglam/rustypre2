// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 moving-sprite renderer — recovered native logic (pure).
//!
//! Walks the active-sprite list, culls/clips, and emits draw commands.
//! Phase A: plan sprites (list walk -> cull -> position -> clip -> SpriteDraw).
//! Phase B: paint sprites onto the 4 EGA plane buffers.

pub const LIST_TOP: usize = 0x5720;
pub const LIST_BASE: usize = 0x4F0A;
pub const SCREEN_W: i32 = 0x140;       // 320 px
pub const SCREEN_H: i32 = 0xB0;        // 176 visible rows
pub const TILE_PX: i32 = 16;

pub const MODE_ERASE: u8 = 0x00;
pub const MODE_NORMAL: u8 = 0x01;
pub const MODE_OPAQUE: u8 = 0x10;

#[inline]
fn s16(v: i32) -> i32 { let v = v & 0xFFFF; if v & 0x8000 != 0 { v - 0x10000 } else { v } }
#[inline]
fn s8(v: i32) -> i32 { let v = v & 0xFF; if v & 0x80 != 0 { v - 0x100 } else { v } }

/// One active-sprite record (18 bytes).
#[derive(Clone, Copy, Default)]
pub struct Sprite {
    pub x: i32,           // [+0] world X (px)
    pub y: i32,           // [+2] world Y (px)
    pub sprite_id: u16,   // [+4] sprite id (0xFFFF = empty)
    pub flags: u8,        // [+5] flags (bit5 set after draw)
    pub life: u8,         // [+0x11] anim/life counter
}

/// Per-sprite-id attributes.
#[derive(Clone, Copy, Default)]
pub struct SpriteAttr {
    pub width: i32,    // low byte of [0x7190]
    pub height: i32,   // high byte of [0x7190]
    pub x_off: i32,    // [0x752A]
    pub y_off: i32,    // [0x752B]
    pub src_seg: usize,// [0x62E8]
    pub src_off: usize,// [0x5F48]
}

/// Camera / scroll inputs.
#[derive(Clone, Copy, Default)]
pub struct Camera {
    pub cam_x: i32,
    pub cam_y: i32,
    pub fine_scroll: i32,
    pub row_factor: i32,
    pub dest_page: usize,
    pub row_stride: usize,
    pub global_shift: u32,
    pub frame: u32,
}

/// A planned blit (Phase A output).
#[derive(Clone, Copy, Default)]
pub struct SpriteDraw {
    pub sprite_id: u16,
    pub src_seg: usize,
    pub src_off: usize,
    pub dest_off: usize,
    pub byte_width: i32,
    pub rows: i32,
    pub shift: u32,
    pub flipped: bool,
    pub mode: u8,
    pub src_bw: i32,
    pub left_skip: i32,
    pub right_skip: i32,
    pub right_clipped: bool,
    pub full_rows: i32,
}

/// Per-frame record mutation.
#[derive(Clone, Copy, Default)]
pub struct SpriteRecordUpdate {
    pub new_life: u8,
    pub new_flags: u8,
}

/// The per-frame mutation 26FA applies to an active-sprite record (the side effect
/// distinct from the pixels): the saturating-decremented life [+0x11] and the new flags
/// byte [+5] (drawn bit cleared each frame, then set iff the sprite was actually drawn).
pub fn plan_record_update(spr: &Sprite, drawn: bool) -> SpriteRecordUpdate {
    let new_life = if spr.life > 0 { spr.life - 1 } else { 0 };
    let mut flags = spr.flags & 0xDF;
    if drawn {
        flags = (flags | 0x20) & 0xBF;
    }
    SpriteRecordUpdate { new_life, new_flags: flags }
}

/// A sprite's resolved placement (the part of 271C..27F5 before the planar clip/blit),
/// shared by the faithful raster planner (`plan_sprite`) and the semantic command
/// builder (`plan_sprite_command`) so they can never drift.
struct Placement {
    flipped: bool,
    mode: u8,
    width: i32,
    height: i32,
    screen_x: i32,     // left edge on screen (px)
    screen_y: i32,     // baseline (bottom) on screen (px)
    top_row: i32,      // top edge on screen (px)
}

/// Resolve flip / blink mode / screen placement, or `None` if culled. [asm 271C..27F5]
fn placement(spr: &Sprite, attr: &SpriteAttr, cam: &Camera) -> Option<Placement> {
    if spr.sprite_id == 0xFFFF { return None; }                        // [asm 2713] empty slot
    let flipped = (spr.sprite_id & 0x8000) != 0;                       // [asm 2739/273B] H-flip = id bit15
    // blink/anim mode [asm 2740..2761]: the decremented life is a local for the decision only;
    // the [si+0x11] write-back is the checkpoint's record-mutation contract.
    let life_after: u8 = if spr.life > 0 { spr.life - 1 } else { 0 };    // [asm 2742/2746] saturating dec
    let bit14 = (spr.sprite_id & 0x4000) != 0;                         // [asm 2757 test bh,0x80] -> id bit14
    let mode = if life_after == 0 || (cam.frame & 3) == 0 {            // expired, or blink "on" 1/4 frames
        if bit14 { MODE_OPAQUE } else { MODE_NORMAL }
    } else {                                                           // [asm 2753] blink off
        MODE_ERASE
    };
    let width = attr.width;
    let height = attr.height;
    let mut x_off = attr.x_off;
    if flipped {                                                      // [asm 2775: al = width - x_off]
        x_off = width - x_off;
    }
    let is_hud = (spr.sprite_id & 0x1FFF) == 0x135;                   // [asm 277E: cmp bx,0x26A] HUD/boss-meter

    let (screen_x, screen_y) = if is_hud {
        // Fixed-screen HUD sprite (id 0x135): FIXED position, NO camera, SKIPS the off-screen-X
        // and screen_y<=0 culls [asm 2784-2790 jmp 27DE]. This is the BOSS HEALTH METER.
        (s16(spr.x - x_off), s16(spr.y + s8(attr.y_off)))              // [asm 2784/2788-278D]
    } else {
        let sx = s16(spr.x - x_off - cam.cam_x * TILE_PX);             // [asm 277A..27B5]
        if sx >= SCREEN_W { return None; }                              // [asm 27A6] off right
        if s16(sx + width) < 0 { return None; }                         // [asm 27AE..27B3] off left
        // screen_y is the BASELINE (bottom); the top row is screen_y - height [asm 27B8..27F5].
        let sy = s16(spr.y + s8(attr.y_off) + cam.row_factor
                    - (cam.cam_y * TILE_PX + cam.fine_scroll));
        if sy <= 0 { return None; }                                    // [asm 27D9: jg] baseline must be on-screen
        (sx, sy)
    };

    let top_row = s16(screen_y - height);                             // [asm 27EB] dest's top row
    if top_row >= SCREEN_H { return None; }                           // [asm 27F0: cmp ax,0xB0] entirely below

    Some(Placement { flipped, mode, width, height, screen_x, screen_y, top_row })
}

/// Plan one sprite's blit (the faithful raster command), or `None` if culled.
/// [asm 271C..28BE]
///
/// Faithful to the ASM: H-flip mirrors the X offset, position is camera-relative
/// (X in px, Y biased by `row_factor` and `fine_scroll`), the sprite is culled
/// against [0,320)×(0,176) with a left-edge width test, and left/right/top/bottom
/// overruns clip the byte-width / rows / source-skip and shift the dest offset.
pub fn plan_sprite(spr: &Sprite, attr: &SpriteAttr, cam: &Camera) -> Option<SpriteDraw> {
    let p = placement(spr, attr, cam)?;
    let flipped = p.flipped;
    let mode = p.mode;
    let width = p.width;
    let height = p.height;
    let screen_x = p.screen_x;
    let screen_y = p.screen_y;
    let top_row = p.top_row;

    let mut byte_width = (width as u32 >> cam.global_shift) as i32;   // [asm 27FC: shr dl,cs:[0]]
    let mut rows = height;                                           // [asm 26EC seed = dh]
    let full_rows = height;   // source plane-block stride = full_rows*src_bw
    let mut src_skip = 0usize;

    let dest_row;
    if top_row < 0 {                                                 // top clip [asm 2811..2826]
        let clip = -top_row;
        rows -= clip;
        src_skip = (clip as usize) * (byte_width as usize);          // [asm 281C: mul dl -> source skip]
        dest_row = 0;                                                // [asm 2824: xor di,di]
    } else {
        dest_row = top_row;
    }
    if screen_y > SCREEN_H {                                         // bottom clip [asm 2849..2858]
        rows -= screen_y - SCREEN_H;
    }

    // dest VRAM offset: top_row*stride + display page, then the horizontal column.
    let mut dest_off = (dest_row as usize * cam.row_stride + cam.dest_page) & 0xFFFF;  // [asm 2828/282E]
    let col = screen_x >> 3;                                         // arithmetic (sar) [asm 2865/287E]
    let src_bw = byte_width;                                         // source row width (full sprite)
    let mut left_skip = 0i32;
    let mut right_skip = 0i32;
    let mut right_clipped = false;

    if col >= 0 {                                                    // [asm 287E: screen_x >= 0]
        dest_off = (dest_off + col as usize) & 0xFFFF;             // [asm 2885: add [26f1], col]
        let end_col = col + byte_width - cam.row_stride as i32;     // right clip [asm 288A..28B1]
        if end_col >= 0 {                                            // col+bw >= stride -> partial-edge blit
            right_clipped = true;
            right_skip = end_col;                                    // [asm 28AD: [26ef]=overflow]
            byte_width -= end_col;                                   // [asm 28A3: sub [26e4],overflow]
        }
    } else {                                                         // [asm 2865: screen_x < 0] left clip
        left_skip = -col;                                            // [asm 2871: [26ed]=-col]
        byte_width += col;                                           // [asm 286C: add [26e4], col(neg)]
    }

    let shift = (screen_x & 7) as u32;                               // [asm 28C8: cl = [26ea]&7]
    let src_off = (attr.src_off + src_skip) & 0xFFFF;               // [asm 283F: [26f3]=[bx+5F48]+skip]
    // The ASM does NOT cull on byte_width: a left-clip sliver can have byte_width==0
    // (only the partial carry byte is drawn). It only culls via the screen-window tests.
    if rows <= 0 || byte_width < 0 {
        return None;
    }
    Some(SpriteDraw {
        sprite_id: spr.sprite_id, src_seg: attr.src_seg, src_off,
        dest_off, byte_width, rows, shift, flipped, mode,
        src_bw, left_skip, right_skip, right_clipped, full_rows,
    })
}

/// Walk the active list top->down and emit a draw command per visible sprite.
///
/// Phase A: returns the ordered `Vec<SpriteDraw>`. (Phase B paints them.)
/// `sprites` is the list of `Sprite` from LIST_TOP-RECORD_BYTES down to
/// LIST_BASE; `attrs` maps sprite_id -> `SpriteAttr`.
pub fn plan_frame(sprites: &[Sprite], attrs: &dyn Fn(u16) -> Option<SpriteAttr>, cam: &Camera) -> Vec<SpriteDraw> {
    let mut draws = Vec::new();
    for spr in sprites {
        if spr.sprite_id == 0xFFFF { continue; }
        if let Some(attr) = attrs(spr.sprite_id) {
            if let Some(d) = plan_sprite(spr, &attr, cam) {
                draws.push(d);
            }
        }
    }
    draws
}

// ── Phase B: the planar blit ──────────────────────────────────────
//
// Models the VGA write path the ASM drives via the Graphics Controller: a CPU byte V
// written at offset o with data-rotate `shift` and logical function `func` becomes
// `plane[p][o] = func(ror8(V,shift), plane[p][o])` for each map-mask-selected plane.
// So the latch/sequencer dance collapses to per-plane reads/writes on de-interleaved
// plane buffers.

#[inline]
fn ror8(v: u8, r: u32) -> u8 {
    let r = r & 7;
    if r == 0 { v } else { v.rotate_right(r) }
}

#[inline]
fn bit_reverse(v: u8) -> u8 {
    v.reverse_bits()
}

/// Fast unflipped AND-mask / OR-all-planes pass across all 4 planes simultaneously.
fn phase_all_planes(
    planes: &mut [Vec<u8>; 4], dest_off: usize, src: &[u8], mut si: usize,
    draw: &SpriteDraw, stride: usize, invert: bool, op_and: bool, final_mask: u8,
) {
    let bw = draw.byte_width as usize;
    let rows = draw.rows as usize;
    let shift = draw.shift;
    let ch = (0xFFu32 << shift) as u8;
    let bh = !ch;
    let mut di = dest_off;
    let [p0, p1, p2, p3] = planes;

    for _ in 0..rows {
        let mut ah: u8 = 0;
        let mut d = di;
        if draw.left_skip != 0 {
            si += draw.left_skip as usize - 1;
            ah = src[si] & bh;
            si += 1;
        }
        if op_and {
            for _ in 0..bw {
                let s = src[si]; si += 1;
                let al = (s & ch) | ah;
                ah = s & bh;
                let val = if invert { !al } else { al };
                let rv = ror8(val, shift);
                p0[d] &= rv;
                p1[d] &= rv;
                p2[d] &= rv;
                p3[d] &= rv;
                d = (d + 1) & 0xFFFF;
            }
            let base = if invert { !ah } else { ah };
            let rv = ror8(base | final_mask, shift);
            p0[d] &= rv;
            p1[d] &= rv;
            p2[d] &= rv;
            p3[d] &= rv;
        } else {
            for _ in 0..bw {
                let s = src[si]; si += 1;
                let al = (s & ch) | ah;
                ah = s & bh;
                let val = if invert { !al } else { al };
                let rv = ror8(val, shift);
                p0[d] |= rv;
                p1[d] |= rv;
                p2[d] |= rv;
                p3[d] |= rv;
                d = (d + 1) & 0xFFFF;
            }
            let base = if invert { !ah } else { ah };
            let rv = ror8(base & final_mask, shift);
            p0[d] |= rv;
            p1[d] |= rv;
            p2[d] |= rv;
            p3[d] |= rv;
        }
        si += draw.right_skip as usize;
        di = (di + stride) & 0xFFFF;
    }
}

/// Fast unflipped OR pass onto a single target plane.
fn phase_single_plane(
    plane: &mut [u8], dest_off: usize, src: &[u8], mut si: usize,
    draw: &SpriteDraw, stride: usize, final_mask: u8,
) {
    let bw = draw.byte_width as usize;
    let rows = draw.rows as usize;
    let shift = draw.shift;
    let ch = (0xFFu32 << shift) as u8;
    let bh = !ch;
    let mut di = dest_off;

    for _ in 0..rows {
        let mut ah: u8 = 0;
        let mut d = di;
        if draw.left_skip != 0 {
            si += draw.left_skip as usize - 1;
            ah = src[si] & bh;
            si += 1;
        }
        for _ in 0..bw {
            let s = src[si]; si += 1;
            let al = (s & ch) | ah;
            ah = s & bh;
            plane[d] |= ror8(al, shift);
            d = (d + 1) & 0xFFFF;
        }
        plane[d] |= ror8(ah & final_mask, shift);
        si += draw.right_skip as usize;
        di = (di + stride) & 0xFFFF;
    }
}

/// Fast H-flipped AND-mask / OR-all-planes pass across all 4 planes simultaneously.
fn phase_flip_all_planes(
    planes: &mut [Vec<u8>; 4], dest_off: usize, src: &[u8], block_base: usize,
    draw: &SpriteDraw, stride: usize, invert: bool, op_and: bool, final_mask: u8,
) {
    let bw = draw.byte_width as usize;
    let rows = draw.rows as usize;
    let shift = draw.shift;
    let src_bw = draw.src_bw as usize;
    let ls = draw.left_skip as usize;
    let ch = (0xFFu32 << shift) as u8;
    let bh = !ch;
    let start = src_bw as isize - 1 - ls as isize;
    let mut di = dest_off;
    let [p0, p1, p2, p3] = planes;

    for r in 0..rows {
        let row = block_base + r * src_bw;
        let mut ah: u8 = if ls != 0 && ls <= src_bw {
            bit_reverse(src[row + src_bw - ls]) & bh
        } else {
            0
        };
        let mut d = di;
        if op_and {
            for c in 0..bw {
                let idx = (row as isize + start - c as isize).max(0) as usize;
                let s = bit_reverse(src[idx]);
                let al = (s & ch) | ah;
                ah = s & bh;
                let val = if invert { !al } else { al };
                let rv = ror8(val, shift);
                p0[d] &= rv;
                p1[d] &= rv;
                p2[d] &= rv;
                p3[d] &= rv;
                d = (d + 1) & 0xFFFF;
            }
            let base = if invert { !ah } else { ah };
            let rv = ror8(base | final_mask, shift);
            p0[d] &= rv;
            p1[d] &= rv;
            p2[d] &= rv;
            p3[d] &= rv;
        } else {
            for c in 0..bw {
                let idx = (row as isize + start - c as isize).max(0) as usize;
                let s = bit_reverse(src[idx]);
                let al = (s & ch) | ah;
                ah = s & bh;
                let val = if invert { !al } else { al };
                let rv = ror8(val, shift);
                p0[d] |= rv;
                p1[d] |= rv;
                p2[d] |= rv;
                p3[d] |= rv;
                d = (d + 1) & 0xFFFF;
            }
            let base = if invert { !ah } else { ah };
            let rv = ror8(base & final_mask, shift);
            p0[d] |= rv;
            p1[d] |= rv;
            p2[d] |= rv;
            p3[d] |= rv;
        }
        di = (di + stride) & 0xFFFF;
    }
}

/// Fast H-flipped OR pass onto a single target plane.
fn phase_flip_single_plane(
    plane: &mut [u8], dest_off: usize, src: &[u8], block_base: usize,
    draw: &SpriteDraw, stride: usize, final_mask: u8,
) {
    let bw = draw.byte_width as usize;
    let rows = draw.rows as usize;
    let shift = draw.shift;
    let src_bw = draw.src_bw as usize;
    let ls = draw.left_skip as usize;
    let ch = (0xFFu32 << shift) as u8;
    let bh = !ch;
    let start = src_bw as isize - 1 - ls as isize;
    let mut di = dest_off;

    for r in 0..rows {
        let row = block_base + r * src_bw;
        let mut ah: u8 = if ls != 0 && ls <= src_bw {
            bit_reverse(src[row + src_bw - ls]) & bh
        } else {
            0
        };
        let mut d = di;
        for c in 0..bw {
            let idx = (row as isize + start - c as isize).max(0) as usize;
            let s = bit_reverse(src[idx]);
            let al = (s & ch) | ah;
            ah = s & bh;
            plane[d] |= ror8(al, shift);
            d = (d + 1) & 0xFFFF;
        }
        plane[d] |= ror8(ah & final_mask, shift);
        di = (di + stride) & 0xFFFF;
    }
}

/// Paint one planned sprite onto the four plane buffers. `src` is the sprite's
/// pixel data from `draw.src_off` (block-sequential by source row width `src_bw`:
/// a mask block then 4 plane blocks for masked modes, or one block for opaque).
/// H-flipped sprites use the mirrored phase.
pub fn paint_sprite(planes: &mut [Vec<u8>; 4], draw: &SpriteDraw, src: &[u8], stride: usize) {
    let block = (draw.src_bw * draw.full_rows) as usize;
    let and_mask: u8 = if draw.right_clipped { 0xFF } else { 0x00 };
    let or_mask: u8 = if draw.right_clipped { 0x00 } else { 0xFF };

    if draw.flipped {
        if draw.mode == MODE_OPAQUE {
            phase_flip_all_planes(planes, draw.dest_off, src, 0, draw, stride, false, false, or_mask);
            return;
        }
        phase_flip_all_planes(planes, draw.dest_off, src, 0, draw, stride, true, true, and_mask);
        if draw.mode == MODE_NORMAL {
            for p in 0..4 {
                phase_flip_single_plane(&mut planes[p], draw.dest_off, src, (p + 1) * block, draw, stride, or_mask);
            }
        }
    } else {
        if draw.mode == MODE_OPAQUE {
            phase_all_planes(planes, draw.dest_off, src, 0, draw, stride, false, false, or_mask);
            return;
        }
        phase_all_planes(planes, draw.dest_off, src, 0, draw, stride, true, true, and_mask);
        if draw.mode == MODE_NORMAL {
            for p in 0..4 {
                phase_single_plane(&mut planes[p], draw.dest_off, src, (p + 1) * block, draw, stride, or_mask);
            }
        }
    }
}
