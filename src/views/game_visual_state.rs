// SPDX-License-Identifier: GPL-3.0-only
//! Frame-boundary GameVisualState capture — the canonical verification substrate for FaithfulVisual.
//!
//! The faithful mirror must reproduce the page the game DISPLAYS, from state that is internally
//! consistent with that page. An ad-hoc live read of `RendererState` is NOT: the double-buffer +
//! per-frame scroll-ring advance mean a live read describes the page being BUILT (the back buffer
//! `[0x2DD8]`), while the displayed page is the committed FRONT — a *different* frame composed with
//! the previous frame's scroll state.
//!
//! The fix is to capture at the **frame-commit boundary**: ``1030:6772`` (the palette-fade entry —
//! the LAST op in the per-frame main loop, AFTER the page flip). There the CRTC ``ega_display_start``
//! is the just-committed frame and the scroll/camera state has not yet advanced (the next frame's
//! update has not run), so ``render_frame(state)@display_start == display_start`` (byte-exact on
//! driven gameplay). Capturing here gives a snapshot whose state matches the displayed page
//! exactly. NB: the boundary is AFTER 26FA already bumped the frame counter ``[0x6BD5]``, so this
//! capture reads the object camera with ``frame_pre_inc=False`` — reading it "pre-inc" (the
//! default) double-counts and shifts the blink phase ``(frame & 3)`` by one, silently corrupting
//! every BLINKING sprite (hit-flash / spawn-invincibility flicker). That was the old "Δ≤5 blink
//! residual" (harmless on light frames, 660+ px in a busy fight); it is fixed below.
//!
//! RULE: never mix the state for the page being BUILT with the page being DISPLAYED. The committed
//! page is ``ega_display_start`` AT THE BOUNDARY; ``RendererState.dest_page`` is overridden to it
//! so every leaf targets the displayed page.

use crate::engine::state::NativeGameState;
use crate::engine::vga::EGA_PLANE_STRIDE;
use crate::recovered::faithful_visual::{render_visual, FaithfulVisualGap, SceneKind};
use crate::recovered::render_frame::RendererState;
use crate::recovered::render_model::HudChromeAsset;
use crate::views::gameplay_effects::{apply_gameplay_effects, GameplayEffects};
use crate::views::render_state::read_renderer_state_full;

// --- scene-state discriminants ---
const IRIS_RADIUS: usize = 0x2DD0; // [0x2DD0] end-level iris radius (0 = no iris running)
const CAM_X: usize = 0x2DE4; // [0x2DE4] camera X (tiles)
const CAM_Y: usize = 0x2DE6; // [0x2DE6] camera Y (tiles)

// --- iris inputs ---
const IRIS_R_ADDR: usize = 0x2DD0; // [asm 320C: mul byte] iris radius (low byte)
const IRIS_X_OFF: usize = 0x2DC6; // [asm 3214] player X (circle centre)
const IRIS_Y_OFF: usize = 0x2DC8; // [asm 3235] player Y (circle centre)
const IRIS_X_CLAMP: usize = 0x2DC4; // [asm 321E/3253] X clamp + draw start column
const IRIS_COS_T: usize = 0x7090; // [asm 3208] quarter-circle cos table (1 byte/col)
const IRIS_SIN_T: usize = 0x6F90; // [asm 322B] quarter-circle sin table
const IRIS_TBL_X: usize = 0x6B14; // [asm 3227] scaled-X column table (words)
const IRIS_TBL_Y: usize = 0x6A88; // [asm 3239] scaled-Y column table (words)
const IRIS_COLS: usize = 0x41; // SCALE_COLUMNS scanned per frame

#[inline]
fn rws(state: &NativeGameState, off: usize) -> i32 {
    let v = state.rw(off) as i32;
    if v & 0x8000 != 0 { v - 0x10000 } else { v }
}

/// Derive the faithful visual `SceneKind` from `NativeGameState`.
///
/// * IRIS  — the end-level iris is running: radius `[0x2DD0] != 0`.
/// * IMAGE — intro / title artwork: VGA linear video mode 13h/19h.
/// * GAMEPLAY — mode 0Dh and the gameplay heuristic holds (a non-origin camera).
/// * SCENE — mode 0Dh otherwise (menu / map / loading / tally / game-over).
pub fn derive_scene_kind(state: &NativeGameState, video_mode: u8) -> SceneKind {
    if state.rb(IRIS_RADIUS) != 0 {
        return SceneKind::Iris;
    }
    let vm = video_mode & 0x7F;
    if vm == 0x13 || vm == 0x19 {
        return SceneKind::Image;
    }
    if is_gameplay_frame(state) {
        return SceneKind::Gameplay;
    }
    SceneKind::Scene
}

/// Heuristic gameplay gate: a level is loaded with a NON-origin camera
/// `[0x2DE4]/[0x2DE6]`. The menu / map / intro / title scenes all sit at camera (0,0); gameplay
/// (and the level-end tally) have a scrolled camera — and the tally is caught earlier by the iris
/// check, so among 0Dh non-iris frames a non-zero camera means gameplay.
fn is_gameplay_frame(state: &NativeGameState) -> bool {
    (state.rw(CAM_X) | state.rw(CAM_Y)) != 0
}

/// The owned iris compose inputs (the faithful path builds `IrisInputs` references from these so
/// the borrow outlives the `render_visual` call). One frame's iris inputs: the radius, circle
/// centre, clamp, page, the cos/sin source tables, and the current (stale) scaled-column tables
/// (build overwrites the first `count` of these; the tail is read as-is by the clear pass).
#[derive(Clone, Default)]
pub struct IrisInputsOwned {
    pub scale: i32,
    pub x_off: i32,
    pub y_off: i32,
    pub x_clamp: i32,
    pub page: usize,
    pub src_x: Vec<u8>,
    pub src_y: Vec<u8>,
    pub tbl_x: Vec<i32>,
    pub tbl_y: Vec<i32>,
}

/// Read the per-frame iris inputs at the block entry (1030:31F4). `page` overrides the page the
/// ASM read ([0x2DD8]) with the committed/displayed page so the iris clear targets what is on
/// screen.
pub fn read_iris_inputs(state: &NativeGameState, page: usize) -> IrisInputsOwned {
    let base = (crate::views::memory_adapter::DATA_SEG << 4) & 0xFFFFF;
    IrisInputsOwned {
        scale: state.rb(IRIS_R_ADDR) as i32,
        x_off: rws(state, IRIS_X_OFF),
        y_off: rws(state, IRIS_Y_OFF),
        x_clamp: rws(state, IRIS_X_CLAMP),
        page,
        src_x: state.data[base + IRIS_COS_T..base + IRIS_COS_T + IRIS_COLS].to_vec(),
        src_y: state.data[base + IRIS_SIN_T..base + IRIS_SIN_T + IRIS_COLS].to_vec(),
        tbl_x: (0..IRIS_COLS).map(|i| rws(state, IRIS_TBL_X + 2 * i)).collect(),
        tbl_y: (0..IRIS_COLS).map(|i| rws(state, IRIS_TBL_Y + 2 * i)).collect(),
    }
}

/// Return `rs` with every page-relative leaf aimed at `page` (the EGA byte offset of the target
/// page: `[0x2DD8]` back buffer, or `ega_display_start` for the displayed page).
///
/// A `RendererState` carries the page in two coupled places — `dest_page` (the frame composer's
/// target) and `object_camera.dest_page` (the sprite pass's target) — which must stay in lockstep.
/// This is the ONE place that retargets both, so the live render, the frame-boundary capture, and
/// any probe all agree on what "render to page P" means. Pure transform on the dataclass; no VM
/// access.
pub fn retarget_page(rs: &mut RendererState, page: usize) {
    let page = page & 0xFFFF;
    rs.dest_page = page;
    if let Some(ref mut cam) = rs.object_camera {
        cam.dest_page = page;
    }
}

/// A frame-boundary-consistent snapshot of everything FaithfulVisual needs to reproduce the
/// DISPLAYED frame. Captured at the commit boundary (1030:6772) so the state matches the
/// committed page — not the back buffer being built.
pub struct GameVisualState<'a> {
    pub scene_kind: SceneKind,
    /// `RendererState` with dest_page == committed_page (the displayed page); `None` for
    /// non-gameplay/iris scene kinds.
    pub renderer_state: Option<RendererState<'a>>,
    /// `ega_display_start` at the boundary = the page actually on screen.
    pub committed_page: usize,
    /// iris compose inputs when scene_kind==IRIS (else None).
    pub iris: Option<IrisInputsOwned>,
    /// `GameplayEffects` overlays (particles/foreground/fireflies) or None.
    pub effects: Option<GameplayEffects>,
}

/// Capture the GameVisualState for the committed (displayed) page. Call ONLY at the frame-commit
/// boundary (1030:6772) so the read is consistent with `display_page`. `display_page` is the CRTC
/// `ega_display_start` at that instant (the page on screen). `effects` is the captured
/// `GameplayEffects` overlay bundle (drawn after the core frame).
///
/// `force_gameplay` overrides the SceneKind heuristic to GAMEPLAY. The VM-less runtime sets it
/// because it only calls this on a KNOWN gameplay frame (native_frame_step's normal/respawn/reveal
/// paths — real scenes are raised as transitions and handled elsewhere), so a camera at the
/// origin — a respawn checkpoint or a level that starts at (0,0) — is gameplay, not a scene.
/// Without it `is_gameplay_frame` (camera != 0) mis-classifies those as the 0Dh game-over/tally
/// SCENE and fails loud (its documented blind spot).
pub fn capture_game_visual_state<'a>(
    state: &'a NativeGameState,
    video_mode: u8,
    display_page: usize,
    hud_chrome: Option<&'a HudChromeAsset>,
    effects: Option<GameplayEffects>,
    force_gameplay: bool,
) -> GameVisualState<'a> {
    let page = display_page & 0xFFFF;
    let kind = if force_gameplay {
        SceneKind::Gameplay
    } else {
        derive_scene_kind(state, video_mode)
    };
    let mut iris = None;
    let mut rs = None;
    if kind == SceneKind::Gameplay || kind == SceneKind::Iris {
        // frame_pre_inc=False: this is the COMMIT boundary (1030:6772), which is AFTER 26FA already
        // bumped the frame counter [0x6BD5] (the value the engine drew with). The default True adds
        // ANOTHER +1, which shifts the blink phase (frame & 3) by one and silently corrupts every
        // BLINKING sprite — the hit-flash / spawn invincibility flicker draws when the VM erased it
        // and vice-versa. That is the "blink-phase residual". The object_render checkpoint's hybrid
        // path (26FA post-inc) reads with False for exactly this reason; the commit-boundary
        // capture must match.
        let mut r = read_renderer_state_full(state, hud_chrome, false);
        retarget_page(&mut r, page); // target the DISPLAYED page, not [0x2DD8]
        if kind == SceneKind::Iris {
            iris = Some(read_iris_inputs(state, page));
        }
        rs = Some(r);
    }
    GameVisualState {
        scene_kind: kind,
        renderer_state: rs,
        committed_page: page,
        iris,
        effects: if kind == SceneKind::Gameplay || kind == SceneKind::Iris {
            effects
        } else {
            None
        },
    }
}

use std::cell::RefCell;

thread_local! {
    static PLANE_POOL: RefCell<Option<[Vec<u8>; 4]>> = const { RefCell::new(None) };
}

/// Take a zeroed 4x64KB EGA plane array from the thread-local pool (or allocate once on first use).
pub fn take_clean_planes() -> [Vec<u8>; 4] {
    PLANE_POOL.with(|pool| {
        if let Some(mut planes) = pool.borrow_mut().take() {
            for p in &mut planes {
                p.fill(0);
            }
            planes
        } else {
            [
                vec![0u8; EGA_PLANE_STRIDE],
                vec![0u8; EGA_PLANE_STRIDE],
                vec![0u8; EGA_PLANE_STRIDE],
                vec![0u8; EGA_PLANE_STRIDE],
            ]
        }
    })
}

/// Return a 4x64KB EGA plane array to the thread-local pool for zero-allocation reuse on the next frame.
pub fn recycle_planes(planes: [Vec<u8>; 4]) {
    PLANE_POOL.with(|pool| {
        *pool.borrow_mut() = Some(planes);
    });
}

/// Render a captured `GameVisualState` into clean planes; returns `(planes, page)`.
///
/// Produces the COMPLETE displayed gameplay frame: the core `render_frame` (background+sprites+HUD)
/// PLUS the effect overlays (particles, foreground tiles, fireflies). Raises
/// `FaithfulVisualGap` for scenes whose leaf is not recovered yet (no silent fallback). Reuses the
/// SAME recovered leaves the checkpoints verify.
pub fn render_game_visual_state<'a>(gvs: &GameVisualState<'a>) -> ([Vec<u8>; 4], usize) {
    let mut planes: [Vec<u8>; 4] = take_clean_planes();
    match gvs.scene_kind {
        SceneKind::Gameplay => {
            let rs = gvs
                .renderer_state
                .as_ref()
                .expect("renderer_state present for gameplay");
            render_visual(SceneKind::Gameplay, rs, &mut planes, None, None);
            if let Some(ref fx) = gvs.effects {
                apply_gameplay_effects(&mut planes, gvs.committed_page, Some(fx));
            }
        }
        SceneKind::Iris => {
            let rs = gvs
                .renderer_state
                .as_ref()
                .expect("renderer_state present for iris");
            // Render base gameplay frame + foreground/effect overlays FIRST, then mask with iris!
            render_visual(SceneKind::Gameplay, rs, &mut planes, None, None);
            if let Some(ref fx) = gvs.effects {
                apply_gameplay_effects(&mut planes, gvs.committed_page, Some(fx));
            }
            if let Some(ref io) = gvs.iris {
                crate::recovered::transition::compose_iris(
                    &mut planes,
                    &io.src_x,
                    &io.src_y,
                    io.scale,
                    io.x_off,
                    io.y_off,
                    io.x_clamp,
                    &io.tbl_x,
                    &io.tbl_y,
                    io.page,
                );
            }
        }
        _ => {
            // IMAGE/SCENE: no recovered leaf — raise the gap loud (no silent VM-framebuffer
            // fallback), exactly as render_visual does for these kinds.
            panic!("{}", FaithfulVisualGap::new(gvs.scene_kind).message);
        }
    }
    (planes, gvs.committed_page)
}
