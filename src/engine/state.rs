// SPDX-License-Identifier: GPL-3.0-only
//! `NativeGameState` — flat memory image and per-frame side-channels for the native runtime.
//!
//! Owns the 1.25 MB address space (`data`) so all gameplay, level-loading, audio, and
//! rendering systems operate directly on the in-memory DGROUP, asset segments, and EGA
//! planar buffers.

use crate::engine::boot_data::IMAGE_LEN;

/// Game data segment (`DGROUP`).
pub const DATA_SEG: usize = 0x1A0F;
/// Linear byte offset of `DGROUP` (`DATA_SEG << 4`).
pub const DS_BASE: usize = DATA_SEG << 4; // 0x1A0F0

/// Complete runtime memory state and per-frame side-channel buffers.
pub struct NativeGameState {
    /// Full address space (1 MB conventional memory + 4 x 64 KB EGA planes = `0x140000` bytes).
    pub data: Vec<u8>,
    /// Sound effect triggers queued during the current frame.
    pub sfx_queue: Vec<u16>,
    /// Optional screen-space X coordinate (`0..320`) for each `sfx_queue` entry (stereo pan).
    pub sfx_queue_x: Vec<Option<i32>>,
    /// Point particles (`[0x7DE6]`) snapshotted at `4B8E` entry before consumption.
    pub particle_capture: Option<Vec<u8>>,
    /// Render-record slots whose hit-flash bit (`0x4000`) was active this frame.
    pub flash_slots: Option<Vec<u16>>,
    /// Song-load request index emitted by gameplay routines.
    pub song_request: Option<u16>,
    /// Active mode-9 final-boss glyph index.
    pub boss_glyph: Option<u16>,
    /// `LEVELG` falling-snow pixel coordinates for the current frame.
    pub snow_plots: Option<Vec<(u16, u8)>>,
    /// Previous frame's particle snapshot retained across non-tick render frames.
    pub particle_capture_last: Option<Vec<u8>>,
    /// Previous frame's hit-flash slot list retained across non-tick render frames.
    pub flash_slots_last: Option<Vec<u16>>,
}

impl NativeGameState {
    /// Construct a `NativeGameState` from an initialized memory image.
    pub fn new(data: Vec<u8>) -> Self {
        let mut data = data;
        if data.len() < IMAGE_LEN {
            data.resize(IMAGE_LEN, 0);
        }
        Self {
            data,
            sfx_queue: Vec::new(),
            sfx_queue_x: Vec::new(),
            particle_capture: None,
            flash_slots: None,
            song_request: None,
            boss_glyph: None,
            snow_plots: None,
            particle_capture_last: None,
            flash_slots_last: None,
        }
    }

    /// Read a byte from `DGROUP` (`DS_BASE + off`).
    #[inline]
    pub fn rb(&self, off: usize) -> u8 {
        self.data[DS_BASE + off]
    }

    /// Read a little-endian 16-bit word from `DGROUP` (`DS_BASE + off`).
    #[inline]
    pub fn rw(&self, off: usize) -> u16 {
        let b = DS_BASE + off;
        self.data[b] as u16 | ((self.data[b + 1] as u16) << 8)
    }

    /// Write a byte to `DGROUP` (`DS_BASE + off`).
    #[inline]
    pub fn wb(&mut self, off: usize, v: u8) {
        self.data[DS_BASE + off] = v;
    }

    /// Write a little-endian 16-bit word to `DGROUP` (`DS_BASE + off`).
    #[inline]
    pub fn ww(&mut self, off: usize, v: u16) {
        let b = DS_BASE + off;
        self.data[b] = v as u8;
        self.data[b + 1] = (v >> 8) as u8;
    }

    /// Read a byte at a linear physical address.
    #[inline]
    pub fn rb_abs(&self, addr: usize) -> u8 {
        self.data[addr & 0xFFFFF]
    }

    /// Read a little-endian 16-bit word at a linear physical address.
    #[inline]
    pub fn rw_abs(&self, addr: usize) -> u16 {
        let a = addr & 0xFFFFF;
        self.data[a] as u16 | ((self.data[a + 1] as u16) << 8)
    }

    /// Write a byte at a linear physical address.
    #[inline]
    pub fn wb_abs(&mut self, addr: usize, v: u8) {
        self.data[addr & 0xFFFFF] = v;
    }

    /// Write a little-endian 16-bit word at a linear physical address.
    #[inline]
    pub fn ww_abs(&mut self, addr: usize, v: u16) {
        let a = addr & 0xFFFFF;
        self.data[a] = v as u8;
        self.data[a + 1] = (v >> 8) as u8;
    }
}
