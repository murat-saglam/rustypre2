// SPDX-License-Identifier: GPL-3.0-only
//! Load the static HUD chrome from its persistent source asset (ALLFONTS.SQZ).
//!
//! The HUD chrome lives entirely in ``ALLFONTS.SQZ`` (verified byte-exact vs the in-VM HUD strip on a
//! mid-game snapshot, and corroborated by the blues_p2 ``pre2_editor`` gameplay runtime). The decoded
//! blob holds, in order: ``[0 .. bg_off)`` other glyph data, ``[bg_off .. bg_off+bg_len)`` the 320x23
//! status-bar PANEL bitmap (planar — the gray panel + caveman face icon + the ``LIVES:``/``SCORE:``/
//! ``ENERGY:`` labels + the boss-meter box), then the 16x12 HUD glyph font at ``0x1610`` (glyph 0).
//!
//! The in-VM bar at ``0x252B:0x0B48`` is a *transient* runtime copy (reused after the level-start
//! blit), so it cannot be read off an arbitrary snapshot — ``ALLFONTS.SQZ`` is the persistent source.
//!
//! Layout (from the editor reference, verified against PRE2.EXE):
//!
//! ```text
//! bg_off = 41 * 48          # 0x07B0  panel bitmap start (right before the font glyphs)
//! bg_len = 320 * 23 // 2    # 0x0E60  = 4 planes x 0x398
//! font glyphs at 0x1610     # 16x12 (0x60 bytes) each, glyph 0 = digit '0'
//! ```
//!
//! Asset filename / offsets live here (bridge); the recovered renderer consumes the typed
//! `HudChromeAsset`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::codecs::unpack_sqz;
use crate::recovered::render_model::HudChromeAsset;

const CHROME_SQZ: &str = "ALLFONTS.SQZ";
const BG_OFF: usize = 41 * 48; // 0x07B0 — status-bar panel bitmap offset within the decoded blob
const BG_LEN: usize = 320 * 23 / 2; // 0x0E60 — 320x23 planar, 4 planes x 0x398

static DECODE_CACHE: Mutex<Option<HashMap<String, Vec<u8>>>> = Mutex::new(None);

/// Decode ALLFONTS.SQZ (cached per path).
fn decode(path: &str) -> Vec<u8> {
    let mut cache_guard = DECODE_CACHE.lock().unwrap();
    let cache = cache_guard.get_or_insert_with(HashMap::new);
    if let Some(dec) = cache.get(path) {
        return dec.clone();
    }
    let raw = std::fs::read(path).unwrap_or_default();
    let decoded = unpack_sqz(&raw);
    cache.insert(path.to_string(), decoded.clone());
    decoded
}

/// Decode the persistent HUD chrome (panel bitmap + glyph font) from ``ALLFONTS.SQZ`` under
/// ``game_root``. Both pieces come from this one asset, so the HUD renders from any snapshot. Cached.
///
/// The font is the whole decoded blob (the glyph blit indexes it from offset 0, glyphs at 0x1610).
pub fn load_hud_chrome(game_root: &std::path::Path) -> Option<HudChromeAsset> {
    let path = crate::engine::assets::resolve_game_path(game_root, CHROME_SQZ).ok()?;
    let dec = decode(path.to_str().unwrap_or(""));
    let bg_end = (BG_OFF + BG_LEN).min(dec.len());
    let bar = dec[BG_OFF..bg_end].to_vec();
    let font = dec.clone(); // full decoded blob (glyph blit indexes from 0x1610)
    Some(HudChromeAsset { bar, font })
}
