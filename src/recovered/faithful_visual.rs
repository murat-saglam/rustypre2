// SPDX-License-Identifier: GPL-3.0-only
//! Faithful visual dispatcher — route a frame to the right recovered visual leaf by scene kind.
//!
//! This is the single composition entry ABOVE `render_frame`. It does NOT replace
//! `render_frame` (the gameplay-frame composer); it ORCHESTRATES the recovered leaves so
//! the WHOLE visual flow — gameplay + transitions + (later) scenes — is one coherent
//! faithful layer instead of a gameplay-only composer. It reuses the SAME recovered
//! functions the checkpoints/probes verify (no second copy): `render_frame` (gameplay) and
//! `compose_iris` (the end-level iris over the gameplay frame).
//!
//! `render_visual` composes `GAMEPLAY` and `IRIS` from recovered leaves; for `IMAGE`/`SCENE`
//! it raises `FaithfulVisualGap` (a loud diagnostic, never a silent VM-framebuffer fallback).
//! The viewer's faithful path composes the RECOVERED non-gameplay scenes from their own
//! recovered leaves.

use crate::recovered::render_frame::{render_frame, RendererState};
use crate::recovered::transition::compose_iris;

/// The scene kind a frame belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneKind {
    /// Normal scrolling gameplay frame -> render_frame.
    Gameplay = 0,
    /// End-level circular iris over the gameplay frame -> render_frame + compose_iris.
    Iris = 1,
    /// Intro / title (mode 13h linear) -> composed by the viewer's faithful 13h path.
    Image = 2,
    /// 0Dh planar: game-over/tally/OLDIES composed via scene_capture; menu/map still blocked.
    Scene = 3,
}

/// The hint text for each unrecovered scene kind.
fn gap_hint(kind: SceneKind) -> &'static str {
    match kind {
        SceneKind::Image => {
            "13h IMAGE — composed by the viewer's faithful 13h path \
             (bridge.image_scene.render_image_scene); render_visual itself only does \
             GAMEPLAY/IRIS only; Image/Scene raised as FaithfulVisualGap"
        }
        SceneKind::Scene => {
            "0Dh SCENE — game-over/tally/OLDIES are composed via scene_capture; the \
             map/CARTE scroll-in via build_carte_page (a pure fn of scroll_x); the \
             mode-select MENU via the stateful MenuScenePage (seeded at the 9718 fill, \
             evolved by the live draw_string/scroll_shift events). Reaching this gap now \
             means a 0Dh scene with no recovered leaf (or a menu with no prior seed, e.g. \
             a mid-menu attach)."
        }
        _ => "recover its leaf and wire it into render_visual",
    }
}

/// The faithful visual layer reached a scene whose recovered leaf does not exist yet.
///
/// Raised LOUD instead of silently falling back to the ASM-populated VRAM — a silent
/// fallback would HIDE exactly the missing visual work we must complete (the "no silent
/// fallback" rule). Carries the `SceneKind` and a precise hint of what to recover.
#[derive(Debug)]
pub struct FaithfulVisualGap {
    pub message: String,
}

impl FaithfulVisualGap {
    pub fn new(scene_kind: SceneKind) -> Self {
        let message = format!(
            "faithful visual gap: {:?} not recovered — {}",
            scene_kind,
            gap_hint(scene_kind)
        );
        Self { message }
    }
}

impl std::fmt::Display for FaithfulVisualGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FaithfulVisualGap {}

/// The duck-typed iris compose inputs (for `IRIS`): `src_x`/`src_y`/`scale`/`x_off`/`y_off`/
/// `x_clamp`/`tbl_x`/`tbl_y`/`page`.
pub struct IrisInputs<'a> {
    pub src_x: &'a [u8],
    pub src_y: &'a [u8],
    pub scale: i32,
    pub x_off: i32,
    pub y_off: i32,
    pub x_clamp: i32,
    pub tbl_x: &'a [i32],
    pub tbl_y: &'a [i32],
    pub page: usize,
}

/// Compose one faithful frame for `scene_kind` into `planes` (clean framebuffer).
///
/// Composes `GAMEPLAY` and `IRIS` here; raises `FaithfulVisualGap` for `IMAGE`/`SCENE`
/// (the viewer's faithful path composes the recovered ones — 13h images, game-over, tally,
/// OLDIES — and only the unrecovered 0Dh compositions surface the gap). NO silent
/// VM-framebuffer fallback, ever.
///
/// `rs` is the gameplay `RendererState`; `iris` (for `IRIS`) carries the iris compose
/// inputs; `dac` is the optional 16-entry DAC palette array.
pub fn render_visual<'a>(
    scene_kind: SceneKind,
    rs: &RendererState<'a>,
    planes: &mut [Vec<u8>; 4],
    iris: Option<&IrisInputs<'_>>,
    dac: Option<&mut [[u8; 3]; 16]>,
) {
    match scene_kind {
        SceneKind::Gameplay => {
            render_frame(rs, planes, dac, true);
        }
        SceneKind::Iris => {
            // the base gameplay frame
            render_frame(rs, planes, dac, true);
            if let Some(iris) = iris {
                // clear everything outside the iris circle
                compose_iris(
                    planes,
                    iris.src_x,
                    iris.src_y,
                    iris.scale,
                    iris.x_off,
                    iris.y_off,
                    iris.x_clamp,
                    iris.tbl_x,
                    iris.tbl_y,
                    iris.page,
                );
            }
        }
        _ => {
            // IMAGE/SCENE: handled by the viewer's scene paths, else loud gap
            panic!("{}", FaithfulVisualGap::new(scene_kind).message);
        }
    }
}
