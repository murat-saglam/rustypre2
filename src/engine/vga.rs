// SPDX-License-Identifier: GPL-3.0-only
//! The native VGA surface — the display-adapter state the VM-less game needs.
//!
//! The recovered game touches three pieces of "VGA hardware":
//! - the 256-entry DAC palette (read by the renderer, programmed via 3C7/3C8/3C9)
//! - the 6→8-bit DAC component expansion
//! - the EGA plane-shadow layout inside the 1.25 MB native memory image

/// The EGA plane shadow: 4 x 64 KB planes stored above the 1 MB real-mode space.
pub const EGA_APERTURE: usize = 0x100000;
pub const EGA_PLANE_STRIDE: usize = 0x10000;

/// Expand a 6-bit VGA DAC component to 8 bits the way real VGA does:
/// `v<<2` alone maxes at 252 (slightly dark); replicating the high bits
/// (`| v>>4`) makes 63 -> 255, matching hardware.
#[inline]
pub fn dac8(v6: u8) -> u8 {
    let v6 = v6 & 0x3F;
    (v6 << 2) | (v6 >> 4)
}

/// The power-on DAC: first 16 entries = the EGA palette,
/// the rest a deterministic grayscale ramp until the game programs the real colours.
const EGA16: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], [0x00, 0x00, 0xAA], [0x00, 0xAA, 0x00], [0x00, 0xAA, 0xAA],
    [0xAA, 0x00, 0x00], [0xAA, 0x00, 0xAA], [0xAA, 0x55, 0x00], [0xAA, 0xAA, 0xAA],
    [0x55, 0x55, 0x55], [0x55, 0x55, 0xFF], [0x55, 0xFF, 0x55], [0x55, 0xFF, 0xFF],
    [0xFF, 0x55, 0x55], [0xFF, 0x55, 0xFF], [0xFF, 0xFF, 0x55], [0xFF, 0xFF, 0xFF],
];

/// The stand-alone display adapter: vga_palette + the DAC port protocol.
///
/// A drop-in for the `dos` parameter every native/bridge call takes
/// (`dos.vga_palette` reads + `dos._track_vga_dac_ports` writes are the entire surface).
pub struct NativeVGA {
    /// The 256-entry palette (8-bit RGB).
    pub vga_palette: [[u8; 3]; 256],
    /// The BIOS video mode (the game runs 0x0D planar mode).
    pub video_mode: u8,
}

impl NativeVGA {
    pub fn new() -> Self {
        let mut palette = [[0u8; 3]; 256];
        palette[..16].copy_from_slice(&EGA16);
        for i in 16..256 {
            palette[i] = [i as u8, i as u8, i as u8];
        }
        Self {
            vga_palette: palette,
            video_mode: 0x0D,
        }
    }
}

impl Default for NativeVGA {
    fn default() -> Self {
        Self::new()
    }
}
