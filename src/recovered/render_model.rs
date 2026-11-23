// SPDX-License-Identifier: GPL-3.0-only
//! Semantic render model types shared between the renderer and asset loaders.

/// Static HUD chrome asset data (`ALLFONTS.SQZ` status-bar background and glyph font).
#[derive(Clone, Default)]
pub struct HudChromeAsset {
    /// 320x23 planar status-bar bitmap: 4 planes x `0x398` bytes (plane-major).
    pub bar: Vec<u8>,
    /// HUD glyph-font segment bytes (`blit_hud_glyph` indexes `0x1610 + glyph * 0x60`).
    pub font: Vec<u8>,
}
