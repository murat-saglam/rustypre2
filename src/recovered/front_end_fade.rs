// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 front-end DAC palette fades (1030:919F/9286).
//!
//! Non-gameplay 13h screen palette fades (TITUS / PRESENT title).

const STEP_IN: u8 = 2;
const STEP_OUT: u8 = 4;
const IN_CLAMP: u8 = 0x3D;

/// Fade-in: ramp 256-colour DAC from black to target.
/// Returns list of 768-byte snapshots (one per retrace frame).
pub fn fade_in_frames(target: &[u8], n_entries: usize) -> Vec<Vec<u8>> {
    let mut dac = vec![0u8; 0x300];
    let mut frames = Vec::new();

    loop {
        let mut changed = 0;
        for idx in 0..n_entries {
            if idx & 0x1F == 0 {
                frames.push(dac.clone());
            }
            let base = idx * 3;
            for c in 0..3 {
                let cur = dac[base + c];
                if target[base + c] > cur && cur <= IN_CLAMP {
                    dac[base + c] = cur + STEP_IN;
                    changed += 1;
                }
            }
        }
        if changed == 0 { break; }
    }
    frames
}

/// Fade-out: ramp from start palette to black.
pub fn fade_out_frames(start: &[u8], n_entries: usize) -> Vec<Vec<u8>> {
    let mut dac = vec![0u8; 0x300];
    let n = start.len().min(0x300);
    dac[..n].copy_from_slice(&start[..n]);

    let mut frames = Vec::new();
    loop {
        let mut changed = 0u16;
        for idx in 0..n_entries {
            if idx & 0x1F == 0 {
                frames.push(dac.clone());
            }
            let base = idx * 3;
            for c in 0..3 {
                let cur = dac[base + c];
                let new = if cur > STEP_OUT { cur - STEP_OUT } else { 0 };
                dac[base + c] = new;
                changed |= cur as u16;
            }
        }
        if changed == 0 { break; }
    }
    frames
}

/// Palette morph: drift from start toward target (title reveal).
pub fn palette_morph_frames(start: &[u8], target: &[u8], initial_phase: u8) -> Vec<Vec<u8>> {
    let mut saved = vec![0u8; 0x300];
    let n = start.len().min(0x300);
    saved[..n].copy_from_slice(&start[..n]);

    let mut displayed = saved.clone();
    let mut frames = Vec::new();
    let mut bl = initial_phase;

    loop {
        let mut changed = 0;
        let mut latch = 0;
        for i in 0..0x300 {
            let cur = saved[i];
            let diff = target[i].wrapping_sub(cur);
            let adiff = if diff < 0x80 { diff } else { (0u8).wrapping_sub(diff) };

            if diff == 0 {
                // already at target
            } else if adiff < 2 {
                changed += 1;
                saved[i] = target[i];
            } else if target[i] > cur {
                saved[i] = cur + 2;
            } else {
                saved[i] = cur - 2;
            }

            latch += 1;
            if latch == 3 {
                displayed[i - 2..=i].copy_from_slice(&saved[i - 2..=i]);
                latch = 0;
            }
            bl = bl.wrapping_add(1);
            if bl == 0x52 {
                frames.push(displayed.clone());
                bl = 0;
            }
        }
        if changed == 0 { break; }
    }
    frames
}
