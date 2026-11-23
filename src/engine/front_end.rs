// SPDX-License-Identifier: GPL-3.0-only
//! Native front-end flow driver — the VM-less scene state machine (cold boot -> intro -> title -> menu -> map ->
//! gameplay), the counterpart of ``native_frame_step`` for the NON-gameplay screens.
//!
//! The recovered scenes already RENDER. What this module IS is the **scene state
//! machine**, the "border" ``scene.py`` calls out (its line 13: *scene logic / state
//! machine PRODUCES SceneState; render_scene only DRAWS it*). It reproduces main's flow
//! spine (1030:00a0):
//!
//! ```text
//! cold boot -> OLDIES (2505/244E) -> title -> menu (2dfa, mode select) -> map (carte, level select)
//!           -> level-init (013e: native_level_init) -> gameplay (0214: native_frame_step) -> level-end
//!           -> map / next level ...
//! ```
//!
//! Each scene is driven the same way: produce its scene state from the recovered scene
//! logic, render it, then run its per-scene input/transition (the scene-wait 0bbe =
//! fire press+release no-timeout; the menu mode-select; the map level-select). Each
//! function collects FrontEndScene frames into a Vec the caller drains — the Rust
//! collects each displayed scene frame so the
//! runner presents the whole front-end animation, then hands off to native_gameplay_frame
//! once a level starts.

use std::path::Path;

use crate::engine::state::NativeGameState;
use crate::engine::state::DS_BASE;
use crate::engine::vga::{dac8, NativeVGA};
use crate::engine::assets::{load_sqz, resolve_game_path};
use crate::engine::level_init::native_level_start;
use crate::engine::sprite_bank::native_build_sprite_bank;
use crate::engine::r#loop::{native_idle_timer_tick, native_gameplay_frame, FrameTransition};
use crate::recovered::scene::{MODE_LINEAR, MODE_PLANAR};
use crate::recovered::front_end_fade::{fade_in_frames, fade_out_frames, palette_morph_frames};
use crate::recovered::input_decode::decode_input;
use crate::recovered::world_map::{
    MapCamera, map_camera_update, map_page_flip, map_pan,
    mode_select_input, mode_select_text_runs, password_text_runs,
    level_select_dispatch, LevelSelectScreen,
    MenuTextRun,
};
use crate::recovered::carte::{
    build_carte_page, carte_display, carte_marker_offset, stamp_carte_marker, SCROLL_START,
};
use crate::recovered::present::scroll_shift_frame;
use crate::recovered::menu_scene::{MenuScenePage, build_shifted_font};
use crate::recovered::text::draw_string;
use crate::recovered::password::{validate_code, is_cheat_sequence, warp_index_to_level, DEFAULT_SEED};
use crate::codecs::unpack_sqz;

// ── DGROUP offsets (the named fields this island reads/writes) ──────────────────────

const CS_BASE: usize = 0x1030 << 4;

const KEY_TABLE: usize       = 0x27F4;
const FIRE_PRIMARY: usize    = 0x27E8;
const FIRE_ALT: usize        = 0x2832;
const FIRE_LATCH: usize      = 0x282D;
const FIRE_SPACE: usize      = 0x2810;
const KEY_1_LATCH: usize     = 0x27F6;
const KEY_2_LATCH: usize     = 0x27F7;
const IN_UP: usize           = 0x27EA;
const IN_DOWN: usize         = 0x27EB;
const IN_RIGHT: usize        = 0x27EC;
const IN_LEFT: usize         = 0x27ED;
const PENDING_KEY: usize     = 0x2874;
const INPUT_SOURCE: usize    = 0x2879;
const DEMO_PTR: usize        = 0x287A;
const DEMO_BYTE: usize       = 0x287C;
const DEMO_CNT: usize        = 0x287D;
const DEMO_LEVEL: usize      = 0x83E;
const DEMO_MODE: usize       = 0x83D;
const RESPAWN_STATE: usize   = 0x6BE4;
const END_SIGNAL: usize      = 0x6BE5;
const LEVEL_END_MODE: usize  = 0x6BE6;
const LIVES: usize           = 0x27D8;
const BONUS_LETTERS: usize   = 0x6CA7;
const ATTACK_V19: usize      = 0x7B19;
const ATTACK_PHASE: usize    = 0x7B18;
const UTENSILS_MASK: usize   = 0x6CA8;
const LEVEL: usize           = 0x2D8A;
const MODE: usize            = 0xB197;
const MODE_COPY: usize       = 0xB198;
const DISPLAY_PAGE: usize    = 0x2DD6;
const LOAD_TOP: usize        = 0x2875;
const RESET_BASE: usize      = 0x0039;
const FG_BANK: usize         = 0x003B;
const FONT_SEG: usize        = 0x003D;
const YEAR: usize            = 0x0037;
const CARTE_MARKER_TABLE: usize = 0xB148;
const CARTE_MARKER_DIMS: usize  = 0x7522;
const CARTE_MASK_SEG: usize     = 0x667A;
const CARTE_MASK_OFF: usize     = 0x62DA;
const MENU_MORPH_SRC: usize  = 0xACE7;
const ROW_SINE_TABLE: usize  = 0x6F90;
const BIOS_SEED: usize       = 0xA333;
const PALETTE_PTR_TABLE: usize = 0x2D00;
const SONG_INDEX_TABLE: usize = 0x2D20;

// password screen offsets
const PW_CODE: usize         = 0xB1B9;
const PW_HIST0: usize        = 0xB1B3;
const PW_HIST1: usize        = 0xB1B5;
const PW_HIST2: usize        = 0xB1B7;
const PW_ECHO: usize         = 0xB170;
const PW_CURSOR: usize       = 0xB1A8;
const PW_STATUS: usize       = 0xB1AA;
const PW_WRONG_TIMER: usize  = 0xB1B0;

const PW_CHAR_TABLE: usize   = 0xB068;
const PW_WRONG_FRAMES: u16   = 0x8C;

// creators screen constants
const MODE_CREATORS: u8      = 0x12;
const CREATORS_W: usize      = 640;
const CREATORS_PLANE: usize  = 0x81B0;

// ── FrontEndScene ───────────────────────────────────────────────────────────────────

/// One displayed front-end frame — a presentable descriptor the runner renders to RGB.
///
/// Mixed-mode (the front-end uses both VGA modes), so the driver stays free of
/// numpy / VGA rendering:
/// * ``MODE_PLANAR`` (0Dh, OLDIES/menu/map): ``planes`` (4 EGA bitplanes) + ``page``
///   display-start offset;
/// * ``MODE_LINEAR`` (13h, title artwork): ``linear`` (320×200 256-colour image).
///
/// ``palette`` is the 256-entry 8-bit ``(r,g,b)`` DAC to display this frame with
/// (already expanded the way the VGA DAC does — [`dac8`]), so the runner just
/// deplanarizes / indexes and blits.
#[derive(Clone, Debug, Default)]
pub struct FrontEndScene {
    pub mode: u8,
    /// 256 × (r, g, b) 8-bit palette.
    pub palette: Vec<[u8; 3]>,
    /// MODE_PLANAR: 4 plane buffers (None when the render system owns the planes).
    pub planes: Option<[Vec<u8>; 4]>,
    /// MODE_PLANAR: display-start offset (the panned start for menu/map).
    pub page: usize,
    /// MODE_LINEAR: 64000-byte 256-colour image.
    pub linear: Option<Vec<u8>>,
    /// MODE_PLANAR pan: fine pel-pan 0..7 (attr 0x33).
    pub pel: u8,
    /// MODE_PLANAR pan: display-start wrap (0x1FFF for the menu window).
    pub wrap: usize,
    /// MODE_PLANAR pan: CRTC H-display width (carte narrows to 312).
    pub active_width: usize,
    /// Front-end screen id for the touch host's per-screen input mapping
    /// ("menu" = the press-1/2 difficulty screen -> a tap presses '1'). Empty elsewhere.
    pub screen: String,
    /// True = this frame is a game-TICK-rate scene (the VM presents it via 44FB -> the
    /// 3-retrace 1C6F wait, ~23.33Hz), NOT the 70Hz retrace of the static front-end
    /// screens. The attract title animation is such a scene.
    pub game_paced: bool,
    /// ENHANCED-presentation payload for the scrolling map screens.
    pub enh: Option<EnhPayload>,
}

/// Enhanced-presentation payload for the scrolling map screens.
#[derive(Clone, Debug)]
pub enum EnhPayload {
    Carte,
    Menu,
}

impl FrontEndScene {
    /// Construct a MODE_PLANAR scene.
    fn planar(palette: Vec<[u8; 3]>, planes: [Vec<u8>; 4], page: usize) -> Self {
        Self {
            mode: MODE_PLANAR,
            palette,
            planes: Some(planes),
            page,
            linear: None,
            pel: 0,
            wrap: 0xFFFF,
            active_width: 320,
            screen: String::new(),
            game_paced: false,
            enh: None,
        }
    }

    /// Construct a MODE_LINEAR scene.
    fn linear_scene(palette: Vec<[u8; 3]>, image: Vec<u8>) -> Self {
        Self {
            mode: MODE_LINEAR,
            palette,
            planes: None,
            page: 0,
            linear: Some(image),
            pel: 0,
            wrap: 0xFFFF,
            active_width: 320,
            screen: String::new(),
            game_paced: false,
            enh: None,
        }
    }
}

// ── Thread-local callbacks for live front-end driving ──────────────────────────────
//
// The front-end collects frames synchronously into a Vec, but its interactive
// loops (scene-wait, menu, carte) need real-time host input to advance.  These
// thread-locals let the runner install two callbacks before calling
// ``native_front_end``:
//   * FRAME_CB  — called for every emitted frame so the runner can display it.
//   * INPUT_CB  — called before each ``decode_input`` in the interactive loops
//     so the runner can poll SDL2 events and update the key table.
// EMIT_ENABLED is toggled false while title frames are pre-collected into temp
// Vecs (so they are only displayed once, when replayed through skippable_intro).

use std::cell::{Cell, RefCell};

thread_local! {
    static FRAME_CB:
        RefCell<Option<Box<dyn FnMut(&FrontEndScene) + 'static>>> =
        RefCell::new(None);
    static INPUT_CB:
        RefCell<Option<Box<dyn FnMut(&mut NativeGameState) + 'static>>> =
        RefCell::new(None);
    static EMIT_ENABLED: Cell<bool> = Cell::new(true);
}

/// Install the front-end frame-display and input-poll callbacks.
/// Both closures must be `'static` (capture owned/Rc state, not borrows).
pub fn set_front_end_callbacks<F, I>(on_frame: F, poll_input: I)
where
    F: FnMut(&FrontEndScene) + 'static,
    I: FnMut(&mut NativeGameState) + 'static,
{
    FRAME_CB.with(|c| *c.borrow_mut() = Some(Box::new(on_frame)));
    INPUT_CB.with(|c| *c.borrow_mut() = Some(Box::new(poll_input)));
}

/// Emit a frame: if a display callback is installed and EMIT_ENABLED is true,
/// call it so the runner can render the frame; then push to the Vec.
fn emit_frame(frames: &mut Vec<FrontEndScene>, scene: FrontEndScene) {
    let do_emit = EMIT_ENABLED.with(|e| e.get());
    if do_emit {
        let handled = FRAME_CB.with(|c| {
            if let Some(ref mut f) = *c.borrow_mut() {
                f(&scene);
                true
            } else {
                false
            }
        });
        if handled {
            // Avoid accumulating 256KB plane buffers in memory when live-streaming to FRAME_CB,
            // while keeping scene metadata (screen, enh, mode, page, pel, wrap, active_width) intact.
            frames.push(FrontEndScene {
                mode: scene.mode,
                palette: Vec::new(),
                planes: None,
                page: scene.page,
                linear: None,
                pel: scene.pel,
                wrap: scene.wrap,
                active_width: scene.active_width,
                screen: scene.screen,
                game_paced: scene.game_paced,
                enh: scene.enh,
            });
            return;
        }
    }
    frames.push(scene);
}

/// Poll host input before decode_input in the interactive loops.
pub fn poll_fe_input(state: &mut NativeGameState) {
    INPUT_CB.with(|c| {
        if let Some(ref mut f) = *c.borrow_mut() {
            f(state);
        }
    });
}

// ── Flow / error types ──────────────────────────────────────────────────────────────

/// A front-end transition the caller must drive (transition signals:
/// Pre2ExpertEater, Pre2GameOverTransition, Pre2LevelEndTransition, Pre2RespawnTransition,
/// Pre2CaveTeleport, Pre2HybridGap).
#[derive(Debug, Clone)]
pub enum FrontEndError {
    /// [asm 0163] The "YOU MUST BE AN EXPERT EATER" wall — a beginner chose level 8/9.
    ExpertEater,
    /// [asm 5326] The cave/teleport-entrance transition.
    CaveTeleport,
    /// [asm 4CBA/4C74] The level-end transition.
    LevelEnd,
    /// [asm 4F6C] The respawn-to-checkpoint transition.
    Respawn,
    /// [asm 5063] Death -> game-over restart.
    GameOver,
    /// [asm 5034] THE END (level 0xE cleared).
    GameComplete,
    /// A recovered-scene gap (fail loud — no silent ASM fallback).
    HybridGap,
}

impl From<FrameTransition> for FrontEndError {
    fn from(t: FrameTransition) -> Self {
        match t {
            FrameTransition::CaveTeleport(_off) => FrontEndError::CaveTeleport,
            FrameTransition::LevelEnd => FrontEndError::LevelEnd,
            FrameTransition::Respawn => FrontEndError::Respawn,
            FrameTransition::GameOver => FrontEndError::GameOver,
            FrameTransition::GameComplete => FrontEndError::GameComplete,
        }
    }
}

// ── scene-wait phases (the ASM's two busy-wait loops at 0bbe) ────────────────────────

pub const WAIT_PRESS: &str = "press";
pub const WAIT_RELEASE: &str = "release";

// ── palette helpers ─────────────────────────────────────────────────────────────────

/// 6-bit DAC palette (768 bytes) -> 256 × (r,g,b) 8-bit, the way the VGA DAC expands it
/// ([asm out 3C9]).
fn expand_palette6(pal6: &[u8]) -> Vec<[u8; 3]> {
    let mut out = vec![[0u8; 3]; 256];
    let n = (pal6.len() / 3).min(256);
    for i in 0..n {
        out[i] = [
            dac8(pal6[i * 3]),
            dac8(pal6[i * 3 + 1]),
            dac8(pal6[i * 3 + 2]),
        ];
    }
    out
}

/// Yield `steps` intermediate 256-entry DAC palettes ramping between black and
/// `target_dac` (the demo level's live palette) — `into=true` fades IN (black ->
/// target), `into=false` fades OUT (target -> black). The attract's [asm 8EB3/8F29]
/// 9286 fade-to-black transitions bracket the demo; native snapped instantly.
fn dac_fade_palettes(target_dac: &[[u8; 3]; 256], into: bool, steps: usize) -> Vec<Vec<[u8; 3]>> {
    let mut result = Vec::with_capacity(steps);
    for i in 1..=steps {
        let k = i as f32 / steps as f32;
        let f = if into { k } else { 1.0 - k };
        let mut pal = vec![[0u8; 3]; 256];
        for j in 0..256 {
            let r = target_dac[j][0];
            let g = target_dac[j][1];
            let b = target_dac[j][2];
            pal[j] = [
                (r as f32 * f) as u8,
                (g as f32 * f) as u8,
                (b as f32 * f) as u8,
            ];
        }
        result.push(pal);
    }
    result
}

/// [asm 9286] Fade a 0Dh PLANAR screen (menu-map / carte) OUT to black before the next
/// screen — yields FrontEndScene frames holding the current planes while the 16-colour
/// DAC dims to black.
///
/// The VM runs this DAC fade-out (the byte-exact 9286 ramp, reused from the title
/// screens) between the mode-select and the carte, and between the carte and the level
/// load; native snapped instantly. `pal6_off` is the DGROUP offset of the screen's
/// 16-entry 6-bit palette (menu-map 0xB118 / carte 0xB0E8). The planes are frozen (a
/// DAC fade changes only the palette), so we re-yield them with each fading DAC
/// snapshot. `enh` carries the caller's frozen enhanced payload so a wide presentation
/// fades wide (not snapping to 320).
fn planar_fade_out(
    state: &NativeGameState,
    pal6_off: usize,
    planes: &[Vec<u8>; 4],
    page: usize,
    pel: u8,
    wrap: usize,
    enh: Option<EnhPayload>,
    frames: &mut Vec<FrontEndScene>,
) {
    // The screen's 16-colour 6-bit palette (3 bytes per entry × 16 entries = 48 bytes).
    let base = DS_BASE + pal6_off;
    let pal6: Vec<u8> = state.data[base..base + 0x10 * 3].to_vec();
    let frozen: [Vec<u8>; 4] = [
        planes[0].clone(),
        planes[1].clone(),
        planes[2].clone(),
        planes[3].clone(),
    ];
    // [asm 9286] 16-entry DAC ramp to black.
    for snap in fade_out_frames(&pal6, 0x10) {
        emit_frame(frames,FrontEndScene {
            mode: MODE_PLANAR,
            palette: expand_palette6(&snap),
            planes: Some(frozen.clone()),
            page,
            pel,
            wrap,
            active_width: 320,
            screen: String::new(),
            game_paced: false,
            enh: enh.clone(),
            linear: None,
        });
    }
}

// ── native_scene_wait ───────────────────────────────────────────────────────────────

/// [asm 0bbe] The scene-wait primitive, one poll per displayed frame: decode input
/// (DC1) then watch the fire key (``[0x27e8] | [0x2832]``). Two phases mirror the ASM's
/// two busy-wait loops — wait for the key to be PRESSED (0bc3-0bcd: ``or al,[0x2832]|
/// [0x27e8]; je loop``), then RELEASED (0bcf-0bd9: ``or al,…; jne loop``) — then the
/// scene transitions (the RET at 0bdb). Returns ``(new_phase, done)``; ``done`` is
/// True the frame the release completes. ``phase`` starts [`WAIT_PRESS`].
pub fn native_scene_wait(state: &mut NativeGameState, phase: &str) -> (String, bool) {
    // [asm 0bc3 / 0bcf] DC1 input decode
    poll_fe_input(state);
    decode_input(state);
    // [asm 0bc6-0bc9 / 0bd2-0bd5] fire = [0x27e8] | [0x2832]
    let mut fire = (state.rb(FIRE_PRIMARY) | state.rb(FIRE_ALT)) & 0xFF;
    // In live interactive play, also allow arrow keys or 1/2 to advance scene-wait screens (OLDIES)
    if INPUT_CB.with(|c| c.borrow().is_some()) {
        fire |= state.rb(IN_UP)
            | state.rb(IN_DOWN)
            | state.rb(IN_LEFT)
            | state.rb(IN_RIGHT)
            | state.rb(KEY_1_LATCH)
            | state.rb(KEY_2_LATCH);
    }
    if phase == WAIT_PRESS {
        // [asm 0bcd] je: loop until pressed
        return (
            if fire != 0 { WAIT_RELEASE.to_string() } else { WAIT_PRESS.to_string() },
            false,
        );
    }
    // [asm 0bd9] jne: loop until released -> RET 0bdb
    (phase.to_string(), fire == 0)
}

// ── _skippable_intro ────────────────────────────────────────────────────────────────

/// Wrap an intro title generator so a fire-key press can skip it (jumping the flow
/// straight to the menu).
///
/// Not in the VM: the real TITUS / PRESENT titles play uninterruptibly. So when
/// `enabled` is False this just re-yields the scenes with NO input polling —
/// byte-exact. When on (the touch default), it decodes DC1 once per displayed frame
/// and stops early on a FRESH fire press; the key must be seen released first, so a
/// fire held over from the OLDIES skip doesn't instantly abort the title. Returns
/// True if it was skipped, so the caller can drop the remaining intro screens too.
fn skippable_intro(
    scenes: &[FrontEndScene],
    state: &mut NativeGameState,
    enabled: bool,
    frames: &mut Vec<FrontEndScene>,
) -> bool {
    if !enabled {
        for s in scenes {
            emit_frame(frames,s.clone());
        }
        return false;
    }
    let mut armed = false; // only skip on a fresh press (wait for a release first)
    for scene in scenes {
        emit_frame(frames,scene.clone());
        // DC1 input decode (same read as native_scene_wait)
        poll_fe_input(state);
        decode_input(state);
        // fire = [0x27e8] | [0x2832] (space/enter)
        let fire = (state.rb(FIRE_PRIMARY) | state.rb(FIRE_ALT)) & 0xFF;
        if fire == 0 {
            armed = true;
        } else if armed {
            return true; // fresh fire press -> skip the rest of the intro
        }
    }
    false
}

// ── missing-dependency helpers ──────────────────────────────────────────────────────

/// The IMAGE_OFF constant (offset of the 64000-byte image in a decoded 13h asset).
const IMAGE_OFF: usize = 0x300;
const IMAGE_LEN: usize = 64000;
const LOGO_OFF: usize = 0x10300;
const LOGO_LEN: usize = 29760; // 320 × 93
const TITLE_ASSET: &str = "PRESENT.SQZ";

/// Return the recovered 64000-byte linear 256-colour image for `name` (decoded from
/// the asset). For the title (PRESENT.SQZ) the logo-top is overlaid only when
/// `with_logo` (it is copied by a separate ASM pass, 9090, AFTER the background; during
/// the fade-in only the background is on screen).
fn render_image_scene(name: &str, game_root: &Path, with_logo: bool) -> Vec<u8> {
    let path = match resolve_game_path(game_root, name) {
        Ok(p) => p,
        Err(_) => return vec![0u8; IMAGE_LEN],
    };
    let raw = std::fs::read(&path).unwrap_or_default();
    let dec = unpack_sqz(&raw);
    if name == TITLE_ASSET && !with_logo {
        // The 64000-byte background image (320x200 linear) at asset offset 0x300.
        let end = (IMAGE_OFF + IMAGE_LEN).min(dec.len());
        let mut img = vec![0u8; IMAGE_LEN];
        img[..end - IMAGE_OFF].copy_from_slice(&dec[IMAGE_OFF..end]);
        return img;
    }
    if name == TITLE_ASSET && with_logo {
        // Compose the full title: background + logo-top overlaid on the top 93 rows.
        let end = (IMAGE_OFF + IMAGE_LEN).min(dec.len());
        let mut img = vec![0u8; IMAGE_LEN];
        if end > IMAGE_OFF {
            img[..end - IMAGE_OFF].copy_from_slice(&dec[IMAGE_OFF..end]);
        }
        let logo_end = (LOGO_OFF + LOGO_LEN).min(dec.len());
        if logo_end > LOGO_OFF {
            let n = logo_end - LOGO_OFF;
            img[..n].copy_from_slice(&dec[LOGO_OFF..logo_end]);
        }
        return img;
    }
    // Single image @0x300.
    let end = (IMAGE_OFF + IMAGE_LEN).min(dec.len());
    let mut img = vec![0u8; IMAGE_LEN];
    if end > IMAGE_OFF {
        img[..end - IMAGE_OFF].copy_from_slice(&dec[IMAGE_OFF..end]);
    }
    img
}

/// The 256-colour 6-bit VGA palette (768 bytes) at the start of a decoded 13h image
/// asset. The fade routines ramp the DAC toward this palette.
fn image_palette(name: &str, game_root: &Path) -> Vec<u8> {
    let path = match resolve_game_path(game_root, name) {
        Ok(p) => p,
        Err(_) => return vec![0u8; 0x300],
    };
    let raw = std::fs::read(&path).unwrap_or_default();
    let dec = unpack_sqz(&raw);
    let end = 0x300.min(dec.len());
    dec[..end].iter().map(|&b| b & 0x3F).collect()
}

/// [asm 0bdc] The game's palette-set primitive: load `count` 6-bit DAC triples from
/// DGROUP `table_off` into DAC colours 0..count-1. The runner owns the DAC, so it
/// reproduces the load. Values are 6-bit; the VGA DAC expands them (`dac8`).
fn native_load_dac_palette(state: &NativeGameState, dos: &mut NativeVGA, table_off: usize, count: usize) {
    for i in 0..count {
        let off = i * 3;
        // r=low byte, g=high byte (adjacent), b=next byte
        let rg = state.rw(table_off + off);
        let b = state.rb(table_off + off + 2);
        dos.vga_palette[i] = [
            dac8((rg & 0xFF) as u8),
            dac8((rg >> 8) as u8),
            dac8(b),
        ];
    }
}

/// [asm 0ba0] Apply the per-level 16-colour VGA palette the standalone runner owns.
/// PRE2 is a 16-colour planar game; each level has its own 16-entry palette.
/// `[0x2d8a]` (the level) indexes the pointer table `[0x2d00+level*2]`, whose 16 RGB
/// triples (6-bit DAC) load into DAC colours 0..15.
fn native_load_level_palette(state: &NativeGameState, dos: &mut NativeVGA) {
    let level = state.rb(LEVEL) as usize;
    let table_off = state.rw(PALETTE_PTR_TABLE + level * 2) as usize;
    native_load_dac_palette(state, dos, table_off, 0x10);
}

/// Song index -> .TRK filename map (recovered from the snapshot corpus).
const SONG_INDEX_TO_FILE: &[(u8, &str)] = &[
    (0, "PRES.TRK"),
    (1, "CARTE.TRK"),
    (2, "CODE.TRK"),
    (3, "PRESENTA.TRK"),
    (4, "GLACE.TRK"),
    (9, "MINES.TRK"),
    (10, "MYSTERY.TRK"),
    (13, "MONSTER.TRK"),
    (14, "FINAL.TRK"),
    (15, "BRAVO.TRK"),
];

/// [asm 01AB-01B7] The music for the CURRENT level: song index = `[0x2D20 + [0x2d8a]]`
/// mapped to its .TRK. Falls back to MINES for an unmapped index.
fn native_level_song_name(state: &NativeGameState) -> String {
    let level = state.rb(LEVEL) as usize;
    let idx = state.rb(SONG_INDEX_TABLE + level);
    for &(i, name) in SONG_INDEX_TO_FILE {
        if i == idx {
            return name.to_string();
        }
    }
    "MINES.TRK".to_string()
}

/// Reproduce the song loader (1030:02cc) for the VM-less runtime: parse the standard
/// .TRK module's order list into [0xDC7] and its length into [0xDC2].
fn native_load_song(state: &mut NativeGameState, name: &str, game_root: &Path) {
    crate::engine::audio::native_load_song(state, name, game_root);
    poll_fe_input(state);
}

/// [asm 2417] Read the OLDIES credit lines + font from the game state.
/// Returns (lines, font_bytes).
fn read_oldies(state: &NativeGameState) -> (Vec<(usize, usize, String)>, Vec<u8>) {
    let year = state.rw(YEAR);
    // Read the credit strings from DGROUP. The credits use offsets in seg 1A0F.
    // (row byte-offset, start indent, string offset in seg 1A0F) [asm 2417]
    let working_str = read_cstring_at(state, 0x28D6);
    let working_tail = read_cstring_at(state, 0x28F2);
    let working = format!("{}{}{}", working_str, crate::recovered::oldies_screen::format_year(year as i32), working_tail);
    let credits: &[(usize, usize, String)] = &[
        (0x0960, 5, read_cstring_at(state, 0x28CA)),   // "YEAAA > > >"
        (0x0B40, 0, working),   // "MY GAME IS STILL WORKING IN" + <year> + " <<"
        (0x1680, 1, read_cstring_at(state, 0x28F6)),   // "PROGRAMMED IN 1992 ON AT >286 12MHZ>"
        (0x1860, 3, read_cstring_at(state, 0x291B)),   // "> > > ENJOY OLDIES<<"
    ];
    let mut lines = Vec::new();
    for (row, indent, s) in credits {
        lines.push((*row, *indent, s.clone()));
    }
    // Read the font segment [0x3D].
    let font_seg = state.rw(FONT_SEG) as usize;
    let fbase = (font_seg << 4) & 0xFFFFF;
    let font_end = (fbase + 0x1800).min(state.data.len());
    let font = state.data[fbase..font_end].to_vec();
    (lines, font)
}

/// Read a NUL-terminated string at a DGROUP offset.
fn read_cstring_at(state: &NativeGameState, off: usize) -> String {
    let base = (DS_BASE + off) & 0xFFFFF;
    let mut end = base;
    while end < state.data.len() && state.data[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&state.data[base..end]).to_string()
}

/// [asm 2417] Compose the OLDIES scene: black background + the recovered credit text.
/// Returns (4 plane buffers, page).
fn build_oldies_scene(state: &NativeGameState, page: usize) -> [Vec<u8>; 4] {
    let (lines, font) = read_oldies(state);
    let mut planes: [Vec<u8>; 4] = [
        vec![0u8; 0x10000],
        vec![0u8; 0x10000],
        vec![0u8; 0x10000],
        vec![0u8; 0x10000],
    ];
    // Render the credits text using the recovered oldies glyph blitter.
    use crate::recovered::oldies_screen::draw_credit_line;
    for (row, indent, text) in &lines {
        draw_credit_line(&mut planes, *row, *indent, text, page, &font);
    }
    planes
}

/// [asm 0163-0176] True when a BEGINNER (``[0xB197]==0``) has advanced to level 8 or 9
/// (``[0x2D8A]`` in {8, 9}) — where main's 0x0163 gate shows the CASTLE "expert eater"
/// wall (`native_expert_eater`) instead of loading the level.
pub fn is_expert_eater_wall(state: &NativeGameState) -> bool {
    let level = state.rb(LEVEL);
    let mode = state.rb(MODE);
    (level == 8 || level == 9) && mode == 0
}

// ── _password_init / _password_step ────────────────────────────────────────────────

/// Reset the ENTER-CODE screen on entry: empty '[[[[' buffer, no chars/scancode, AND
/// clear the rolling cheat-group history + the wrong-code state so a fresh session
/// starts clean.
fn password_init(state: &mut NativeGameState) {
    // the last-typed scancode latch (shared with DC1)
    state.wb(PENDING_KEY, 0);
    // [asm 9ACB] the empty-slot placeholders (0x5B = '[')
    for i in 0..4 {
        state.wb(PW_ECHO + i, 0x5B);
    }
    state.ww(PW_CURSOR, 0);
    state.ww(PW_CODE, 0);
    state.ww(PW_WRONG_TIMER, 0);
    state.ww(PW_HIST0, 0);
    state.ww(PW_HIST1, 0);
    state.ww(PW_HIST2, 0);
    state.wb(PW_STATUS, 0);
}

/// [asm 9985/99AA-9ADF] One frame of the ENTER-CODE state machine. Returns
/// `Some((level, expert))` when a valid code is entered, `None` otherwise.
fn password_step(state: &mut NativeGameState) -> Option<(usize, bool)> {
    // [asm 99AA/9AB4] the wrong-code feedback pause
    if state.rb(PW_STATUS) == 1 {
        let wt = state.rw(PW_WRONG_TIMER).wrapping_add(1) & 0xFFFF;
        state.ww(PW_WRONG_TIMER, wt);
        if wt >= PW_WRONG_FRAMES {
            // [asm 9AC0] pause elapsed -> clear echo, accept again
            for i in 0..4 {
                state.wb(PW_ECHO + i, 0x5B); // [asm 9AC8] '[[[[' placeholders
            }
            state.wb(PW_STATUS, 0); // [asm 9AD4]
            state.ww(PW_CURSOR, 0); // [asm 9AD9]
        }
        return None; // input ignored while the wrong code is shown
    }

    let scan = state.rb(PENDING_KEY); // [asm 99BE] the pending make code
    if scan == 0 || (scan & 0x80) != 0 {
        return None; // [asm 99C2] none / a break code -> nothing
    }
    state.wb(PENDING_KEY, 0); // [asm 99D6] consume it

    // [asm 99DD] scancode -> ASCII (the char table at 0xB068)
    let al = state.rb((PW_CHAR_TABLE + scan as usize) & 0xFFFF);
    if al == 0x2D {
        return None; // [asm 99E1] '-' (a non-code key, e.g. space) -> ignore
    }
    let mut ah = al.wrapping_sub(0x30); // [asm 99EA] char - '0'
    if ah > 9 {
        ah = ah.wrapping_sub(7); // [asm 99ED/99F2] 'A'..'F' -> extra -7
    }
    // [asm 99F5-9A0D] shift the nibble in
    let code = (state.rw(PW_CODE) << 4) | (ah & 0x0F) as u16;
    state.ww(PW_CODE, code);

    let cur = state.rw(PW_CURSOR) as usize; // [asm 9A11] echo cursor / char count
    state.wb(PW_ECHO + cur, al); // [asm 9A15] echo the char
    state.ww(PW_CURSOR, ((cur + 1) & 0xFFFF) as u16); // [asm 9A19]

    if cur + 1 < 4 {
        return None; // [asm 9A1F] need 4 chars before validating
    }

    state.ww(PW_WRONG_TIMER, 0); // [asm 9A28] reset the wrong-code timer

    // [asm 9A2E-9A36] the three prior groups
    let di = state.rw(PW_HIST0);
    let si = state.rw(PW_HIST1);
    let bp = state.rw(PW_HIST2);

    // [asm 932F] the BIOS seed (0 on the GOG build -> 0x20)
    let seed = {
        let s = state.rw(BIOS_SEED);
        if s != 0 { s } else { DEFAULT_SEED }
    };
    // cs:[5] rotate count (== 3 on this build)
    let rot = state.data[(CS_BASE + 5) & 0xFFFFF];

    let mut m: Option<(usize, bool)> = None;

    // [asm 9A53-9A62] DEAD C0DE F00D held in the history
    if is_cheat_sequence(di, si, bp) {
        let idx = (code.wrapping_sub(1)) & 0xFFFF; // [asm 9A64-9A68] level number
        if idx <= 0x13 {
            // [asm 9A69] a valid warp target
            m = Some(warp_index_to_level(idx as usize)); // [asm 9A9B-9AAA]
        }
    }
    if m.is_none() {
        // [asm 9A6E-9A7F] else the normal per-level codes
        m = validate_code(code, seed, rot);
    }

    if let Some((level, expert)) = m {
        // [asm 9A9B-9AAA] a level matched -> commit + advance
        state.wb(LEVEL, level as u8); // [asm 9AA6] the selected level
        state.wb(MODE, if expert { 1 } else { 0 }); // [asm 9AAA] beginner / expert
        return m;
    }

    // [asm 9A81-9A94] miss -> shift this group into the history
    state.ww(PW_HIST0, si);
    state.ww(PW_HIST1, bp);
    state.ww(PW_HIST2, code);
    state.wb(PW_STATUS, 1); // [asm 9A94] -> the wrong-code feedback pause
    None
}

// ── _native_title_screen ────────────────────────────────────────────────────────────

/// [asm 912b/9090 -> 919f] Drive one 13h title screen as a sequence of FrontEndScene
/// frames: the image is decoded+composited once (`render_image_scene`), then the DAC
/// fade-IN, the hold, and the fade-OUT are the verified `front_end_fade` palettes
/// applied to it.
///
/// `n_entries` = the DAC entry count the fade ramps (TITUS 0x10, PRESENT 0xFF);
/// `hold` = the steady retrace count between fade-in and fade-out ([asm 9146 cx]).
fn native_title_screen(
    game_root: &Path,
    name: &str,
    n_entries: usize,
    hold: usize,
    frames: &mut Vec<FrontEndScene>,
) {
    // [asm 91A4/9090] 64000-byte 13h image
    let image = render_image_scene(name, game_root, true);
    // [asm 91D5 DS:si] the asset's target palette
    let pal6 = image_palette(name, game_root);
    // [asm 919F loop] black -> target
    let fade_in = fade_in_frames(&pal6, n_entries);
    let held = fade_in.last().cloned().unwrap_or_else(|| vec![0u8; 0x300]);
    // [asm 9286 loop] target -> black
    let fade_out = fade_out_frames(&held, n_entries);

    // [asm 919F] fade-in retrace frames
    for p6 in &fade_in {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(p6), image.clone()));
    }
    let held8 = expand_palette6(&held);
    // [asm 9146-914C] steady hold
    for _ in 0..hold {
        emit_frame(frames,FrontEndScene::linear_scene(held8.clone(), image.clone()));
    }
    // [asm 914E -> 9286] fade-out retrace frames
    for p6 in &fade_out {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(p6), image.clone()));
    }
}

// ── _native_present_screen ──────────────────────────────────────────────────────────

/// [asm 9090] Drive the PREHISTORIK-2 (PRESENT.SQZ) title as FrontEndScene frames: the
/// background fades IN (919F, n=0xFF), the logo-top is composited on, then the palette
/// MORPHS toward `morph_target` (911D) — the title's colour reveal.
///
/// `morph_target` is the 6-bit palette at DGROUP 0xACE7 (read from the game state).
fn native_present_screen(
    game_root: &Path,
    morph_target: &[u8],
    frames: &mut Vec<FrontEndScene>,
) {
    // [asm 91A4] fade-in target (background only — logo copied AFTER 919F returns)
    let background = render_image_scene(TITLE_ASSET, game_root, false);
    // [asm 9090 logo copy] morph image (background + logo)
    let full = render_image_scene(TITLE_ASSET, game_root, true);
    let pal6 = image_palette(TITLE_ASSET, game_root);
    // [asm 919F] black -> background palette
    let fade_in = fade_in_frames(&pal6, 0xFF);
    let held = fade_in.last().cloned().unwrap_or_else(|| vec![0u8; 0x300]);
    for p6 in &fade_in {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(p6), background.clone()));
    }
    // the 911D retrace counter enters with the BL the fade-in left = the green of its
    // last entry (entry 0xFE)
    let phase = held[(0xFF - 1) * 3 + 1];
    // [asm 911D] colour reveal
    let morph = palette_morph_frames(&held, morph_target, phase);
    for p6 in &morph {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(p6), full.clone()));
    }
    // [asm 9286] fade the revealed title back OUT to black before the menu loads.
    // The whole 256-colour title fades to black (n=0x100).
    let morph_last = morph.last().cloned().unwrap_or_else(|| held.clone());
    for p6 in fade_out_frames(&morph_last, 0x100) {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(&p6), full.clone()));
    }
}

// ── native_expert_eater ─────────────────────────────────────────────────────────────

/// [asm 0163->0178] The "YOU MUST BE AN EXPERT EATER" wall.
///
/// When a BEGINNER ([0xB197]==0) advances into level 8 or 9 ([0x2D8A] in {8,9}),
/// main's 0x0163 gate shows CASTLE.SQZ (resource 0x2C) INSTEAD of loading the level:
/// fade it IN (919F, cl=0xFF = the full 256-entry DAC), HOLD it while the scene-wait
/// (0BBE) waits for the fire key press+release, then re-enter the press-1/2 MENU.
/// There is NO fade-OUT (unlike THE END).
pub fn native_expert_eater(
    state: &mut NativeGameState,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) {
    // [asm 0178->107B res 0x2c] 64000-byte 13h image
    let image = render_image_scene("CASTLE.SQZ", game_root, true);
    // the 256-entry DAC at the asset's head
    let pal6 = image_palette("CASTLE.SQZ", game_root);
    // [asm 018C cl=0xFF -> 919F] black -> target
    let fade_in = fade_in_frames(&pal6, 0xFF);
    let held = fade_in.last().cloned().unwrap_or_else(|| vec![0u8; 0x300]);

    // [asm 919F] fade-in retrace frames
    for p6 in &fade_in {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(p6), image.clone()));
    }
    let held8 = expand_palette6(&held);
    // [asm 0196 -> 0BBE] wait for fire press+release
    let mut phase = WAIT_PRESS.to_string();
    loop {
        emit_frame(frames,FrontEndScene::linear_scene(held8.clone(), image.clone()));
        let (new_phase, done) = native_scene_wait(state, &phase);
        phase = new_phase;
        if done {
            break;
        }
    }
    // [asm 0199 mov ax,1 / jmp 012F -> 8E45] no fade-out — the caller re-enters the
    // press-1/2 menu directly.
}

// ── native_the_end ──────────────────────────────────────────────────────────────────

/// [asm 5034] THE END — the game-complete screen (the player cleared the final level
/// 0xE, [0x6be5]==0xFF).
///
/// Loads THEEND.SQZ and shows it the way 5034 does: fade it IN (919F, cl=0x10 = 16 DAC
/// entries), HOLD it while the scene-wait (0BBE) waits for the fire key to be pressed
/// AND released, then fade it OUT (9286). A generator yielding FrontEndScene frames so
/// the flow driver presents the whole sequence; when it returns the caller re-enters
/// the front-end (attract/title). FINAL.TRK (the ending song) is loaded by the caller
/// (audio seam).
pub fn native_the_end(
    state: &mut NativeGameState,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) {
    // [asm 5052->919F] 64000-byte 13h image
    let image = render_image_scene("THEEND.SQZ", game_root, true);
    // [asm 5052] DS:si palette (16 live entries)
    let pal6 = image_palette("THEEND.SQZ", game_root);
    // [asm 919F cl=0x10] black -> target
    let fade_in = fade_in_frames(&pal6, 0x10);
    let held = fade_in.last().cloned().unwrap_or_else(|| vec![0u8; 0x300]);

    // [asm 919F] fade-in retrace frames
    for p6 in &fade_in {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(p6), image.clone()));
    }
    let held8 = expand_palette6(&held);
    // [asm 5055 -> 0BBE] wait for fire press+release
    let mut phase = WAIT_PRESS.to_string();
    loop {
        emit_frame(frames,FrontEndScene::linear_scene(held8.clone(), image.clone()));
        let (new_phase, done) = native_scene_wait(state, &phase);
        phase = new_phase;
        if done {
            break;
        }
    }
    // [asm 5058 -> 9286] fade-out retrace frames
    for p6 in fade_out_frames(&held, 0x10) {
        emit_frame(frames,FrontEndScene::linear_scene(expand_palette6(&p6), image.clone()));
    }
    // [asm 505B] the CREATORS screen (year-gated Easter egg) follows the THEEND
    // fade-out, in the same 5034 call.
    native_creators_screen(state, game_root, frames);
}

// ── native_creators_screen ──────────────────────────────────────────────────────────

/// [asm 25F6] The developers' "HOLLY DAY IN MEYRUES!" photo montage shown after THE END
/// (call 505B), gated on the system year `[0x37] >= 0x7CA` (1994) — an Easter egg that
/// is always on now.
///
/// A mode-12h (640x480) 4-plane GRAYSCALE screen: LEVELH.SQZ supplies planes 0,1 and
/// LEVELI.SQZ planes 2,3; the DAC is a 16-step grey ramp. Yields one FrontEndScene
/// (MODE_CREATORS) per frame while the scene-wait (0BBE) holds for the fire key.
pub fn native_creators_screen(
    state: &mut NativeGameState,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) {
    let year = state.rw(YEAR);
    if year < 0x7CA {
        return; // [asm 25F6] year < 1994 -> skip
    }
    // Decode the two SQZ assets (the VM's 12h planar blit needs no CRTC emulation).
    let h = {
        let Ok(path) = crate::engine::assets::resolve_game_path(game_root, "LEVELH.SQZ") else { return; };
        let raw = std::fs::read(&path).unwrap_or_default();
        unpack_sqz(&raw)
    };
    let i = {
        let Ok(path) = crate::engine::assets::resolve_game_path(game_root, "LEVELI.SQZ") else { return; };
        let raw = std::fs::read(&path).unwrap_or_default();
        unpack_sqz(&raw)
    };
    let p = CREATORS_PLANE;
    // planes 0,1,2,3
    let mut planes: [Vec<u8>; 4] = [
        h[..p.min(h.len())].to_vec(),
        if h.len() > p { h[p..(2 * p).min(h.len())].to_vec() } else { vec![] },
        i[..p.min(i.len())].to_vec(),
        if i.len() > p { i[p..(2 * p).min(i.len())].to_vec() } else { vec![] },
    ];
    for pl in planes.iter_mut() {
        pl.resize(p, 0);
    }
    // [asm 2636-264B] 0,4,8,..,0x3C — a 16-step grey ramp
    let mut grey = vec![[0u8; 3]; 256];
    for v in 0..16usize {
        let gv = dac8((v * 4) as u8);
        grey[v] = [gv, gv, gv];
    }
    // [asm 26BE -> 0BBE] wait for fire
    let mut phase = WAIT_PRESS.to_string();
    loop {
        emit_frame(frames,FrontEndScene {
            mode: MODE_CREATORS,
            palette: grey.clone(),
            planes: Some(planes.clone()),
            page: 0,
            linear: None,
            pel: 0,
            wrap: 0xFFFF,
            active_width: CREATORS_W,
            screen: String::new(),
            game_paced: false,
            enh: None,
        });
        let (new_phase, done) = native_scene_wait(state, &phase);
        phase = new_phase;
        if done {
            break;
        }
    }
}

// ── _native_menu_map ────────────────────────────────────────────────────────────────

/// [asm 96D5] Drive the 0Dh scrolling world-map screen VM-less (`kind` = "password"
/// 9985 / "mode_select" 991F). Yields FrontEndScene (MODE_PLANAR) frames — the
/// caveman-tiled map (MOTIF.SQZ) scrolling left, with the screen's text (ENTER CODE +
/// the code, or MODE + BEGINNER/EXPERT) stamped on top each frame via draw_string
/// against the bit-rotated font.
///
/// Returns `Ok(())` when the screen exits (fire pressed or code matched), or
/// `Err(FrontEndError::HybridGap)` on an unrecovered path.
fn native_menu_map(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    game_root: &Path,
    kind: &str,
    frames: &mut Vec<FrontEndScene>,
) -> Result<(), FrontEndError> {
    // [asm 97A5] the map's 16-colour palette
    native_load_dac_palette(state, dos, 0xB118, 0x10);

    let mut page = MenuScenePage::new();
    // [asm 96EC/9718] planes 0,1 from MOTIF.SQZ
    let motif = {
        let path = match resolve_game_path(game_root, "MOTIF.SQZ") {
            Ok(p) => p,
            Err(_) => return Err(FrontEndError::HybridGap),
        };
        let raw = std::fs::read(&path).unwrap_or_default();
        let dec = unpack_sqz(&raw);
        let n = 0x3E80.min(dec.len());
        dec[..n].to_vec()
    };
    page.seed(&motif);

    // the font segment ([0x3d])
    let fseg = state.rw(FONT_SEG) as usize;
    let fbase = (fseg << 4) & 0xFFFFF;
    let font_end = (fbase + 0x3000).min(state.data.len());
    let font_bytes = &state.data[fbase..font_end];
    // [asm 972E] shift copies
    let font = build_shifted_font(font_bytes);

    let pal: Vec<[u8; 3]> = dos.vga_palette.to_vec();

    let scr = if kind == "mode_select" { "mode_select".to_string() } else { String::new() };

    // [asm 978D] phase seeds at 2
    let mut cam = MapCamera { x: 0, row: 0, phase: 2, prev_x: 0, prev_row: 0, blit_off: 0 };
    let mut prev_page: usize = 0;
    let mut prev_arrow = false; // edge-detect the arrow for the BEGINNER/EXPERT toggle
    let mut prev_fire = INPUT_CB.with(|c| c.borrow().is_some()); // require menu fire release first in live play
    // [asm 9AF5] cs:[5] (=3): the vertical sine-bounce is ON
    let bounce = state.data[(CS_BASE + 5) & 0xFFFFF] != 0;
    // [asm 9B00] the row-bounce sine table (DGROUP 0x6f90)
    let sine_base = DS_BASE + ROW_SINE_TABLE;
    let sine_end = (sine_base + 0x100).min(state.data.len());
    let sine: Vec<u8> = state.data[sine_base..sine_end].to_vec();

    let make_scene = |planes: &[Vec<u8>; 4], page_off: usize, pel: u8, _cam_x: u16, _cam_row: u16| FrontEndScene {
        mode: MODE_PLANAR,
        palette: pal.clone(),
        planes: Some([
            planes[0].clone(), planes[1].clone(), planes[2].clone(), planes[3].clone(),
        ]),
        page: page_off,
        pel,
        wrap: 0x1FFF,
        active_width: 320,
        screen: scr.clone(),
        game_paced: false,
        enh: Some(EnhPayload::Menu),
        linear: None,
    };

    if kind == "password" {
        password_init(state); // start with the empty '[[[[' buffer, no stale key
    }

    // frame 0 = the seeded page
    emit_frame(frames,make_scene(&page.planes, 0, 0, cam.x, cam.row));

    let wrap = 0x1FFFusize;

    loop {
        // [asm 9AE0] scroll left 4/frame + row sine-bounce
        cam = map_camera_update(cam, bounce, &sine);
        // [asm 97A8/97F2] display-start AND pel from the SAME (current) x
        let (ds, pel) = map_pan(cam.x, cam.row);
        // [asm 97CA] page_draw=ds, page_clear=prev
        let (page_draw, page_clear) = map_page_flip(ds, prev_page);

        // Clear the text planes (2|3) each frame and re-stamp fresh so the text stays
        // clean (the VM double-buffers the text across the two ring pages).
        for i in 0..0x2000 {
            page.planes[2][i] = 0;
            page.planes[3][i] = 0;
        }

        // [asm callback 9985/991F] stamp the text
        let mode_byte = state.rb(MODE);
        let text_runs: (MenuTextRun, MenuTextRun) = if kind == "password" {
            password_text_runs()
        } else {
            mode_select_text_runs(mode_byte)
        };
        for run in &[text_runs.0, text_runs.1] {
            // Read the NUL-terminated string at DS:run.addr
            let str_off = run.addr as usize;
            let text = read_cstring_bytes(state, str_off);
            // draw_string swaps (page_clear, page_draw) to match the map page-flip order
            // params — the swap is intentional (the map page flip swaps draw/clear).
            draw_string(
                &mut page.planes,
                &text,
                &font,
                cam.blit_off,
                run.pen as usize,
                run.advance as usize,
                page_clear as usize,
                page_draw as usize,
            );
        }

        // [asm 9804] H + V (bounce) pan
        scroll_shift_frame(
            &mut page.planes,
            cam.prev_x as i32,
            cam.x as i32,
            cam.row as i32,
            cam.prev_row as i32,
            page_draw as usize,
            wrap,
        );

        prev_page = page_draw as usize;
        emit_frame(frames,make_scene(&page.planes, ds, pel, cam.x, cam.row));

        // DC1: host keys -> the FSM arrow/fire flags
        poll_fe_input(state);
        decode_input(state);

        if kind == "mode_select" {
            // [asm 994E] an arrow toggles BEGINNER <-> EXPERT
            let arrow_sc: u8 = if state.rb(IN_UP) != 0 { 0x48 }
                else if state.rb(IN_DOWN) != 0 { 0x50 }
                else if state.rb(IN_RIGHT) != 0 { 0x4D }
                else if state.rb(IN_LEFT) != 0 { 0x4B }
                else { 0 };
            if arrow_sc != 0 && !prev_arrow {
                let input = mode_select_input(arrow_sc, false);
                if input.toggle {
                    // flip the selection (the text re-renders next frame)
                    let m = state.rb(MODE);
                    state.wb(MODE, m ^ 1);
                }
            }
            prev_arrow = arrow_sc != 0;
        } else if kind == "password" {
            // [asm 9985] accumulate the typed hex code
            if let Some(m) = password_step(state) {
                // a VALID 4-char code AUTO-ADVANCES (no separate confirm)
                // [0x2D8A]/[0xB197] hold the matched level.
                let (_level, _expert) = m;
                let mode_val = state.rb(MODE);
                state.wb(MODE_COPY, mode_val);
                state.wb(DEMO_MODE, mode_val);
                // [asm 9286] fade to black
                planar_fade_out(
                    state, 0xB118, &page.planes, ds, pel, wrap,
                    Some(EnhPayload::Menu),
                    frames,
                );
                return Ok(()); // ...then the carte + loader for the chosen level
            }
        }

        let fire = (state.rb(FIRE_PRIMARY) | state.rb(FIRE_ALT)) != 0;
        if fire && !prev_fire {
            if kind == "mode_select" {
                // [asm 8F12/8F18/8ED7] commit the difficulty, start L1
                let mode_val = state.rb(MODE);
                state.wb(MODE_COPY, mode_val);
                state.wb(DEMO_MODE, mode_val);
                state.wb(LEVEL, 0); // BEGINNER/EXPERT both begin at level 1
                planar_fade_out(
                    state, 0xB118, &page.planes, ds, pel, wrap,
                    Some(EnhPayload::Menu),
                    frames,
                );
                return Ok(()); // ...then the carte (its own palette) scrolls in
            }
            // password: FIRE with no valid code EXITS the screen too — [0x2D8A] stays
            // 0xFF (set at entry, 8EE9) and the 8E45 dispatch loops back to the
            // press-1/2 menu.
            planar_fade_out(
                state, 0xB118, &page.planes, ds, pel, wrap,
                Some(EnhPayload::Menu),
                frames,
            );
            return Ok(());
        }
        prev_fire = fire;
    }
}

/// Read NUL-terminated bytes at a DGROUP offset.
fn read_cstring_bytes(state: &NativeGameState, off: usize) -> Vec<u8> {
    let base = (DS_BASE + off) & 0xFFFFF;
    let mut end = base;
    while end < state.data.len() && state.data[end] != 0 {
        end += 1;
    }
    state.data[base..end].to_vec()
}

// ── _native_carte ───────────────────────────────────────────────────────────────────

/// [asm 9520] The CARTE world-map scroll-in shown between the mode-select and the level
/// load.
///
/// Loads MAP.SQZ (the 4-plane map master at [0x2875]), stamps the per-level 'you are
/// here' marker (the player's position on the world map) onto it, then reveals it
/// column-by-column with `build_carte_page` (965A) as the CRTC pans. MAP.SQZ is a
/// TRANSIENT load (the loader reloads over it), so the load pointer [0x2875] is
/// restored afterwards. Fire (once released after the mode-select confirm) advances
/// to the loader.
pub fn native_carte(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) -> Result<(), FrontEndError> {
    // [asm 9530] MAP.SQZ loads at the top...
    let saved_top = state.rw(LOAD_TOP);
    let seg = match load_sqz(state, "MAP.SQZ", game_root) {
        Ok(s) => s,
        Err(_) => return Err(FrontEndError::HybridGap),
    };
    // ...but is transient — restore the load pointer so the loader stacks the level
    // exactly where the VM does.
    state.ww(LOAD_TOP, saved_top);

    // the 4-plane map master (planes @0/3E80/7D00/BB80)
    let master_base = (seg as usize) << 4;
    let master_end = (master_base + 0xFA00).min(state.data.len());
    let mut master: Vec<u8> = state.data[master_base..master_end].to_vec();
    master.resize(0x10000, 0);

    // [asm 9543-95CD] stamp the per-level 'you are here' marker.
    let lv = state.rb(LEVEL) as usize;
    let dims = state.rw(CARTE_MARKER_DIMS);
    let mw = ((dims & 0xFF) >> 3) as usize; // marker width (bytes wide)
    let mh = (dims >> 8) as usize;          // marker height (rows)

    // CarteMarkerEntry at CARTE_MARKER_TABLE + lv*4
    let entry_base = (CARTE_MARKER_TABLE + lv * 4) & 0xFFFF;
    let marker_x = state.rw(entry_base) as i32;
    let marker_y = state.rw(entry_base + 2) as i32;

    let di = carte_marker_offset(marker_x, marker_y);

    // [0x667a]:[0x62da] — mask + 4 colour planes
    let mask_seg = state.rw(CARTE_MASK_SEG) as usize;
    let mask_off = state.rw(CARTE_MASK_OFF) as usize;
    let msrc = ((mask_seg << 4) + mask_off) & 0xFFFFF;
    let marker_len = 5 * mw * mh;
    let marker_end = (msrc + marker_len).min(state.data.len());
    let marker: Vec<u8> = state.data[msrc..marker_end].to_vec();

    master = stamp_carte_marker(&master, &marker, di, mw, mh);

    // the carte song
    native_load_song(state, "CARTE.TRK", game_root);
    // [asm 95EB] the carte's 16-colour palette
    native_load_dac_palette(state, dos, 0xB0E8, 0x10);
    let pal: Vec<[u8; 3]> = dos.vga_palette.to_vec();

    // [asm 95E2] carte begins at scroll 8
    let mut scroll_x = SCROLL_START;
    let mut prev_fire = true; // require the mode-select fire to RELEASE first

    loop {
        // [asm 9613] reveal the map up to scroll_x
        let planes = build_carte_page(&master, scroll_x, SCROLL_START);
        // [asm 97A8-style] CRTC display start + pel pan
        let (ds, pel) = carte_display(scroll_x, 0);
        // [asm CRTC r01 -> 312px] the carte narrows the H-display to 312
        emit_frame(frames,FrontEndScene {
            mode: MODE_PLANAR,
            palette: pal.clone(),
            planes: Some([
                planes[0].clone(), planes[1].clone(),
                planes[2].clone(), planes[3].clone(),
            ]),
            page: ds,
            pel,
            wrap: 0x1FFF,
            active_width: 312,
            screen: String::new(),
            game_paced: false,
            enh: Some(EnhPayload::Carte),
            linear: None,
        });

        poll_fe_input(state);
        decode_input(state);
        let fire = (state.rb(FIRE_PRIMARY) | state.rb(FIRE_ALT)) != 0;
        // confirm on a fresh fire press OR when the scroll-in reaches the end
        // (auto-advance)
        if (fire && !prev_fire) || scroll_x >= 639 {
            // [asm 9286] fade to black -> the loader
            planar_fade_out(
                state, 0xB0E8, &planes, ds, pel, 0x1FFF,
                Some(EnhPayload::Carte),
                frames,
            );
            return Ok(());
        }
        prev_fire = fire;
        if scroll_x < 639 {
            scroll_x += 1; // scroll the map in +1/frame
        }
    }
}

// ── native_carte_and_load ───────────────────────────────────────────────────────────

/// [asm 9520 CARTE + main 01A5..01D2] Show the world-map scroll-in for the CHOSEN
/// level, then load and start it.
///
/// The level ([0x2d8a]) and mode ([0xB197]) must already be committed. Yields the carte
/// FrontEndScene frames; on return the level is loaded and ready for
/// `native_gameplay_frame`.
pub fn native_carte_and_load(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) -> Result<(), FrontEndError> {
    // [asm 0163] the EXPERT-EATER gate: main checks it AFTER the level is committed
    // but BEFORE the carte (9520) and the loader (447d).
    if is_expert_eater_wall(state) {
        return Err(FrontEndError::ExpertEater);
    }
    // 9520 CARTE: the world-map scroll-in.
    native_carte(state, dos, game_root, frames)?;

    // The map selected a level ([0x2d8a]); the loader (main 01A5..01D2).
    // [asm 012F / 447D] Reset [0x2875] (LOAD_TOP) to [0x0039] (RESET_BASE) before loading.
    let reset_base = state.rw(RESET_BASE);
    if reset_base != 0 {
        state.ww(LOAD_TOP, reset_base);
    }
    // [asm 01B7] the level song (plays at start)
    let song_name = native_level_song_name(state);
    native_load_song(state, &song_name, game_root);
    // [asm 013e..0155] load + init + level-start (lives, etc.)
    native_level_start(state, game_root);
    // clear the residual key table (the DC1 raw keys)
    let kt_base = DS_BASE + KEY_TABLE;
    for i in 0..0x80 {
        if kt_base + i < state.data.len() {
            state.data[kt_base + i] = 0;
        }
    }
    Ok(())
}

// ── _native_attract ─────────────────────────────────────────────────────────────────

/// [asm 8E98] The menu-idle ATTRACT: show the PREHISTORIK-2 title (the VM's resource-0xC
/// blit), then the CARTE (as a normal level load does), then play the demo's level
/// ([0x83E]) in DEMO-PLAYBACK input mode ([0x2879]=1). Any key or the 0x55AA end
/// sentinel makes DC1 set [0x6BE5] (a game-over-style death), which kills the player
/// and returns to the menu. On the end transition it clears the demo state and
/// returns, and the caller re-shows the menu.
fn native_attract(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) -> Result<(), FrontEndError> {
    let level = state.rb(DEMO_LEVEL); // [asm 8ebb] the demo's level ([0x83E], normally 0 = L1)
    state.wb(LEVEL, level);
    let attract_mode = state.rb(DEMO_MODE); // [asm 8ec4]
    state.wb(MODE_COPY, attract_mode);
    state.wb(PENDING_KEY, 0);

    // [asm 8f2d] the running-caveman title animation (119 frames; abort early if user presses a key)
    if native_attract_title(state, game_root, frames) {
        state.wb(PENDING_KEY, 0);
        return Ok(());
    }
    // [asm 9520] the world-map scroll-in (auto-advances; abort to menu if user presses a key)
    // Preserve the DS:0x3F..0x83F demo buffer across native_carte so its live-mode decode_input
    // polls don't overwrite the built-in attract demo with 631 ticks of idle zeros.
    let saved_demo_buf: Vec<u8> = state.data[DS_BASE + 0x3F..DS_BASE + 0x83F].to_vec();
    native_carte(state, dos, game_root, frames)?;
    state.data[DS_BASE + 0x3F..DS_BASE + 0x83F].copy_from_slice(&saved_demo_buf);
    if state.rb(PENDING_KEY) != 0 {
        state.wb(PENDING_KEY, 0);
        return Ok(());
    }

    // --- now the gameplay demo ---
    // [asm 8eb6] demo-playback input mode
    state.wb(INPUT_SOURCE, 1);
    // Skip the first 3 entries (0..6) of DS:0x3F if they are the 334-tick OLDIES idle wait from the boot snapshot,
    // so the demo caveman starts running and swinging his club immediately (~0.3s in) instead of standing still for 15s!
    let start_ptr = if state.rb(0x3F) == 0 && state.rb(0x40) == 0xFF && state.rb(0x41) == 0 {
        6u16
    } else {
        0u16
    };
    state.ww(DEMO_PTR, start_ptr);
    state.wb(DEMO_BYTE, 0);
    state.wb(DEMO_CNT, 0);
    state.wb(PENDING_KEY, 0); // no pending key yet
    state.wb(RESPAWN_STATE, 0);
    state.wb(END_SIGNAL, 0);
    state.wb(LEVEL_END_MODE, 0);
    // clear the residual key table
    let kt_base = DS_BASE + KEY_TABLE;
    for i in 0..0x80 {
        if kt_base + i < state.data.len() {
            state.data[kt_base + i] = 0;
        }
    }

    let song_name = native_level_song_name(state);
    native_load_song(state, &song_name, game_root);
    // [asm 012F / 447D] Reset [0x2875] (LOAD_TOP) to [0x0039] (RESET_BASE) before loading demo level.
    let reset_base = state.rw(RESET_BASE);
    if reset_base != 0 {
        state.ww(LOAD_TOP, reset_base);
    }
    // load the demo's level
    native_level_start(state, game_root);
    // [asm 0ba0] the per-level DAC palette (else the demo renders under the leftover
    // MENU palette — wrong colours)
    native_load_level_palette(state, dos);

    let disp = state.rw(DISPLAY_PAGE) as usize;
    let mut last_page = disp;
    let mut last_planes: Option<[Vec<u8>; 4]> = None;
    let mut first = true;
    let gr_str = game_root.to_string_lossy().to_string();

    loop {
        poll_fe_input(state);
        if state.rb(PENDING_KEY) != 0 {
            break;
        }
        match native_gameplay_frame(state) {
            Ok(()) => {
                crate::engine::render::native_sync_render_state(state);
                let disp_cur = state.rw(DISPLAY_PAGE) as usize;
                let mut opts = crate::engine::render::RenderOptions::default();
                opts.force_gameplay = true;
                let (planes_opt, page) = crate::engine::render::native_render(
                    state,
                    dos,
                    disp_cur,
                    &gr_str,
                    opts,
                );
                if first {
                    // [asm 8EB3 fade-to-black -> the demo] fade IN from black over
                    // the demo's first rendered frame
                    first = false;
                    for pal in dac_fade_palettes(&dos.vga_palette, true, 0x10) {
                        emit_frame(frames,FrontEndScene {
                            mode: MODE_PLANAR,
                            palette: pal,
                            planes: planes_opt.clone(),
                            page,
                            linear: None,
                            pel: 0,
                            wrap: 0xFFFF,
                            active_width: 320,
                            screen: String::new(),
                            game_paced: false,
                            enh: None,
                        });
                    }
                }
                emit_frame(frames,FrontEndScene {
                    mode: MODE_PLANAR,
                    palette: dos.vga_palette.to_vec(),
                    planes: planes_opt.clone(),
                    page,
                    linear: None,
                    pel: 0,
                    wrap: 0xFFFF,
                    active_width: 320,
                    screen: String::new(),
                    game_paced: true,
                    enh: None,
                });
                last_page = page;
                last_planes = planes_opt;
            }
            Err(transition) => {
                // any end/gap in the idle demo -> back to the menu
                match transition {
                    FrameTransition::GameOver
                    | FrameTransition::LevelEnd
                    | FrameTransition::Respawn
                    | FrameTransition::CaveTeleport(_)
                    | FrameTransition::GameComplete => break,
                }
            }
        }
    }

    // fade the demo OUT to black before re-showing the menu
    if !first && last_planes.is_some() {
        for pal in dac_fade_palettes(&dos.vga_palette, false, 0x10) {
            emit_frame(frames,FrontEndScene {
                mode: MODE_PLANAR,
                palette: pal,
                planes: last_planes.clone(),
                page: last_page,
                linear: None,
                pel: 0,
                wrap: 0xFFFF,
                active_width: 320,
                screen: String::new(),
                game_paced: false,
                enh: None,
            });
        }
    }

    // back to live keyboard input
    state.wb(INPUT_SOURCE, 0);
    state.ww(DEMO_PTR, 0);
    state.wb(PENDING_KEY, 0);
    state.wb(RESPAWN_STATE, 0);
    state.wb(END_SIGNAL, 0);
    state.wb(LEVEL_END_MODE, 0);
    // clear the residual key table
    let kt_base = DS_BASE + KEY_TABLE;
    for i in 0..0x80 {
        if kt_base + i < state.data.len() {
            state.data[kt_base + i] = 0;
        }
    }

    Ok(())
}

// ── native_attract_title ───────────────────────────────────────────────────────────

/// [asm 8F2D..9046] The attract-title animation — the running-caveman title.
///
/// After the mode-select menu sits idle past the timeout, the VM plays a scripted
/// 2-phase animation over the jungle "PREHISTORIK 2" title (MENU2.SQZ, a 16-colour
/// planar EGA image):
/// * PHASE 1: a caveman runs RIGHT across the screen chased by a dino, until the
///   caveman scrolls off the right edge.
/// * PHASE 2: the dino record is respawned as 3 staggered bouncing objects and the
///   caveman runs BACK LEFT until off the left edge.
///
/// Returns `true` if interrupted early by a key press (`[0x2874] != 0`).
fn native_attract_title(
    state: &mut NativeGameState,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) -> bool {
    use crate::recovered::player::player_advance_anim;

    const ATTRACT_PAGE: usize = 0x2000;

    // Decode MENU2.SQZ as a 16-colour planar EGA image (4 planes + 48-byte palette).
    let (base_planes, pal6) = title_planar_image("MENU2.SQZ", game_root);
    let pal256 = expand_palette6(&pal6);

    let make_frame = |planes: [Vec<u8>; 4]| FrontEndScene {
        mode: MODE_PLANAR,
        palette: pal256.clone(),
        planes: Some(planes),
        page: ATTRACT_PAGE,
        linear: None,
        pel: 0,
        wrap: 0xFFFF,
        active_width: 320,
        screen: String::new(),
        game_paced: true, // the VM presents each animation frame at the game rate
        enh: None,
    };

    // -------- SETUP (8F2D..8F8F) -------- #
    // Clear all 74 render slots (0x4F0A..0x5720) so no leftover level entities or hit-flash
    // flags from a previous attract demo cycle leak onto the MENU2 title screen!
    for k in 0..0x74 {
        let slot = 0x4F0A + k * 0x12;
        state.ww(slot + 4, 0xFFFF);
        state.wb(slot + 5, 0);
        state.wb(slot + 0x11, 0);
    }

    // [8F46..8F50] light-fade state
    state.wb(0x6C01, 0); // to_dark = 0
    state.wb(0x6C02, 1); // to_light = 1
    state.wb(0x6C03, 0); // step = 0
    // [8F55] fine-scroll byte + row_factor
    state.wb(0x6BC4, 0);
    state.ww(0x6BF8, 0);
    // [8F5C/8F5F] camera = origin
    state.ww(0x2DE4, 0); // cam_col
    state.ww(0x2DE6, 0); // cam_row
    state.ww(0x6BA4, 0); // cam_x
    state.ww(0x6BC6, 0); // cam_y

    // caveman (= player slot) X=0 Y=0xA9 facing=right
    state.ww(0x4F1C, 0);       // x = 0
    state.ww(0x4F1E, 0xA9);    // y = 0xA9
    state.ww(0x4F1C + 9, 0);   // facing = 0 (right)
    state.ww(0x4F1C + 6, 0);   // xvel = 0
    state.ww(0x4F1C + 0xC, 0x7BA7); // anim_ptr = 0x7BA7

    // the dino / first bounce object (shares the projectile arena at 0x4F2E)
    let dino_slot = 0x4F2E;
    state.ww(dino_slot, 0x30);       // x = 0x30
    state.ww(dino_slot + 2, 0xA9);    // y = 0xA9
    state.ww(dino_slot + 9, 0);       // dino.source = 0 (facing right, NOT +4!)
    state.ww(dino_slot + 6, 0);       // xvel = 0
    state.ww(dino_slot + 0xC, 0xAB4B); // anim_ptr = 0xAB4B

    state.wb(0x6BC5, 0); // glider = 0

    // -------- PHASE 1 (8F94..8FC4): caveman runs right, dino chases -------- #
    let mut bp: u16 = 0;
    loop {
        poll_fe_input(state);
        if state.rb(PENDING_KEY) != 0 {
            return true;
        }
        bp = bp.wrapping_add(1); // [8F96] inc bp

        // [8F9B] 638B — advance the caveman's animation frame
        let pv_facing = state.rb(0x4F1C + 9);
        let pv_anim_ptr = state.rw(0x4F1C + 0xC);
        let (frame, new_ptr, bcf) = player_advance_anim(state, pv_anim_ptr, pv_facing);
        state.ww(0x4F1C + 4, frame); // sprite
        state.ww(0x4F1C + 0xC, new_ptr); // anim_ptr
        state.wb(0x6BCF, bcf); // anim_hi

        // [8FA1] 9047 — advance the dino's animation
        advance_obj_anim(state, dino_slot);

        // [8FA4 restore + 8FA7 26FA] — compose the frame (base planes + sprites)
        let mut planes: [Vec<u8>; 4] = [
            vec![0u8; 0x10000],
            vec![0u8; 0x10000],
            vec![0u8; 0x10000],
            vec![0u8; 0x10000],
        ];
        for p in 0..4 {
            let n = base_planes[p].len().min(0x10000 - ATTRACT_PAGE);
            planes[p][ATTRACT_PAGE..ATTRACT_PAGE + n].copy_from_slice(&base_planes[p][..n]);
        }
        // Render sprites using the recovered renderer & check if caveman is still on screen
        let onscreen = compose_attract_frame(state, &mut planes, ATTRACT_PAGE);

        // [8FAA] present
        emit_frame(frames,make_frame(planes));

        // [8FAD] caveman X += 5
        let pv_x = state.rw(0x4F1C);
        state.ww(0x4F1C, pv_x.wrapping_add(5));
        // [8FB4] dino += 5 / 4 every 4th
        let dino_x = state.rw(dino_slot);
        let dino_step = if bp & 3 != 0 { 5 } else { 4 };
        state.ww(dino_slot, dino_x.wrapping_add(dino_step));

        // [8FBF/8FC4] exit when caveman off-screen
        if !onscreen {
            break;
        }
    }

    // -------- PHASE 2 setup (8FC6..9000): kill dino, spawn 3 bouncing objects -------- #
    state.ww(dino_slot + 9, 0xFFFF); // [8FC8] dino.source = 0xFFFF (H-flip left for the 3 bouncing enemies!)
    state.ww(dino_slot + 0xE, 0);   // [8FCE] dino.yvel = 0
    state.ww(0x4F1C + 9, 0xFFFF);   // [8FD4] caveman facing = left (H-flip)

    // [8FDA] rep movsw 4F2E->4F40, OVERLAPPING forward copy (propagates the dino record)
    for w in 0..0x3F {
        let s = 0x4F2E + w * 2;
        let t = 0x4F40 + w * 2;
        let sb = (DS_BASE + s) & 0xFFFFF;
        let tb = (DS_BASE + t) & 0xFFFFF;
        if tb + 1 < state.data.len() && sb + 1 < state.data.len() {
            state.data[tb] = state.data[sb];
            state.data[tb + 1] = state.data[sb + 1];
        }
    }

    // [8FEB/8FEE] staggered X/Y for the 3 bounce objects
    let mut ax = state.rw(0x4F1C);       // pv.x
    let mut dx = state.rw(0x4F1C + 2);    // pv.y
    for i in 0..3 {
        ax = ax.wrapping_add(0x1E);  // [8FF2] staggered X
        dx = dx.wrapping_sub(3);     // [8FF5] staggered Y
        let obj = 0x4F2E + i * 0x12;
        state.ww(obj, ax);
        state.ww(obj + 2, dx);
    }

    // -------- PHASE 2 loop (9002..9044): objects fall/bounce, caveman runs back left -------- #
    loop {
        poll_fe_input(state);
        if state.rb(PENDING_KEY) != 0 {
            return true;
        }
        let pv_x = state.rw(0x4F1C) as i16;
        if pv_x < 0 {
            break; // [9044 jns] while caveman X >= 0
        }

        // [9002] 638B — caveman anim
        let pv_facing = state.rb(0x4F1C + 9);
        let pv_anim_ptr = state.rw(0x4F1C + 0xC);
        let (frame, new_ptr, bcf) = player_advance_anim(state, pv_anim_ptr, pv_facing);
        state.ww(0x4F1C + 4, frame);
        state.ww(0x4F1C + 0xC, new_ptr);
        state.wb(0x6BCF, bcf);

        // [900F..9034] per bouncing object
        for i in 0..3 {
            let obj = 0x4F2E + i * 0x12;
            // [900F] X -= 5
            let ox = state.rw(obj);
            state.ww(obj, ox.wrapping_sub(5));
            // [9012] 9047 — advance the object's anim
            advance_obj_anim(state, obj);
            // [9015] bounce: Y > 0xA9 -> negate velocity
            let oy = state.rw(obj + 2);
            let mut yvel = state.rw(obj + 0xE) as i16;
            if oy > 0xA9 {
                yvel = -yvel;
            }
            // [901F] vel += 8 (gravity)
            let yvel_u = (yvel as i32 + 8) as u16;
            state.ww(obj + 0xE, yvel_u);
            // [9023] Y += vel>>4
            let yvel_s = yvel_u as i16;
            state.ww(obj + 2, oy.wrapping_add((yvel_s >> 4) as u16));
        }

        // [9036 restore + 9039 26FA] — compose + present
        let mut planes: [Vec<u8>; 4] = [
            vec![0u8; 0x10000],
            vec![0u8; 0x10000],
            vec![0u8; 0x10000],
            vec![0u8; 0x10000],
        ];
        for p in 0..4 {
            let n = base_planes[p].len().min(0x10000 - ATTRACT_PAGE);
            planes[p][ATTRACT_PAGE..ATTRACT_PAGE + n].copy_from_slice(&base_planes[p][..n]);
        }
        compose_attract_frame(state, &mut planes, ATTRACT_PAGE);
        emit_frame(frames,make_frame(planes));

        // [903F] caveman X -= 7
        let px = state.rw(0x4F1C);
        state.ww(0x4F1C, px.wrapping_sub(7));
    }
    false
}

/// [asm 9047] Generic object animation stepper for the attract title.
fn advance_obj_anim(state: &mut NativeGameState, slot: usize) {
    let mut bx = state.rw(slot + 0xC) as usize; // anim_ptr
    let mut ax = state.rw(bx);
    if ax & 0x8000 != 0 {
        // [9051] negative = loop marker: rewind + reload
        let delta = ax as i16 as i32;
        bx = ((bx as i32 + delta) & 0xFFFF) as usize;
        ax = state.rw(bx);
    }
    // [9057] ah&=0x1f (keep low 13 bits); [905A] +0x138
    let masked = (ax & 0x1FFF) + 0x138;
    // [905D/9063] ah |= [si+9]&0x80 (facing flip bit)
    let flip = state.rb(slot + 9) & 0x80;
    let sprite = (masked & 0xFFFF) | ((flip as u16) << 8);
    state.ww(slot + 4, sprite); // [9065] [si+4] = ax
    state.ww(slot + 0xC, bx.wrapping_add(2) as u16); // [906A] [si+0xc] = bx+2
}

/// Render the attract-title sprites (the caveman + dino/bounce objects) onto the
/// planes using the recovered renderer, and return true if the caveman (0x4F1C) is on screen.
fn compose_attract_frame(state: &mut NativeGameState, planes: &mut [Vec<u8>; 4], page: usize) -> bool {
    use crate::recovered::object_render::{plan_frame, plan_sprite, paint_sprite, Sprite, SpriteAttr, Camera};

    // Pin the render page so cam.dest_page is stable
    state.ww(0x2DD8, page as u16);

    // Build the sprite attribute lookup
    let attrs = |sprite_id: u16| -> Option<SpriteAttr> {
        let bx = ((sprite_id & 0x1FFF) << 1) as usize;
        let wh = state.rw(0x7190 + bx);
        Some(SpriteAttr {
            width: (wh & 0xFF) as i32,
            height: ((wh >> 8) & 0xFF) as i32,
            x_off: state.rb(0x752A + bx) as i32,
            y_off: state.rb(0x752A + bx + 1) as i32,
            src_seg: state.rw(0x62E8 + bx) as usize,
            src_off: state.rw(0x5F48 + bx) as usize,
        })
    };

    // Read the camera (frame_pre_inc = false, global_shift = 3)
    let cam = Camera {
        cam_x: state.rw(0x2DE4) as i16 as i32,
        cam_y: state.rw(0x2DE6) as i16 as i32,
        fine_scroll: state.rb(0x6BC4) as i32,
        row_factor: state.rw(0x6BF8) as i16 as i32,
        dest_page: page & 0xFFFF,
        row_stride: state.rw(0x2DB0) as usize,
        global_shift: 3,
        frame: state.rw(0x6BD5) as u32,
    };

    // Check if caveman (0x4F1C) is on screen ([8FBF/8FC4] test [0x4F21] & 0x20 set by 26FA)
    let pv_sid = state.rw(0x4F1C + 4);
    let pv_spr = Sprite {
        x: state.rw(0x4F1C) as i16 as i32,
        y: state.rw(0x4F1C + 2) as i16 as i32,
        sprite_id: pv_sid,
        flags: state.rb(0x4F1C + 5),
        life: state.rb(0x4F1C + 0x11),
    };
    let caveman_onscreen = attrs(pv_sid)
        .and_then(|a| plan_sprite(&pv_spr, &a, &cam))
        .is_some();

    // Collect active sprites (0x4F0A..0x5720)
    let mut sprites: Vec<Sprite> = Vec::new();
    let mut off = 0x5720usize;
    while off > 0x4F0A {
        off -= 0x12;
        let sprite_id = state.rw(off + 4);
        sprites.push(Sprite {
            x: state.rw(off) as i16 as i32,
            y: state.rw(off + 2) as i16 as i32,
            sprite_id,
            flags: state.rb(off + 5),
            life: state.rb(off + 0x11),
        });
    }

    // Render each visible sprite
    for draw in plan_frame(&sprites, &attrs, &cam) {
        let attr = match attrs(draw.sprite_id) {
            Some(a) => a,
            None => continue,
        };
        let bank_seg = attr.src_seg;
        let bank_base = (bank_seg << 4) & 0xFFFFF;
        let src_off = attr.src_off;
        let size = (attr.width * attr.height * 6 + 64) as usize;
        let bank_end = (bank_base + src_off + size).min(state.data.len());
        let bank_data = &state.data[bank_base + src_off..bank_end];
        paint_sprite(planes, &draw, bank_data, cam.row_stride);
    }

    caveman_onscreen
}

/// [asm resource-0xC] Decode a 16-colour PLANAR EGA image asset to its four 8000-byte
/// bitplanes + 16-entry 6-bit palette. Returns `(planes: [Vec<u8>; 4], pal6: Vec<u8>)`.
fn title_planar_image(name: &str, game_root: &Path) -> ([Vec<u8>; 4], Vec<u8>) {
    let path = match resolve_game_path(game_root, name) {
        Ok(p) => p,
        Err(_) => return (
            [vec![0u8; 8000], vec![0u8; 8000], vec![0u8; 8000], vec![0u8; 8000]],
            vec![0u8; 48],
        ),
    };
    let raw = std::fs::read(&path).unwrap_or_default();
    let dec = unpack_sqz(&raw);
    let plane_len = 320 * 200 / 8; // 8000 bytes per EGA plane
    let mut planes: [Vec<u8>; 4] = [
        vec![0u8; plane_len],
        vec![0u8; plane_len],
        vec![0u8; plane_len],
        vec![0u8; plane_len],
    ];
    for p in 0..4 {
        let start = p * plane_len;
        let end = (start + plane_len).min(dec.len());
        if end > start {
            planes[p][..end - start].copy_from_slice(&dec[start..end]);
        }
    }
    let pal_start = 4 * plane_len;
    let pal_end = (pal_start + 0x10 * 3).min(dec.len());
    let pal6: Vec<u8> = if pal_end > pal_start {
        dec[pal_start..pal_end].iter().map(|&b| b & 0x3F).collect()
    } else {
        vec![0u8; 48]
    };
    (planes, pal6)
}

// ── native_menu_flow ────────────────────────────────────────────────────────────────

/// [asm 8e45..01D2] The front-end from the press-1/2 MENU through the loaded level:
/// menu (+ the idle ATTRACT loop), the mode-select/password map, the CARTE, then the
/// loader (native_level_start — the FRESH level-start block runs: lives/BONUS/utensils
/// reset, exactly the VM's fresh-start ax!=0 path). Shared by the cold boot AND the
/// game-over restart (main 011C re-enters the front-end at this screen).
pub fn native_menu_flow(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    game_root: &Path,
    frames: &mut Vec<FrontEndScene>,
) -> Result<(), FrontEndError> {
    // [asm 015a/0107 ax=3] the title/MENU song — the shared restart path.
    native_load_song(state, "PRESENTA.TRK", game_root);

    // [asm 0141..0155] the FRESH-start block runs BEFORE the menu.
    state.wb(LIVES, 2);            // [asm 0141] lives=2
    state.wb(BONUS_LETTERS, 0);    // [asm 0146] BONUS-letters mask
    state.wb(ATTACK_V19, 0x14);    // [asm 014B] damage/energy
    state.wb(ATTACK_PHASE, 0);     // [asm 0150]
    state.wb(UTENSILS_MASK, 0);    // [asm 0155] utensils mask

    // 8e45: the "press 1/2" difficulty screen (MENU.SQZ = resource 8, a 13h image)
    // faded in over 0xA0 DAC entries, then held while the dispatch waits for a choice.
    let menu_img = render_image_scene("MENU.SQZ", game_root, true);
    let menu_pal6 = image_palette("MENU.SQZ", game_root);
    let menu_fade = fade_in_frames(&menu_pal6, 0xA0); // [asm 919f] black -> the menu palette
    let held8 = expand_palette6(menu_fade.last().unwrap_or(&menu_pal6));

    // The MENU <-> ATTRACT <-> CODE loop [asm 8e45]
    loop {
        // Ensure menu music (PRESENTA.TRK) is active on first entry and when returning from attract/password
        native_load_song(state, "PRESENTA.TRK", game_root);

        // Guard against Enter/Space key carry-over from the preceding screen (TITUS / PRESENT / ATTRACT / PASSWORD):
        // if Enter or Space is still held down upon entering the 1/2 menu, wait until it is
        // released before treating a fire press as a menu selection.
        let mut fire_armed = (state.rb(FIRE_LATCH) | state.rb(FIRE_SPACE)) == 0;

        // fade the menu in (first entry + after each return); allow early key press to skip the fade
        let mut early_choice = LevelSelectScreen::Wait;
        for p6 in &menu_fade {
            emit_frame(frames,FrontEndScene {
                mode: MODE_LINEAR,
                palette: expand_palette6(p6),
                planes: None,
                page: 0,
                linear: Some(menu_img.clone()),
                pel: 0,
                wrap: 0xFFFF,
                active_width: 320,
                screen: "menu".to_string(),
                game_paced: false,
                enh: None,
            });
            poll_fe_input(state);
            let flag_mode = state.rb(KEY_1_LATCH);
            let flag_password = state.rb(KEY_2_LATCH);
            let raw_fire = state.rb(FIRE_LATCH) | state.rb(FIRE_SPACE);
            if raw_fire == 0 {
                fire_armed = true;
            }
            let input_active = if fire_armed { raw_fire } else { 0 };
            early_choice = level_select_dispatch(flag_mode, flag_password, input_active, 0);
            if early_choice != LevelSelectScreen::Wait {
                break;
            }
        }

        let mut wait: u16 = 0;
        let mut choice = early_choice;

        // [asm 8e7b] the dispatch wait loop
        while choice == LevelSelectScreen::Wait {
            poll_fe_input(state);
            let flag_mode = state.rb(KEY_1_LATCH);
            let flag_password = state.rb(KEY_2_LATCH);
            let raw_fire = state.rb(FIRE_LATCH) | state.rb(FIRE_SPACE);
            if raw_fire == 0 {
                fire_armed = true;
            }
            let input_active = if fire_armed { raw_fire } else { 0 };
            choice = level_select_dispatch(flag_mode, flag_password, input_active, wait);
            if choice != LevelSelectScreen::Wait {
                break;
            }
            // screen="menu": the touch host maps a TAP here to '1' (beginner mode-select).
            emit_frame(frames,FrontEndScene {
                mode: MODE_LINEAR,
                palette: held8.clone(),
                planes: None,
                page: 0,
                linear: Some(menu_img.clone()),
                pel: 0,
                wrap: 0xFFFF,
                active_width: 320,
                screen: "menu".to_string(),
                game_paced: false,
                enh: None,
            });
            wait += 1;
        }

        if choice == LevelSelectScreen::AutoAdvance {
            // [asm 8E98] idle timeout -> attract, then re-loop
            native_attract(state, dos, game_root, frames)?;
            continue;
        }

        // 96d5: the chosen 0Dh scrolling world-map — password entry (for '2') or
        // mode-select (for '1'/fire).
        // the map song (starts on entering 96d5, not the menu)
        native_load_song(state, "CODE.TRK", game_root);

        if choice == LevelSelectScreen::Password {
            state.wb(LEVEL, 0xFF); // [asm 8ee9] no level chosen yet
            native_menu_map(state, dos, game_root, "password", frames)?;
            // [asm 8ef1-8ef8] fire with NO valid code -> back to the press-1/2 menu
            if state.rb(LEVEL) & 0x80 != 0 {
                continue;
            }
            break; // a matched code chose its level -> the carte/loader
        }

        native_menu_map(state, dos, game_root, "mode_select", frames)?;
        break;
    }

    native_carte_and_load(state, dos, game_root, frames)?;
    Ok(())
}

// ── _native_front_end_frames ────────────────────────────────────────────────────────

/// Drive the front-end scene state machine from the entry state, yielding each
/// displayed scene frame, until a level starts — then the caller switches to
/// `native_gameplay_frame` for gameplay.
///
/// The scene sequence (FRONT_END_SEQUENCE) mirrors main 00a0. Each scene: produce its
/// scene state (recovered scene logic), render it, run its per-scene
/// input/transition.
fn native_front_end_frames(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    display_page: u16,
    game_root: &Path,
    intro_skippable: bool,
    frames: &mut Vec<FrontEndScene>,
) -> Result<(), FrontEndError> {
    // --- OLDIES (240a): the recovered credits render leaf + the verified scene-wait
    //     (0bbe) ---
    // [asm 0b92] the OLDIES screen loads its OWN 16-colour palette from the DGROUP
    // table at 0x287e (int10 AX=1012).
    native_load_dac_palette(state, dos, 0x287E, 0x10);

    let mut phase = WAIT_PRESS.to_string();
    let mut safety_counter = 0u32;
    loop {
        // Safety guard: break after 10,000 iterations to prevent an infinite loop
        safety_counter += 1;
        if safety_counter > 10000 {
            log::warn!("Front-end OLDIES loop safety limit reached");
            break;
        }
        // [asm 240a] credits over black
        let planes = build_oldies_scene(state, display_page as usize);
        emit_frame(frames,FrontEndScene::planar(
            dos.vga_palette.to_vec(),
            planes,
            display_page as usize,
        ));
        let (new_phase, done) = native_scene_wait(state, &phase); // [asm 0bbe] fire press -> release
        phase = new_phase;
        if done {
            break;
        }
    }

    // --- 07c9 sound init (after OLDIES): load the SFX sample bank (SAMPLE.SQZ) ---
    crate::audio::native::native_load_sfx_bank(state, game_root);

    // --- TITUS screen (912b): the first 13h title, TIMED (main 00FE-0104) ---
    // Disable frame emission while collecting into title_frames (these are
    // replayed one-by-one through skippable_intro which DOES emit them).
    EMIT_ENABLED.with(|e| e.set(false));
    let mut title_frames: Vec<FrontEndScene> = Vec::new();
    native_title_screen(game_root, "TITUS.SQZ", 0x10, 70, &mut title_frames);
    EMIT_ENABLED.with(|e| e.set(true));
    let _skipped_titus = skippable_intro(&title_frames, state, intro_skippable, frames);

    // --- PRESENT.SQZ title (9090): 02cc loads the title song PRESENTA.TRK (the
    //     FIRST music) — reproduced as an audio-command state write; then the
    //     "PREHISTORIK 2" title — fade-IN 256 over the background, then the 911D
    //     palette MORPH 234 over the background+logo (the title's colour reveal).
    //     The morph target is the static palette at DGROUP 0xACE7. ---
    // A keypress during TITUS only skips the TITUS screen itself; PRESENT.SQZ still plays
    // (and can be skipped independently with a fresh keypress).
    native_load_song(state, "PRESENTA.TRK", game_root); // [asm 02cc] the PRESENT title song
    // Read the morph target palette from DGROUP 0xACE7
    let morph_base = DS_BASE + MENU_MORPH_SRC;
    let morph_end = (morph_base + 0x300).min(state.data.len());
    let morph_target: Vec<u8> = state.data[morph_base..morph_end]
        .iter()
        .map(|&b| b & 0x3F)
        .collect();
    let mut present_frames: Vec<FrontEndScene> = Vec::new();
    EMIT_ENABLED.with(|e| e.set(false));
    native_present_screen(game_root, &morph_target, &mut present_frames);
    EMIT_ENABLED.with(|e| e.set(true));
    skippable_intro(&present_frames, state, intro_skippable, frames);

    // --- FRONT.SQZ (the HUD front-panel) is stacked permanently before the sprite
    //     bank ([0x2875] 0x27be -> 0x2cd7), exactly as main() does between the titles
    //     and 2dfa. ---
    let front_seg = match load_sqz(state, "FRONT.SQZ", game_root) {
        Ok(s) => s,
        Err(_) => return Err(FrontEndError::HybridGap),
    };
    // [asm 0123] mov [0x3b],ax — the FOREGROUND tile-gfx bank
    state.ww(FG_BANK, front_seg);

    // --- 2dfa: decode the shared SPRITES.SQZ sprite bank into the game state ---
    native_build_sprite_bank(state, game_root);

    // [asm 0129-012c] mov ax,[0x2875]; mov [0x39],ax — save the post-front-end
    // load-top as the per-level allocation RESET base
    let load_top = state.rw(LOAD_TOP);
    state.ww(RESET_BASE, load_top);

    // --- 8e45 onward (press-1/2 menu -> mode-select/password map -> carte -> loader):
    //     shared with the GAME-OVER restart ---
    native_menu_flow(state, dos, game_root, frames)?;
    Ok(())
}

// ── native_front_end ───────────────────────────────────────────────────────────────

/// Drive the whole VM-less front-end, advancing the wall-clock idle counter
/// `[0x27F0]` by one 70Hz timer tick per displayed frame. The front-end runs the timer
/// exactly as the VM does (its scenes spin on the retrace), so gameplay starts with
/// `[0x27F0]` at a lived-in value rather than 0 — a value the VM never has (its timer
/// has run since boot). Without it the idle player at level start picks the wrong
/// fidget pose (a crouch instead of the upright stand, since the idle-fidget selector
/// 5DC9 reads `[0x27F0] & 0x1FF`).
///
/// `intro_skippable` (an opt-in; default for touch): let a fire-key press during the
/// TITUS / PREHISTORIK-2 title screens skip straight to the menu. OFF = byte-accurate
/// (the titles play with no input polling).
pub fn native_front_end(
    state: &mut NativeGameState,
    dos: &mut NativeVGA,
    display_page: u16,
    game_root: &Path,
    intro_skippable: bool,
) -> Result<Vec<FrontEndScene>, FrontEndError> {
    let mut frames: Vec<FrontEndScene> = Vec::new();
    // Drive the scene state machine, collecting all frames.
    native_front_end_frames(
        state, dos, display_page, game_root, intro_skippable, &mut frames,
    )?;
    // Advance the wall-clock idle counter by one 70Hz timer tick per displayed frame.
    for _ in &frames {
        native_idle_timer_tick(state, 1);
    }
    Ok(frames)
}
