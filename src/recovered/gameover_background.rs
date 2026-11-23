// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 game-over background (1030:9B66 + 9C87).
//!
//! The diorama image from GAMEOVER.SQZ composed into a staging buffer.

const BG_ROWS: usize = 200;
const STAGING_ROWS: usize = 400;
const PLANE_SIZE: usize = 0x1F40; // 8000 bytes per plane

/// Build the game-over staging buffer from the decoded GAMEOVER.SQZ asset.
/// Top 200 rows = black, bottom 200 rows = the asset.
/// Returns 4 plane-major buffers (each STAGING_ROWS * 0x28 bytes).
pub fn build_gameover_staging(gameover_sqz_decoded: &[u8]) -> [Vec<u8>; 4] {
    let mut planes: [Vec<u8>; 4] = [
        vec![0u8; STAGING_ROWS * 0x28],
        vec![0u8; STAGING_ROWS * 0x28],
        vec![0u8; STAGING_ROWS * 0x28],
        vec![0u8; STAGING_ROWS * 0x28],
    ];

    // The asset is 4 plane-major planes of 0x1F40 bytes each
    for p in 0..4 {
        let src_start = p * PLANE_SIZE;
        let src_end = (src_start + PLANE_SIZE).min(gameover_sqz_decoded.len());
        let src = &gameover_sqz_decoded[src_start..src_end];

        // Bottom 200 rows = the asset
        let dst_start = BG_ROWS * 0x28;
        let n = src.len().min(PLANE_SIZE);
        planes[p][dst_start..dst_start + n].copy_from_slice(&src[..n]);
    }

    planes
}

/// Scroll-copy a 176-row window at vertical offset into the display page.
pub fn gameover_window_scroll(
    planes: &[Vec<u8>; 4], dest: &mut [Vec<u8>; 4],
    v_offset: usize, page: usize, dest_rows: usize,
) {
    let row_bytes = 0x28;
    for y in 0..dest_rows {
        let src_row = v_offset + y;
        let src_off = src_row * row_bytes;
        let dst_off = page + y * row_bytes;
        for p in 0..4 {
            if src_off + row_bytes <= planes[p].len() && dst_off + row_bytes <= dest[p].len() {
                dest[p][dst_off..dst_off + row_bytes].copy_from_slice(&planes[p][src_off..src_off + row_bytes]);
            }
        }
    }
}
