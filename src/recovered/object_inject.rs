// SPDX-License-Identifier: GPL-3.0-only
//! The secondary-entity render-injection pass (1030:6913..698B + the 0x7Dxx/0x7Exx/0x7Fxx handlers).
//!
//! After the main object-update walker (684E..6913), a SECOND pass walks the variable-stride
//! entity list at ``0x8489`` (entry 0 = the player). Each entry's ``[+1]`` handler index
//! dispatches through ``cs:[bx+0x6AC3]``. Most handlers project the entity into a free slot
//! of the MAIN object list ``0x4FD0`` as a render record, so the moving-sprite renderer
//! (``26FA``) draws it, then set the entity's mode byte ``[entry+4]``.
//!
//! All functions use the direct NativeGameState API (state.rb/rw/wb/ww) — no dict/overlay.
//! All operations mutate `NativeGameState` directly in place mutation,
//! since later reads naturally see earlier writes in a mutable borrow.

use crate::engine::state::NativeGameState;
use crate::recovered::object_update::on_screen_tile;
use crate::recovered::prng;

// ── constants ────────────────────────────────────────────────────

/// [asm 7F52] the entity mode set on a successful projection (wrappers override it).
const INJECT_MODE: u8 = 0x17;

/// The main object record list (shared with the walker).
const OBJ_BASE: usize = 0x4FD0;
const OBJ_STRIDE: usize = 0x12;
const OBJ_COUNT: usize = 12;

/// The variable-stride 2nd-pass entity list (entry 0 = the player).
const ENTITY_LIST: usize = 0x8489;
/// [si] >= this ends the walk.
const ENTITY_STRIDE_END: u8 = 0x32;

/// 16-slot ring index into the spawn X-offset table.
const SPAWN_OFFSET_RING: usize = 0xA341;
/// [0xA32E] = the last projected object slot.
const PROJ_SLOT_PTR: usize = 0xA32E;
/// [0x6BCC] idx0 alternating ±0xC0 side flag.
const AURA_TOGGLE: usize = 0x6BCC;

/// Level-map segment pointer (es = [0x2DDA]).
const MAP_SEG_PTR: usize = 0x2DDA;

/// Player struct base (render slot #1).
const PLAYER_X: usize = 0x4F1C;
const PLAYER_Y: usize = 0x4F1E;

/// Current level index.
const LEVEL: usize = 0x2D8A;

/// Camera tile column / row (word reads for on-screen culling).
const CAM_COL_WORD: usize = 0x2DE4;
const CAM_ROW_WORD: usize = 0x2DE6;

/// Camera-shake/boss-phase state gates (level 5).
const CAMERA_SHAKE: usize = 0x6BEA;
/// The L6/boss fight phase (advances every 7 hits).
const BOSS_PHASE: usize = 0xA326;

/// The map's bottom row bound.
const MAP_ROWS: usize = 0x2CF5;

/// The committed mode copy the loader reads [0xB198].
const MODE_COPY: usize = 0xB198;

/// RNG LCG state bytes [0x2CEC..0x2CEF].
const RNG_STATE: usize = 0x2CEC;

// ── read-only asset table constants ──────────────────────────────

/// Tile id -> ground property (solidity).
const FLOOR_PROPS_BASE: usize = 0x7F5E;
/// Spawn ring -> signed X offset (word), indexed by the ring's absolute cursor value.
const SPAWN_OFFSET_TABLE: u16 = 0x5CBD;
/// Per-entity anim-frame descriptor table (section-marked scan).
const ANIM_FRAME_TABLE: u16 = 0xA86F;
/// Section marker word within the anim-frame table.
const ANIM_SECTION_MARKER: u16 = 0x7D01;

// ── helpers ─────────────────────────────────────────────────────

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 {
        v - 0x10000
    } else {
        v
    }
}

/// `add [off],1 ; sbb [off],0` — increment a byte counter saturating at 0xFF.
#[inline]
fn saturating_inc(v: u8) -> u8 {
    v.saturating_add(1)
}

/// Read a byte from the level map (es:[off]). `es` = [0x2DDA].
#[inline]
fn read_tile(state: &NativeGameState, off: u16) -> u8 {
    let map_seg = state.rw(MAP_SEG_PTR) as usize;
    state.rb_abs((map_seg << 4) + off as usize)
}

/// Floor property byte for a tile (table at 0x7F5E).
#[inline]
fn floor_props(state: &NativeGameState, tile: u8) -> u8 {
    state.rb(FLOOR_PROPS_BASE + tile as usize)
}

/// The spawn X-offset word at the ring's absolute cursor position.
/// `ring` is the value at [0xA341] (0..0xE, stepping by 2); the actual read is at
/// `(ring - SPAWN_OFFSET_TABLE) & 0xFFFF` = `0xA343 + ring`.
#[inline]
fn spawn_x_offset(state: &NativeGameState, ring: u16) -> u16 {
    state.rw(ring.wrapping_sub(SPAWN_OFFSET_TABLE) as usize)
}

// ── 806C: find_free_object_slot ──────────────────────────────────

/// Recover ``1030:806C`` — the first free slot index of the object list ``0x4FD0``
/// (``[slot+4]==0xFFFF``), or ``None`` if all 12 are taken.
pub fn find_free_object_slot(state: &NativeGameState) -> Option<usize> {
    for slot in 0..OBJ_COUNT {
        // [asm 8070 cx=0xC]
        if state.rw(OBJ_BASE + slot * OBJ_STRIDE + 4) == 0xFFFF {
            // [asm 8073 cmp [di+4],-1]
            return Some(slot);
        }
    }
    None // [asm 807F stc -> CF=1]
}

// ── RNG ─────────────────────────────────────────────────────────

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

// ── 6954: lookup_anim_frame ──────────────────────────────────────

/// Recover ``1030:6954..6981`` — resolve a projected entity's anim-frame descriptor pointer.
///
/// Scan the table at ``0xA86F`` for the ``0x7D01`` section marker whose following word matches
/// ``entry_type``, then within that section find the entry whose word equals
/// ``entry_id - 0x138``. Returns the descriptor pointer the walker stores into the projected
/// object slot's ``[+0xC]``.
pub fn lookup_anim_frame(state: &NativeGameState, entry_id: u16, entry_type: u8) -> u16 {
    let target = entry_id.wrapping_sub(0x138);
    let mut bx = ANIM_FRAME_TABLE;
    // Find the 0x7D01 marker for this type.
    for _ in 0..4096 {
        bx = bx.wrapping_add(2);
        if state.rw(bx as usize) == ANIM_SECTION_MARKER
            && state.rw(bx.wrapping_add(2) as usize) == entry_type as u16
        {
            break;
        }
    }
    // Past the marker + type word.
    bx = bx.wrapping_add(4);
    // Find the matching id.
    for _ in 0..4096 {
        if state.rw(bx as usize) == target {
            return bx;
        }
        bx = bx.wrapping_add(2);
    }
    panic!("6954 anim-frame lookup: entry_id={:#x} entry_type={} not found", entry_id, entry_type);
}

// ── 7F26: project_entity ────────────────────────────────────────

/// Recover ``1030:7F26`` — project a 2nd-pass entity into a free object-list slot for
/// rendering.
///
/// Culls off-screen via ``on_screen_tile`` (``8022``); allocates a free object slot via
/// ``find_free_object_slot`` (the recovered ``806C``); copies the entity X (``[entry+9]``),
/// Y (``[entry+0xB]``), sprite id (``[entry+2]``) and a back-pointer (``[entry] ->
/// record[+6]``) into the slot, zeroes the velocity/state fields, sets the record's flip
/// byte from ``[entry+5]``, and latches ``[0xA32E]`` to the projected slot base. Does NOT
/// write the entity mode byte — each handler writes its own mode.
///
/// Returns ``Some(slot)`` on success, ``None`` when off-screen or no free slot.
/// ``cull=false`` enters at the ``0x7F31`` mid-routine label (skip the on-screen test) —
/// the proximity-gated handler ``7D1B`` calls it that way after doing its own
/// player-relative range check.
fn project_entity(
    state: &mut NativeGameState,
    si: usize,
    cam_x: i32,
    cam_y: i32,
    cull: bool,
) -> Option<usize> {
    let entry_x = state.rw(si + 9);
    let entry_y = state.rw(si + 0xB);
    let entry_sprite = state.rw(si + 2);
    let entry_aux5 = state.rb(si + 5);

    if cull && !on_screen_tile(entry_x, entry_y, cam_x, cam_y) {
        // [7F26-7F2F] off-screen -> CF=1
        return None;
    }
    let slot = find_free_object_slot(state)?; // [7F31-7F34] no free slot -> CF=1
    let base = OBJ_BASE + slot * OBJ_STRIDE;

    // Write the projected object record fields directly.
    state.wb(base + 0x10, 0); // [7F36] hits (cleared first)
    state.ww(PROJ_SLOT_PTR, base as u16); // [7F96]-equivalent latch
    state.ww(base + 0x00, entry_x); // [7F3E-7F41] X
    state.ww(base + 0x02, entry_y); // [7F43-7F46] Y
    state.ww(base + 0x04, entry_sprite); // [7F49-7F4C] sprite id (from [entry+2])
    state.ww(base + 0x06, (si & 0xFFFF) as u16); // [7F4F] back-pointer to the entity
    state.ww(base + 0x08, 0); // [7F56] Xvel = 0
    state.ww(base + 0x0A, 0); // [7F5B] Yvel = 0
    state.wb(base + 0x0E, 0); // [7F60] state = 0
    state.wb(base + 0x0F, entry_aux5); // [7F64-7F67] flip/aux byte (from [entry+5])

    Some(slot)
}

// ── 7D9B: handler_ground_snap_spawn (idx10) ─────────────────────

/// Recover ``1030:7D9B`` — second-pass handler idx10: a player-proximity-gated,
/// ground-snapped sprite projector.
///
/// Gated by: on level 5, the camera-shake/boss-phase state; a per-entity saturating
/// counter (``[si+7]``) throttle vs ``[si+6]``; and a player-proximity window
/// (``[si+9]/[si+0xA]`` origin, ``[si+0xB]/[si+0xC]`` extent, in tile cells). On a pass it
/// allocates a free object slot, advances the spawn offset ring, and projects a sprite at
/// ``playerX + a rotating ring offset`` snapped onto a standable ground surface (scans the
/// terrain map upward for a solid-with-2-empty-above cell), then finishes the record.
fn handler_ground_snap_spawn(state: &mut NativeGameState, si: usize) -> bool {
    let level = state.rb(LEVEL);
    if level == 5 {
        // [7D9B] level-5 special gates
        if state.rb(CAMERA_SHAKE) != 0 {
            // [7DA2] earthquake/shake active -> no draw
            return false;
        }
        if state.rw(BOSS_PHASE) == 3 {
            // [7DA9]
            return false;
        }
    }

    // [7DB0] saturating ++ (add ; sbb) — unconditional
    let counter = saturating_inc(state.rb(si + 7));
    state.wb(si + 7, counter);
    if state.rb(si + 6) > (counter >> 2) {
        // [7DBB] throttle
        return false;
    }

    let px_cell = ((s16(state.rw(PLAYER_X) as i32) >> 4) & 0xFF) as u8; // [7DC8]
    let origin_x_cell = state.rb(si + 9);
    if px_cell < origin_x_cell {
        // [7DD0] jb
        return false;
    }
    let rel_x = px_cell.wrapping_sub(origin_x_cell);
    if state.rb(si + 0xB) < rel_x {
        // [7DD4] jb
        return false;
    }

    let py_cell = ((s16(state.rw(PLAYER_Y) as i32) >> 4) & 0xFF) as u8; // [7DD9]
    let origin_y_cell = state.rb(si + 0xA);
    if py_cell < origin_y_cell {
        // [7DDE] jb
        return false;
    }
    let rel_y = py_cell.wrapping_sub(origin_y_cell);
    if state.rb(si + 0xC) < rel_y {
        // [7DE2] jb
        return false;
    }

    let slot = match find_free_object_slot(state) {
        // [7DE7]
        Some(s) => s,
        None => return false,
    };
    let base = OBJ_BASE + slot * OBJ_STRIDE;

    // [7DF4] place at playerX + the next ring offset, advance the ring
    let ring = state.rw(SPAWN_OFFSET_RING);
    let player_x = state.rw(PLAYER_X);
    let new_x = player_x.wrapping_add(spawn_x_offset(state, ring));
    state.ww(SPAWN_OFFSET_RING, (ring.wrapping_add(2)) & 0x0F);
    // [7E0C] dx=0 / not dx (sign toward the player)
    let xvel = if s16(player_x as i32) >= s16(new_x as i32) {
        0u16
    } else {
        0xFFFF
    };

    // [7DEC-7E15] these slot writes are BEFORE the terrain scan, so they land UNCONDITIONALLY
    // — even when the scan below finds no standable surface and the entity is NOT drawn, the
    // hits/proj-ptr/X/Xvel residue is left in the (still-empty) slot.
    state.wb(base + 0x10, 0); // [7DEC] hits
    state.ww(PROJ_SLOT_PTR, base as u16); // [7DF0] projected slot
    state.ww(base + 0x00, new_x); // [7E0A] X
    state.ww(base + 0x08, xvel); // [7E15] Xvel sign

    // [7E18] scan the terrain map upward for a standable surface (solid here, 2 empty above)
    let start = ((((py_cell as u16).wrapping_add(4)) << 8) | ((new_x >> 4) as u16)) & 0xFFFF;
    // bp = ((playerY>>4)+4):(newX>>4)
    let limit = (state.rb(MAP_ROWS) as u16) << 8; // dx = mapheight*0x100
    let mut bp = start;
    let mut ground_row: Option<u8> = None;
    for _ in 0..0x0A {
        // [7E66] ah = 0xA tries
        if bp < limit {
            // [7E3B] bp below the map bottom?
            let t0 = floor_props(state, read_tile(state, bp));
            if t0 != 0 {
                // [7E3F] solid here
                let t1 = floor_props(state, read_tile(state, bp.wrapping_sub(0x100)));
                if t1 == 0 {
                    // [7E48] empty one above
                    let t2 = floor_props(state, read_tile(state, bp.wrapping_sub(0x200)));
                    if t2 == 0 {
                        // [7E52] empty two above -> standable
                        ground_row = Some((bp >> 8) as u8);
                        break;
                    }
                }
            }
        }
        bp = bp.wrapping_sub(0x100); // [7E5C] up a row
        if bp < 0x300 {
            // [7E60] ran off the top -> give up
            break;
        }
    }

    let ground_row = match ground_row {
        // [7E95] no surface -> not drawn (the residue above remains)
        Some(r) => r,
        None => return false,
    };

    // [7E6C-7E90] a standable surface was found -> finish the record
    state.ww(base + 0x02, ((ground_row as u16) << 4) & 0xFFFF); // [7E74] Y = surface row * 16
    state.ww(base + 0x04, state.rw(si + 2)); // [7E77] sprite id
    state.ww(base + 0x06, (si & 0xFFFF) as u16); // [7E7D] back-pointer
    state.wb(base + 0x0E, 0); // [7E84] state byte
    state.ww(base + 0x0A, 0); // [7E88] Yvel
    state.wb(base + 0x0F, state.rb(si + 5)); // [7E90] flip byte
    state.wb(si + 4, 0x17); // [7E80] entity mode
    true
}

// ── per-type walker handlers ─────────────────────────────────────
// cs:[bx+0x6AC3], bx = ([entry+1]<<1)&0xFF
// Each mutates state directly and returns whether drawn.

/// idx1/2/3/5-8 (``7F26``/``7EE2``/``7ED8``/``7EB5``) — project + set the entity mode byte.
/// ``mode`` is the only difference: 0x17 (idx1 worker), 5 (idx2 ``7EE2`` / idx5-8 ``7EB5``),
/// 0x37 (idx3 ``7ED8``).
fn handler_project_mode(
    state: &mut NativeGameState,
    si: usize,
    cam_x: i32,
    cam_y: i32,
    mode: u8,
) -> bool {
    if project_entity(state, si, cam_x, cam_y, true).is_none() {
        return false;
    }
    state.wb(si + 4, mode);
    true
}

/// idx4 (``7EBF``) — project, mode 5, and clear the entity's ``[+0xF]``/``[+0x10]`` bytes.
fn handler_7ebf(state: &mut NativeGameState, si: usize, cam_x: i32, cam_y: i32) -> bool {
    if project_entity(state, si, cam_x, cam_y, true).is_none() {
        return false;
    }
    state.wb(si + 4, 5); // [7EC4]
    state.wb(si + 0xF, 0); // [7EC8/7ECF]
    state.wb(si + 0x10, 0); // [7ED3]
    true
}

/// idx9 (``7E97``) — clear ``[si+0x11]`` unconditionally; on a draw set the mode to
/// ``(old [si+4] | 5)``, OR-ing bit7 on level 6.
fn handler_7e97(
    state: &mut NativeGameState,
    si: usize,
    cam_x: i32,
    cam_y: i32,
    level: u8,
) -> bool {
    state.wb(si + 0x11, 0); // [7E97] cleared unconditionally
    let old_mode = state.rb(si + 4); // [7E9B] saved before project
    if project_entity(state, si, cam_x, cam_y, true).is_none() {
        return false;
    }
    let mut m = old_mode | 5; // [7EA5]
    if level == 6 {
        // [7EA7] level 6 -> bit7
        m |= 0x80;
    }
    state.wb(si + 4, m);
    true
}

/// idx11 (``7D6E``) — saturating counter ``[si+7]`` throttle vs ``[si+6]``; on a draw set
/// mode 0x37 and jitter the projected Y down by ``rng_lcg() & 0x3F`` (advancing the shared
/// generator).
fn handler_7d6e(state: &mut NativeGameState, si: usize, cam_x: i32, cam_y: i32) -> bool {
    let counter = saturating_inc(state.rb(si + 7));
    state.wb(si + 7, counter);
    if state.rb(si + 6) > (counter >> 2) {
        // [7D7D]
        return false;
    }
    let slot = match project_entity(state, si, cam_x, cam_y, true) {
        Some(s) => s,
        None => return false,
    };
    let base = OBJ_BASE + slot * OBJ_STRIDE;
    state.wb(si + 4, 0x37); // [7D87]
    let ret = rng_lcg_next(state); // [7D8B call 39DF]
    let projected_y = state.rw(base + 0x02);
    state.ww(base + 0x02, projected_y.wrapping_sub((ret & 0x3F) as u16)); // [7D8E/7D91]
    true
}

/// idx12 (``7D1B``) — player-proximity gate (player below the entity; |dx|<0x280; if
/// |dx|<0x140 also a 0xB4<dy<0x168 Y-band) + saturating-counter throttle; then a no-cull
/// projection (``7F31``), mode 0x8F, and reset ``[si+7]``.
fn handler_7d1b(state: &mut NativeGameState, si: usize, cam_x: i32, cam_y: i32) -> bool {
    let pv_y = s16(state.rw(PLAYER_Y) as i32);
    let rec_y = s16(state.rw(si + 0xB) as i32);
    if pv_y <= rec_y {
        // [7D1E/7D22] player not below entity (word view: world Y)
        return false;
    }
    let pv_x = s16(state.rw(PLAYER_X) as i32);
    let rec_x = s16(state.rw(si + 9) as i32);
    let mut dxv = rec_x - pv_x; // [7D24/7D27] (word view: world X)
    if dxv < 0 {
        dxv = -dxv; // [7D2D]
    }
    if dxv >= 0x280 {
        // [7D2F/7D32]
        return false;
    }
    if dxv < 0x140 {
        // [7D34] near -> apply the Y-band
        let dyv = pv_y - rec_y; // [7D39/7D3C]
        if dyv >= 0x168 || dyv <= 0xB4 {
            // [7D3F/7D44]
            return false;
        }
    }
    let counter = saturating_inc(state.rb(si + 7)); // [7D49]
    state.wb(si + 7, counter);
    if state.rb(si + 6) > (counter >> 2) {
        // [7D58]
        return false;
    }
    if project_entity(state, si, cam_x, cam_y, false).is_none() {
        // [7D5D] 7F31 (no on-screen cull)
        return false;
    }
    state.wb(si + 4, 0x8F); // [7D62]
    state.wb(si + 7, 0); // [7D66] counter reset on draw
    true
}

/// idx0 (``7F6C``) — a player-relative aura: if the player tile is within the entity's
/// window (origin [si+9]/[si+0xA], extent [si+0xB]/[si+0xC]), project a sprite at
/// ``playerX ± 0xC0`` (the side alternating via the ``[0x6BCC]`` toggle), ``playerY -
/// 0xB0``, mode 7. Own projection (does not call 7F26).
fn handler_7f6c(state: &mut NativeGameState, si: usize) -> bool {
    let pv_x = state.rw(PLAYER_X);
    let pv_y = state.rw(PLAYER_Y);

    let al = ((s16(pv_x as i32) >> 4) & 0xFF) as u8; // [7F6E/7F71] playerX tile (low byte)
    let origin_x_cell = state.rb(si + 9);
    if al < origin_x_cell {
        // [7F76/7F78]
        return false;
    }
    let al = al.wrapping_sub(origin_x_cell);
    if state.rb(si + 0xB) < al {
        // [7F7A/7F7D]
        return false;
    }

    let al2 = ((s16(pv_y as i32) >> 4) & 0xFF) as u8; // [7F7F/7F82] playerY tile (low byte)
    let origin_y_cell = state.rb(si + 0xA);
    if al2 < origin_y_cell {
        // [7F84/7F86]
        return false;
    }
    let al2 = al2.wrapping_sub(origin_y_cell);
    if state.rb(si + 0xC) < al2 {
        // [7F88/7F8B]
        return false;
    }

    let slot = match find_free_object_slot(state) {
        // [7F8D]
        Some(s) => s,
        None => return false,
    };
    let toggle = state.rb(AURA_TOGGLE) ^ 1; // [7F9D] xor [0x6bcc],1
    let off: u16 = if toggle == 0 {
        0xC0 // [7FA2/7FA4] je keeps +0xC0
    } else {
        (0u16).wrapping_sub(0xC0) // else neg
    };
    let base = OBJ_BASE + slot * OBJ_STRIDE;

    state.wb(base + 0x10, 0); // [7F92] hits
    state.wb(AURA_TOGGLE, toggle); // [7F9D]
    state.ww(base + 0x00, pv_x.wrapping_add(off)); // [7FA6/7FAB] X = playerX ± 0xC0
    state.ww(base + 0x02, pv_y.wrapping_sub(0xB0)); // [7FAD/7FB3] Y = playerY - 0xB0
    state.ww(base + 0x04, state.rw(si + 2)); // [7FB6] sprite id
    state.ww(base + 0x06, (si & 0xFFFF) as u16); // [7FBC] back-pointer
    state.wb(si + 4, 7); // [7FBF] entity mode
    state.wb(base + 0x0E, 0); // [7FC3]
    state.ww(base + 0x0A, 0); // [7FC7]
    state.ww(base + 0x08, 0); // [7FCC]
    state.wb(base + 0x0F, state.rb(si + 5)); // [7FD1] flip
    state.ww(PROJ_SLOT_PTR, base as u16); // [7F96]
    true
}

// ── dispatch_handler ────────────────────────────────────────────

/// Dispatch the 2nd-pass per-type handler (cs:[bx+0x6AC3]). Returns whether drawn.
/// Fails loud on an unknown index (no silent fallback).
fn dispatch_handler(state: &mut NativeGameState, idx: u8, si: usize, cam_x: i32, cam_y: i32) -> bool {
    match idx {
        10 => handler_ground_snap_spawn(state, si),
        0 => handler_7f6c(state, si),
        1 => handler_project_mode(state, si, cam_x, cam_y, INJECT_MODE),
        2 => handler_project_mode(state, si, cam_x, cam_y, 5),
        3 => handler_project_mode(state, si, cam_x, cam_y, 0x37),
        5 => handler_project_mode(state, si, cam_x, cam_y, 5),
        6 => handler_project_mode(state, si, cam_x, cam_y, 5),
        7 => handler_project_mode(state, si, cam_x, cam_y, 5),
        8 => handler_project_mode(state, si, cam_x, cam_y, 5),
        4 => handler_7ebf(state, si, cam_x, cam_y),
        9 => {
            let level = state.rb(LEVEL);
            handler_7e97(state, si, cam_x, cam_y, level)
        }
        11 => handler_7d6e(state, si, cam_x, cam_y),
        12 => handler_7d1b(state, si, cam_x, cam_y),
        _ => panic!("second-pass walker: unrecovered handler index {}", idx),
    }
}

// ── 6913..698B: second_pass_tick ────────────────────────────────

/// Compose the SECOND per-frame pass (1030:6913..698B) — the variable-stride entity-list
/// walk + per-type dispatch + anim-frame resolve.
///
/// Walks the list at ``0x8489``: per entry, skip if stride ``[si] >= 0x32`` (end),
/// ``[si+2]==-1`` (empty), ``[si+4] & 4``, or (``[0xB198]!=1`` and ``[si+1] & 0x80``).
/// Otherwise dispatch handler index ``[si+1] & 0x7F`` (it projects the entity into a free
/// object slot + sets the mode), and on a draw resolve the anim-frame descriptor
/// (``lookup_anim_frame``) into the projected slot's ``[+0xC]`` (``di=[0xA32E]``). Advance
/// ``si`` by the stride.
///
/// Returns the final ``si`` cursor at the terminator entry (the ASM's 698B si).
pub fn second_pass_tick(state: &mut NativeGameState) -> u16 {
    let cam_x = state.rw(CAM_COL_WORD) as i32;
    let cam_y = state.rw(CAM_ROW_WORD) as i32;
    let b198 = state.rb(MODE_COPY); // [0xB198]

    let mut si: usize = ENTITY_LIST;
    loop {
        let stride = state.rb(si); // [6916] read the record length once
        if stride >= ENTITY_STRIDE_END {
            // [691B..] the terminator entry ends the walk
            return si as u16;
        }
        let sprite_ref = state.rw(si + 2);
        let mode = state.rb(si + 4);
        let flags1 = state.rb(si + 1);
        let empty = sprite_ref == 0xFFFF; // [691B] sprite ref == -1
        let skip_flag = (mode & 4) != 0; // [6921] skip bit
        let off_screen_cull = (flags1 & 0x80) != 0; // [6927/692E]
        let skip = empty || skip_flag || (b198 != 1 && off_screen_cull);

        if !skip {
            let idx = flags1 & 0x7F; // [6934/6939]
            let drawn = dispatch_handler(state, idx, si, cam_x, cam_y);
            if drawn {
                // [6952] CF==0 -> resolve the anim frame
                let desc = lookup_anim_frame(state, sprite_ref, idx); // [6954..697B]
                let di = state.rw(PROJ_SLOT_PTR) as usize; // [697D] di = [0xA32E]
                state.ww(di + 0x0C, desc); // [6981] [di+0xC] = descriptor
            }
        }
        si = (si + stride as usize) & 0xFFFF; // [6984] advance by the (positive) stride
    }
}
