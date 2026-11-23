// SPDX-License-Identifier: GPL-3.0-only
//! Mode-select menu scene buffer (`1030:96D5` controller).
//!
//! Manages the 4-plane menu background buffer (`9718` initial fill) and builds the
//! 8-shift pre-rotated menu font (`972E..9765`).

pub const PLANE_LEN: usize = 0x10000;
const BG_PLANE: usize = 0x1F40;   // bytes per background plane in the initial fill (`9718`)
const FONT_SRC: usize = 0x1FE0;   // `9736`: glyph row source offset in the font segment
const FONT_WORDS: usize = 0x560;  // `9739`: words per shift copy

/// Build the menu's 8-shift pre-rotated font (`1030:972E..9765`) from the raw font segment (`[0x3D]`).
pub fn build_shifted_font(font_seg: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for shift in 0..8u32 {
        let mut si = FONT_SRC;
        for _ in 0..FONT_WORDS {
            let mut ax = font_seg[si] as u16 | ((font_seg[si + 1] as u16) << 8);
            ax = ((ax & 0xFF) << 8) | (ax >> 8);
            let mut carry: u8 = 0;
            for _ in 0..shift {
                let bit = ax & 1;
                ax >>= 1;
                carry = ((carry >> 1) | ((bit as u8) << 7)) & 0xFF;
            }
            ax = ((ax & 0xFF) << 8) | (ax >> 8);
            out.push((ax & 0xFF) as u8);
            out.push(((ax >> 8) & 0xFF) as u8);
            out.push(carry);
            si += 2;
        }
        out.extend(std::iter::repeat(0u8).take(0x60));
    }
    out
}

/// Stateful owner of the mode-select menu's evolving 4-plane EGA page (`1030:96D5`).
pub struct MenuScenePage {
    pub planes: [Vec<u8>; 4],
    pub seeded: bool,
}

impl MenuScenePage {
    pub fn new() -> Self {
        Self {
            planes: [
                vec![0u8; PLANE_LEN],
                vec![0u8; PLANE_LEN],
                vec![0u8; PLANE_LEN],
                vec![0u8; PLANE_LEN],
            ],
            seeded: false,
        }
    }

    /// Perform the `9718` initial fill: planes 0 and 1 receive the background motif from `[0x2875]`;
    /// planes 2 and 3 start cleared.
    pub fn seed(&mut self, asset: &[u8]) {
        self.planes = [
            vec![0u8; PLANE_LEN],
            vec![0u8; PLANE_LEN],
            vec![0u8; PLANE_LEN],
            vec![0u8; PLANE_LEN],
        ];
        let n0 = BG_PLANE.min(asset.len());
        self.planes[0][..n0].copy_from_slice(&asset[..n0]);
        let src1_start = BG_PLANE;
        let n1 = BG_PLANE.min(asset.len().saturating_sub(src1_start));
        if n1 > 0 {
            self.planes[1][..n1].copy_from_slice(&asset[src1_start..src1_start + n1]);
        }
        self.seeded = true;
    }
}
