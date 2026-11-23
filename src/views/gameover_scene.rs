// SPDX-License-Identifier: GPL-3.0-only
//! Game-over scene compositor (`GAMEOVER.SQZ` diorama background + object and HUD overlays).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::engine::state::NativeGameState;
use crate::engine::vga::EGA_PLANE_STRIDE;
use crate::codecs::unpack_sqz;
use crate::recovered::gameover_background;
use crate::recovered::hud;
use crate::recovered::object_render::{self, SpriteAttr};
use crate::recovered::render_frame::RendererState;
use crate::recovered::scene_compositor::{
    RecoveredBackground, SceneBackground, SceneStatus, compose_scene,
};
use crate::views::game_visual_state::retarget_page;
use crate::views::hud_chrome::load_hud_chrome;
use crate::views::render_state::read_renderer_state_full;

const GAMEOVER_ASSET: &str = "GAMEOVER.SQZ";
const SCROLL: usize = 0x6BC4; // [0x6BC4] vertical scroll offset (9C87)
const DATA: usize = 0x1A0F;

static ASSET_CACHE: Mutex<Option<HashMap<String, Vec<u8>>>> = Mutex::new(None);

/// Decode `GAMEOVER.SQZ` via the SQZ codec, cached per `game_root`.
pub fn load_gameover_asset(game_root: &std::path::Path) -> Vec<u8> {
    let key = game_root.to_string_lossy().to_string();
    let mut guard = ASSET_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(dec) = cache.get(&key) {
        return dec.clone();
    }
    let path = crate::engine::assets::resolve_game_path(game_root, GAMEOVER_ASSET)
        .unwrap_or_else(|_| game_root.join(GAMEOVER_ASSET));
    let raw = std::fs::read(&path).unwrap_or_default();
    let decoded = unpack_sqz(&raw);
    cache.insert(key, decoded.clone());
    decoded
}

/// Build the game-over diorama background for `page`.
pub fn gameover_background(mem: &NativeGameState, game_root: &std::path::Path, page: usize) -> SceneBackground {
    let scroll = mem.data[(DATA << 4) + SCROLL];
    let staging = load_gameover_asset(game_root);
    let staging_planes = gameover_background::build_gameover_staging(&staging);
    let mut planes: [Vec<u8>; 4] = [
        vec![0u8; EGA_PLANE_STRIDE],
        vec![0u8; EGA_PLANE_STRIDE],
        vec![0u8; EGA_PLANE_STRIDE],
        vec![0u8; EGA_PLANE_STRIDE],
    ];
    gameover_background::gameover_window_scroll(&staging_planes, &mut planes, scroll as usize, page, 176);
    SceneBackground::Recovered(RecoveredBackground { planes })
}

/// Apply the object pass overlay (falling `GAME OVER` letters, characters, and birds).
pub fn apply_object_overlay<'a>(planes: &mut [Vec<u8>; 4], rs: &RendererState<'a>, _page: usize) {
    let cam = match &rs.object_camera {
        Some(c) => c.clone(),
        None => return,
    };
    let fallback_attrs = |_: u16| None;
    let attrs: &dyn Fn(u16) -> Option<SpriteAttr> = match &rs.object_attrs {
        Some(f) => f.as_ref(),
        None => &fallback_attrs,
    };
    let fallback_banks = |_: usize| -> &'a [u8] { &[] };
    let banks_fn: &dyn Fn(usize) -> &'a [u8] = match &rs.object_src_banks {
        Some(f) => f.as_ref(),
        None => &fallback_banks,
    };
    let draws = object_render::plan_frame(&rs.object_sprites, attrs, &cam);
    for draw in &draws {
        let bank: &[u8] = banks_fn(draw.src_seg);
        let size = (draw.src_bw as usize) * (draw.full_rows as usize) * 6 + 64;
        let src = if draw.src_off + size <= bank.len() {
            &bank[draw.src_off..draw.src_off + size]
        } else if draw.src_off < bank.len() {
            &bank[draw.src_off..]
        } else {
            &[][..]
        };
        object_render::paint_sprite(planes, draw, src, cam.row_stride);
    }
}

/// Apply the HUD overlay (static status bar + dynamic lives/score/energy).
pub fn apply_hud_overlay(planes: &mut [Vec<u8>; 4], rs: &RendererState, page: usize) {
    if let Some(chrome) = rs.hud_chrome {
        hud::draw_status_bar(planes, page, &chrome.bar);
        if let Some(ref hs) = rs.hud_state {
            hud::draw_hud(planes, hs, &chrome.font, page);
        }
    }
}

/// Compose the full game-over scene: diorama background + object and HUD overlays.
pub fn build_gameover_scene<'a>(
    mem: &'a NativeGameState,
    game_root: &std::path::Path,
    page: usize,
    background: Option<SceneBackground>,
) -> ([Vec<u8>; 4], SceneStatus) {
    let bg = background.unwrap_or_else(|| gameover_background(mem, game_root, page));
    let hud_chrome = load_hud_chrome(game_root);
    let hud_chrome_ref = hud_chrome.as_ref();
    let mut rs = read_renderer_state_full(mem, hud_chrome_ref, false);
    retarget_page(&mut rs, page);

    let object_overlay = |planes: &mut [Vec<u8>; 4], _pg: usize| {
        apply_object_overlay(planes, &rs, _pg);
    };
    let hud_overlay = |planes: &mut [Vec<u8>; 4], pg: usize| {
        apply_hud_overlay(planes, &rs, pg);
    };

    let overlays: [&dyn Fn(&mut [Vec<u8>; 4], usize); 2] = [&object_overlay, &hud_overlay];
    compose_scene(&bg, &overlays, page)
}
