// SPDX-License-Identifier: GPL-3.0-only
//! SceneCompositor — layers a full-screen planar background with dynamic overlays.

const PLANE_LEN: usize = 0x10000;

/// Decoded full-screen 4-plane background image.
#[derive(Clone, Debug)]
pub struct RecoveredBackground {
    pub planes: [Vec<u8>; 4],
}

/// Scene background wrapper.
#[derive(Clone, Debug)]
pub enum SceneBackground {
    Recovered(RecoveredBackground),
}

/// Status of the composited scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneStatus {
    Complete,
}

/// Overlay callback drawing onto `(planes, page)` in place.
pub type Overlay<'a> = &'a dyn Fn(&mut [Vec<u8>; 4], usize);

/// Compose a scene into fresh EGA planes; returns `(planes, status)`.
pub fn compose_scene(
    background: &SceneBackground,
    overlays: &[Overlay],
    page: usize,
) -> ([Vec<u8>; 4], SceneStatus) {
    let mut planes: [Vec<u8>; 4] = [
        vec![0u8; PLANE_LEN],
        vec![0u8; PLANE_LEN],
        vec![0u8; PLANE_LEN],
        vec![0u8; PLANE_LEN],
    ];

    let status = match background {
        SceneBackground::Recovered(rb) => {
            for p in 0..4 {
                let n = rb.planes[p].len().min(PLANE_LEN);
                planes[p][..n].copy_from_slice(&rb.planes[p][..n]);
            }
            SceneStatus::Complete
        }
    };

    for overlay in overlays {
        overlay(&mut planes, page);
    }

    (planes, status)
}
