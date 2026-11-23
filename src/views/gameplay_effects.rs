// SPDX-License-Identifier: GPL-3.0-only
//! The gameplay effect overlays applied ON TOP of the core ``render_frame`` output.
//!
//! ``render_frame`` reproduces the background + sprites + HUD, but three effect passes draw over
//! that result each frame and are captured from VM state at their OWN hook instants (the core
//! frame can't reconstruct them at the 6772 commit because they have transient state):
//!
//!   * point particles (``4B8E``)        — one-shot; drawn+killed each frame, so snapshot at 4B8E entry
//!   * foreground tiles (``3721``)       — redraw flag-0x40 tiles OVER sprites; the active list is
//!                                        rebuilt each frame, so snapshot at the 3732 pass entry
//!   * firefly swarm (``54AB``)          — persistent slots; readable at the 6772 commit
//!
//! This module bundles those three captured states and applies their recovered draws in the
//! engine's on-top order (particles, then foreground tiles, then fireflies) so both faithful
//! render paths share ONE effect-compositing step instead of duplicating it. A ``None`` field
//! means that effect is inactive this frame.

use crate::engine::state::NativeGameState;

// --- firefly swarm layout (1030:54AB, the 0x6EA9 20-slot array, stride 8) ---
const FIREFLY_BASE: usize = 0x6EA9;
const FIREFLY_COUNT: usize = 20;
const FIREFLY_STRIDE: usize = 8;
const FIREFLY_DEAD: u16 = 0x55AA;
const FIREFLY_CAM_COL: usize = 0x2DE4;
const FIREFLY_CAM_ROW: usize = 0x2DE6;

// --- foreground-tile pass layout (1030:3721) ---
const FG_FLAG: u8 = 0x40;
const FG_PLANE_STRIDE: usize = 0x20; // tile graphic: 0x20 bytes per plane (16 rows * 2)

const VIEW_W: u16 = 0x140; // 320 — off-screen X cull
const VIEW_H: u16 = 0xB0; // 176 — off-screen Y cull
const ROW: usize = 0x28; // bytes per screen row

/// The active particles + the inputs ``draw_particles`` needs, snapshotted at 4B8E entry.
#[derive(Clone, Default)]
pub struct ParticleFrame {
    /// `(x, y, angle, speed)` for the active slots (X != 0xFFFF).
    pub particles: Vec<(u16, u16, u8, u8)>,
    pub cam_col: u16,
    pub cam_row: u16,
    pub y_bias: u8,
    /// 256-byte signed cos slice ([0x6F90]).
    pub cos: Vec<u8>,
    /// 256-byte signed sin slice ([0x7090]).
    pub sin: Vec<u8>,
}

/// Everything the foreground pass needs, lifted from VM memory by the bridge.
#[derive(Clone, Default)]
pub struct ForegroundState {
    /// active list entries `(x, y, id)` — signed x/y.
    pub sprites: Vec<(i32, i32, u16)>,
    /// tile-index grid (tilemap seg [0x2DDA]), indexed by cell.
    pub grid: Vec<u8>,
    /// [0x805E..]; flag_tbl[tile] & 0x40 -> foreground.
    pub flag_tbl: Vec<u8>,
    /// the tile-graphic segment ([0x003b]) as a byte block.
    pub gfx: Vec<u8>,
    /// `word[0x8167 + tile*2]` per tile (graphic offset >> 7).
    pub gfx_index: Vec<u16>,
    /// [0x2DE4] low byte.
    pub cam_col: u8,
    /// [0x2DE6] low byte.
    pub cam_row: u8,
    /// [0x2DD8].
    pub page: u16,
    /// [0x6BC4].
    pub y_bias: u8,
}

/// The firefly swarm slots + camera/page for the faithful renderer.
#[derive(Clone, Default)]
pub struct FireflyState {
    /// `(x, y, timer)` for the live slots (x != 0x55AA).
    pub slots: Vec<(u16, u16, u8)>,
    pub cam_col: u8,
    pub cam_row: u8,
}

/// The four effect-overlay states captured for one displayed frame (any may be `None`).
pub struct GameplayEffects {
    /// snapshotted at 4B8E entry (pre-kill).
    pub particles: Option<ParticleFrame>,
    /// snapshotted at the 3732 pass entry.
    pub foreground: Option<ForegroundState>,
    /// read at the 6772 commit (slots persist).
    pub fireflies: Option<FireflyState>,
    /// LEVELG falling snow (scroll_script_snow plot list).
    pub snow: Option<Vec<(u16, u8)>>,
}

/// Read the live firefly slots + camera for the faithful renderer.
pub fn read_fireflies(state: &NativeGameState) -> FireflyState {
    let mut slots = Vec::new();
    let mut si = FIREFLY_BASE;
    for _ in 0..FIREFLY_COUNT {
        let x = state.rw(si);
        if x != FIREFLY_DEAD {
            let y = state.rw(si + 2);
            let timer = state.rb(si + 6);
            slots.push((x, y, timer));
        }
        si += FIREFLY_STRIDE;
    }
    FireflyState {
        slots,
        cam_col: state.rb(FIREFLY_CAM_COL),
        cam_row: state.rb(FIREFLY_CAM_ROW),
    }
}

/// Bundle the stashed particle/foreground captures with the fireflies (read live at the commit).
///
/// `particle_frame`/`foreground_frame` are the one-shot captures stashed by the runtime at their
/// hook instants; `None` means that effect is inactive this frame. The LEVELG snow pixels are
/// computed by the scroll-script render half and stashed on the state (`snow_plots`); empty/absent
/// on every other level (wind `[0x6BF6]` == 0).
pub fn capture_gameplay_effects(
    state: &NativeGameState,
    particle_frame: Option<ParticleFrame>,
    foreground_frame: Option<ForegroundState>,
) -> GameplayEffects {
    let ff = read_fireflies(state);
    // The LEVELG snow pixels are computed by native_scroll_script (the 3922 render half) and
    // stashed on the state; empty/absent on every other level (wind [0x6bf6] == 0).
    let snow = state.snow_plots.clone().filter(|s| !s.is_empty());
    GameplayEffects {
        particles: particle_frame,
        foreground: foreground_frame,
        fireflies: if !ff.slots.is_empty() { Some(ff) } else { None },
        snow,
    }
}

/// Draw the active effect overlays onto ``planes`` (the rendered core frame) in engine on-top
/// order: particles, then foreground tiles, then fireflies, then snow.
pub fn apply_gameplay_effects(
    planes: &mut [Vec<u8>; 4],
    page: usize,
    fx: Option<&GameplayEffects>,
) {
    let fx = match fx {
        None => return,
        Some(fx) => fx,
    };
    if let Some(ref pf) = fx.particles {
        draw_particles(planes, pf, page);
    }
    if let Some(ref fg) = fx.foreground {
        render_foreground_tiles(planes, fg);
    }
    if let Some(ref f) = fx.fireflies {
        draw_fireflies(planes, f, page);
    }
    if let Some(ref snow) = fx.snow {
        draw_snow(planes, snow, page);
    }
}

// ---------------------------------------------------------------------------
// Effect overlay draw primitives. These write to the detached `planes` buffers
// reading from the captured frame state so the overlay matches what was on screen
// at each hook instant.
// ---------------------------------------------------------------------------

#[inline]
fn s8(v: i32) -> i32 {
    let v = v & 0xFF;
    if v & 0x80 != 0 { v - 0x100 } else { v }
}

#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

/// Advance one particle by its angle/speed [asm 4BB0-4BD2]. Returns `(nx, ny)` (wrapped 16-bit).
///
/// `nx = x + ((s8(cos[angle])>>2) * s8(speed)) >> 4` (all arithmetic shifts), `ny` the sin
/// equivalent.
fn advance_particle(x: u16, y: u16, angle: u8, speed: u8, cos: &[u8], sin: &[u8]) -> (u16, u16) {
    let sp = s8(speed as i32);
    let cx = s8(cos[(angle as usize) & 0xFF] as i32);
    let sy = s8(sin[(angle as usize) & 0xFF] as i32);
    let nx = (x as i32 + ((cx >> 2) * sp >> 4)) & 0xFFFF; // [asm 4BB0-4BC0]
    let ny = (y as i32 + ((sy >> 2) * sp >> 4)) & 0xFFFF; // [asm 4BC2-4BD2]
    (nx as u16, ny as u16)
}

/// Plot one advanced particle's white pixel if on-screen [asm 4BD5-4C1A].
fn plot_particle(
    planes: &mut [Vec<u8>; 4],
    nx: u16,
    ny: u16,
    cam_x: u16,
    cam_y: u16,
    yb: i32,
    page: usize,
) {
    let sy = (ny as i32 - yb - cam_y as i32) & 0xFFFF; // [asm 4BDA-4BE9]
    if (sy as u16) >= VIEW_H {
        // [asm 4BEB jae]
        return;
    }
    let sx = (nx as i32 - cam_x as i32) & 0xFFFF; // [asm 4BF6-4BFE]
    if (sx as u16) >= VIEW_W {
        // [asm 4C00 jae]
        return;
    }
    let off = (page + sy as usize * ROW + (sx as usize >> 3)) & 0xFFFF; // [asm 4BF0-4C12]
    let bit = 0x80u8 >> (sx as u8 & 7); // [asm 4C16]
    for p in 0..4usize {
        // [asm 4C1A xchg, OR all planes]
        planes[p][off] |= bit;
    }
}

/// Recover ``1030:4B8E`` (draw only). Plots each particle's pixel into `planes` (OR).
fn draw_particles(planes: &mut [Vec<u8>; 4], pf: &ParticleFrame, page: usize) {
    let cam_x = (pf.cam_col << 4) & 0xFFFF;
    let cam_y = (pf.cam_row << 4) & 0xFFFF;
    let yb = s8(pf.y_bias as i32);
    let page = page & 0xFFFF;
    for &(x, y, angle, speed) in &pf.particles {
        let (nx, ny) = advance_particle(x, y, angle, speed, &pf.cos, &pf.sin);
        plot_particle(planes, nx, ny, cam_x, cam_y, yb, page);
    }
}

#[inline]
fn sar(v: i32, n: u32) -> i32 {
    // arithmetic right shift of a 16-bit value (sign-extended)
    let v = s16(v);
    v >> n
}

/// OR each live firefly's flicker pixel into `planes` (4 EGA planes) on `page` (color 15).
fn draw_fireflies(planes: &mut [Vec<u8>; 4], f: &FireflyState, page: usize) {
    let cam_x = (f.cam_col as u16) << 4 & 0xFFFF; // cam_col*16
    let cam_y = (f.cam_row as u16) << 4 & 0xFFFF;
    let page = page & 0xFFFF;
    for &(x, y, _timer) in &f.slots {
        let sy = ((sar(y as i32, 3) - cam_y as i32) & 0xFFFF) as u16;
        if sy >= 0xB0 {
            // cmp ax,0xb0 ; jae  (unsigned)
            continue;
        }
        let sx = ((sar(x as i32, 3) - cam_x as i32) & 0xFFFF) as u16;
        if sx >= 0x140 {
            // cmp ax,0x140 ; jae
            continue;
        }
        let off = (page + (sy as usize & 0xFF) * ROW + (sx as usize >> 3)) & 0xFFFF;
        let bit = 0x80u8 >> (sx as u8 & 7);
        planes[0][off] |= bit;
        planes[1][off] |= bit;
        planes[2][off] |= bit;
        planes[3][off] |= bit;
    }
}

/// OR the ``scroll_script_snow`` pixels into `planes` (4 EGA planes) on `page`.
///
/// Each plot is `(page_relative_byte_offset, bit_mask)`; the mask is OR'd into every plane, so the
/// pixel is colour 15 (white) — matching ``xchg es:[bx], al`` under map-mask 0x0F + GC OR mode.
fn draw_snow(planes: &mut [Vec<u8>; 4], plots: &[(u16, u8)], page: usize) {
    for &(off, mask) in plots {
        let a = (page + off as usize) & 0xFFFF;
        planes[0][a] |= mask;
        planes[1][a] |= mask;
        planes[2][a] |= mask;
        planes[3][a] |= mask;
    }
}

/// Yield `(tile, cell)` for every foreground tile 3721/3732 would redraw, in ASM order.
fn select_foreground_cells(fg: &ForegroundState) -> Vec<(u8, u16)> {
    let mut out = Vec::new();
    let cam_row = fg.cam_row as usize & 0xFF;
    let cam_col = fg.cam_col as usize & 0xFF;
    for &(x, y, sid) in &fg.sprites {
        if sid == 0xFFFF {
            continue;
        }
        if sid & 0x2000 == 0 {
            // bh & 0x20 -> bit 13 of the id word [asm 3747]
            continue;
        }
        // base cell = (tile_row - 1)*0x100 + tile_col   [asm 3751-375D]
        let row = (((y >> 4) - 1) & 0xFF) as u16;
        let col = ((x >> 4) & 0xFF) as u16;
        let mut di = (row << 8) | col;
        let mut sch = (row.wrapping_sub(cam_row as u16)) & 0xFF; // ch = screen tile row
        let scl_raw = (col.wrapping_sub(cam_col as u16)) & 0xFF; // cl = screen tile col (signed via 0x80 wrap)
        let scl = if scl_raw >= 0x80 {
            scl_raw as i32 - 0x100
        } else {
            scl_raw as i32
        };
        let mut rows = 3;
        if y & 0xF != 0 {
            // sub-tile Y -> one extra row, shifted down [asm 376C]
            di = di.wrapping_add(0x100);
            sch = (sch.wrapping_add(1)) & 0xFF;
            rows += 1;
        }
        for _ in 0..rows {
            if sch >= 0xB {
                // off the bottom of the viewport [asm 3779]
                di = di.wrapping_sub(0x100);
                continue;
            }
            sch = (sch.wrapping_sub(1)) & 0xFF; // [asm 377E]
            // the four horizontal neighbours, each gated by the screen column [asm 3782..37E0]
            if scl >= 2 {
                maybe_fg(&fg.grid, &fg.flag_tbl, di.wrapping_sub(2), &mut out);
            }
            if scl >= 1 {
                maybe_fg(&fg.grid, &fg.flag_tbl, di.wrapping_sub(1), &mut out);
            }
            if 0 <= scl && scl < 0x14 {
                maybe_fg(&fg.grid, &fg.flag_tbl, di, &mut out);
            }
            if -1 <= scl && scl < 0x13 {
                maybe_fg(&fg.grid, &fg.flag_tbl, di.wrapping_add(1), &mut out);
            }
            di = di.wrapping_sub(0x100);
        }
    }
    out
}

fn maybe_fg(grid: &[u8], flag: &[u8], cell: u16, out: &mut Vec<(u8, u16)>) {
    let tile = grid[(cell & 0xFFFF) as usize];
    if (flag[tile as usize] & FG_FLAG) != 0 {
        out.push((tile, cell));
    }
}

/// Blit one foreground tile (1030:37F7): color-0-keyed transparent tile blit.
fn blit_tile(planes: &mut [Vec<u8>; 4], tile: u8, cell: u16, fg: &ForegroundState) -> bool {
    let gfx_off = ((fg.gfx_index[tile as usize] as usize) << 7) & 0xFFFF;
    let screen_row = ((cell >> 8) as usize).wrapping_sub(fg.cam_row as usize & 0xFF) & 0xFF;
    let screen_col = (cell as usize & 0xFF).wrapping_sub(fg.cam_col as usize & 0xFF) & 0xFFFF;
    let mut di = ((screen_row * 0x280) + (screen_col * 2) + fg.page as usize) & 0xFFFF;
    di = (di.wrapping_sub(0x28 * fg.y_bias as usize)) & 0xFFFF;
    // [asm 3835-3846] destination clip: only blit if page <= di < page+0x1900
    let page = fg.page as usize & 0xFFFF;
    let pe = (page + 0x1900) & 0xFFFF;
    if !(page <= di && di < pe) {
        return false;
    }
    let gfx = &fg.gfx;
    for r in 0..16usize {
        let rowbase = (gfx_off + r * 2) & 0xFFFF;
        let mut pl = [0u16; 4];
        for p in 0..4 {
            let lo = gfx[(rowbase + p * FG_PLANE_STRIDE) & 0xFFFF] as u16;
            let hi = gfx[(rowbase + p * FG_PLANE_STRIDE + 1) & 0xFFFF] as u16;
            pl[p] = lo | (hi << 8);
        }
        let mask = pl[0] | pl[1] | pl[2] | pl[3]; // opaque footprint
        let keep = !mask & 0xFFFF;
        let o = di & 0xFFFF;
        let o2 = (di + 1) & 0xFFFF;
        for p in 0..4 {
            let cur = planes[p][o] as u16 | ((planes[p][o2] as u16) << 8);
            let val = (cur & keep) | pl[p];
            planes[p][o] = (val & 0xFF) as u8;
            planes[p][o2] = ((val >> 8) & 0xFF) as u8;
        }
        di = (di + 0x28) & 0xFFFF;
    }
    true
}

/// Apply the foreground pass onto `planes` (4 EGA planes) — recover 3721 + 37F7 together.
///
/// Returns the number of tiles blitted (the live replacement uses it to pick the EGA exit state:
/// the 37F7 blit leaves the sequencer map-mask / read-map at plane 3, so only when a blit ran).
fn render_foreground_tiles(planes: &mut [Vec<u8>; 4], fg: &ForegroundState) -> usize {
    let mut n = 0;
    for (tile, cell) in select_foreground_cells(fg) {
        if blit_tile(planes, tile, cell, fg) {
            // clipped tiles don't blit (and don't set the EGA exit state)
            n += 1;
        }
    }
    n
}
