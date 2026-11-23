// SPDX-License-Identifier: GPL-3.0-only
//! Native asset loader — the standalone replacement for the .SQZ loader at 1030:107B.
//!
//! Booting from game files means loading them the way the original does. 107B is the
//! one asset entry point: given a filename, it reads the file, SQZ-decompresses it,
//! writes the result to the stacking load pointer [0x2875], bumps that pointer by the
//! asset's paragraph count, and returns the segment the asset landed at.

use std::path::{Path, PathBuf};
use crate::codecs::sqz::{unpack_sqz, sqz_bump_advance};
use crate::engine::state::{NativeGameState, DS_BASE};

/// Read the NUL-terminated ASCII filename at DS:off (how the loader receives it in DX).
pub fn read_cstring(state: &NativeGameState, off: u16) -> String {
    let base = (DS_BASE + (off as usize & 0xFFFF)) & 0xFFFFF;
    let mut end = base;
    while end < state.data.len() && state.data[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&state.data[base..end]).to_string()
}

/// Case-insensitive resolve of an asset name in the game directory.
pub fn resolve_game_path(game_root: &Path, name: &str) -> Result<PathBuf, std::io::Error> {
    let p = game_root.join(name);
    if p.exists() {
        return Ok(p);
    }
    let upper = name.to_uppercase();
    for entry in std::fs::read_dir(game_root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().to_uppercase() == upper {
            return Ok(entry.path());
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, name))
}

/// Load + decompress one .SQZ asset into the stacking buffer at [0x2875],
/// bump the pointer, and return the segment it landed at.
/// [asm 1030:107B + the allocator bump @147D/1208/10E6]
pub fn load_sqz(state: &mut NativeGameState, name: &str, game_root: &Path) -> Result<u16, Box<dyn std::error::Error>> {
    let path = resolve_game_path(game_root, name)?;
    let raw = std::fs::read(&path)?;
    let out = unpack_sqz(&raw);

    // Read current load_top from DGROUP [0x2875]
    let out_seg = state.rw(0x2875);
    let base = ((out_seg as usize) << 4) & 0xFFFFF;
    state.data[base..base + out.len()].copy_from_slice(&out);

    // Bump load_top by paragraph count
    let bump = sqz_bump_advance(&raw);
    state.ww(0x2875, out_seg.wrapping_add(bump));

    log::debug!("load_sqz: {} → {} bytes at seg 0x{:04X} (bump +{} paras)",
        name, out.len(), out_seg, bump);

    Ok(out_seg)
}

/// As called from ASM: the filename is the C-string at DS:dx.
pub fn load_sqz_by_dx(state: &mut NativeGameState, dx: u16, game_root: &Path) -> Result<u16, Box<dyn std::error::Error>> {
    let name = read_cstring(state, dx);
    load_sqz(state, &name, game_root)
}
