// SPDX-License-Identifier: GPL-3.0-only
//! Native level loader — the VM-less replacement for 1030:3ed6 (the LEVELx.SQZ loader).
//!
//! main() (1030:00a0) calls 3ed6(level) once per level to unpack LEVEL<n>.SQZ into DGROUP tables
//! + the planar tile cache: the seed state the per-frame gameplay loop then runs on. Booting a
//! NativeGameState from game files (no EXE, no snapshot) means reproducing this loader over the
//! recovered pieces — load_sqz (107B), decode_sprite_cache (4316/4389), classify_sprites (4232)
//! — plus the loader's own table-copy bookkeeping.

use std::path::Path;
use crate::engine::state::{NativeGameState, DS_BASE};
use crate::engine::assets::load_sqz_by_dx;
use crate::engine::dgroup_offsets::*;

// ---- DGROUP offsets not in dgroup_offsets.rs (used only by the loader) ----
const COLLECTED_COUNTER: usize     = 0x2A76; // g.collected_counter
const COLLECTED_LINKED: usize      = 0x2A7A; // g.collected_linked
const TERRAIN_ENTITY_SRC: usize    = 0x9107; // g.terrain_entities (16x stride 0xF)
const PROXIMITY_TRIGGER_BASE: usize = 0x83F3; // v.triggers (15x stride 0xA)
const CHECKPOINT_X: usize          = 0x6BAD; // g.checkpoint_x
const CHECKPOINT_Y: usize          = 0x6BAF; // g.checkpoint_y

/// The 0x3f3c rep-movsw block: a single contiguous copy of the level's property tables.
pub const PROP_LEN: usize = 0x300 + 11 + 0x200 + 0x8c + 0x96 + 0x800 + 4 + 0x190 + 0x100 + 0x1ea + 0xf0 + 4 + 8; // == 0x13a7
pub const PROP_DST: usize = 0x7e5e;
pub const TILE_INDEX_DST: usize = 0x25ce;
pub const OBJ_POOL: usize = 0x4f1c;

const PARALLAX_BASE: usize  = 0x7E80; // BG_PTR_BIAS — the parallax base the renderer reads
const PARALLAX_PLANE: usize = 0x1F40; // 200 rows * 0x28 = one screen per plane

/// Sign-extend a byte to i16.
#[inline]
fn s8(v: u8) -> i16 {
    v as i8 as i16
}

// =============================================================================
//  native_level_load_dgroup — prologue scalars + clears, SQZ load, tile-index,
//  property-table memcpy [asm 3ede..3f78]
// =============================================================================

/// Reproduce the deterministic DGROUP half of 3ed6:
/// prologue scalars + clears, the SQZ load, the tile-index table, and the property-table memcpy.
pub fn native_level_load_dgroup(
    state: &mut NativeGameState,
    level: u8,
    game_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // --- prologue [asm 3ede..3f26] ---
    state.wb(LEVEL_INDEX, level); // g.level = level

    if level <= 0x0A {
        state.ww(COLLECT_TOTAL_DECOR, 0); // lg.collect_total_decor = 0
        state.ww(COLLECT_TOTAL_MAIN, 0);   // lg.collect_total_main = 0
        state.ww(COLLECTED_LINKED, 0);      // g.collected_linked = 0
        state.ww(COLLECTED_COUNTER, 0);     // g.collected_counter = 0
    } else {
        // halve the totals
        let d = state.rw(COLLECT_TOTAL_DECOR);
        state.ww(COLLECT_TOTAL_DECOR, d >> 1);
        let m = state.rw(COLLECT_TOTAL_MAIN);
        state.ww(COLLECT_TOTAL_MAIN, m >> 1);
    }

    // gfx_group = ByteTable(rb, GFX_GROUP_TABLE)[level]
    let gfx_group = state.rb(GFX_GROUP_TABLE + level as usize);
    state.wb(GFX_GROUP, gfx_group);

    // filename digit: level + '1', then 'A'.. past '9'
    let mut al = level.wrapping_add(0x31);
    if al > 0x39 {
        al = al.wrapping_add(7);
    }
    state.wb(FILENAME_DIGIT, al);

    // clear property region [asm 3f16] — fill 0xFF
    for i in DS_BASE + PROP_DST..DS_BASE + PROP_DST + 0x13A5 {
        state.data[i] = 0xFF;
    }

    // clear object pool [asm 3f20] — fill 0xFF
    for i in DS_BASE + OBJ_POOL..DS_BASE + OBJ_POOL + 0x40B * 2 {
        state.data[i] = 0xFF;
    }

    // --- load LEVEL<n>.SQZ at the load pointer [0x2875] [asm 3f2b] ---
    let seg = load_sqz_by_dx(state, 0x2D8B, game_root)?;
    state.ww(LEVEL_DATA_SEG, seg); // g.map_seg = seg

    // --- 4316: header skip + tile-index table [asm 4316..433e] ---
    let bh = state.rb(LEVEL_HEADER_TABLE + level as usize);
    state.wb(LEVEL_BOTTOM_LIMIT, bh); // g.map_rows = bh

    // base_seg = seg + (bh << 4)
    let base_seg = ((seg as u32) + ((bh as u32) << 4)) & 0xFFFF;
    let base = ((base_seg as usize) << 4) & 0xFFFFF;

    // copy 0x100-word tile index
    let dst = DS_BASE + TILE_INDEX_DST;
    let tile_copy: Vec<u8> = state.data[base..base + 0x200].to_vec();
    state.data[dst..dst + 0x200].copy_from_slice(&tile_copy);

    // property-table source offset: advance bp past local tile graphics (0x80 bytes each)
    let mut bp = 0x200usize;
    for i in 0..0x100 {
        let code = state.data[base + i * 2] as u16 | ((state.data[base + i * 2 + 1] as u16) << 8);
        if code < 0x100 {
            bp += 0x80;
        }
    }

    // --- property tables: one contiguous 0x13a7-byte copy [asm 3f3c..3f78] ---
    let src = base + bp;
    let prop_dst = DS_BASE + PROP_DST;
    let prop_copy: Vec<u8> = state.data[src..src + PROP_LEN].to_vec();
    state.data[prop_dst..prop_dst + PROP_LEN].copy_from_slice(&prop_copy);

    log::debug!(
        "Level {} dgroup: seg=0x{:04X}, map_rows={}, base_seg=0x{:04X}, prop_copy={} bytes",
        level, seg, bh, base_seg, PROP_LEN
    );

    Ok(())
}

// =============================================================================
//  _rebase_effect_sprites — 4159 (called as 414d)
//  Rebase the float-effect + terrain-entity sprite refs [+4] from level-bank
//  to engine-bank (-dx + 0x35).
// =============================================================================

fn rebase_effect_sprites(state: &mut NativeGameState) {
    let dx = state.rw(SPRITE_BANK_LO); // 0x8C89

    // Effect sources: 70 entries at 0x8F1D, stride 7, sprite at +4
    for k in 0..0x46 {
        let base = EFFECT_SPRITE_SRC + k * 7;
        let sprite = state.rw(base + 4);
        if sprite != 0xFFFF {
            state.ww(base + 4, sprite.wrapping_sub(dx).wrapping_add(0x35));
        }
    }

    // Terrain entities: 16 entries at 0x9107, stride 0xF, sprite at +4
    for k in 0..16 {
        let base = TERRAIN_ENTITY_SRC + k * 0xF;
        let sprite = state.rw(base + 4);
        if sprite != 0xFFFF {
            state.ww(base + 4, sprite.wrapping_sub(dx).wrapping_add(0x35));
        }
    }
}

// =============================================================================
//  _rebase_entity_sprites — 4182
//  Rebase the second-pass entity list [0x8489] (variable stride = [si], <=0x32)
//  sprite ref [+2] across two banks, then reset [0x8c89]=0x35 / [0x8c8b]=0x138.
// =============================================================================

fn rebase_entity_sprites(state: &mut NativeGameState) {
    let dx = state.rw(SPRITE_BANK_LO); // 0x8C89
    let bx = state.rw(SPRITE_BANK_HI); // 0x8C8B
    if dx != 0xFFFF {
        let mut si: usize = ENTITY_LIST_2NDPASS; // 0x8489
        loop {
            let stride = state.rb(si & 0xFFFF);
            if stride > 0x32 {
                break;
            }
            let ax = state.rw((si + 2) & 0xFFFF);
            if ax != 0xFFFF {
                if ax >= bx {
                    state.ww((si + 2) & 0xFFFF, ax.wrapping_sub(bx).wrapping_add(0x138));
                } else if ax >= dx {
                    state.ww((si + 2) & 0xFFFF, ax.wrapping_sub(dx).wrapping_add(0x35));
                }
            }
            // advance cursor by signed stride, wrapping at 16 bits
            si = ((si as i32 + s8(stride) as i32) as u16) as usize;
        }
    }
    state.ww(SPRITE_BANK_LO, 0x35);
    state.ww(SPRITE_BANK_HI, 0x138);
}

// =============================================================================
//  _ensure_password_seed — 932F 9336..9396
//  Lazy one-time seed init: if [0xA335] is clear, compute the BIOS-ROM checksum
//  into [0xA333] and set the flag. Idempotent.
// =============================================================================

fn ensure_password_seed(state: &mut NativeGameState) {
    if state.rb(SEED_COMPUTED_FLAG) != 0 {
        return;
    }
    let seed = crate::recovered::password::bios_seed(&state.data);
    state.ww(BIOS_SEED, seed);
    state.wb(SEED_COMPUTED_FLAG, 1);
}

// =============================================================================
//  _assign_random_decor — 40bd
//  Random decorative-sprite assignment. Collect float-effect entries whose
//  sprite ref (& 0x1FFF) is in [0x11b, 0x12b) into a pointer list at [0x6a88];
//  if the count is a nonzero multiple of 4, bubble-sort by first word, then
//  deal each a pseudo-random sprite (0x11b + a 4-bit nibble of the level's
//  password code, MSB-first, groups of 4 reusing the code).
// =============================================================================

fn assign_random_decor(state: &mut NativeGameState) {
    ensure_password_seed(state);

    // Collect pointers to effect-source entries whose sprite (& 0x1FFF) is in [0x11B, 0x12B)
    let mut ptrs: Vec<usize> = Vec::new();
    for k in 0..0x46 {
        let si = EFFECT_SPRITE_SRC + k * 7;
        let v = state.rw(si + 4);
        if ((v & 0x1FFF).wrapping_sub(0x11B)) & 0xFFFF <= 0xF {
            ptrs.push(si);
        }
    }

    // Mirror [0x6a88] (words) for fidelity
    for (i, &p) in ptrs.iter().enumerate() {
        state.ww(DECOR_PTR_LIST + i * 2, p as u16);
    }

    let bp = ptrs.len();
    if bp == 0 || bp & 3 != 0 {
        return;
    }

    // Bubble sort the pointers by the pointed entry's first word [si] (x at +0)
    let mut swapped = true;
    while swapped {
        swapped = false;
        for i in 0..bp - 1 {
            let a = state.rw(ptrs[i]);
            let b = state.rw(ptrs[i + 1]);
            if b < a {
                ptrs.swap(i, i + 1);
                swapped = true;
            }
        }
    }

    // Re-mirror sorted pointers
    for (i, &p) in ptrs.iter().enumerate() {
        state.ww(DECOR_PTR_LIST + i * 2, p as u16);
    }

    // Compute the level password code
    let level = state.rb(LEVEL_INDEX) as u16;
    let mode_copy = state.rb(MODE_COPY);
    let level = if mode_copy != 0 { level + 0x0A } else { level };
    let seed = state.rw(BIOS_SEED);
    let code = crate::recovered::password::level_code(level, seed, crate::recovered::password::DEFAULT_ROT);

    // Groups of 4 entries, each group re-using code, 4 bits per entry MSB-first
    for gi in (0..bp).step_by(4) {
        let mut dx = code;
        for j in 0..4 {
            if gi + j >= bp {
                break;
            }
            let di = ptrs[gi + j];
            // rotate left 4 so the next nibble is the low 4 bits (MSB-first extraction)
            dx = dx.rotate_left(4);
            let nib = dx & 0xF;
            let val = 0x11Bu16 + nib;
            state.ww(di + 4, val);
        }
    }
}

// =============================================================================
//  _self_patch_secret_tiles — 3ead
//  Hide the secret/bonus tiles in the level-data grid. Walk the 0x50-entry
//  bonus-cell list [0x8c8d] (stride 5): each entry's [+3] word bx is a cell
//  offset into the level data. For a live entry (bx != 0xFFFF) swap
//  level_data[bx] with the entry's [+0] byte (the hidden tile) and stash the
//  displaced (revealed) tile in [+1]. Then add the count to [0x2a74].
// =============================================================================

fn self_patch_secret_tiles(state: &mut NativeGameState) {
    let seg = state.rw(LEVEL_DATA_SEG); // 0x2DDA
    let base = ((seg as usize) << 4) & 0xFFFFF;
    let mut dx: u16 = 0;

    for k in 0..0x50 {
        let cell_base = BONUS_CELL_LIST + k * 5; // 0x8C8D + k*5
        let bx = state.rw(cell_base + 3);
        if bx == 0xFFFF {
            continue;
        }
        dx += 1;
        let cell = (base + bx as usize) & 0xFFFFF;
        let hidden_tile = state.rb(cell_base); // [+0] the hidden-tile replacement

        // stash the original (revealed) tile in [+1]
        let orig_tile = state.data[cell];
        state.wb(cell_base + 1, orig_tile);

        // patch the cell to hidden
        state.data[cell] = hidden_tile;
    }

    let total = state.rw(COLLECT_TOTAL_MAIN);
    state.ww(COLLECT_TOTAL_MAIN, total.wrapping_add(dx));
}

// =============================================================================
//  _dup_double_buffer — 4065
//  Dup the rebased [0x815e..] block to [0x9203..] (0x10a5 bytes, the working copy).
// =============================================================================

fn dup_double_buffer(state: &mut NativeGameState) {
    let src = DS_BASE + LEVEL_PROP_HEADER; // 0x815E
    let dst = DS_BASE + DBL_BUFFER_BACKUP;  // 0x9203
    let n = 0x10A5;
    let tmp: Vec<u8> = state.data[src..src + n].to_vec();
    state.data[dst..dst + n].copy_from_slice(&tmp);
}

// =============================================================================
//  _build_trigger_bank — 41ca
//  Save the pristine proximity-scenery map blocks into the [0x2875] bank.
//  For each live [0x83F3] entry (15 x stride 0xA, [+0] != 0xFFFF): the block
//  spans from [+0] - [+8]*0x100 over [+3]+[+8] rows x [+2] columns of the level
//  map. Bank record: {word map_off, byte rows, byte width, tiles row-major},
//  0xFFFF-terminated; the entry's [+6] gets the bank offset of the LAST saved
//  row.
// =============================================================================

fn build_trigger_bank(state: &mut NativeGameState) {
    let bank_seg = state.rw(LOAD_TOP);     // 0x2875 — es = [0x2875]
    let map_seg = state.rw(LEVEL_DATA_SEG); // 0x2DDA — ds = [0x2DDA]
    let bank_base = (bank_seg as usize) << 4;
    let map_base = (map_seg as usize) << 4;

    // Segment-relative addressing helpers (offset wraps at 64K like 16-bit registers)
    let bank_addr = |off: usize| -> usize { (bank_base + (off & 0xFFFF)) & 0xFFFFF };
    let map_addr = |off: usize| -> usize { (map_base + (off & 0xFFFF)) & 0xFFFFF };

    let mut di: usize = 0;

    // [asm 41d0] empty-bank terminator
    state.ww_abs(bank_addr(di), 0xFFFF);

    for k in 0..15 {
        let trig_base = PROXIMITY_TRIGGER_BASE + k * 0xA;

        // [asm 41d5/41d8] read the trigger entry
        let block_top = state.rw(trig_base); // [+0]
        if block_top == 0xFFFF {
            // [asm 41db] dead entry
            continue;
        }
        let width = state.rb(trig_base + 2);   // [+2]
        let height = state.rb(trig_base + 3);   // [+3]
        let countdown = state.rb(trig_base + 8); // [+8]

        let count = countdown as u16;
        // [asm 41ed] top minus the rows it will rise
        let mut src = block_top.wrapping_sub(count << 8);
        // [asm 41ef/41f2] everything the mod touches
        let rows = (height as u16 + count) & 0xFF;
        let rows_usize = rows as usize;
        let width_usize = width as usize;

        // [asm 4200-4203] record: map offset, rows, width
        state.ww_abs(bank_addr(di), src);
        state.wb_abs(bank_addr(di + 2), rows as u8);
        state.wb_abs(bank_addr(di + 3), width);
        di += 4; // [asm 4207]

        // [asm 420a-421d] save the pristine block (row-major)
        for _r in 0..rows_usize {
            for c in 0..width_usize {
                let b = state.data[map_addr((src as usize + c) & 0xFFFF)];
                state.data[bank_addr(di)] = b;
                di += 1;
            }
            // [asm 4215/4217] next map row
            src = src.wrapping_add(0x100);
        }

        // [asm 4224/4228] reveal_cursor = the LAST saved row's offset
        let reveal_cursor = (di - width_usize) & 0xFFFF;
        state.ww(trig_base + 6, reveal_cursor as u16);

        // [asm 422b] terminator after this entry
        state.ww_abs(bank_addr(di), 0xFFFF);
    }
}

// =============================================================================
//  _per_level_pointers — 4038..4056
//  Per-level word [0x2d40+level*2] -> [0x2dbc] (scroll script ptr), [0x2dbe]=0;
//  and checkpoint_x/y from the property header.
// =============================================================================

fn per_level_pointers(state: &mut NativeGameState) {
    let level = state.rb(LEVEL_INDEX) as usize;

    // script_ptr = WordTable(rw, SCROLL_SCRIPT_TABLE)[level * 2]
    let script_ptr = state.rw(SCROLL_SCRIPT_TABLE + level * 2);
    state.ww(SCROLL_SCRIPT_PTR, script_ptr); // [0x2DBC]

    // checkpoint from the property header
    let start_x = state.rw(PLAYER_START_X); // [0x8160]
    let start_y = state.rw(PLAYER_START_Y); // [0x8162]
    state.ww(CHECKPOINT_X, start_x); // [0x6BAD]
    state.ww(CHECKPOINT_Y, start_y); // [0x6BAF]

    // frame counter = 0
    state.ww(SCROLL_SCRIPT_PTR2, 0); // [0x2DBE]
}

// =============================================================================
//  _count_decor — 4073..40bb
//  Count float-effect entries whose (rebased) sprite [+4] is in [0x6e,0x75] or
//  [0x80,0xdb]; add to [0x2a78]; then if level<0xA and [0x2cf6+level]!=0xff,
//  double [0x2a78] and [0x2a74].
// =============================================================================

fn count_decor(state: &mut NativeGameState) {
    let mut dx: u16 = 0;
    for k in 0..0x46 {
        let si = EFFECT_SPRITE_SRC + k * 7;
        let ax = state.rw(si + 4);
        if ax == 0xFFFF || ax < 0x6E || ax > 0xDB {
            continue;
        }
        if ax >= 0x80 || ax <= 0x75 {
            dx += 1;
        }
    }

    let decor = state.rw(COLLECT_TOTAL_DECOR);
    state.ww(COLLECT_TOTAL_DECOR, decor.wrapping_add(dx));

    let level = state.rb(LEVEL_INDEX) as usize;
    if level < 0x0A && state.rb(WARP_TABLE + level) != 0xFF {
        let d = state.rw(COLLECT_TOTAL_DECOR);
        state.ww(COLLECT_TOTAL_DECOR, d << 1);
        let m = state.rw(COLLECT_TOTAL_MAIN);
        state.ww(COLLECT_TOTAL_MAIN, m << 1);
    }
}

// =============================================================================
//  native_level_load_objects — the DGROUP-only object/effect-table setup
//  [asm 4059..409d + 3ead + 40bd + 41ca + 4073..40bb]
// =============================================================================

/// The DGROUP-only object/effect-table setup that runs after the property copy:
/// the two sprite-bank rebases, the secret-tile self-patch, the RNG decor
/// assignment, the double-buffer dup, the trigger bank build, and the decor count.
pub fn native_level_load_objects(state: &mut NativeGameState) {
    per_level_pointers(state);       // 4038..4056
    rebase_effect_sprites(state);     // 414d
    rebase_entity_sprites(state);     // 4182
    self_patch_secret_tiles(state);   // 3ead: hide the secret/bonus tiles + count into [0x2a74]
    assign_random_decor(state);       // 40bd
    dup_double_buffer(state);         // 4065
    build_trigger_bank(state);        // 41ca: save the pristine scenery blocks -> [0x2875]
    count_decor(state);               // 4073..40bb
}

// =============================================================================
//  native_level_load_background — BACK<group>.SQZ -> the 0x7E80 parallax base
// =============================================================================

/// Load the level's parallax BACKDROP into the 0x7E80 parallax base the renderer reads.
pub fn native_level_load_background(
    state: &mut NativeGameState,
    game_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::engine::assets::{read_cstring, resolve_game_path};
    use crate::engine::vga::{EGA_APERTURE, EGA_PLANE_STRIDE};
    use crate::codecs::sqz::unpack_sqz;

    let name = read_cstring(state, 0x2DA6); // BACK<group>.SQZ
    let path = resolve_game_path(game_root, &name)?;
    let raw = std::fs::read(&path)?;
    let back = unpack_sqz(&raw);

    for p in 0..4 {
        let dst = EGA_APERTURE + p * EGA_PLANE_STRIDE + PARALLAX_BASE;
        let src_start = p * PARALLAX_PLANE;
        let src_end = (src_start + PARALLAX_PLANE).min(back.len());
        let dst_end = (dst + PARALLAX_PLANE).min(state.data.len());
        let len = (src_end - src_start).min(dst_end - dst);
        state.data[dst..dst + len].copy_from_slice(&back[src_start..src_start + len]);
    }

    log::debug!("Background loaded: {} ({} bytes)", name, back.len());
    Ok(())
}

// =============================================================================
//  native_level_load_anim_tables — 42af
//  Build the 4 tile-animation lookup tables [0x6688]/[0x6788]/[0x6888]/[0x6988]
//  (0x100 bytes each).
// =============================================================================

/// [asm 42af] Build the 4 tile-animation lookup tables that the renderer reads
/// to animate tiles.
pub fn native_level_load_anim_tables(state: &mut NativeGameState) {
    let flag_base = DS_BASE + 0x805E; // the [0x805E] per-tile byte
    let t0_base = DS_BASE + 0x6688;
    let t1_base = DS_BASE + 0x6788;
    let t2_base = DS_BASE + 0x6888;
    let t3_base = DS_BASE + 0x6988;

    let mut has_anim = 0u8;
    let mut bx: usize = 0;

    while bx < 0x100 {
        let flag = state.data[flag_base + bx];
        if flag & 0x80 == 0 {
            // [asm 42bb] static tile: maps to itself in tables 0/1/2, 0 in table 3
            let t = bx as u8;
            state.data[t0_base + bx] = t;
            state.data[t1_base + bx] = t;
            state.data[t2_base + bx] = t;
            state.data[t3_base + bx] = 0;
            bx += 1; // [asm 42d3 -> 430c]
        } else {
            // [asm 42d5] animated: rotated 3-frame cycle
            has_anim = 1;
            let t = bx as u8;
            let t1 = t.wrapping_add(1);
            let t2 = t.wrapping_add(2);
            state.data[t0_base + bx] = t;
            state.data[t0_base + bx + 1] = t1;
            state.data[t0_base + bx + 2] = t2;
            state.data[t1_base + bx] = t1;
            state.data[t1_base + bx + 1] = t2;
            state.data[t1_base + bx + 2] = t;
            state.data[t2_base + bx] = t2;
            state.data[t2_base + bx + 1] = t;
            state.data[t2_base + bx + 2] = t1;
            state.data[t3_base + bx] = 1;
            state.data[t3_base + bx + 1] = 1;
            state.data[t3_base + bx + 2] = 1;
            bx += 3; // [asm 430a/430b/430c: inc bx x3]
        }
    }

    // [asm 4311] any-animated flag (page_dirty = has_anim)
    state.wb(ANY_ANIMATED_FLAG, has_anim);
}

// =============================================================================
//  native_level_load_planar — 4316 (LOCAL)
//  Demux each index-table sprite with code < 0x100 into the four EGA shadow
//  planes the renderer reads.
// =============================================================================

/// The LOCAL planar tile-cache blit [asm 4316]: demux each index-table sprite
/// with code < 0x100 into the four EGA shadow planes.
pub fn native_level_load_planar(state: &mut NativeGameState) {
    use crate::views::sprites::{compute_local_slots, write_slots};

    let seg = state.rw(LEVEL_DATA_SEG);
    let level = state.rb(LEVEL_INDEX) as usize;
    let bh = state.rb(LEVEL_HEADER_TABLE + level);
    let base_seg = ((seg as u32) + ((bh as u32) << 4)) & 0xFFFF;

    let slots = compute_local_slots(state, base_seg as u16);
    log::debug!("Planar: {} local slots", slots.len());
    write_slots(state, &slots);
}

// =============================================================================
//  native_level_load_shared — 4389 (SHARED/UNION)
//  The global UNION.SQZ bank, then every index-table sprite with code >= 0x100
//  demuxed into the planar cache.
// =============================================================================

/// The SHARED/union sprite-bank pass [asm 4389]: the global UNION.SQZ bank,
/// then every index-table sprite with code >= 0x100 demuxed into the planar cache.
pub fn native_level_load_shared(
    state: &mut NativeGameState,
    game_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::views::sprites::{compute_shared_slots, write_slots};

    // Save the level-data grid (UNION will overwrite it temporarily)
    let lvl_seg = state.rw(LEVEL_DATA_SEG);
    let lvl_base = ((lvl_seg as usize) << 4) & 0xFFFFF;
    let grid_save: Vec<u8> = state.data[lvl_base..lvl_base + 0x12000].to_vec();

    // [asm 3fb1-3fb8] load UNION at a small base over the level segment
    let saved_bump = state.rw(LOAD_TOP);
    state.ww(LOAD_TOP, lvl_seg);
    let union_seg = load_sqz_by_dx(state, 0x0A25, game_root)?;
    state.ww(LOAD_TOP, saved_bump);
    state.ww(UNION_BANK_SEG, union_seg);

    // [asm 4389] demux code>=0x100 into the cache
    let slots = compute_shared_slots(state, union_seg);
    log::debug!("Shared: {} union slots", slots.len());
    write_slots(state, &slots);

    // Restore the level-data grid UNION overwrote
    state.data[lvl_base..lvl_base + 0x12000].copy_from_slice(&grid_save);

    Ok(())
}

// =============================================================================
//  native_level_load_classify — 4232
//  Classify each populated cache slot into the type table [0x4DF8] and save the
//  partial transparency masks compacted at [0x2DF8].
// =============================================================================

/// 4232: classify each populated cache slot into the type table [0x4DF8]
/// (0 opaque / 1 empty / >=2 partial) and save the partial transparency masks
/// compacted at [0x2DF8].
pub fn native_level_load_classify(state: &mut NativeGameState) {
    use crate::views::sprites::read_sprite_cache;
    use crate::recovered::sprite_classify::classify_sprites;

    let cache = read_sprite_cache(state);
    let res = classify_sprites(&cache);

    // Write type table at [0x4DF8]
    let type_dst = DS_BASE + TILE_TYPE_TABLE;
    let type_len = res.types.len();
    state.data[type_dst..type_dst + type_len].copy_from_slice(&res.types);

    // Write masks at [0x2DF8]
    let mask_dst = DS_BASE + TILE_MASK_TABLE;
    let n = res.partial_count * 0x20;
    let n = n.min(res.masks.len());
    state.data[mask_dst..mask_dst + n].copy_from_slice(&res.masks[..n]);

    log::debug!("Classify: {} types, {} partials", res.types.len(), res.partial_count);
}

// =============================================================================
//  native_level_load — the top-level orchestrator (1030:3ed6)
//  Composed from the recovered halves of 3ed6.
// =============================================================================

/// Load LEVEL<n>.SQZ into a NativeGameState — the seed the gameplay loop runs on,
/// composed from the recovered halves of 1030:3ed6.
pub fn native_level_load(
    state: &mut NativeGameState,
    level: u8,
    game_root: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    native_level_load_dgroup(state, level, game_root)?;
    // [asm 3f8d] the parallax blit runs HERE — right after the LEVEL<n> load,
    // BEFORE UNION/BACK0.
    native_level_load_background(state, game_root)?;
    native_level_load_objects(state);
    native_level_load_anim_tables(state);   // 42af: tile-animation tables [0x6688]
    native_level_load_planar(state);         // LOCAL tile cache (code < 0x100)
    native_level_load_shared(state, game_root)?; // SHARED/union bank (code >= 0x100)
    native_level_load_classify(state);       // 4232: tile/sprite type tables (full cache)

    log::info!("Level {} fully loaded", level);
    Ok(())
}
