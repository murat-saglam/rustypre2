// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 screen-transition primitives — recovered native logic (pure).
//!
//! The end-level effect (`1030:31D0` loop) is a **circular iris/vignette**: a circle of visible
//! image around the player shrinks to a point over several frames (darkness closing in) while the
//! palette fades. (The ledger long called this the "scale/zoom transition" — it is mathematically a
//! circle, not an image rescale: `build_scaled_columns` reads a quarter-circle cos/sin table
//! × a shrinking radius; `draw_scale_frame` clears everything *outside* that circle.) This
//! module recovers its renderer primitives, one bounded routine at a time, so the transition's
//! pixel work becomes clean recovered source (the multi-frame loop that *drives* them is a thin
//! controller).
//!
//! Primitives recovered here:
//!
//! * `clear_span` (`1030:32DE`) — clear a horizontal pixel span across all four EGA planes, with
//!   partial-byte edge masks. Used to wipe the area outside the iris circle.
//! * `fade_palette` (`1030:6772`) — one step of a linear VGA DAC palette fade from a source
//!   palette toward a target, stepping every component by a growing amount until all arrive.
//!   Used by screen transitions (room/level/death fades).
//!
//! Pure logic operating directly on byte slices and plane buffers. Plane/palette buffers are passed in; the
//! VM↔memory translation lives in the bridge layer.

/// 16 colours × 3 (R,G,B), 6-bit each [asm 6794: cx=0x30]
pub const DAC_COMPONENTS: usize = 0x30;
/// 65 source columns scanned per frame [asm 3244: cmp 0x40, jbe]
pub const SCALE_COLUMNS: usize = 0x41;
/// 320 px [asm 32E3: cmp bx,0x140]
pub const SCREEN_W: usize = 0x140;
/// 200 rows [asm 32EF: cmp dx,0xC8]
pub const SCREEN_H: usize = 0xC8;
/// 40 bytes per row
pub const ROW_STRIDE: usize = 0x28;

/// Interpret a 16-bit value as signed (the transition's compares are `jge` = signed).
#[inline]
fn s16(v: i32) -> i32 {
    let v = v & 0xFFFF;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

/// Clear pixels [x, x+width) at `row` across all 4 EGA planes (1030:32DE).
///
/// `planes` is the four EGA plane buffers; the caller has selected SC map mask 0x0F
/// so every write hits all planes. No-op if out of bounds (matching the ASM guards).
pub fn clear_span(
    planes: &mut [Vec<u8>; 4],
    x: usize,
    width: usize,
    row: usize,
    page: usize,
    stride: usize,
) {
    // [asm 32E3..32F3] bounds guards
    if x >= SCREEN_W || width > SCREEN_W || row >= SCREEN_H {
        return;
    }
    // [asm 32F5..3305]
    let mut di = (row * stride + page + (x >> 3)) & 0xFFFF;
    let x_sub = x & 7;
    let cl;
    if x_sub != 0 || width >= 8 {
        // [asm 3308 jne / 330D cmp 8,jb]
        // left partial: keep the bits before x in this byte, clear from x to byte end.
        // [asm 331E-3322: not(0xFF>>cl)]
        let keep = (!(0xFFu8 >> x_sub)) & 0xFF;
        for p in 0..4 {
            planes[p][di] &= keep; // GC AND, map mask 0x0F
        }
        di = (di + 1) & 0xFFFF; // [asm 3328]
        let total = (width + x_sub) & 0xFFFF; // [asm 3334-3337: cx += x&7]
        let full = total >> 3; // [asm 333C-3340]
        // [asm 3342 je / 3344 dec cx / rep stosb]
        let full_iters = if full > 0 { full - 1 } else { 0 };
        for _ in 0..full_iters {
            for p in 0..4 {
                planes[p][di] = 0;
            }
            di = (di + 1) & 0xFFFF;
        }
        cl = total & 7; // [asm 3349 pop cx / 334A and cl,7]
    } else {
        // aligned + width<8 -> right partial only
        cl = width & 7;
    }
    if cl != 0 {
        // [asm 334D je / 334F-335A]
        let right_keep = (0xFFu8 >> cl) & 0xFF;
        for p in 0..4 {
            planes[p][di] &= right_keep;
        }
    }
}

/// Build one frame's scaled-column table for the end-level iris transition (1030:31F4-3249).
///
/// `src_x`/`src_y` are a **quarter-circle cos/sin table** (`[0x7090]`/`[0x6F90]`,
/// one byte per column: `src_x[i]=round(64·cos)`, `src_y[i]=round(64·sin)`, so
/// `src_x²+src_y²≈64²`). `scale` is `[0x2DD0]` — the iris **radius** (only the low byte
/// is used — `mul byte`); it shrinks each frame. Each kept column is a point on the circle of
/// that radius about `(x_off, y_off)` (`[0x2DC6]`/`[0x2DC8]` — the player). The kept
/// columns form a strictly-decreasing-X envelope clamped to `x_clamp` — one octant of the
/// iris outline (`draw_scale_frame` mirrors it to the full circle). Pure geometry; no VRAM.
///
/// `columns` defaults to `SCALE_COLUMNS`, `running_init` defaults to `0x7D0`.
pub fn build_scaled_columns(
    src_x: &[u8],
    src_y: &[u8],
    scale: i32,
    x_off: i32,
    y_off: i32,
    x_clamp: i32,
    columns: usize,
    running_init: i32,
) -> (Vec<i32>, Vec<i32>) {
    let mut xs: Vec<i32> = Vec::new();
    let mut ys: Vec<i32> = Vec::new();
    let mut running = running_init; // [asm 31F6: [2DCC]=0x7D0]
    let sc = scale & 0xFF; // [asm 320C: mul byte ptr [2DD0]]
    let clamp = s16(x_clamp);

    for i in 0..columns {
        // [asm 3204..3249: bx=0..0x40]
        if i >= src_x.len() || i >= src_y.len() {
            break;
        }
        // [asm 320C-3214]
        let sx = s16((((s16((src_x[i] as i32 * sc) & 0xFFFF)) >> 6) + x_off) & 0xFFFF);
        if sx >= running {
            // [asm 3218-321C: cmp / jge]
            continue;
        }
        if sx >= clamp {
            // [asm 321E-3222: cmp / jge]
            continue;
        }
        running = sx; // [asm 3224: [2DCC]=ax]
        // [asm 322B-3235]
        let sy = s16((((s16((src_y[i] as i32 * sc) & 0xFFFF)) >> 6) + y_off) & 0xFFFF);
        xs.push(sx); // [asm 3227: [si+6B14]=ax]
        ys.push(sy); // [asm 3239: [si+6A88]=ax]
    }
    (xs, ys)
}

/// One frame's clear pass of the circular IRIS transition (1030:324B-32AE).
///
/// Walk rows inward; per row clear everything OUTSIDE the iris circle via `clear_span`,
/// using the circle's 4-fold symmetry (the row's left+right spans and their mirror about
/// `(x_off, y_off)` = the player) and the iris-circle column table. Writes the 4 EGA planes.
///
/// `table_x`/`table_y` are the raw `[0x6B14]`/`[0x6A88]` words (pass the live region, not just
/// the first `count` entries: the loop reads `table_x[si]` every row and may step into stale
/// tail entries exactly as the ASM does). The window is symmetric about `(x_off, y_off)`;
/// `page` is the destination CRTC page `[0x2DD8]`. Caller has set SC map mask 0x0F (all planes)
/// and reset the GC (1030:452B).
///
/// Returns the terminal `(cur_y, cur_x)` — the values the ASM leaves in `[0x2DCA]`/`[0x2DD2]`
/// at 32B0, so a live caller can write the DGROUP scratch back exactly.
///
/// `stride` defaults to `ROW_STRIDE`.
pub fn draw_scale_frame(
    planes: &mut [Vec<u8>; 4],
    table_x: &[i32],
    table_y: &[i32],
    count: usize,
    x_off: i32,
    y_off: i32,
    x_clamp: i32,
    page: usize,
    stride: usize,
) -> (i32, i32) {
    let mut cur_y = y_off; // [asm 324D: [2DCA]=[2DC8]]
    let mut si = 0usize;
    let mut bp = count as i32;
    let mut cur_x = x_clamp & 0xFFFF; // [asm 3253: [2DD2]=[2DC4]]

    loop {
        if si < table_x.len() && table_x[si] == cur_x {
            // [asm 3259-3261]
            bp -= 1; // [asm 3263]
            if bp < 0 {
                // [asm 3264: js 32B0]
                break;
            }
            cur_y = if si < table_y.len() {
                table_y[si]
            } else {
                cur_y
            }; // [asm 3266]
            si += 1; // [asm 326D]
        }
        let w_right = ((0x140i32 - cur_y) & 0xFFFF) as usize; // [asm 3281: cx=0x140-bx]
        let mrow = ((2 * x_off - cur_x) & 0xFFFF) as usize; // [asm 328A-3291]
        let w_left = ((2 * y_off - cur_y) & 0xFFFF) as usize; // [asm 3296-329D]
        clear_span(planes, cur_y as usize, w_right, cur_x as usize, page, stride); // [asm 3287]
        clear_span(planes, cur_y as usize, w_right, mrow, page, stride); // [asm 3293]
        clear_span(planes, 0, w_left, mrow, page, stride); // [asm 32A1]
        clear_span(planes, 0, w_left, cur_x as usize, page, stride); // [asm 32A5]
        cur_x = (cur_x - 1) & 0xFFFF; // [asm 32A8: dec [2DD2]]
        if cur_x == 0 {
            // [asm 32AC: je 32B0]
            break;
        }
    }
    (cur_y & 0xFFFF, cur_x & 0xFFFF) // terminal [0x2DCA]/[0x2DD2]
}

/// One iris frame: build this scale step's circle column table and clear everything OUTSIDE
/// the circle into `planes` (over a base frame already drawn). The single shared compose used
/// by BOTH the verify checkpoint (1030:31F4) and the faithful visual dispatcher — no second
/// copy.
///
/// `tbl_x`/`tbl_y` are the live cos/sin regions (`[0x6B14]`/`[0x6A88]`); the first `len(xs)`
/// entries are overwritten with this frame's scaled columns (the rest stay as the ASM leaves
/// them). Returns `(xs, ys, cur_y, cur_x)` (the scaled tables + the terminal scratch the ASM
/// leaves at 32B0).
pub fn compose_iris(
    planes: &mut [Vec<u8>; 4],
    src_x: &[u8],
    src_y: &[u8],
    scale: i32,
    x_off: i32,
    y_off: i32,
    x_clamp: i32,
    tbl_x: &[i32],
    tbl_y: &[i32],
    page: usize,
) -> (Vec<i32>, Vec<i32>, i32, i32) {
    let (xs, ys) = build_scaled_columns(
        src_x,
        src_y,
        scale,
        x_off,
        y_off,
        x_clamp,
        SCALE_COLUMNS,
        0x7D0,
    );
    let mut tx: Vec<i32> = tbl_x.to_vec();
    let mut ty: Vec<i32> = tbl_y.to_vec();
    for (i, &v) in xs.iter().enumerate() {
        if i < tx.len() {
            tx[i] = v;
        }
    }
    for (i, &v) in ys.iter().enumerate() {
        if i < ty.len() {
            ty[i] = v;
        }
    }
    let (cur_y, cur_x) =
        draw_scale_frame(planes, &tx, &ty, xs.len(), x_off, y_off, x_clamp, page, ROW_STRIDE);
    (xs, ys, cur_y, cur_x)
}

/// One step of a linear DAC palette fade (1030:6772).
///
/// Each of the `DAC_COMPONENTS` 6-bit components moves at most `fade_amt` toward the
/// target; a component within `fade_amt` snaps to `b`. Returns the new 48-byte 6-bit
/// DAC palette and whether *every* component has arrived (the caller then ends the fade).
///
/// `a` is the side being stepped (`lodsb` source); the original swaps `a`/`b` via
/// the direction flag `[0x6C02]` so the same routine runs the fade in either direction.
pub fn fade_palette(a: &[u8], b: &[u8], fade_amt: u8) -> (Vec<u8>, bool) {
    let mut out = vec![0u8; DAC_COMPONENTS];
    let mut all_arrived = true;
    for i in 0..DAC_COMPONENTS {
        let ai = a[i] as i16;
        let bi = b[i] as i16;
        let diff = ai - bi; // [asm 67AE: sub al,ah  (signed)]
        if diff.unsigned_abs() <= fade_amt as u16 {
            // [asm 67B6: cmp al,dl / jbe -> snap]
            out[i] = bi as u8; // [asm 67B8: mov al,ah]
        } else {
            // [asm 67BB: al = orig - (±fade_amt)]
            let step = if diff >= 0 { fade_amt as i16 } else { -(fade_amt as i16) };
            out[i] = ((ai - step) & 0xFF) as u8;
            all_arrived = false; // [asm 67BF: inc bp  (not-arrived count)]
        }
    }
    (out, all_arrived)
}
