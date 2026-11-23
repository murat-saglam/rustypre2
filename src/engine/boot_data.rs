// SPDX-License-Identifier: GPL-3.0-only
//! Boot memory image builder for the native runtime.
//!
//! Initializes the 1.25 MB address space (1 MB conventional address space plus the
//! four 64 KB EGA plane buffers at `0x100000..0x140000`) with the 64 KB DGROUP
//! image (`assets/dgroup.bin`) at `DS_BASE` (`0x1A0F0`) and the four code-segment
//! table windows at `CS_BASE` (`0x10300`).

use std::collections::HashMap;

/// DGROUP paragraph base (`0x1A0F << 4`).
pub const DS_BASE: usize = 0x1A0F << 4;
/// Code segment base (`0x1030 << 4`).
pub const CS_BASE: usize = 0x1030 << 4;
/// 1 MB conventional address space + 4 x 64 KB EGA plane shadow.
pub const IMAGE_LEN: usize = 0x140000;

/// Decompressed 64 KB DGROUP image embedded at compile time.
const DGROUP_BIN: &[u8] = include_bytes!("../../assets/dgroup.bin");

/// Code-segment lookup table windows referenced by gameplay and rendering routines.
fn cs_tables() -> HashMap<usize, Vec<u8>> {
    let mut tables = HashMap::new();
    tables.insert(
        0x0000,
        vec![
            0x03, 0x05, 0xFF, 0x1F, 0x00, 0x03, 0x00, 0xA0, 0xFA, 0xFC, 0xBA, 0xFF, 0xFF, 0xBE,
            0x80, 0x00,
        ],
    );
    tables.insert(
        0x1D60,
        vec![
            0xF7, 0xEB, 0x02, 0xF8, 0xC3, 0xF9, 0xC3, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xB8,
        ],
    );
    tables.insert(0x6770, vec![0xC3, 0x00, 0xA0, 0x01]);
    tables.insert(
        0x6AA0,
        vec![
            0x0F, 0xF6, 0xD8, 0x02, 0xC2, 0x98, 0x5A, 0x5B, 0xC3, 0x90, 0x7C, 0x8C, 0x7C, 0x2D,
            0x7C, 0x91, 0x7B, 0xDF, 0x7A, 0x60, 0x7A, 0xEC, 0x78, 0x98, 0x78, 0xDE, 0x77, 0x3D,
            0x77, 0x65, 0x76, 0x0F, 0x76, 0xC4, 0x75, 0x6C, 0x7F, 0x26, 0x7F, 0xE2, 0x7E, 0xD8,
            0x7E, 0xBF, 0x7E, 0xB5, 0x7E, 0xB5,
        ],
    );
    tables
}

/// Build the initial native memory image: zeroed address space populated with DGROUP and CS tables.
pub fn build_boot_memory() -> Vec<u8> {
    let mut data = vec![0u8; IMAGE_LEN];

    let dg_len = DGROUP_BIN.len().min(0x10000);
    data[DS_BASE..DS_BASE + dg_len].copy_from_slice(&DGROUP_BIN[..dg_len]);

    for (off, blob) in cs_tables() {
        let base = CS_BASE + off;
        let len = blob.len().min(IMAGE_LEN - base);
        data[base..base + len].copy_from_slice(&blob[..len]);
    }

    data
}
