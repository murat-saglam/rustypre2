// SPDX-License-Identifier: GPL-3.0-only
//! rustypre2 — Build script for cross-platform SDL2 linking and Windows PE icon embedding.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Build a standalone x86_64/i686 COFF object file containing `RT_ICON` (3) and
/// `RT_GROUP_ICON` (14) PE resources from an `.ico` file, with zero external tools required.
fn build_windows_icon_coff(ico_path: &Path, out_obj: &Path, is_64bit: bool) -> std::io::Result<()> {
    let ico = fs::read(ico_path)?;
    if ico.len() < 6 {
        return Ok(());
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    if count == 0 || ico.len() < 6 + count * 16 {
        return Ok(());
    }

    // Build GRPICONDIR payload (RT_GROUP_ICON, ID 1) and collect RT_ICON payloads (IDs 1..=count)
    let mut grp_data = Vec::with_capacity(6 + count * 14);
    grp_data.extend_from_slice(&ico[0..6]);
    let mut icon_blobs: Vec<&[u8]> = Vec::with_capacity(count);

    for i in 0..count {
        let ent = 6 + i * 16;
        let bytes_in_res = u32::from_le_bytes([
            ico[ent + 8],
            ico[ent + 9],
            ico[ent + 10],
            ico[ent + 11],
        ]) as usize;
        let img_off = u32::from_le_bytes([
            ico[ent + 12],
            ico[ent + 13],
            ico[ent + 14],
            ico[ent + 15],
        ]) as usize;
        if img_off + bytes_in_res > ico.len() {
            return Ok(());
        }
        grp_data.extend_from_slice(&ico[ent..ent + 12]);
        push_u16(&mut grp_data, (i + 1) as u16);
        icon_blobs.push(&ico[img_off..img_off + bytes_in_res]);
    }

    // .rsrc$02 holds the raw resource payloads (8-byte aligned)
    let mut rsrc02 = Vec::new();
    let mut data_offsets_sizes: Vec<(u32, u32)> = Vec::with_capacity(count + 1);
    for blob in &icon_blobs {
        let off = rsrc02.len() as u32;
        let sz = blob.len() as u32;
        rsrc02.extend_from_slice(blob);
        while rsrc02.len() % 8 != 0 {
            rsrc02.push(0);
        }
        data_offsets_sizes.push((off, sz));
    }
    let grp_off = rsrc02.len() as u32;
    let grp_sz = grp_data.len() as u32;
    rsrc02.extend_from_slice(&grp_data);
    while rsrc02.len() % 8 != 0 {
        rsrc02.push(0);
    }
    data_offsets_sizes.push((grp_off, grp_sz));

    // .rsrc$01 layout:
    //   0x0000: Root Dir (16B) + 2 entries (16B) = 32B
    //   0x0020: Type 3 (RT_ICON) Dir (16B) + count entries (count * 8B)
    //   dir_grp_off: Type 14 (RT_GROUP_ICON) Dir (16B) + 1 entry (8B) = 24B
    //   lang_dirs_off: (count + 1) Language Dirs, each (16B + 8B = 24B)
    //   data_entries_off: (count + 1) IMAGE_RESOURCE_DATA_ENTRY, each 16B
    let dir_icon_off: u32 = 32;
    let dir_icon_len: u32 = 16 + (count as u32) * 8;
    let dir_grp_off: u32 = dir_icon_off + dir_icon_len;
    let dir_grp_len: u32 = 24;
    let lang_dirs_off: u32 = dir_grp_off + dir_grp_len;
    let total_items = (count + 1) as u32;
    let data_entries_off: u32 = lang_dirs_off + total_items * 24;

    let mut rsrc01 = Vec::new();
    // Root directory (2 ID entries: 3 = RT_ICON, 14 = RT_GROUP_ICON)
    push_u32(&mut rsrc01, 0);
    push_u32(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 2);
    push_u32(&mut rsrc01, 3);
    push_u32(&mut rsrc01, 0x8000_0000 | dir_icon_off);
    push_u32(&mut rsrc01, 14);
    push_u32(&mut rsrc01, 0x8000_0000 | dir_grp_off);

    // Type 3 (RT_ICON) directory: `count` ID entries (1..=count)
    push_u32(&mut rsrc01, 0);
    push_u32(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, count as u16);
    for i in 0..count as u32 {
        push_u32(&mut rsrc01, i + 1);
        push_u32(&mut rsrc01, 0x8000_0000 | (lang_dirs_off + i * 24));
    }

    // Type 14 (RT_GROUP_ICON) directory: 1 ID entry (1)
    push_u32(&mut rsrc01, 0);
    push_u32(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 0);
    push_u16(&mut rsrc01, 1);
    push_u32(&mut rsrc01, 1);
    push_u32(&mut rsrc01, 0x8000_0000 | (lang_dirs_off + (count as u32) * 24));

    // Language directories (one per resource item, lang = 0x0409)
    for i in 0..total_items {
        push_u32(&mut rsrc01, 0);
        push_u32(&mut rsrc01, 0);
        push_u16(&mut rsrc01, 0);
        push_u16(&mut rsrc01, 0);
        push_u16(&mut rsrc01, 0);
        push_u16(&mut rsrc01, 1);
        push_u32(&mut rsrc01, 0x0409);
        push_u32(&mut rsrc01, data_entries_off + i * 16);
    }

    // IMAGE_RESOURCE_DATA_ENTRY array (each 16B; OffsetToData relocated against .rsrc$02 symbol #1)
    let mut relocs = Vec::new();
    let reloc_type: u16 = if is_64bit { 0x0003 } else { 0x0007 }; // ADDR32NB
    for (i, &(off_in_rsrc02, sz)) in data_offsets_sizes.iter().enumerate() {
        let entry_pos = data_entries_off + (i as u32) * 16;
        push_u32(&mut rsrc01, off_in_rsrc02);
        push_u32(&mut rsrc01, sz);
        push_u32(&mut rsrc01, 0);
        push_u32(&mut rsrc01, 0);

        // COFF relocation (10 bytes): VirtualAddress(4), SymbolTableIndex(4), Type(2)
        push_u32(&mut relocs, entry_pos);
        push_u32(&mut relocs, 1); // symbol index 1 = .rsrc$02
        push_u16(&mut relocs, reloc_type);
    }

    // Assemble COFF object:
    //   Header (20B) + 2 Section Headers (80B) = 100B
    //   .rsrc$01 raw data
    //   .rsrc$01 relocations (total_items * 10B)
    //   .rsrc$02 raw data
    //   Symbol table (2 symbols * 18B = 36B) + String table (4B)
    let machine: u16 = if is_64bit { 0x8664 } else { 0x014C };
    let sec1_raw_ptr: u32 = 100;
    let sec1_rel_ptr: u32 = sec1_raw_ptr + rsrc01.len() as u32;
    let sec2_raw_ptr: u32 = sec1_rel_ptr + relocs.len() as u32;
    let symtab_ptr: u32 = sec2_raw_ptr + rsrc02.len() as u32;

    let mut obj = Vec::new();
    // COFF File Header (20B)
    push_u16(&mut obj, machine);
    push_u16(&mut obj, 2); // NumberOfSections
    push_u32(&mut obj, 0); // TimeDateStamp
    push_u32(&mut obj, symtab_ptr);
    push_u32(&mut obj, 2); // NumberOfSymbols
    push_u16(&mut obj, 0); // SizeOfOptionalHeader
    push_u16(&mut obj, 0); // Characteristics

    let flags: u32 = 0x4000_0040; // IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ

    // Section 1 Header: .rsrc$01 (40B)
    obj.extend_from_slice(b".rsrc$01");
    push_u32(&mut obj, 0); // VirtualSize
    push_u32(&mut obj, 0); // VirtualAddress
    push_u32(&mut obj, rsrc01.len() as u32);
    push_u32(&mut obj, sec1_raw_ptr);
    push_u32(&mut obj, sec1_rel_ptr);
    push_u32(&mut obj, 0); // PointerToLinenumbers
    push_u16(&mut obj, total_items as u16); // NumberOfRelocations
    push_u16(&mut obj, 0); // NumberOfLinenumbers
    push_u32(&mut obj, flags);

    // Section 2 Header: .rsrc$02 (40B)
    obj.extend_from_slice(b".rsrc$02");
    push_u32(&mut obj, 0);
    push_u32(&mut obj, 0);
    push_u32(&mut obj, rsrc02.len() as u32);
    push_u32(&mut obj, sec2_raw_ptr);
    push_u32(&mut obj, 0);
    push_u32(&mut obj, 0);
    push_u16(&mut obj, 0);
    push_u16(&mut obj, 0);
    push_u32(&mut obj, flags);

    // Section data + relocations
    obj.extend_from_slice(&rsrc01);
    obj.extend_from_slice(&relocs);
    obj.extend_from_slice(&rsrc02);

    // Symbol 0: .rsrc$01 (section 1, storage class 3 = IMAGE_SYM_CLASS_STATIC)
    obj.extend_from_slice(b".rsrc$01");
    push_u32(&mut obj, 0);
    push_u16(&mut obj, 1);
    push_u16(&mut obj, 0);
    obj.push(3);
    obj.push(0);

    // Symbol 1: .rsrc$02 (section 2, storage class 3 = IMAGE_SYM_CLASS_STATIC)
    obj.extend_from_slice(b".rsrc$02");
    push_u32(&mut obj, 0);
    push_u16(&mut obj, 2);
    push_u16(&mut obj, 0);
    obj.push(3);
    obj.push(0);

    // Empty string table (size = 4)
    push_u32(&mut obj, 4);

    fs::write(out_obj, obj)
}

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/icon.png");
    println!("cargo:rerun-if-changed=assets/overlay.png");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        // Automatically add standard Homebrew (Apple Silicon & Intel) and MacPorts SDL2 library paths on macOS
        for dir in &[
            "/opt/homebrew/lib",
            "/usr/local/lib",
            "/opt/local/lib",
        ] {
            if Path::new(dir).exists() {
                println!("cargo:rustc-link-search=native={}", dir);
            }
        }
        if Path::new("/Library/Frameworks").exists() {
            println!("cargo:rustc-link-search=framework=/Library/Frameworks");
        }
    } else if target_os == "windows" {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
        let ico_path = manifest_dir.join("assets").join("icon.ico");
        if ico_path.exists() {
            let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
            let obj_path = out_dir.join("rustypre2_icon.o");
            let is_64 = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap_or_default() == "64";
            if build_windows_icon_coff(&ico_path, &obj_path, is_64).is_ok() {
                println!("cargo:rustc-link-arg={}", obj_path.display());
            }
        }
    }
}
