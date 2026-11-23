// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 .SQZ asset decompression — Rust port of pre2.codecs.sqz
//!
//! Three formats dispatched by header:
//! - LZSS (b4 4c...): graphics assets — the hot path
//! - LZW (data[1] & 0xF0 == 0x10): keyb/castle/present/titus
//! - Huffman+RLE: sample/theend

const SQZ_LZSS_MAGIC: [u8; 2] = [0xB4, 0x4C];
const LZSS_STREAM_OFFSET: usize = 17;
const LZW_STREAM_OFFSET: usize = 4;

/// Decompress a complete .SQZ file, dispatching on the header.
pub fn unpack_sqz(data: &[u8]) -> Vec<u8> {
    if data.len() < 2 {
        return Vec::new();
    }
    if data[0..2] == SQZ_LZSS_MAGIC {
        unpack_sqz_lzss(data, LZSS_STREAM_OFFSET)
    } else if data[1] & 0xF0 == 0x10 {
        unpack_sqz_lzw(data, LZW_STREAM_OFFSET)
    } else {
        unpack_sqz_other(data)
    }
}

/// Paragraphs the bump allocator advances after this asset.
pub fn sqz_bump_advance(data: &[u8]) -> u16 {
    if data.len() >= 17 && data[0..2] == SQZ_LZSS_MAGIC {
        let dl = data[14] as u16;
        let mut ax = (data[15] as u16 | ((data[16] as u16) << 8)) & 0xFFFF;
        let mut dl = dl >> 2;
        for _ in 0..4 {
            let cf = dl & 1;
            dl >>= 1;
            ax = ((cf << 15) | (ax >> 1)) & 0xFFFF;
        }
        return (ax + 1) & 0xFFFF;
    }
    let reserved = sqz_reserved_size(data);
    ((reserved >> 4) + 1) as u16 & 0xFFFF
}

fn sqz_reserved_size(data: &[u8]) -> u32 {
    if data.len() >= 17 && data[0..2] == SQZ_LZSS_MAGIC {
        return ((data[14] as u32) << 16) | data[15] as u32 | ((data[16] as u32) << 8);
    }
    if data.len() >= 4 && data[1] & 0xF0 == 0x10 {
        return ((data[0] as u32 & 15) << 16) | data[2] as u32 | ((data[3] as u32) << 8);
    }
    if data.len() >= 4 {
        return (((data[0] as u32) | ((data[1] as u32) << 8)) << 16)
            | data[2] as u32
            | ((data[3] as u32) << 8);
    }
    0
}

// ─── LZSS decoder ─────────────────────────────────────────────────────────

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bp: u32, // 16-bit bit accumulator
    dl: u32, // bits remaining
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], start: usize) -> Self {
        let bp = data[start] as u32 | ((data[start + 1] as u32) << 8);
        Self {
            data,
            pos: start + 2,
            bp,
            dl: 16,
        }
    }

    #[inline]
    fn getbit(&mut self) -> u32 {
        let cf = self.bp & 1;
        self.bp >>= 1;
        self.dl -= 1;
        if self.dl == 0 {
            self.bp = self.data[self.pos] as u32 | ((self.data[self.pos + 1] as u32) << 8);
            self.pos += 2;
            self.dl = 16;
        }
        cf
    }
}

fn unpack_sqz_lzss(data: &[u8], start: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(65536);
    let mut bits = BitReader::new(data, start);

    loop {
        // control bit: 1 -> literal, 0 -> match
        if bits.getbit() != 0 {
            // literal byte
            out.push(bits.data[bits.pos]);
            bits.pos += 1;
            continue;
        }

        // match: read low distance byte
        let b1 = bits.getbit();
        let bl = bits.data[bits.pos];
        bits.pos += 1;
        let mut bh: u8 = 0xFF;

        let mut cl: u32 = 0;

        if b1 != 0 {
            // long distance
            bh = ((bh << 1) | bits.getbit() as u8) & 0xFF;
            if bits.getbit() == 0 {
                // distance extension loop
                let mut dh: u8 = 2;
                let mut cl_d = 3;
                loop {
                    if bits.getbit() != 0 {
                        break;
                    }
                    bh = ((bh << 1) | bits.getbit() as u8) & 0xFF;
                    dh = dh.wrapping_add(dh);
                    cl_d -= 1;
                    if cl_d == 0 {
                        break;
                    }
                }
                bh = bh.wrapping_sub(dh);
            }

            // length
            let mut dh: u8 = 2;
            let mut cl_l = 4;
            let mut jumped = false;
            loop {
                dh = dh.wrapping_add(1);
                if bits.getbit() != 0 {
                    jumped = true;
                    break;
                }
                cl_l -= 1;
                if cl_l == 0 {
                    break;
                }
            }
            let mut long_len = false;
            if !jumped {
                if bits.getbit() != 0 {
                    dh = dh.wrapping_add(1);
                    if bits.getbit() != 0 {
                        dh = dh.wrapping_add(1);
                    }
                } else {
                    if bits.getbit() != 0 {
                        // explicit length byte
                        cl = bits.data[bits.pos] as u32 + 0x11;
                        bits.pos += 1;
                        long_len = true;
                    } else {
                        // 3 raw bits + 9
                        dh = 0;
                        for _ in 0..3 {
                            dh = ((dh << 1) | bits.getbit() as u8) & 0xFF;
                        }
                        dh = dh.wrapping_add(9);
                    }
                }
            }
            if !long_len {
                cl = dh as u32;
            }
        } else {
            // short distance
            if bits.getbit() != 0 {
                for _ in 0..3 {
                    bh = ((bh << 1) | bits.getbit() as u8) & 0xFF;
                }
                bh = bh.wrapping_sub(1);
                cl = 2;
            } else {
                // end-of-stream marker
                if bl == bh {
                    break;
                }
                cl = 2;
            }
        }

        let bx = ((bh as i32) << 8) | bl as i32;
        let off = if bx >= 0x8000 { bx - 0x10000 } else { bx };
        let mut ref_pos = out.len() as i32 + off;
        for _ in 0..cl {
            out.push(out[ref_pos as usize]);
            ref_pos += 1;
        }
    }

    out
}

// ─── LZW decoder ──────────────────────────────────────────────────────────

fn unpack_sqz_lzw(data: &[u8], start: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(65536);
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut bitpos = start * 8;

    const CLEAR: u32 = 0x100;
    const END: u32 = 0x101;
    const FIRST: u32 = 0x102;

    loop {
        let mut width: u32 = 9;
        let mut threshold: u32 = 0x200;
        let mut next_code: u32 = FIRST;

        let mut code = get_code(data, &mut bitpos, &mut width, &mut threshold, next_code);
        if code == END {
            break;
        }
        let mut prev: u32 = code;
        let mut last_byte = code as u8;
        out.push(last_byte);

        loop {
            code = get_code(data, &mut bitpos, &mut width, &mut threshold, next_code);
            if code == CLEAR {
                break;
            }
            if code == END {
                return out;
            }
            let saved = code;
            let mut stack: Vec<u8> = Vec::new();
            let mut cur: u32;
            if code >= next_code {
                // KwKwK
                stack.push(last_byte);
                cur = prev;
            } else {
                cur = code;
            }
            while cur >> 8 != 0 {
                stack.push(suffix[cur as usize]);
                cur = prefix[cur as usize] as u32;
            }
            stack.push(cur as u8);
            last_byte = cur as u8;
            // emit reversed
            for &b in stack.iter().rev() {
                out.push(b);
            }
            if next_code < 0x1000 {
                suffix[next_code as usize] = last_byte;
                prefix[next_code as usize] = prev as u16;
                next_code += 1;
            }
            prev = saved;
        }
    }

    out
}

#[inline]
fn get_code(data: &[u8], bitpos: &mut usize, width: &mut u32, threshold: &mut u32, next_code: u32) -> u32 {
    if *threshold == next_code && *width < 12 {
        *width += 1;
        *threshold <<= 1;
    }
    let i = *bitpos >> 3;
    let off = (*bitpos & 7) as u32;
    let b0 = data[i] as u32;
    let b1 = if i + 1 < data.len() { data[i + 1] as u32 } else { 0 };
    let b2 = if i + 2 < data.len() { data[i + 2] as u32 } else { 0 };
    let code = (((b0 << 16) | (b1 << 8) | b2) >> (24 - off - *width)) & ((1 << *width) - 1);
    *bitpos += *width as usize;
    code
}

// ─── Huffman+RLE decoder ──────────────────────────────────────────────────

fn unpack_sqz_other(data: &[u8]) -> Vec<u8> {
    if data.len() < 6 {
        return Vec::new();
    }
    let word0 = data[0] as u32 | ((data[1] as u32) << 8);
    let word1 = data[2] as u32 | ((data[3] as u32) << 8);
    let size = (word0 << 16) | word1;
    let tree_size = data[4] as usize | ((data[5] as usize) << 8);
    let tree = &data[6..6 + tree_size];
    let mut bitpos = (6 + tree_size) * 8;
    let mut out: Vec<u8> = Vec::with_capacity(size as usize);

    let get_bit = |data: &[u8], bitpos: &mut usize| -> u32 {
        let bit = (data[*bitpos >> 3] >> (7 - (*bitpos & 7))) & 1;
        *bitpos += 1;
        bit as u32
    };

    let symbol = |data: &[u8], tree: &[u8], bitpos: &mut usize| -> u32 {
        let mut node: usize = 0;
        loop {
            if get_bit(data, bitpos) != 0 {
                node += 2;
            }
            let val = tree[node] as u32 | ((tree[node + 1] as u32) << 8);
            if val & 0x8000 != 0 {
                return val & 0x7FFF;
            }
            node = val as usize;
        }
    };

    while out.len() < size as usize {
        let sym = symbol(data, tree, &mut bitpos);
        if sym < 0x100 {
            out.push(sym as u8);
            continue;
        }
        let low = sym & 0xFF;
        let count: u32 = if low >= 2 {
            low
        } else if low == 0 {
            symbol(data, tree, &mut bitpos)
        } else {
            ((symbol(data, tree, &mut bitpos) & 0xFF) << 8)
                | (symbol(data, tree, &mut bitpos) & 0xFF)
        };
        let last = *out.last().unwrap();
        for _ in 0..count {
            out.push(last);
        }
    }

    out.truncate(size as usize);
    out
}
