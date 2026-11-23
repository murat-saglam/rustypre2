// SPDX-License-Identifier: GPL-3.0-only
//! Prehistorik 2 world-MAP / level-select screen (1030:8E45 dispatch + 96D5 scroll controller).
//!
//! After the title sequence (and the SPRITES.SQZ decode at 2dfa), the front-end runs the world-map
//! level-select: a 0Dh planar map that pans (the "bouncing camera") with the level markers drawn on
//! top, entered from a 13h "press 1/2" difficulty screen. It is driven by a state machine at
//! `8E45..8EFD` that, per difficulty, calls the `96D5` scroll controller with a per-frame UI callback
//! (`991F` / `9985`) that stamps the level text and reads input; the chosen level goes to `[0x2D8A]`
//! and the map hands off to the level loader (`447D` / `3ED6`).
//!
//! The render leaves already exist (`menu_scene`'s `MenuScenePage` is really this map's page;
//! `carte` is the carte column-blit) — what is recovered here is the controller's per-frame logic.
//!
//! Pure logic operating directly on state primitives.

// --- [asm 97BB] and bh,0x1f  (the 0x2000-paragraph display-start wrap) ---
const DISPLAY_WRAP: usize = 0x1FFF;
/// [asm 97A8] 40 bytes per row
const ROW_STRIDE: usize = 0x28;
/// [asm 9AE6] the map auto-scrolls left 4 fine-units per frame
const SCROLL_STEP: u16 = 4;
/// [asm 9B1A] (prev_x & 7) * 0x1080 — the page self-copy source offset
const BLIT_UNIT: usize = 0x1080;

// --- the mode-select screen (1030:991F, the 96D5 callback for [0x2D8A]==0) ---
/// "MODE"      (DGROUP string)
const MODE_TEXT: u16 = 0xB180;
/// "BEGINNER"
const BEGINNER_TEXT: u16 = 0xB185;
/// " EXPERT "
const EXPERT_TEXT: u16 = 0xB18E;
/// up / left / right / down [asm 9965-9977]
const ARROW_SCANCODES: [u8; 4] = [0x48, 0x4B, 0x4D, 0x50];

// --- the password-entry screen (1030:9985, the 96D5 callback for [0x2D8A]==0xFF) ---
/// "ENTER CODE"
const ENTER_CODE_TEXT: u16 = 0xB175;
/// the 4-char typed buffer ("[[[[" when empty)
const CODE_BUFFER: u16 = 0xB170;

// --- the level-select dispatcher (1030:8E45, main loop 8E7B-8E96) ---
/// [asm 8E90] cmp [0x27F0], 0x10E
const LS_TIMEOUT: u16 = 0x10E;

/// The world-map camera state.
///
/// `x` / `row` are `map_pan`'s inputs; `prev_x` / `prev_row` are the previous frame's values the
/// page self-copy uses; `blit_off` = `(prev_x & 7) * 0x1080` ([0xB1AC]).
#[derive(Clone, Copy, Default, Debug)]
pub struct MapCamera {
    /// [0xB19D] fine X (a wrapping word)
    pub x: u16,
    /// [0xB19F] row
    pub row: u16,
    /// [0xB1A5] bounce phase counter (byte)
    pub phase: u8,
    /// [0xB199]
    pub prev_x: u16,
    /// [0xB19B]
    pub prev_row: u16,
    /// [0xB1AC]
    pub blit_off: usize,
}

/// [asm 97A8-97F7] The world-map's CRTC pan from its camera position.
///
/// `camera_x` is the 16-bit `[0xB19D]` (the fine X scroll, a wrapping word — e.g. `0xFFFC` pans
/// left of the origin); `camera_row` is the 16-bit `[0xB19F]` (the row). Returns
/// `(display_start, pel)`: the EGA CRTC display-start (start address, masked to the 0x2000-word
/// panning window) and the attribute-controller pel-pan (0..7). `mul` keeps only the low 16 bits
/// before the `& 0x1FFF`, so the formula is exact for any 16-bit inputs.
///
/// [asm 1030:97A8 — world-map CRTC pan: EGA display-start =
/// (0x28*[0xB19F] + [0xB19D]>>3) & 0x1FFF, pel-pan = [0xB19D] & 7.
pub fn map_pan(camera_x: u16, camera_row: u16) -> (usize, u8) {
    let display_start = (ROW_STRIDE * (camera_row as usize) + ((camera_x as usize) >> 3)) & DISPLAY_WRAP;
    // [asm 97F2-97F5] mov al,[0xB19D]; and al,7
    let pel = (camera_x & 7) as u8;
    (display_start, pel)
}

/// [asm 9AE0] Advance the bouncing map camera one frame.
///
/// The camera scrolls left a fixed 4 fine-units (`[0xB19D] -= 4`); when `bounce` (`cs:[5] != 0`) the
/// row is nudged by `(signed)sine_table[phase] >> 4` (arithmetic) with `phase` pre-incremented, and
/// the old row is stashed in `prev_row`. `prev_x` always takes the old X, and
/// `blit_off = (prev_x & 7) * 0x1080`. When `bounce` is off the row and `prev_row` are left unchanged
/// (the ASM skips that block).
///
/// [asm 1030:9AE0 — scroll left 4/frame ([0xB19D]-=4); the row bounces by
/// (signed sine_table[++phase] >> 4) when cs:[5] is set; derive [0xB199]/[0xB19B]/[0xB1AC].
pub fn map_camera_update(cam: MapCamera, bounce: bool, sine_table: &[u8]) -> MapCamera {
    // [asm 9AE6] sub ax,4
    let new_x = cam.x.wrapping_sub(SCROLL_STEP);
    // [asm 9AE9-9AED] xchg -> [0xB199] = old X
    let prev_x = cam.x;
    // [asm 9AF0] inc [0xB1A5]
    let phase = cam.phase.wrapping_add(1);
    let mut new_row = cam.row;
    let mut prev_row = cam.prev_row;

    if bounce {
        // [asm 9AF5] cmp cs:[5],0; je
        // [asm 9B00-9B03] xlatb
        let b = sine_table[phase as usize];
        // [asm 9B04] cbw (signed)
        let signed = if b >= 0x80 { b as i8 as i32 } else { b as i32 };
        // [asm 9B05-9B0D] sar ax,4 ; add [0xB19F]
        new_row = cam.row.wrapping_add((signed >> 4) as u16);
        // [asm 9B11] xchg -> [0xB19B] = old row
        prev_row = cam.row;
    }

    // [asm 9B14-9B1F]
    let blit_off = ((prev_x & 7) as usize) * BLIT_UNIT;

    MapCamera {
        x: new_x,
        row: new_row,
        phase,
        prev_x,
        prev_row,
        blit_off,
    }
}

// --- the mode-select screen text + input --------------------------------------------

/// One `draw_string` run the menu callback issues: the DGROUP address of the NUL-terminated
/// string plus the pen (`[0xB1A6]`) and per-char advance (`[0xB1AB]`) it sets. The font/page come
/// from the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuTextRun {
    pub addr: u16,
    pub pen: u16,
    pub advance: u8,
}

/// [asm 991F-994B] The two text runs the mode-select screen draws: `MODE` then the chosen
/// difficulty — `BEGINNER` when `[0xB197] == 0` else ` EXPERT `.
///
/// [asm 1030:991F — mode-select draw: 'MODE' (pen 0xC38, adv 4 @0xB180) then 'BEGINNER' (@0xB185)
/// or ' EXPERT ' (@0xB18E, chosen by [0xB197]) (pen 0x1185, adv 3).
pub fn mode_select_text_runs(selection: u8) -> (MenuTextRun, MenuTextRun) {
    // [asm 9941-9948] cmp [0xB197],0; jne -> 0xB18E
    let difficulty = if selection != 0 { EXPERT_TEXT } else { BEGINNER_TEXT };
    (
        MenuTextRun { addr: MODE_TEXT, pen: 0x0C38, advance: 4 },
        MenuTextRun { addr: difficulty, pen: 0x1185, advance: 3 },
    )
}

/// The mode-select input step's effects on state: `toggle` flips `[0xB197]` (BEGINNER/EXPERT);
/// `confirm` bumps `[0xB1B2]` (the chosen-difficulty exit counter); `consume` clears `[0x2874]`.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ModeSelectInput {
    pub toggle: bool,
    pub confirm: bool,
    pub consume: bool,
}

/// [asm 994E-9984] One mode-select input step.
///
/// `scancode` is the latched key `[0x2874]` (bit7 set = a release event); `fire` is
/// `[0x282d] | [0x2810]` (the confirm flag). A release is a no-op; fire confirms (bumps
/// `[0xB1B2]`) without consuming; otherwise the key is consumed (`[0x2874]=0`) and an arrow
/// toggles the BEGINNER/EXPERT selection (`[0xB197] ^= 1`).
///
/// [asm 1030:994E — arrow (0x48/4B/4D/50) toggles [0xB197]; fire ([0x282d]|[0x2810]) bumps
/// [0xB1B2]; a consumed key clears [0x2874]. A released key (bit7) is a no-op.
pub fn mode_select_input(scancode: u8, fire: bool) -> ModeSelectInput {
    // [asm 9952-9955] released -> no-op
    if scancode & 0x80 != 0 {
        return ModeSelectInput::default();
    }
    // [asm 9957-995E] [0x282d]|[0x2810] -> confirm
    if fire {
        return ModeSelectInput { toggle: false, confirm: true, consume: false };
    }
    // [asm 9965-9977] an arrow
    let toggle = ARROW_SCANCODES.contains(&scancode);
    // [asm 9960] consume the key
    ModeSelectInput { toggle, confirm: false, consume: true }
}

// --- the 96D5 controller's per-frame plumbing (the 97A8 loop, minus the callback) ---

/// [asm 97CA-97CE] `xchg [0xB1A1], bx` then `[0xB1A3] = bx`: the new draw page is the display-start
/// (from `map_pan`), and the clear page becomes the previous draw page. Returns
/// `(page_draw, page_clear)`.
///
/// [asm 1030:97CA — the draw page [0xB1A1] becomes the new CRTC display-start, the clear page
/// [0xB1A3] becomes the previous draw page (the scrolling single-buffer page flip).
pub fn map_page_flip(display_start: usize, prev_page_draw: usize) -> (u16, u16) {
    (display_start as u16, prev_page_draw as u16)
}

// --- the password-entry screen (1030:9985) ------------------------------------------

/// [asm 9988-99A7] The two text runs the password screen draws: the `ENTER CODE` label then the
/// typed code buffer (whose bytes evolve as the player types — the caller reads them from state).
pub fn password_text_runs() -> (MenuTextRun, MenuTextRun) {
    (
        MenuTextRun { addr: ENTER_CODE_TEXT, pen: 0x0AF2, advance: 3 },
        MenuTextRun { addr: CODE_BUFFER, pen: 0x12C9, advance: 4 },
    )
}

// --- the level-select dispatcher (1030:8E45, main loop 8E7B-8E96) --------------------

/// The screen the level-select dispatcher decides to run.
///
/// * `ModeSelect`   — [asm 8EC9] -> 96D5 callback 991F
/// * `Password`     — [asm 8EDE] -> 96D5 callback 9985
/// * `Wait`         — [asm 8E96] keep counting [0x27F0]
/// * `AutoAdvance`  — [asm 8E98] timeout -> the default level from [0x83E]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelSelectScreen {
    ModeSelect,
    Password,
    Wait,
    AutoAdvance,
}

/// [asm 8E7B-8E96] One iteration of the level-select main loop's decision.
///
/// `flag_mode` = `[0x27F6]`, `flag_password` = `[0x27F7]` (screen-request flags);
/// `input_active` = `[0x282d] | [0x2810]` (a keypress); `wait_count` = `[0x27F0]` (the idle frame
/// counter). Returns the screen to run, or `Wait` to keep idling, or `AutoAdvance` once the
/// timeout elapses.
///
/// [asm 1030:8E7B — enter mode-select if [0x27F6] or input ([0x282d]|[0x2810]); password if
/// [0x27F7]; else count [0x27F0] up to 0x10E and auto-advance to the default level on timeout.
pub fn level_select_dispatch(
    flag_mode: u8,
    flag_password: u8,
    input_active: u8,
    wait_count: u16,
) -> LevelSelectScreen {
    // [asm 8E7B] cmp [0x27F6],0; jne 8EC9
    if flag_mode != 0 {
        return LevelSelectScreen::ModeSelect;
    }
    // [asm 8E81] cmp [0x27F7],0; jne 8EDE
    if flag_password != 0 {
        return LevelSelectScreen::Password;
    }
    // [asm 8E87] [0x282d]|[0x2810]; jne 8EC9
    if input_active != 0 {
        return LevelSelectScreen::ModeSelect;
    }
    // [asm 8E90] cmp [0x27F0],0x10E; jb 8E7B
    if wait_count < LS_TIMEOUT {
        return LevelSelectScreen::Wait;
    }
    // [asm 8E98] timeout -> default level
    LevelSelectScreen::AutoAdvance
}
