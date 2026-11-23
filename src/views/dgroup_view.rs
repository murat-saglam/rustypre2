// SPDX-License-Identifier: GPL-3.0-only
//! Typed DGROUP field accessors over `NativeGameState`.
//!
//! Maps named camera, scroll-ring, and tile-animation globals to their DGROUP offsets.

use crate::engine::state::NativeGameState;

/// Base offset of the player's render/kinematics record in `DGROUP`.
pub const PLAYER_BASE: usize = 0x4F1C;

/// Named accessors for camera, scroll-ring, and animated-tile remap globals in `DGROUP`.
pub struct PlayerGlobals<'a> {
    pub state: &'a mut NativeGameState,
}

impl<'a> PlayerGlobals<'a> {
    pub fn new(state: &'a mut NativeGameState) -> Self {
        Self { state }
    }

    /// Previous-camera cell X / `0x55AA` grid-dirty sentinel (`[0x2DE0]`).
    pub fn set_grid_dirty_token(&mut self, v: u16) {
        self.state.ww(0x2DE0, v);
    }

    /// One-tile direct re-blit page flag (`[0x6BBD]`).
    pub fn page_dirty(&self) -> u8 {
        self.state.rb(0x6BBD)
    }

    /// Camera tile column word (`[0x2DE4]`).
    pub fn cam_col_word(&self) -> u16 {
        self.state.rw(0x2DE4)
    }

    /// Camera tile row word (`[0x2DE6]`).
    pub fn cam_row_word(&self) -> u16 {
        self.state.rw(0x2DE6)
    }

    /// Background column ring index (`[0x2DE8]`, `camera_x % 0x14`).
    pub fn col_ring(&self) -> u16 {
        self.state.rw(0x2DE8)
    }

    pub fn set_col_ring(&mut self, v: u16) {
        self.state.ww(0x2DE8, v);
    }

    /// Background row ring index (`[0x2DEA]`, `camera_y % 0x0C`).
    pub fn row_ring(&self) -> u16 {
        self.state.rw(0x2DEA)
    }

    pub fn set_row_ring(&mut self, v: u16) {
        self.state.ww(0x2DEA, v);
    }

    /// Per-level directional friction / animation cadence word (`[0x6BF6]`).
    pub fn friction(&self) -> u16 {
        self.state.rw(0x6BF6)
    }

    /// Vertical-scroll VRAM copy-source offset (`[0x2DBA]`).
    pub fn set_scroll_copy_src(&mut self, v: u16) {
        self.state.ww(0x2DBA, v);
    }

    /// Animated-tile remap frame pointer (`[0x6BC2]`).
    pub fn anim_remap_ptr(&self) -> u16 {
        self.state.rw(0x6BC2)
    }

    pub fn set_anim_remap_ptr(&mut self, v: u16) {
        self.state.ww(0x6BC2, v);
    }

    /// Animated-tile remap threshold/counter byte (`[0x6BD4]`).
    pub fn anim_remap_thresh(&self) -> u8 {
        self.state.rb(0x6BD4)
    }

    pub fn set_anim_remap_thresh(&mut self, v: u8) {
        self.state.wb(0x6BD4, v);
    }
}
