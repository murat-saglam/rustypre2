// SPDX-License-Identifier: GPL-3.0-only
//! The shared SPRITE BANK builder (1030:2DFA).
//!
//! At front-end time the game decodes SPRITES.SQZ (unpack_sqz) into a flat run of
//! 4-plane sprites, then rebuilds it into the 5-plane bank the renderer uses:
//! each sprite gains a leading OR-MASK plane (the union of its 4 colour planes —
//! the silhouette used for transparency/collision). The bank grows 5/4 = 1.25x.

/// [asm 2E95-2EAD] Parse the 0x7190 descriptor table into per-sprite per-plane byte size.
///
/// Each entry is a little-endian word (al, ah); the count nibble is ror(al, 3)
/// with a high-bit fixup, and bp = count * ah. The table ends at al == 0.
pub fn sprite_descriptor_sizes(table: &[u8]) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut i = 0;
    while i + 1 < table.len() {
        let al = table[i];
        let ah = table[i + 1];
        if al == 0 {
            break; // [asm 2E97] or al,al; je -> end of table
        }
        let mut a = (al >> 3) | (al << 5); // [asm 2E9B] ror al, 3
        if a & 0xE0 != 0 {
            a = (a & 0x1F) + 1; // [asm 2EA5-2EA7] and 0x1F; inc
        }
        sizes.push(a as usize * ah as usize); // [asm 2EAB] mul ah
        i += 2;
    }
    sizes
}

/// [asm 2E15-2E29] Convert the raw per-sprite Y draw-offset table ([0x752A]) into
/// the bottom-anchored form the renderer expects.
///
/// For each id in order, while the height byte is non-zero:
/// y_off = (height - y_off) & 0xFF.
pub fn bottom_anchor_y_offsets(descriptor: &[u8], draw_offsets: &[u8]) -> Vec<u8> {
    let mut out = draw_offsets.to_vec();
    let mut bx = 0;
    while bx + 1 < out.len() && bx + 1 < descriptor.len() {
        let height = descriptor[bx + 1]; // [asm 2E17] al = [bx+0x7191]
        if height == 0 {
            break; // [asm 2E1B/2E1D] or al,al; je -> table terminator
        }
        out[bx + 1] = height.wrapping_sub(out[bx + 1]); // [asm 2E1F sub / 2E23 neg]
        bx += 2; // [asm 2E27/2E28] inc bx; inc bx
    }
    out
}

/// Build the 5-plane shared sprite bank from the decoded SPRITES.SQZ and the 0x7190 table.
///
/// `sprites` is the flat 4-plane data; the result is each sprite as
/// mask + plane0..3 concatenated (sum(5*bp) bytes), byte-exact vs the ASM.
pub fn build_sprite_bank(sprites: &[u8], table: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut off = 0;
    for bp in sprite_descriptor_sizes(table) {
        let total = 4 * bp;
        if off + total > sprites.len() {
            break;
        }
        let pl = &sprites[off..off + total]; // [asm 2EAF-2EB7] the sprite's 4 planes

        // [asm 2EBA-2ECF] the OR-mask plane: al = src[j] | src[bp+j] | src[2bp+j] | src[3bp+j]
        let mask: Vec<u8> = (0..bp)
            .map(|j| pl[j] | pl[bp + j] | pl[2 * bp + j] | pl[3 * bp + j])
            .collect();
        out.extend_from_slice(&mask); // the OR-mask plane
        out.extend_from_slice(pl); // [asm 2ED1] rep movsw: the 4 planes verbatim
        off += total;
    }
    out
}

/// [asm 2EE2-2F2A] The per-sprite far pointers into the 5-plane bank.
///
/// Offsets accumulate bp*5; when an offset reaches 0x1000 the segment advances 0x100.
/// Returns (offsets, segments) — the [0x5f48] and [0x62e8] tables.
pub fn build_sprite_offset_tables(table: &[u8], base_segment: u16) -> (Vec<u16>, Vec<u16>) {
    let mut offsets = Vec::new();
    let mut segments = Vec::new();
    let mut dx: u16 = 0;
    let mut bx = base_segment & 0xFFFF;
    for bp in sprite_descriptor_sizes(table) {
        offsets.push(dx); // [asm 2F0E] [di+0x5f48] = dx
        segments.push(bx); // [asm 2F12] [di+0x62e8] = bx
        let bp5 = (bp * 5) as u16;
        if dx >= 0x1000 {
            // [asm 2F16-2F22] cmp 0x1000; sub 0x1000; add bx,0x100
            dx = dx.wrapping_sub(0x1000);
            bx = bx.wrapping_add(0x100) & 0xFFFF;
        }
        dx = dx.wrapping_add(bp5); // [asm 2F26] add dx, ax
    }
    (offsets, segments)
}
