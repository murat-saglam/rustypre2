// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 renderer — the consolidated, VM-independent frame entry.
//!
//! This is the **replaceable-renderer seam**. `render_frame` composes the recovered
//! renderer leaves in the exact per-frame order the original runs them (validated in-VM
//! on gameplay snapshots 185902/212037 — see `docs/pre2/renderer_status.md`):
//!
//! ```text
//! palette fade (6772) -> animated-grid redraw (3668) -> grid redraw (35A1)
//!     -> scroll copy (3A27)   [-> moving-sprite pass 26FA, layered by the caller]
//! ```
//!
//! `RendererState` is the stable INPUT contract: a plain-data snapshot of everything the
//! renderer reads (tile map + attribute tables, camera/scroll bookkeeping, the current
//! animation-frame remap, and the palette-fade step). It is reconstructed from game state
//! by `views::render_state` (read-only, one place).
//!
//! Border (confirmed by profiling, see renderer_island.md): `render_frame` produces the
//! **renderer-owned** pixels — the scrolling tile background, the palette, and the
//! moving-sprite *list* pass. The **object system** (`65A0`/`8BFF` iterating the
//! ObjectSlot data model) layers the gameplay sprites on top via the *shared* blit; it
//! owns gameplay state and is deliberately NOT part of the renderer.

use crate::recovered::frame_renderer::*;
use crate::recovered::hud::{self, HudState};
use crate::recovered::object_render::{self, Camera, Sprite, SpriteAttr};
use crate::recovered::render_model::HudChromeAsset;
use crate::recovered::transition;

/// Planar ASSET region (per 64 KiB EGA plane): the tile-graphic cache (0x5E80) + the
/// parallax base layer (0x7E80) sit above the scroll ring (0x3F40..0x5E00). Everything
/// from here to the end of the plane is level asset data the tile draws read —
/// captured as `RendererState.asset_planes`.
pub const ASSET_LO: usize = 0x5E80;
pub const ASSET_HI: usize = 0x10000;

/// One palette-fade step's resolved inputs (direction already applied): step the
/// 6-bit palette `a` toward `b` by `amount`. `None` when no fade is active.
#[derive(Clone, Default)]
pub struct FadeStep {
    pub a: Vec<u8>,
    pub b: Vec<u8>,
    pub amount: u8,
}

/// Stable, VM-independent input contract for one rendered frame.
///
/// Plain data only (no `mem`); reconstructed by `pre2.views.render_state`.
pub struct RendererState<'a> {
    // --- tile map + per-tile attribute tables (all indexed by tile id) ---
    /// Level tile indices (row-major, stride 0x100).
    pub tiles: &'a [u8],
    /// `1A0F:0x805E` — OR'd into `[0x2DF2]` over the grid.
    pub type_tbl: &'a [u8],
    /// `1A0F:0x6988` — animated-tile draw flag.
    pub flag_tbl: &'a [u8],
    /// `1A0F:0x4DF8` — sprite/tile transparency class.
    pub blit_type: &'a [u8],
    /// `1A0F:0x2DF8` — type>=2 transparency masks.
    pub mask_region: &'a [u8],
    /// Current animation frame remap (`[[0x6BC2] .. +256]`).
    pub anim_xlat: &'a [u8],
    // --- camera / scroll bookkeeping ---
    /// `[0x2DE4]` tiles.
    pub camera_x: u8,
    /// `[0x2DE6]` tiles.
    pub camera_y: u8,
    /// `[0x2DE0]` (dirty compare; 0x55AA sentinel forces redraw).
    pub prev_x: u8,
    /// `[0x2DE2]`.
    pub prev_y: u8,
    /// `[0x2DE8]` column ring index / animated-grid fine column.
    pub col_ring: usize,
    /// `[0x6BC4]` sub-tile pixel scroll.
    pub fine_scroll: usize,
    /// `[0x2DEA]` row ring index.
    pub row_ring: usize,
    /// `[0x2DBA]` source offset into the ring buffer.
    pub scroll_src: usize,
    /// `[0x2DD8]` destination page (back buffer).
    pub dest_page: usize,
    /// `[0x6BF8]` scroll row-stride factor.
    pub row_factor: usize,
    /// `[0x2DF4]`.
    pub dirty: u8,
    /// `[0x2DF5]`.
    pub dirty_rows: u8,
    /// Palette fade step, or `None` when inactive.
    pub fade: Option<FadeStep>,
    /// Status-bar values score/lives/energy (`render_model.HudState`).
    pub hud_state: Option<HudState>,
    /// Static HUD chrome assets (bar bitmap + glyph font).
    pub hud_chrome: Option<&'a HudChromeAsset>,
    /// 4x bytes of `plane[ASSET_LO:0x10000]`, or `None` to use planes as-is.
    pub asset_planes: Option<[&'a [u8]; 4]>,
    pub boss_glyph_tiles: Option<[u8; 42]>,
    // --- moving-sprite pass (26FA); object_camera None => skip it ---
    /// Object-render Camera (frame counter post-incremented).
    pub object_camera: Option<Camera>,
    /// The active-sprite list (object_render.Sprite records).
    pub object_sprites: Vec<Sprite>,
    /// `sprite_id -> object_render.SpriteAttr`.
    pub object_attrs: Option<Box<dyn Fn(u16) -> Option<SpriteAttr> + 'a>>,
    /// `src_seg -> 64 KiB sprite-pixel segment bytes`.
    pub object_src_banks: Option<Box<dyn Fn(usize) -> &'a [u8] + 'a>>,
}

/// Result of the grid redraw.
pub struct FrameResult;

/// Adapt `RendererState` to the `TileMapView` shape `draw_grid` expects (it reads
/// tile indices + the two attribute tables it ORs and dispatches on). Keeps
/// `RendererState` flat while reusing the verified draw.
fn tilemap_view<'a>(s: &RendererState<'a>) -> TileMapView<'a> {
    TileMapView {
        tiles: s.tiles,
        tile_flags: s.type_tbl,  // draw_grid ORs this into [0x2DF2]
        tile_type: s.blit_type,  // draw_grid dispatches the blit on this
        plane_attr: &[],         // not used by draw_grid
    }
}

/// `[asm 6C0D]` the fixed staging page the boss glyph is blitted into.
const GLYPH_PAGE: usize = 0x3F40;
/// `[asm 6C10]` byte offset within the row -> x = 192 px.
const GLYPH_XBYTE: usize = 0x18;

/// `[asm 6C0D..6CA6]` The mode-9 final-boss glyph blit: a 6-wide x 7-tall grid of LEVEL TILES
/// (16x16), each a write-mode-1 latched copy of 16 rows x 2 bytes from the plane-resident
/// tile cache (`0x5E80 + idx*32` — the same cache the background draws from) into the
/// staging page at `0x3F40 + 0x18`. `tiles` = the 42 index bytes, row-major (the bridge
/// reads them from the level segment at `[glyph*6 + 0x14]` with the ASM's 0xFA row stride).
fn draw_boss_glyph(planes: &mut [Vec<u8>; 4], tiles: &[u8]) {
    for row in 0..7 {                                      // [asm 6C9F] ch=7 tile rows
        for col in 0..6 {                                  // [asm 6C93] cl=6 tile columns
            let src = ASSET_LO + ((tiles[row * 6 + col] as usize) << 5);  // [asm 6C2F-6C3C] idx*32 + 0x5E80
            let dst = GLYPH_PAGE + GLYPH_XBYTE + row * 16 * 0x28 + col * 2;  // [asm 6C40/6C9B] strides
            for p in 0..4 {                                // write mode 1: all four planes latched
                let pl = &mut planes[p];
                let (mut s0, mut d0) = (src, dst);
                for _ in 0..16 {                           // [asm 6C3E..6C8B] 16 rows x 2 bytes
                    pl[d0] = pl[s0];
                    pl[d0 + 1] = pl[s0 + 1];
                    s0 += 2;
                    d0 += 0x28;
                }
            }
        }
    }
}

/// Render one frame's **renderer-owned** output into `planes` (and `dac`).
///
/// `planes` is the four EGA plane buffers (the scrolling ring buffer + the visible
/// pages + the sprite cache all live within them, exactly as in VRAM). `dac`, if
/// given, is a 16-entry list of `[r, g, b]` 6-bit DAC colours updated by the fade.
///
/// Composes the recovered leaves in the original per-frame order. Returns the
/// `GridResult` from the grid redraw (its dirty / prev-camera contract), so the
/// caller can persist it if running statefully.
///
/// The moving-sprite pass (`1030:26FA object_render`) and the object system are layered
/// by the caller on top of this result (see the module docstring's border note).
pub fn render_frame<'a>(
    state: &RendererState<'a>,
    planes: &mut [Vec<u8>; 4],
    dac: Option<&mut [[u8; 3]; 16]>,
    rebuild: bool,
) -> Option<FrameResult> {
    let s = state;

    // 0) restore the planar ASSET data (tile cache @ 0x5E80 + parallax base @ 0x7E80) into the
    //    framebuffer, so the tile draws below find their graphics even on a clean framebuffer.
    if let Some(asset) = s.asset_planes {
        for p in 0..4 {
            let len = asset[p].len().min(ASSET_HI - ASSET_LO);
            planes[p][ASSET_LO..ASSET_LO + len].copy_from_slice(&asset[p][..len]);
        }
    }

    // 1) palette fade — 6772 (DAC only; no plane effect)
    if let Some(ref fade) = s.fade {
        if let Some(dac) = dac {
            let (out, _done) = transition::fade_palette(&fade.a, &fade.b, fade.amount);
            for i in 0..16 {
                dac[i][0] = out[3 * i] & 0x3F;
                dac[i][1] = out[3 * i + 1] & 0x3F;
                dac[i][2] = out[3 * i + 2] & 0x3F;
            }
        }
    }

    let grid_result;
    if rebuild {
        // FULL rebuild from a clean ring: draw EVERY tile (incl. type-0 opaque) into the ring
        // from RendererState + assets, then advance animated (type-0) tiles to the current
        // frame. This is the explicit init/full-redraw path that removes the per-frame reliance
        // on the ASM-built ring. The normal incremental path (else) is unchanged.
        let tmv = tilemap_view(s);
        build_background_ring(
            planes, &tmv, s.camera_x, s.camera_y, s.scroll_src, s.col_ring,
            s.fine_scroll, s.blit_type, s.mask_region,
        );
        redraw_animated_grid(
            planes, s.tiles, s.type_tbl, s.flag_tbl, s.anim_xlat, s.blit_type,
            s.camera_x, s.camera_y, s.col_ring, s.scroll_src,
        );
        grid_result = None;

    } else {
        // 2) animated-grid redraw — 3668 (redraw the animated background tiles into the ring)
        redraw_animated_grid(
            planes, s.tiles, s.type_tbl, s.flag_tbl, s.anim_xlat, s.blit_type,
            s.camera_x, s.camera_y, s.col_ring, s.scroll_src,
        );

        // 3) grid redraw — 35A1 (full visible-grid redraw; early-exits unless the camera moved)
        let tmv = tilemap_view(s);
        let _g = draw_grid(
            planes, &tmv, s.camera_x, s.camera_y, s.prev_x, s.prev_y,
            s.dirty, s.dirty_rows, s.scroll_src, s.col_ring, s.fine_scroll,
            s.blit_type, s.mask_region,
        );
        grid_result = Some(FrameResult);
    }

    // 3b) the mode-9 FINAL-BOSS glyph — 6C0D (the boss image, 6x7 level tiles latched into the
    //     staging page at byte 0x18; runs every 6822 tick on level 9, alive AND defeated). Drawn
    //     into the ring BEFORE the scroll copy, exactly as the ASM stages it.
    if let Some(ref tiles) = s.boss_glyph_tiles {
        draw_boss_glyph(planes, tiles);
    }

    // 4) scroll copy — 3A27 (blit the ring buffer to the visible page)
    scroll_copy(
        planes, s.scroll_src, s.dest_page, s.col_ring, s.fine_scroll,
        s.row_ring, s.row_factor,
    );

    // 5) moving-sprite pass — 26FA (the active-sprite list; cull/animate/position/clip
    //    + planar blit). Layered on top of the scrolled background. Record mutations
    //    (life/drawn) are the object-record write-back contract, not part of the pixels.
    if let Some(cam) = &s.object_camera {
        let fallback_attrs = |_: u16| None;
        let attrs: &dyn Fn(u16) -> Option<SpriteAttr> = match &s.object_attrs {
            Some(f) => f.as_ref(),
            None => &fallback_attrs,
        };
        let draws = object_render::plan_frame(&s.object_sprites, attrs, cam);
        let banks = &s.object_src_banks;
        for draw in &draws {
            let bank: &[u8] = if let Some(b) = banks {
                b(draw.src_seg)
            } else {
                &[]
            };
            let size = (draw.src_bw * draw.full_rows * 6 + 64) as usize;
            let src = if draw.src_off + size <= bank.len() {
                &bank[draw.src_off..draw.src_off + size]
            } else {
                &bank[draw.src_off.min(bank.len())..]
            };
            object_render::paint_sprite(planes, draw, src, cam.row_stride);
        }
    }

    // 6) HUD region — only on a full rebuild (the incremental path leaves the HUD to its own
    //    redraw). Static status-bar chrome (320x23 panel from the persistent ALLFONTS.SQZ
    //    asset, BRIDGE-FED) then the dynamic values (RECOVERED) on top, into the same page as
    //    the frame. The static-chrome *compositor* (level-init build of the panel) is NOT YET
    //    RECOVERED — the panel is consumed as a named renderer asset (HudChromeAsset.bar),
    //    proven byte-exact vs the VM strip (test_hud_chrome).
    if rebuild {
        if let Some(chrome) = s.hud_chrome {
            hud::draw_status_bar(planes, s.dest_page, &chrome.bar);
            if let Some(ref hs) = s.hud_state {
                hud::draw_hud(planes, hs, &chrome.font, s.dest_page);
            }
        }
    }

    grid_result
}
