// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 level-init / re-init sequence — the transition spine's foundation.
//!
//! Composes level_load + 5237 re-init + player_init + camera-init.

use crate::engine::state::NativeGameState;
use crate::engine::level_load;
use crate::recovered::prng;
use crate::views::dgroup_view::PLAYER_BASE;

const DS: usize = 0x1A0F;
const DS_BASE: usize = DS << 4;

const PLAYER_SLOT: usize = 0x4F1C;
const RENDER_SLOTS_BASE: usize = 0x4F0A;

// DGROUP offsets for 5237 re-init
const TIMER_STATE_BLOCK: usize = 0x6BC0;
const LEVEL_PROP_HEADER: usize = 0x815E;
const DBL_BUFFER_BACKUP: usize = 0x9203;
const WALL_MARKER_TABLE: usize = 0x6EA9;
const FILL_BLOCK_7DE6: usize = 0x7DE6;
const FILL_BLOCK_7DAF: usize = 0x7DAF;

// Proximity-scenery trigger table [0x83F3] (15 entries x stride 0xA) — shared with
// level_load::build_trigger_bank (41ca) which SAVES the pristine blocks; this restores them.
const PROXIMITY_TRIGGER_BASE: usize = 0x83F3;
// [0x2875] = the 41CA bank segment; [0x2DDA] = the level collision-map segment.
use crate::engine::dgroup_offsets::{LOAD_TOP, LEVEL_DATA_SEG};

/// 55fc: (re)initialise the object pool + the player for level start.
pub fn native_player_init(state: &mut NativeGameState) {
    let d = &mut state.data;
    // Bulk object-pool clear (0x40B words = 0x816 bytes)
    let base = (DS_BASE + PLAYER_SLOT) & 0xFFFFF;
    let end = base + 0x40B * 2;
    for b in base..end.min(d.len()) {
        d[b] = 0;
    }
    // Mark 0x74 object slots free (stride 0x12)
    for k in 0..0x74 {
        let slot_base = (DS_BASE + RENDER_SLOTS_BASE + k * 0x12) & 0xFFFFF;
        // [di+4] = 0xFFFF (sprite = dead)
        if slot_base + 5 < d.len() {
            d[slot_base + 4] = 0xFF;
            d[slot_base + 5] = 0xFF;
        }
        // [di+0x11] = 0 (life)
        if slot_base + 0x11 < d.len() {
            d[slot_base + 0x11] = 0;
        }
    }
    // Player start pos from property header
    let start_x = state.rw(0x8160);
    let start_y = state.rw(0x8162);
    state.ww(PLAYER_BASE, start_x);
    state.ww(PLAYER_BASE + 2, start_y);
}

/// 5237: The level / respawn RE-INIT.
/// Zero the timer block, restore the pristine double-buffer, re-init the object pool + player,
/// seed the decor-RNG table, and reset the energy / boss / scroll-script state.
pub fn native_5237(state: &mut NativeGameState) {
    let d = &mut state.data;

    // Zero the timer/state block (0x48 bytes at TIMER_STATE_BLOCK)
    let timer_base = (DS_BASE + TIMER_STATE_BLOCK) & 0xFFFFF;
    for b in timer_base..(timer_base + 0x48).min(d.len()) {
        d[b] = 0;
    }

    // Restore the animation remap pointer [0x6BC2] — the timer block zeroing above clears it,
    // but the boot table sets it to ANIM_BASE (0x6688). Restore it here so the animated-tile
    // remap cycle starts at the correct frame table.
    {
        let anim_off = (DS_BASE + 0x6BC2) & 0xFFFFF;
        d[anim_off] = 0x88;
        d[anim_off + 1] = 0x66;
    }

    // Restore the pristine double-buffer from backup (0x10A5 bytes)
    let backup_base = (DS_BASE + DBL_BUFFER_BACKUP) & 0xFFFFF;
    let prop_base = (DS_BASE + LEVEL_PROP_HEADER) & 0xFFFFF;
    let n = 0x10A5;
    for i in 0..n {
        if prop_base + i < d.len() && backup_base + i < d.len() {
            d[prop_base + i] = d[backup_base + i];
        }
    }

    // run_flag = 0, grid_dirty_token = 0x55AA, trail_ring = 0x4F76
    {
        let off = (DS_BASE + PLAYER_BASE + 0x10) & 0xFFFFF;
        if off < d.len() { d[off] = 0; }
    }
    {
        let off = (DS_BASE + 0x2DE0) & 0xFFFFF;
        if off + 1 < d.len() { d[off] = 0xAA; d[off + 1] = 0x55; }
    }
    {
        let off = (DS_BASE + 0x6BBE) & 0xFFFFF;
        if off + 1 < d.len() { d[off] = 0x76; d[off + 1] = 0x4F; }
    }

    // Read RNG state before dropping d
    let mut a = d[(DS_BASE + 0x2CEC) & 0xFFFFF];
    let mut b = d[(DS_BASE + 0x2CED) & 0xFFFFF];
    let mut c = d[(DS_BASE + 0x2CEE) & 0xFFFFF];
    let mut dd = {
        let off = (DS_BASE + 0x2CEF) & 0xFFFFF;
        (d[off] as u16) | ((d[off + 1] as u16) << 8)
    };
    let data_len = d.len();
    let decor_base = (DS_BASE + 0x6CA9) & 0xFFFFF;

    // native_player_init (55fc)
    native_player_init(state);

    // Fill the decor RNG table (256 words)
    let d = &mut state.data;
    for i in 0..0x100 {
        let (na, nb, nc, nd, _r1) = prng::rng_lcg(a, b, c, dd);
        a = na; b = nb; c = nc; dd = nd;
        let (na2, nb2, nc2, nd2, r2) = prng::rng_lcg(a, b, c, dd);
        a = na2; b = nb2; c = nc2; dd = nd2;
        let word = ((r2 as u16) << 8) | r2 as u16;
        let off = decor_base + i * 2;
        if off + 1 < data_len {
            d[off] = (word & 0xFF) as u8;
            d[off + 1] = (word >> 8) as u8;
        }
    }
    // Write back RNG state
    {
        let off = (DS_BASE + 0x2CEC) & 0xFFFFF;
        d[off] = a;
        let off = (DS_BASE + 0x2CED) & 0xFFFFF;
        d[off] = b;
        let off = (DS_BASE + 0x2CEE) & 0xFFFFF;
        d[off] = c;
        let off = (DS_BASE + 0x2CEF) & 0xFFFFF;
        d[off] = (dd & 0xFF) as u8; d[off + 1] = (dd >> 8) as u8;
    }

    // Wall marker table: fill with 0x55AA pattern (0x50 entries)
    let wall_base = (DS_BASE + WALL_MARKER_TABLE) & 0xFFFFF;
    for i in 0..0x50 {
        let off = wall_base + i * 2;
        if off + 1 < d.len() {
            d[off] = 0xAA;
            d[off + 1] = 0x55;
        }
    }

    // [asm 5292] restore the pristine proximity-scenery map blocks
    native_52d2(state);
    let d = &mut state.data;

    // Fill blocks
    let fb1 = (DS_BASE + FILL_BLOCK_7DE6) & 0xFFFFF;
    for b in fb1..(fb1 + 0x78).min(d.len()) { d[b] = 0xFF; }
    let fb2 = (DS_BASE + FILL_BLOCK_7DAF) & 0xFFFFF;
    for b in fb2..(fb2 + 0x37).min(d.len()) { d[b] = 0xFF; }

    // Energy = 3
    {
        let off = (DS_BASE + 0x27D6) & 0xFFFFF;
        if off < d.len() { d[off] = 3; }
    }
    // Boss script ptr = 0xFFFF
    {
        let off = (DS_BASE + 0xA517) & 0xFFFFF;
        if off + 1 < d.len() { d[off] = 0xFF; d[off + 1] = 0xFF; }
    }

    // [asm 52b3] scroll.script_ptr = scroll.script_table[g.level] ([0x2DBC] = [0x2D40 + level * 2])
    let level = d[(DS_BASE + 0x2D8A) & 0xFFFFF] as usize;
    let script_table_base = (DS_BASE + 0x2D40) & 0xFFFFF;
    let script_ptr_off = script_table_base + level * 2;
    if script_ptr_off + 1 < d.len() {
        let lo = d[script_ptr_off];
        let hi = d[script_ptr_off + 1];
        let off = (DS_BASE + 0x2DBC) & 0xFFFFF;
        if off + 1 < d.len() { d[off] = lo; d[off + 1] = hi; }
    }
    // [asm 52c2] scroll.frame_counter = 0 ([0x2DBE] = 0)
    {
        let off = (DS_BASE + 0x2DBE) & 0xFFFFF;
        if off + 1 < d.len() { d[off] = 0; d[off + 1] = 0; }
    }
}

/// 52d2: Restore the pristine PROXIMITY-SCENERY map blocks.
/// Walk the 41CA-built save bank at `[0x2875]:0` (`{word dest_map_off, byte rows,
/// byte width, width*rows tiles}`, 0xFFFF-terminated) and copy each saved block
/// back over the level collision map (`es=[0x2DDA]`, one row per 0x100 stride).
/// This un-collapses every fired earthquake/breakable-scenery trigger on respawn
/// AND at level start. Called from native_5237 @5292.
///
/// When every `[0x83F3]` trigger is dead (`[+0]==0xFFFF` for all 15) the bank is
/// just the 0xFFFF terminator, so we return without touching the volatile
/// `[0x2875]` scratch — byte-identical and robust to a clobbered scratch.
pub fn native_52d2(state: &mut NativeGameState) {
    // [asm 52d2] es = [0x2DDA]; [asm 52d8] ds = [0x2875]
    // If every [0x83F3] trigger is dead, there is nothing to restore.
    let all_dead = (0..15).all(|k| {
        state.rw(PROXIMITY_TRIGGER_BASE + k * 0xA) == 0xFFFF
    });
    if all_dead {
        return;
    }

    let bank_seg = state.rw(LOAD_TOP) as usize;       // 0x2875
    let map_seg = state.rw(LEVEL_DATA_SEG) as usize;   // 0x2DDA
    let bank_base = (bank_seg << 4) & 0xFFFFF;
    let map_base = (map_seg << 4) & 0xFFFFF;
    let len = state.data.len();

    // Segment-relative addressing helpers (offset wraps at 64K like 16-bit regs)
    let bank_addr = |off: usize| -> usize { (bank_base + (off & 0xFFFF)) & 0xFFFFF };
    let map_addr = |off: usize| -> usize { (map_base + (off & 0xFFFF)) & 0xFFFFF };

    let mut si: usize = 0; // [asm 52dc]
    for _ in 0..0x10 {
        // bank holds at most 15 entries (41CA cx=0xf)
        // record header: word map_off, byte rows, byte width
        let dest = {
            let a = bank_addr(si);
            if a + 1 < len {
                (state.data[a] as u16) | ((state.data[a + 1] as u16) << 8)
            } else {
                0xFFFF
            }
        };
        if dest == 0xFFFF {
            // [asm 52e0] terminator
            return;
        }
        let rows = state.data[bank_addr(si + 2)];
        let width = state.data[bank_addr(si + 3)];
        si += 4; // [asm 52e8]

        let mut dest_row = dest;
        for _r in 0..rows as usize {
            // rep movsb (one map row): copy `width` bytes bank -> map
            for k in 0..width as usize {
                let src_byte = state.data[bank_addr(si + k)];
                let dst = map_addr((dest_row as usize + k) & 0xFFFF);
                if dst < len {
                    state.data[dst] = src_byte;
                }
            }
            si += width as usize;
            dest_row = (dest_row.wrapping_add(0x100)) & 0xFFFF; // [asm 52f1] next map row
        }
    }
    // [asm falls through past the 15-entry loop without a terminator] — only reachable
    // on a level WITH live [0x83F3] triggers but a corrupted bank.
    log::warn!("52D2 scenery-restore: no 0xFFFF terminator within 15 entries on a level WITH live [0x83F3] triggers — the [0x2875] trigger bank was built (41CA) but got corrupted");
}

/// 3af2: The level-start camera-init.
/// Reset camera cells, then snap-scroll the camera onto the player.
pub fn native_3af2(state: &mut NativeGameState) {
    // Delegate to the full snap-scroll implementation in camera_scroll
    crate::engine::camera_scroll::native_3af2(state);
}

/// 01cf: The full level-init sequence.
pub fn native_level_init(state: &mut NativeGameState, game_root: &std::path::Path) {
    let level = state.rb(0x2D8A);
    // native_level_load (3ed6)
    let _ = level_load::native_level_load(state, level, game_root);
    // native_5237 (re-init)
    native_5237(state);
    // native_player_init (55fc)
    native_player_init(state);
    // native_3af2 (camera-init)
    native_3af2(state);
    // spawn_offset_ring = 0
    state.ww(0xA341, 0);
    // aura_toggle = 0
    state.wb(0x6BCC, 0);
    // Re-seed RNG to the fixed level-start value
    state.wb(0x2CEC, 5);
    state.wb(0x2CED, 0x22);
    state.wb(0x2CEE, 0x86);
    state.ww(0x2CEF, 0xE58D);
    // pending_key = 0
    state.wb(0x2874, 0);
}

/// The full LEVEL-START sequence (main 0x13e..0x0155).
pub fn native_level_start(state: &mut NativeGameState, game_root: &std::path::Path) {
    native_level_init(state, game_root);
    // Reset lives, bonus, attack
    state.wb(0x27D8, 2);       // lives = 2
    state.wb(0x6CA7, 0);       // bonus_letters = 0
    state.wb(0x7B19, 0x14);    // attack_v19 = 0x14
    state.wb(0x7B18, 0);       // attack_phase = 0
    state.wb(0x6CA8, 0);       // utensils_mask = 0
    state.wb(0x6BE6, 0);       // level_end_mode = 0
    state.wb(0x6BE4, 0);       // respawn_state = 0
    state.wb(0x6BE5, 0);       // end_signal = 0
}
