// SPDX-License-Identifier: GPL-3.0-only
//! The native (VM-less) render entry — turn a NativeGameState into a displayed frame.
//!
//! The recovered faithful renderer reconstructs the frame from the HIGH-LEVEL game state (camera,
//! tiles, objects, palette, HUD) — not from the ASM render cluster's draw list — so it runs directly
//! over a NativeGameState: the bridge readers only touch `mem.data` (which NativeGameState exposes).
//! The only things it needs beyond the game-state image are the VGA hardware pieces — the palette
//! (`dos`) and the on-screen page (`ega_display_start`) — which a standalone runtime owns instead
//! of the emulated VGA.
//!
//! This is the seam between the recovered gameplay sim (NativeGameState) and the recovered renderer:
//! gameplay produces the state, `native_render` produces the pixels. See engine/loop.rs for the
//! gameplay side.

use crate::engine::state::{NativeGameState, DS_BASE};
use crate::engine::vga::{NativeVGA, dac8};
use crate::recovered::frame_renderer::calc_scroll_source;
use crate::recovered::animation::advance_animation;
use crate::recovered::particles::{PARTICLE_BASE, PARTICLE_COUNT, PARTICLE_STRIDE, COS_TABLE, SIN_TABLE};
use crate::views::dgroup_view::PlayerGlobals;
use crate::views::gameplay_effects::{
    capture_gameplay_effects, ForegroundState, ParticleFrame,
};
use crate::views::game_visual_state::{capture_game_visual_state, render_game_visual_state};
use crate::views::render_state::load_hud_chrome_ref;

/// The tile-ring moduli (see bridge/frame.py).
const RING_COLS: usize = 0x14;
const RING_ROWS: usize = 0x0C;

/// [asm 6791] the fixed "lights off" dark 16-colour palette (6-bit RGB).
const LIGHT_DARK_PAL: usize = 0xACB7;

/// The per-level palette pointer table base.
const PALETTE_PTR_TABLE: usize = 0x2D00;
/// [0x2D8A] the current level id.
const LEVEL_ID: usize = 0x2D8A;

// Light-fade DGROUP offsets (LightFadeView).
const LIGHT_TO_DARK: usize = 0x6C01;
const LIGHT_TO_LIGHT: usize = 0x6C02;
const LIGHT_STEP: usize = 0x6C03;
const LIGHTS_OFF: usize = 0x6C04;

// Particle read inputs.
const CAM_COL: usize = 0x2DE4;
const CAM_ROW: usize = 0x2DE6;
const Y_BIAS: usize = 0x6BC4;
const DEAD_SLOT: u16 = 0xFFFF;

// Foreground-tile pass layout.
const FG_LIST_BASE: usize = 0x4F0A;
const FG_LIST_END: usize = 0x5732;
const FG_LIST_STRIDE: usize = 0x12;
const FG_NUM_TILES: usize = 256;
const DATA_SEG: usize = 0x1A0F;

/// Sign-extend a 16-bit value to i32.
#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

// ── native_load_dac_palette ────────────────────────────────────────────────────────

/// [asm 0bdc] The game's palette-set primitive: load `count` 6-bit DAC triples from DGROUP
/// `table_off` into DAC colours 0..count-1 (`int 10h ax=0x1012, bx=0, cx=count` from `DS:table_off`).
/// The runner owns the DAC, so it reproduces the load. Used by the per-level palette (0ba0) and the
/// OLDIES screen (0xb92 -> the green/yellow table at 0x287e). Values are 6-bit; the VGA DAC expands
/// them (`dac8`).
pub fn native_load_dac_palette(
    state: &NativeGameState,
    dos: &mut NativeVGA,
    table_off: usize,
    count: usize,
) {
    // (vga_palette is always a full 256-entry array in Rust — no padding needed)
    for i in 0..count {
        let off = i * 3; // the 6-bit RGB triple in the DGROUP palette table
        let rg = state.rw((table_off + off) & 0xFFFF); // r=low byte, g=high byte (adjacent)
        let r = dac8((rg & 0xFF) as u8);
        let g = dac8(((rg >> 8) & 0xFF) as u8);
        let b = dac8(state.rb((table_off + off + 2) & 0xFFFF));
        dos.vga_palette[i] = [r, g, b];
    }
}

// ── native_load_level_palette ──────────────────────────────────────────────────────

/// [asm 0ba0] Apply the per-level 16-colour VGA palette the standalone runner owns.
///
/// PRE2 is a 16-colour planar game; each level has its own 16-entry palette. `[0x2d8a]` (the level)
/// indexes the pointer table `[0x2d00+level*2]`, whose 16 RGB triples (6-bit DAC) load into DAC
/// colours 0..15 — exactly what 0ba0 does via int 10h ax=0x1012/cx=0x10
/// (`native_load_dac_palette`). native_level_init skips 0ba0 as 'render' (it touches no DGROUP, only
/// the DAC), so without this a different `--level` shows the bootstrap snapshot's palette. The
/// per-level palettes are global in DGROUP, so this just selects the right one.
pub fn native_load_level_palette(state: &NativeGameState, dos: &mut NativeVGA) {
    let level = state.rb(LEVEL_ID) as usize;
    let table_off = state.rw((PALETTE_PTR_TABLE + level * 2) & 0xFFFF) as usize;
    native_load_dac_palette(state, dos, table_off, 0x10);
}

// ── native_sync_render_state ───────────────────────────────────────────────────────

/// Maintain the render-ONLY scroll state the VM-less gameplay step leaves stale.
///
/// The faithful renderer was built over the VM, where the ASM render cluster (35A1/3A27)
/// re-derives the tile-ring indices [0x2DE8]/[0x2DEA] (= camera_x % 0x14 / camera_y % 0x0C) and the
/// previous-camera cells [0x2DE0]/[0x2DE2] from the camera every frame. `native_camera_follow`
/// advances the camera ([0x2DE4]/[0x2DE6]) but not these render mirrors, so a standalone runner must
/// re-derive them here — otherwise the renderer reads a stale ring index and the tiles corrupt the
/// moment the camera scrolls. (Render-only, so it lives outside `native_gameplay_frame` and never
/// perturbs the byte-exact gameplay verify.)
pub fn native_sync_render_state(state: &mut NativeGameState) {
    let mut g = PlayerGlobals::new(state);
    let cam_x = g.cam_col_word();
    let cam_y = g.cam_row_word();

    // the render-mirror ring indices + prev-camera cells
    g.set_col_ring((cam_x as usize % RING_COLS) as u16);
    g.set_row_ring((cam_y as usize % RING_ROWS) as u16);
    g.set_grid_dirty_token(cam_x); // 0x2DE0: a union with the 0x55AA grid-dirty sentinel
    // 0x2DE2: prev_cam_cell_y (no PlayerGlobals setter — write directly)
    g.state.ww(0x2DE2, cam_y);

    // Also re-derive the scroll-copy SOURCE [0x2DBA] (the ring offset build_background_ring places
    // tiles at) from the freshly-derived ring indices. native_camera_follow sets it during normal
    // gameplay, but a camera JUMP that bypasses the follow — the death-respawn checkpoint
    // (native_3af2) or a level start at a non-origin camera — leaves it pointing at the OLD ring
    // position, so the rebuild lays the correct tiles at the wrong column offset (the level-6
    // respawn glitch: a narrow tree top rendered full-width + a garbage band).
    let col_ring = g.col_ring() as usize;
    let row_ring = (g.row_ring() & 0xFF) as usize;
    let src = (calc_scroll_source(col_ring, row_ring) & 0xFFFF) as u16;
    g.set_scroll_copy_src(src);

    // Advance the animated-tile remap cycle (1030:367D) — the render-cluster step the gameplay pass
    // omits. The original engine steps [0x6BC2]/[0x6BD4] once per redraw, BEFORE the grid walk reads
    // the current remap table, so this must run once per displayed frame to keep animated tiles
    // (waving foliage, water, etc.) synchronized with the per-redraw cadence.
    let fp = g.anim_remap_ptr(); // the animated-tile remap frame pointer + threshold
    let thr = g.anim_remap_thresh();
    let active = g.page_dirty() != 0;
    let speed = g.friction();
    let (new_fp, new_thr, _advanced) = advance_animation(fp, thr, active, speed);
    g.set_anim_remap_ptr(new_fp);
    g.set_anim_remap_thresh(new_thr);
}

// ── native_apply_palette_fade ──────────────────────────────────────────────────────

/// [asm 6772] The per-frame gameplay DAC fade between the level palette `[0x2D00+level]` and the dark
/// "lights off" palette `0xACB7`, driven by the light pickups (player_interaction 876C/8790:
/// `[0x6C01]`=fade to dark, `[0x6C02]`=fade to the level palette, `[0x6C03]`=step, `[0x6C04]`=LIGHT_STATE
/// resting bit).
///
/// The pass is SPLIT along the state/render seam: the tick-owned half — the [0x6C03] step increment +
/// the fade-complete flag clear — runs in the gameplay frame (`loop.native_light_fade_step`, [asm
/// 0267]); this render half only READS that state and reproduces the DAC ramp on `dos.vga_palette`
/// (colours 0..15). It is idempotent per render call, so the fade advances per game TICK exactly like
/// the VM (it previously advanced per render call, i.e. at --fps rate, and its state was invisible to
/// state-only verification — found by the safe-hooks demo 230900 at tick 382). No-op in the common
/// case (no fade, lights on).
pub fn native_apply_palette_fade(state: &NativeGameState, dos: &mut NativeVGA) {
    let to_dark = state.rb(LIGHT_TO_DARK);
    let to_light = state.rb(LIGHT_TO_LIGHT);
    let active = (to_dark | to_light) != 0;

    let level = state.rb(LEVEL_ID) as usize;
    let level_palette = state.rw((PALETTE_PTR_TABLE + level * 2) & 0xFFFF) as usize;

    let mut dac = [0u8; 0x30]; // 16 colours x 3

    if active {
        let step = state.rb(LIGHT_STEP); // [asm 677B]'s inc already ran in the tick
        // [asm 6787/6791]
        let mut src = level_palette;
        let mut dst = LIGHT_DARK_PAL;
        if to_light != 0 {
            // [asm 6799-67A0] fading BACK -> swap
            std::mem::swap(&mut src, &mut dst);
        }
        for k in 0..0x30 {
            // [asm 67A2-67C6] 16 colours x 3
            let s = state.rb((src + k) & 0xFFFF) as i16;
            let b = state.rb((dst + k) & 0xFFFF) as i16;
            let diff = s - b;
            if diff.abs() > step as i16 {
                // [asm 67B3 ja] ramp s toward b
                dac[k] = if diff >= 0 {
                    (s - step as i16) as u8
                } else {
                    (s + step as i16) as u8
                };
            } else {
                // [asm 67B7] within a step -> snap to b
                dac[k] = b as u8;
            }
        }
    } else if state.rb(LIGHTS_OFF) != 0 {
        // at rest with the lights off -> the dark pal
        for k in 0..0x30 {
            dac[k] = state.rb((LIGHT_DARK_PAL + k) & 0xFFFF);
        }
    } else {
        // at rest with the lights ON -> the level pal.
        // NOT an early return: the DAC colours 0..15 must be RESTORED to the level palette here. If a
        // lights-off period ends WITHOUT a fade-back — a death/respawn resets [0x6C04]=0 with no
        // [0x6C02] fade ([asm 5237] re-init) — the DAC was last left on the dark palette and nothing
        // else rewrites it, so the screen stays dark after respawn (user: "die with the lights off and
        // it doesn't undo the light-off"). The VM's respawn reloads the level DAC; mirror it every
        // at-rest frame — idempotent with native_load_level_palette (same [0x2D00+level] source), so
        // lights-never-used levels are unchanged.
        for k in 0..0x30 {
            dac[k] = state.rb((level_palette + k) & 0xFFFF);
        }
    }

    for c in 0..0x10 {
        dos.vga_palette[c] = [
            dac8(dac[c * 3]),
            dac8(dac[c * 3 + 1]),
            dac8(dac[c * 3 + 2]),
        ];
    }
}

// ── particle read helpers ────────────────────

/// Build the common ParticleFrame fields (camera/bias/tables) from live state.
fn particle_frame_common(state: &NativeGameState, particles: Vec<(u16, u16, u8, u8)>) -> ParticleFrame {
    ParticleFrame {
        particles,
        cam_col: state.rw(CAM_COL),
        cam_row: state.rw(CAM_ROW),
        y_bias: state.rb(Y_BIAS),
        // 256-byte signed cos slice ([0x6F90]) and sin slice ([0x7090]).
        cos: state.data[(DS_BASE + COS_TABLE)..(DS_BASE + COS_TABLE + 256)].to_vec(),
        sin: state.data[(DS_BASE + SIN_TABLE)..(DS_BASE + SIN_TABLE + 256)].to_vec(),
    }
}

/// Snapshot the active particle slots + camera/bias + the sin·cos tables (live read).
/// Call at 1030:4B8E entry (before the engine advances/kills the slots).
fn read_particles(state: &NativeGameState) -> ParticleFrame {
    let mut particles = Vec::new();
    let mut b = PARTICLE_BASE;
    for _ in 0..PARTICLE_COUNT {
        let x = state.rw(b);
        if x != DEAD_SLOT {
            // inactive slot sentinel
            particles.push((x, state.rw(b + 2), state.rb(b + 4), state.rb(b + 5)));
        }
        b += PARTICLE_STRIDE;
    }
    particle_frame_common(state, particles)
}

/// Build a ParticleFrame from a stashed raw particle-array snapshot (the bytes loop.rs saved at
/// the 4B8E entry). The particle slots are read from the snapshot; the camera/bias/tables come from
/// the current state (unchanged within the same tick).
fn read_particles_from_snapshot(state: &NativeGameState, snapshot: &[u8]) -> ParticleFrame {
    let mut particles = Vec::new();
    for k in 0..PARTICLE_COUNT {
        let b = k * PARTICLE_STRIDE;
        if b + 6 > snapshot.len() {
            break;
        }
        let x = (snapshot[b] as u16) | ((snapshot[b + 1] as u16) << 8);
        if x == DEAD_SLOT {
            continue;
        }
        let y = (snapshot[b + 2] as u16) | ((snapshot[b + 3] as u16) << 8);
        let angle = snapshot[b + 4];
        let speed = snapshot[b + 5];
        particles.push((x, y, angle, speed));
    }
    particle_frame_common(state, particles)
}

// ── foreground read helper ───────────

/// Read the foreground-tile pass state (1030:3721) from VM memory. Layout only.
///
/// The pass runs with ds=1A0F. The tile-index grid is in the tilemap segment word[0x2DDA]; the tile
/// graphics are in segment word[0x003b] at word[0x8167 + tile*2] << 7; the foreground-flag table is
/// [0x805E..].
fn read_foreground_state(state: &NativeGameState, display_page: usize) -> Option<ForegroundState> {
    let mut sprites = Vec::with_capacity(16);
    let mut off = FG_LIST_BASE;
    while off < FG_LIST_END {
        let sid = state.rw(off + 4);
        if sid != 0xFFFF && (sid & 0x2000) != 0 {
            let x = s16(state.rw(off) as i32);
            let y = s16(state.rw(off + 2) as i32);
            sprites.push((x, y, sid));
        }
        off += FG_LIST_STRIDE;
    }
    if sprites.is_empty() {
        return None;
    }

    let grid_seg = state.rw(0x2DDA) as usize;
    let gbase = (grid_seg << 4) & 0xFFFFF;
    let grid_slice = &state.data[gbase..gbase + 0x10000];

    let fbase = ((DATA_SEG << 4) + 0x805E) & 0xFFFFF;
    let flag_slice = &state.data[fbase..fbase + FG_NUM_TILES];

    // Fast pre-check on live slices: if no foreground-flagged tile (`& 0x40`) exists near
    // any active sprite, skip allocating the 128 KB grid/gfx buffers altogether.
    let cam_row = state.rb(0x2DE6) as usize & 0xFF;
    let cam_col = state.rb(0x2DE4) as usize & 0xFF;
    let mut any_fg = false;
    'outer: for &(x, y, _sid) in &sprites {
        let row = (((y >> 4) - 1) & 0xFF) as u16;
        let col = ((x >> 4) & 0xFF) as u16;
        let mut di = (row << 8) | col;
        let mut sch = (row.wrapping_sub(cam_row as u16)) & 0xFF;
        let scl_raw = (col.wrapping_sub(cam_col as u16)) & 0xFF;
        let scl = if scl_raw >= 0x80 { scl_raw as i32 - 0x100 } else { scl_raw as i32 };
        let mut rows = 3;
        if y & 0xF != 0 {
            di = di.wrapping_add(0x100);
            sch = (sch.wrapping_add(1)) & 0xFF;
            rows += 1;
        }
        for _ in 0..rows {
            if sch >= 0xB {
                di = di.wrapping_sub(0x100);
                continue;
            }
            sch = (sch.wrapping_sub(1)) & 0xFF;
            for &(cond, cell) in &[
                (scl >= 2, di.wrapping_sub(2)),
                (scl >= 1, di.wrapping_sub(1)),
                (0 <= scl && scl < 0x14, di),
                (-1 <= scl && scl < 0x13, di.wrapping_add(1)),
            ] {
                if cond {
                    let tile = grid_slice[cell as usize];
                    if (flag_slice[tile as usize] & 0x40) != 0 {
                        any_fg = true;
                        break 'outer;
                    }
                }
            }
            di = di.wrapping_sub(0x100);
        }
    }
    if !any_fg {
        return None;
    }

    let grid = grid_slice.to_vec();
    let flag_tbl = flag_slice.to_vec();
    let gfx_index: Vec<u16> = (0..FG_NUM_TILES)
        .map(|t| state.rw(0x8167 + t * 2))
        .collect();
    let gfx_seg = state.rw(0x003B) as usize;
    let gxb = (gfx_seg << 4) & 0xFFFFF;
    let gfx = state.data[gxb..gxb + 0x10000].to_vec();

    Some(ForegroundState {
        sprites,
        grid,
        flag_tbl,
        gfx,
        gfx_index,
        cam_col: state.rb(0x2DE4),
        cam_row: state.rb(0x2DE6),
        page: (display_page & 0xFFFF) as u16,
        y_bias: state.rb(0x6BC4),
    })
}

// ── native_render ──────────────────────────────────────────────────────────────────

/// The options bag for `native_render` — the mid-frame overlay captures.
pub struct RenderOptions {
    /// The pre-consume particle snapshot (stashed at 4B8E entry), or None for a live read.
    pub particle_capture: Option<ParticleFrame>,
    /// The foreground-tile layer capture (3721), or None for a live read.
    pub foreground_capture: Option<ForegroundState>,
    /// Override the SceneKind heuristic to GAMEPLAY.
    pub force_gameplay: bool,
    /// Skip the ~7ms faithful raster (the enhanced/interpolation path rebuilds the frame itself).
    pub skip_raster: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            particle_capture: None,
            foreground_capture: None,
            force_gameplay: false,
            skip_raster: false,
        }
    }
}

/// Render one gameplay frame from `state` (a NativeGameState). `dos` carries the VGA palette +
/// registers; `display_page` is the on-screen page (`ega_display_start`). `opts.particle_capture`/
/// `opts.foreground_capture` are the mid-frame overlay captures; when not supplied (the standalone
/// path) they are read straight from the game state here, since both the FOREGROUND tile layer (3721
/// — tiles drawn IN FRONT of the player) and the one-shot point PARTICLES (4b8e) are reconstructable
/// from state native maintains (render slots, tilemap, camera, the particle list). Without this the
/// standalone runner drew only the core frame + fireflies, so the front-tile layer was missing.
/// Returns `(Option<planes>, page)` — four EGA plane buffers + the committed page, or `(None, page)`
/// when `skip_raster` (the caller uses the enhanced compose).
///
/// Composes the frame from the recovered render leaves at the commit boundary, sourced from native
/// game state (never a VM). Raises FaithfulVisualGap for non-gameplay scenes (no silent fallback).
pub fn native_render(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    display_page: usize,
    game_root: &str,
    opts: RenderOptions,
) -> (Option<[Vec<u8>; 4]>, usize) {
    // [asm 6772] the light-pickup DAC fade (skipped by the gameplay frame as 'render'); no-op idle
    native_apply_palette_fade(state, dos);

    // ── foreground capture ──────────────────────────────────────────────────────
    let mut foreground_capture = opts.foreground_capture;
    if foreground_capture.is_none() && !opts.skip_raster {
        foreground_capture = read_foreground_state(state, display_page);
    }

    // ── particle capture ─────────────────────────────────────────────────────────
    // prefer the pre-consume snapshot native_gameplay_frame stashed at the 4B8E entry (the one-shot
    // [0x7DE6] particles — spider-threads/sparkles — are KILLED by the consume, so the live read here
    // would be empty); fall back to a live read for callers that render without a preceding gameplay
    // frame.
    let pf_bytes = state.particle_capture.take(); // one-shot handoff — don't reuse a stale frame
    let pf = if opts.particle_capture.is_some() {
        opts.particle_capture.unwrap()
    } else {
        match &pf_bytes {
            Some(bytes) => read_particles_from_snapshot(state, bytes),
            None => read_particles(state), // [4b8e] one-shot point particles (live)
        }
    };
    let particle_capture = if pf.particles.is_empty() { None } else { Some(pf) };
    // non-destructive stash for SAME-tick consumers AFTER this render (the interpolation extractor
    // runs post-native_frame_step, by which time the one-shot above is already consumed).
    // Overwritten every render — None on particle-less ticks — so it can never go stale across
    // ticks.
    state.particle_capture_last = if particle_capture.is_some() { pf_bytes } else { None };

    // ── flash slots stash ────────────────────────────────────────────────────────
    // The one-frame OPAQUE flash flag (id bit14 = [+5]&0x40) on the slots
    // native_object_render_state captured before 26FA cleared it — stashed for the SAME-tick
    // extractor.
    let flash = state.flash_slots.take(); // one-shot, like particle_capture
    state.flash_slots_last = flash.clone(); // non-destructive stash (same-tick extractor)

    if opts.skip_raster {
        // The enhanced/interpolation path rebuilds the whole frame itself (its own background +
        // compositor); it needs only the effect STASHES above, not the ~7ms faithful raster. Skip it
        // (the single biggest per-tick cost when interpolation/widescreen is on). Returns no planes —
        // the caller uses the enhanced compose.
        return (None, display_page & 0xFFFF);
    }

    // ── capture effects ─────────────────────────────────────────────────────────
    let fx = capture_gameplay_effects(state, particle_capture, foreground_capture);

    // ── flash flag restore ───────────────────────────────────────────────────────
    // Re-apply the flash flag so the hit/death flash draws as the VM's solid-white silhouette; the
    // renderer state is a SNAPSHOT (capture_game_visual_state reads it synchronously), so restore
    // state.data right after — leaving the flag set would desync the next frame's carried-forward
    // state from the cleared record.
    let mut saved: Option<Vec<(u16, u8)>> = None;
    if let Some(ref flash_slots) = flash {
        let mut s = Vec::with_capacity(flash_slots.len());
        for &off in flash_slots {
            // the flags byte (opaque-flash bit) at slot offset + 5
            s.push((off, state.rb(off as usize + 5)));
        }
        saved = Some(s);
        for &off in flash_slots {
            let off = off as usize;
            state.wb(off + 5, state.rb(off + 5) | 0x40);
        }
    }

    // ── capture + render ──────────────────────────────────────────────────────────
    let hud_chrome = load_hud_chrome_ref(std::path::Path::new(game_root));

    let (planes, page) = {
        let gvs = capture_game_visual_state(
            state,
            dos.video_mode,
            display_page,
            hud_chrome,
            Some(fx),
            opts.force_gameplay,
        );
        render_game_visual_state(&gvs)
    }; // gvs dropped — the immutable borrow of state ends here

    // restore the flash flags (the game state must be clean for the next frame)
    if let Some(ref saved) = saved {
        for &(off, v) in saved {
            state.wb(off as usize + 5, v);
        }
    }

    (Some(planes), page)
}
