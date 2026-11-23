// SPDX-License-Identifier: GPL-3.0-only
//! Reconstruct the renderer's input contract (RendererState) from VM memory.
//!
//! Read-only bridge between NativeGameState and render_frame.

use crate::engine::state::NativeGameState;
use crate::recovered::hud::{self, HudState};
use crate::recovered::object_render::{Camera, Sprite, SpriteAttr};
use crate::recovered::render_frame::RendererState;
use crate::recovered::render_model::HudChromeAsset;
use crate::codecs::unpack_sqz;

const DS: usize = 0x1A0F;

const ANIM_FRAME_PTR: usize = 0x6BC2;
const FRAME_CTR: usize = 0x6BD5;
const ROW_FACTOR: usize = 0x6BF8;
const SCORE: usize = 0x6C0E;
const LIVES: usize = 0x27D8;
const ENERGY: usize = 0x27D6;
const BONUS_FLASH: usize = 0x6C00;
const BONUS_MASK: usize = 0x6CA7;

// Object render layout
const LIST_BASE: usize = 0x4F0A;
const LIST_TOP: usize = 0x5720;
const LIST_STRIDE: usize = 0x12;
const TBL_WIDTH_HEIGHT: usize = 0x7190;
const TBL_XY_OFFSET: usize = 0x752A;
const TBL_SRC_SEG: usize = 0x62E8;
const TBL_SRC_OFF: usize = 0x5F48;
const VAR_CAMERA_X: usize = 0x2DE4;
const VAR_CAMERA_Y: usize = 0x2DE6;
const VAR_FINE_SCROLL: usize = 0x6BC4;
const VAR_DEST_PAGE: usize = 0x2DD8;
const VAR_ROW_STRIDE: usize = 0x2DB0;

// HUD chrome
const HUD_BG_OFF: usize = 41 * 48; // 0x07B0
const HUD_BG_LEN: usize = 320 * 23 / 2; // 0x0E60

fn rb(mem: &NativeGameState, off: usize) -> u8 {
    mem.data[((DS << 4) + off) & 0xFFFFF]
}
fn rw(mem: &NativeGameState, off: usize) -> u16 {
    let b = ((DS << 4) + off) & 0xFFFFF;
    (mem.data[b] as u16) | ((mem.data[b + 1] as u16) << 8)
}
fn rws(mem: &NativeGameState, off: usize) -> i32 {
    let v = rw(mem, off) as i32;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

fn anim_xlat<'a>(mem: &'a NativeGameState) -> &'a [u8] {
    let base = ((DS << 4) + rw(mem, ANIM_FRAME_PTR) as usize) & 0xFFFFF;
    &mem.data[base..base + 0x100]
}

fn hud_state(mem: &NativeGameState) -> HudState {
    let score = ((rw(mem, SCORE) as u32) | ((rw(mem, SCORE + 2) as u32) << 16)) * 10;
    let bonus = hud::effective_bonus_mask(rb(mem, BONUS_MASK), rb(mem, BONUS_FLASH) != 0, rb(mem, FRAME_CTR));
    HudState {
        score,
        lives: rb(mem, LIVES),
        energy: rb(mem, ENERGY),
        bonus_mask: bonus,
    }
}

/// Read the object render camera from NativeGameState.
/// `frame_pre_inc` matches the object renderer's +1 to [0x6BD5] applied at 26FA entry; pass `false`
/// at the frame-commit boundary (1030:6772, AFTER 26FA already bumped it) to read the value the
/// engine drew with (a pre-inc read there double-counts and shifts the blink phase (frame & 3),
/// silently corrupting every BLINKING sprite — the hit-flash / spawn-invincibility flicker).
fn read_camera(mem: &NativeGameState, frame_pre_inc: bool) -> Camera {
    let frame = rw(mem, FRAME_CTR) as u32 + if frame_pre_inc { 1 } else { 0 };
    Camera {
        cam_x: rws(mem, VAR_CAMERA_X),
        cam_y: rws(mem, VAR_CAMERA_Y),
        fine_scroll: rb(mem, VAR_FINE_SCROLL) as i32,
        row_factor: rws(mem, ROW_FACTOR),
        dest_page: rw(mem, VAR_DEST_PAGE) as usize,
        row_stride: rw(mem, VAR_ROW_STRIDE) as usize,
        global_shift: 3,  // [cs:0] = 3 — pixel-to-byte shift divisor (32px >> 3 = 4 bytes)
        frame,
    }
}

/// Read all active sprites from the list at 0x4F0A..0x5720 (top->down order), skipping empty slots.
fn read_active_sprites(mem: &NativeGameState) -> Vec<Sprite> {
    let mut sprites = Vec::with_capacity(32);
    let mut off = LIST_TOP;
    while off > LIST_BASE {
        off -= LIST_STRIDE;
        let sprite_id = mem.rw(off + 4);
        if sprite_id == 0xFFFF {
            continue;
        }
        sprites.push(Sprite {
            x: rws(mem, off) as i32,
            y: rws(mem, off + 2) as i32,
            sprite_id,
            flags: mem.rb(off + 5),
            life: mem.rb(off + 0x11),
        });
    }
    sprites
}

/// Read sprite attributes for a given sprite_id.
fn read_sprite_attr(mem: &NativeGameState, sprite_id: u16) -> SpriteAttr {
    let bx = ((sprite_id & 0x1FFF) << 1) as usize;
    let wh = rw(mem, TBL_WIDTH_HEIGHT + bx);
    SpriteAttr {
        width: (wh & 0xFF) as i32,
        height: ((wh >> 8) & 0xFF) as i32,
        x_off: rb(mem, TBL_XY_OFFSET + bx) as i32,
        y_off: rb(mem, TBL_XY_OFFSET + bx + 1) as i32,
        src_seg: rw(mem, TBL_SRC_SEG + bx) as usize,
        src_off: rw(mem, TBL_SRC_OFF + bx) as usize,
    }
}

/// Decode ALLFONTS.SQZ once and return a static reference to the cached `HudChromeAsset` (zero per-frame clones).
pub fn load_hud_chrome_ref(game_root: &std::path::Path) -> Option<&'static HudChromeAsset> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<HudChromeAsset>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let path = crate::engine::assets::resolve_game_path(game_root, "ALLFONTS.SQZ").ok()?;
            let raw = std::fs::read(&path).ok()?;
            let decoded = unpack_sqz(&raw);
            let bg_off = HUD_BG_OFF;
            let bg_end = (bg_off + HUD_BG_LEN).min(decoded.len());
            let bar = decoded[bg_off..bg_end].to_vec();
            let font = decoded;
            Some(HudChromeAsset { bar, font })
        })
        .as_ref()
}

/// Snapshot renderer inputs from NativeGameState into a RendererState.
/// This is the FULL bridge — includes object render inputs and HUD chrome.
/// `frame_pre_inc` matches the object renderer's +1 to [0x6BD5]; pass `false` at the commit
/// boundary (see `read_camera`).
pub fn read_renderer_state_full<'a>(
    mem: &'a NativeGameState,
    hud_chrome: Option<&'a HudChromeAsset>,
    frame_pre_inc: bool,
) -> RendererState<'a> {
    let ds_base = DS << 4;
    let map_seg = rw(mem, 0x2DDA) as usize;
    let map_base = (map_seg << 4) & 0xFFFFF;
    let _map_rows = rb(mem, 0x2CF5) as usize;

    let camera_x = rb(mem, 0x2DE4);
    let camera_y = rb(mem, 0x2DE6);
    let scroll_src = rw(mem, 0x2DBA) as usize;  // read from [0x2DBA] — set by native_sync_render_state
    let dest_page = rw(mem, 0x2DD8) as usize;
    let row_factor = rb(mem, ROW_FACTOR) as usize;
    let prev_x = rb(mem, 0x2DE0);
    let prev_y = rb(mem, 0x2DE2);
    let col_ring = rb(mem, 0x2DE8) as usize;
    let fine_scroll = rb(mem, 0x6BC4) as usize;
    let row_ring = rb(mem, 0x2DEA) as usize;
    let dirty = rb(mem, 0x2DF4);
    let dirty_rows = rb(mem, 0x2DF5);

    // Object render inputs — borrow slices directly from `mem.data` (zero leaks, zero 64KB clones!)
    let cam = read_camera(mem, frame_pre_inc);
    let sprites = read_active_sprites(mem);

    let attrs_ref: Box<dyn Fn(u16) -> Option<SpriteAttr> + 'a> =
        Box::new(move |id: u16| Some(read_sprite_attr(mem, id)));
    let banks_ref: Box<dyn Fn(usize) -> &'a [u8] + 'a> =
        Box::new(move |seg: usize| -> &'a [u8] {
            let base = (seg << 4) & 0xFFFFF;
            let end = (base + 0x10000).min(mem.data.len());
            if base < end {
                &mem.data[base..end]
            } else {
                &[]
            }
        });

    // Borrow the planar ASSET region (tile cache + parallax base, ASSET_LO..ASSET_HI)
    // directly from each of the four EGA planes in `mem.data`.
    let asset_lo = crate::recovered::render_frame::ASSET_LO;
    let asset_hi = crate::recovered::render_frame::ASSET_HI;
    let asset_len = asset_hi - asset_lo;
    let ap = crate::engine::vga::EGA_APERTURE;
    let st = crate::engine::vga::EGA_PLANE_STRIDE;
    let asset_refs: [&'a [u8]; 4] = [
        &mem.data[ap + 0 * st + asset_lo..ap + 0 * st + asset_lo + asset_len],
        &mem.data[ap + 1 * st + asset_lo..ap + 1 * st + asset_lo + asset_len],
        &mem.data[ap + 2 * st + asset_lo..ap + 2 * st + asset_lo + asset_len],
        &mem.data[ap + 3 * st + asset_lo..ap + 3 * st + asset_lo + asset_len],
    ];

    // Mode-9 final-boss glyph (6x7 level-tile indices, level 9 only) [asm 6C0D]
    let boss_glyph_tiles: Option<[u8; 42]> = if rb(mem, 0x2D8A) == 9 {
        let glyph_opt: Option<usize> = if rw(mem, 0xA519) != 0 {
            if let Some(g) = mem.boss_glyph {
                Some(g as usize)
            } else {
                let ptr = rw(mem, 0xA517) as usize;
                let g = rb(mem, ptr.wrapping_sub(1) & 0xFFFF);
                if g < 0x80 { Some(g as usize) } else { None }
            }
        } else {
            Some(2usize) // [asm 6B16] defeated/victory glyph
        };
        if let Some(glyph) = glyph_opt {
            let tbl = glyph * 6 + 0x14;
            let mut out = [0u8; 42];
            for row in 0..7 {
                let src = map_base + tbl + row * 0x100;
                if src + 6 <= mem.data.len() {
                    out[row * 6..row * 6 + 6].copy_from_slice(&mem.data[src..src + 6]);
                }
            }
            Some(out)
        } else {
            None
        }
    } else {
        None
    };

    RendererState {
        tiles: &mem.data[map_base..(map_base + 0x10000).min(mem.data.len())],
        type_tbl: &mem.data[ds_base + 0x805E..ds_base + 0x805E + 256],
        flag_tbl: &mem.data[ds_base + 0x6988..ds_base + 0x6988 + 256],
        blit_type: &mem.data[ds_base + 0x4DF8..ds_base + 0x4DF8 + 256],
        mask_region: &mem.data[ds_base + 0x2DF8..ds_base + 0x2DF8 + 256 * 32],
        anim_xlat: anim_xlat(mem),
        camera_x, camera_y, prev_x, prev_y,
        col_ring, fine_scroll, row_ring,
        scroll_src, dest_page, row_factor,
        dirty, dirty_rows,
        fade: None,
        asset_planes: Some(asset_refs),
        boss_glyph_tiles,
        object_camera: Some(cam),
        object_sprites: sprites,
        object_attrs: Some(attrs_ref),
        object_src_banks: Some(banks_ref),
        hud_state: Some(hud_state(mem)),
        hud_chrome,
    }
}
