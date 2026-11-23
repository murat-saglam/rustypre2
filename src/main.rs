#![windows_subsystem = "windows"]
// SPDX-License-Identifier: GPL-3.0-only
//! rustypre2 — Native Rust engine and SDL2 runtime for Prehistorik 2.

mod codecs;
mod engine;
mod recovered;
mod render;
mod views;
mod audio;

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::{Keycode, Mod, Scancode};
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{BlendMode, Canvas, Texture};
use sdl2::video::{FullscreenType, Window, WindowPos};

use audio::sdl_output::{NullAudio, SdlAudioOutput};
use engine::assets::load_sqz;
use engine::audio::NativeAudio;
use engine::boot_data;
use engine::front_end::{FrontEndError, FrontEndScene};
use engine::r#loop::FrameTransition;
use engine::sprite_bank::native_build_sprite_bank;
use engine::vga::dac8;
use engine::{NativeGameState, NativeVGA};

/// Exact DOS gameplay tick period: 70 Hz VGA vertical retrace / 3 retraces per tick = 42.857143 ms (~23.333 Hz).
const GAME_TICK_DURATION: Duration = Duration::from_nanos(42_857_143);
/// Exact DOS 70 Hz single-retrace period for front-end fade/scroll animations (~14.2857 ms).
const FE_TICK_DURATION: Duration = Duration::from_nanos(14_285_714);
/// Hide the mouse cursor after 1.5 seconds of inactivity in both windowed and fullscreen modes.
const CURSOR_HIDE_TIMEOUT: Duration = Duration::from_millis(1500);

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Map an SDL2 `Scancode` to the corresponding DOS XT Set-1 make scancode.
fn sdl_to_game_scancode(sc: Scancode) -> u8 {
    match sc {
        Scancode::Escape => 0x01,
        Scancode::Num1 | Scancode::Kp1 => 0x02,
        Scancode::Num2 | Scancode::Kp2 => 0x03,
        Scancode::Num3 | Scancode::Kp3 => 0x04,
        Scancode::Num4 | Scancode::Kp4 => 0x05,
        Scancode::Num5 | Scancode::Kp5 => 0x06,
        Scancode::Num6 | Scancode::Kp6 => 0x07,
        Scancode::Num7 | Scancode::Kp7 => 0x08,
        Scancode::Num8 | Scancode::Kp8 => 0x09,
        Scancode::Num9 | Scancode::Kp9 => 0x0A,
        Scancode::Num0 | Scancode::Kp0 => 0x0B,
        Scancode::Q => 0x10,
        Scancode::W => 0x11,
        Scancode::E => 0x12,
        Scancode::R => 0x13,
        Scancode::T => 0x14,
        Scancode::Y => 0x15,
        Scancode::U => 0x16,
        Scancode::I => 0x17,
        Scancode::O => 0x18,
        Scancode::Return | Scancode::KpEnter => 0x1C,
        Scancode::LCtrl | Scancode::RCtrl => 0x1D,
        Scancode::A => 0x1E,
        Scancode::S => 0x1F,
        Scancode::D => 0x20,
        Scancode::F => 0x21,
        Scancode::G => 0x22,
        Scancode::H => 0x23,
        Scancode::J => 0x24,
        Scancode::K => 0x25,
        Scancode::L => 0x26,
        Scancode::Z => 0x2C,
        Scancode::X => 0x2D,
        Scancode::C => 0x2E,
        Scancode::V => 0x2F,
        Scancode::B => 0x30,
        Scancode::N => 0x31,
        Scancode::M => 0x32,
        Scancode::LAlt | Scancode::RAlt => 0x38,
        Scancode::Space => 0x39,
        Scancode::F1 => 0x3B,
        Scancode::F2 => 0x3C,
        Scancode::Up => 0x48,
        Scancode::Left => 0x4B,
        Scancode::Right => 0x4D,
        Scancode::Down => 0x50,
        _ => 0,
    }
}

const OVERLAY_PNG: &[u8] = include_bytes!("../assets/overlay.png");
const OVERLAY_W: u32 = 1920;
const OVERLAY_H: u32 = 1080;
const OVERLAY_GAME_X: u32 = 280;
const OVERLAY_GAME_Y: u32 = 144;
const OVERLAY_GAME_W: u32 = 1360;
const OVERLAY_GAME_H: u32 = 723;

/// Decode the embedded `assets/overlay.png` (1920x1080) bezel illustration and punch a clean
/// alpha cutout in the central game viewport while preserving all stone/foliage/flower edges.
fn decode_overlay_abgr8888() -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(OVERLAY_PNG, image::ImageFormat::Png)
        .ok()?
        .to_rgb8();
    let (w, h) = img.dimensions();
    if w != OVERLAY_W || h != OVERLAY_H {
        return None;
    }
    let raw = img.as_raw();
    let mut out = vec![0u8; (w * h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 3) as usize;
            let r = raw[idx];
            let g = raw[idx + 1];
            let b = raw[idx + 2];

            let alpha = if (470..1450).contains(&x) && (200..810).contains(&y) {
                0u8
            } else if (280..1640).contains(&x) && (144..867).contains(&y) {
                let lum = (r as f32 + g as f32 + b as f32) / 3.0;
                ((lum - 8.0) * (255.0 / 24.0)).clamp(0.0, 255.0) as u8
            } else {
                255u8
            };

            let dst = ((y * w + x) * 4) as usize;
            if alpha == 0 {
                out[dst] = 0;
                out[dst + 1] = 0;
                out[dst + 2] = 0;
                out[dst + 3] = 0;
            } else {
                out[dst] = r;
                out[dst + 1] = g;
                out[dst + 2] = b;
                out[dst + 3] = alpha;
            }
        }
    }
    Some(out)
}

/// Crisp 8x10 bitmap glyph rows for the pause prompt text (`Press ` and ` to continue`).
fn pause_glyph(ch: char) -> (&'static [&'static str; 10], i32) {
    match ch {
        'P' => (
            &[
                "#######.",
                "##....##",
                "##....##",
                "##....##",
                "#######.",
                "##......",
                "##......",
                "##......",
                "##......",
                "........",
            ],
            8,
        ),
        'r' => (
            &[
                "........",
                "........",
                "##.###..",
                "###..##.",
                "##......",
                "##......",
                "##......",
                "##......",
                "##......",
                "........",
            ],
            7,
        ),
        'e' => (
            &[
                "........",
                "........",
                ".#####..",
                "##...##.",
                "#######.",
                "##......",
                "##...##.",
                ".#####..",
                "........",
                "........",
            ],
            8,
        ),
        's' => (
            &[
                "........",
                "........",
                ".#####..",
                "##...##.",
                ".####...",
                "....###.",
                "##...##.",
                ".#####..",
                "........",
                "........",
            ],
            8,
        ),
        't' => (
            &[
                "..##....",
                "..##....",
                "######..",
                "..##....",
                "..##....",
                "..##....",
                "..##.##.",
                "...###..",
                "........",
                "........",
            ],
            7,
        ),
        'o' => (
            &[
                "........",
                "........",
                ".#####..",
                "##...##.",
                "##...##.",
                "##...##.",
                "##...##.",
                ".#####..",
                "........",
                "........",
            ],
            8,
        ),
        'c' => (
            &[
                "........",
                "........",
                ".#####..",
                "##...##.",
                "##......",
                "##......",
                "##...##.",
                ".#####..",
                "........",
                "........",
            ],
            8,
        ),
        'n' => (
            &[
                "........",
                "........",
                "######..",
                "##...##.",
                "##...##.",
                "##...##.",
                "##...##.",
                "##...##.",
                "........",
                "........",
            ],
            8,
        ),
        'i' => (
            &[
                "..##....",
                "........",
                ".###....",
                "..##....",
                "..##....",
                "..##....",
                "..##....",
                ".####...",
                "........",
                "........",
            ],
            6,
        ),
        'u' => (
            &[
                "........",
                "........",
                "##...##.",
                "##...##.",
                "##...##.",
                "##...##.",
                "##...##.",
                ".######.",
                "........",
                "........",
            ],
            8,
        ),
        _ => (
            &[
                "........",
                "........",
                "........",
                "........",
                "........",
                "........",
                "........",
                "........",
                "........",
                "........",
            ],
            5,
        ),
    }
}

/// 19x18 pixel-art 3D physical keyboard `[P]` keycap sprite (with bright white `H` counter inside P).
const KEYCAP_P_ROWS: [&str; 18] = [
    "..KKKKKKKKKKKKKKK..",
    ".KHHHHHHHHHHHHHHHK.",
    "KHHFFFFFFFFFFFFFBHK",
    "KHFFFFFFFFFFFFFFFBK",
    "KHFFFFPPPPPPPFFFFBK",
    "KHFFFFPPHHHPPFFFFBK",
    "KHFFFFPPHHHPPFFFFBK",
    "KHFFFFPPHHHPPFFFFBK",
    "KHFFFFPPPPPPPFFFFBK",
    "KHFFFFPPFFFFFFFFFBK",
    "KHFFFFPPFFFFFFFFFBK",
    "KHFFFFPPFFFFFFFFFBK",
    "KHFFFFFFFFFFFFFFFBK",
    "KBBBBBBBBBBBBBBBBBK",
    "KSSSSSSSSSSSSSSSSSK",
    "KDDDDDDDDDDDDDDDDDK",
    ".KKKKKKKKKKKKKKKKK.",
    "..KKKKKKKKKKKKKKK..",
];

fn keycap_pixel_color(code: u8) -> Option<[u8; 3]> {
    match code {
        b'K' => Some([0, 0, 0]),
        b'D' => Some([78, 86, 100]),
        b'S' => Some([124, 134, 150]),
        b'B' => Some([205, 212, 224]),
        b'H' => Some([255, 255, 255]),
        b'F' => Some([246, 249, 254]),
        b'P' => Some([18, 22, 30]),
        _ => None,
    }
}

/// Compose the pause scene over the current 320x200 game framebuffer into `out`:
/// darkens the entire game area, renders a subtle centered focus band, and draws
/// crisp bright-white `'Press [P] to continue'` with a 3D keyboard `[P]` keycap icon.
fn render_pause_scene_into(base_rgb: &[u8], out: &mut Vec<u8>) {
    const W: usize = render::sdl_view::WIDTH;
    const H: usize = render::sdl_view::HEIGHT;
    if out.len() != W * H * 3 {
        out.resize(W * H * 3, 0);
    } else {
        out.fill(0);
    }
    let n = out.len().min(base_rgb.len());

    // 1. Darken the 320x200 game area to ~26% brightness
    for i in 0..n {
        out[i] = ((base_rgb[i] as u16 * 66) >> 8) as u8;
    }

    // 2. Centered horizontal focus band (y = 74..115) with soft left/right edges
    for y in 74..116usize {
        for x in 0..W {
            let edge = if x < 36 {
                x as f32 / 36.0
            } else if x > 283 {
                (319 - x) as f32 / 36.0
            } else {
                1.0
            };
            let off = (y * W + x) * 3;
            if y == 74 || y == 115 {
                let add = (75.0 * edge) as u16;
                out[off] = (out[off] as u16 + add).min(255) as u8;
                out[off + 1] = (out[off + 1] as u16 + add).min(255) as u8;
                out[off + 2] = (out[off + 2] as u16 + add).min(255) as u8;
            } else {
                let keep = (256.0 - 170.0 * edge) as u16;
                out[off] = ((out[off] as u16 * keep) >> 8) as u8;
                out[off + 1] = ((out[off + 1] as u16 * keep) >> 8) as u8;
                out[off + 2] = ((out[off + 2] as u16 * keep) >> 8) as u8;
            }
        }
    }

    // 3. Layout: "Press " + [19x18 P keycap] + " to continue"
    let left_text = "Press ";
    let right_text = " to continue";
    let left_w: i32 = left_text.chars().map(|c| pause_glyph(c).1).sum();
    let key_w: i32 = KEYCAP_P_ROWS[0].len() as i32;
    let right_w: i32 = right_text.chars().map(|c| pause_glyph(c).1).sum::<i32>() - 1;
    let total_w = left_w + key_w + right_w;

    let start_x = (W as i32 - total_w) / 2;
    let text_y = 90i32;
    let key_x = start_x + left_w;
    let key_y = 85i32;

    let mut fg_mask = [false; W * H];
    let mut cx = start_x;
    for ch in left_text.chars() {
        let (rows, adv) = pause_glyph(ch);
        for (ry, row_str) in rows.iter().enumerate() {
            for (rx, cell) in row_str.bytes().enumerate() {
                if cell == b'#' {
                    let px = cx + rx as i32;
                    let py = text_y + ry as i32;
                    if (0..W as i32).contains(&px) && (0..H as i32).contains(&py) {
                        fg_mask[py as usize * W + px as usize] = true;
                    }
                }
            }
        }
        cx += adv;
    }

    cx = key_x + key_w;
    for ch in right_text.chars() {
        let (rows, adv) = pause_glyph(ch);
        for (ry, row_str) in rows.iter().enumerate() {
            for (rx, cell) in row_str.bytes().enumerate() {
                if cell == b'#' {
                    let px = cx + rx as i32;
                    let py = text_y + ry as i32;
                    if (0..W as i32).contains(&px) && (0..H as i32).contains(&py) {
                        fg_mask[py as usize * W + px as usize] = true;
                    }
                }
            }
        }
        cx += adv;
    }

    // 4. Draw 1px black outline and drop shadow around the text
    for py in 0..H as i32 {
        for px in 0..W as i32 {
            if fg_mask[py as usize * W + px as usize] {
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let nx = px + dx;
                        let ny = py + dy;
                        if (0..W as i32).contains(&nx) && (0..H as i32).contains(&ny) {
                            let off = (ny as usize * W + nx as usize) * 3;
                            out[off] = 0;
                            out[off + 1] = 0;
                            out[off + 2] = 0;
                        }
                    }
                }
                let sx = px + 1;
                let sy = py + 2;
                if (0..W as i32).contains(&sx) && (0..H as i32).contains(&sy) {
                    let off = (sy as usize * W + sx as usize) * 3;
                    out[off] = 0;
                    out[off + 1] = 0;
                    out[off + 2] = 0;
                }
            }
        }
    }

    // 5. Fill glyph pixels with pure bright white (#FFFFFF)
    for py in 0..H {
        for px in 0..W {
            if fg_mask[py * W + px] {
                let off = (py * W + px) * 3;
                out[off] = 255;
                out[off + 1] = 255;
                out[off + 2] = 255;
            }
        }
    }

    // 6. Draw the 3D physical [P] keyboard keycap icon in the center
    for (ry, row_str) in KEYCAP_P_ROWS.iter().enumerate() {
        for (rx, code) in row_str.bytes().enumerate() {
            if let Some(rgb) = keycap_pixel_color(code) {
                let px = key_x + rx as i32;
                let py = key_y + ry as i32;
                if (0..W as i32).contains(&px) && (0..H as i32).contains(&py) {
                    let off = (py as usize * W + px as usize) * 3;
                    out[off] = rgb[0];
                    out[off + 1] = rgb[1];
                    out[off + 2] = rgb[2];
                }
            }
        }
    }
}

/// Shared main-thread host context for SDL2 rendering, event pumping, and audio.
struct HostContext {
    canvas: Canvas<Window>,
    texture: Texture<'static>,
    overlay_texture: Option<Texture<'static>>,
    event_pump: sdl2::EventPump,
    mouse: sdl2::mouse::MouseUtil,
    last_mouse_activity: Instant,
    cursor_visible: bool,
    sdl_audio: Option<SdlAudioOutput>,
    native_audio: NativeAudio,
    fallback_pal: [[u8; 3]; 256],
    running: bool,
    paused: bool,
    fullscreen: bool,
    windowed_size: (u32, u32),
    suppress_enter: bool,
    last_rgb: Vec<u8>,
    pause_rgb: Vec<u8>,
    fe_next_deadline: Option<Instant>,
    pending_scan: u8,
    latched_keys: HashSet<u8>,
}

impl HostContext {
    /// Set the pause state, freeze/resume audio mixing, clear pending input latches,
    /// and immediately refresh the displayed frame.
    fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        if let Some(ref mut audio) = self.sdl_audio {
            audio.set_paused(paused);
        }
        self.latched_keys.clear();
        self.pending_scan = 0;
        self.present_current_frame();
    }

    /// Re-render the most recent game frame (with the pause scene applied if `self.paused` is true).
    fn present_current_frame(&mut self) {
        if self.last_rgb.is_empty() {
            self.last_rgb = vec![0u8; render::sdl_view::WIDTH * render::sdl_view::HEIGHT * 3];
        }
        if self.paused {
            render_pause_scene_into(&self.last_rgb, &mut self.pause_rgb);
            let _ = self.texture.update(None, &self.pause_rgb, render::sdl_view::WIDTH * 3);
        } else {
            let _ = self.texture.update(None, &self.last_rgb, render::sdl_view::WIDTH * 3);
        }
        self.draw_current_texture_to_canvas();
    }

    /// Block simulation progress while the game is paused until the user presses `P` (or quits).
    fn wait_while_paused(&mut self) {
        while self.paused && self.running {
            self.present_current_frame();
            std::thread::sleep(Duration::from_millis(16));
            self.pump_events();
        }
    }

    /// Toggle between windowed mode (no overlay) and desktop fullscreen mode (with stone bezel overlay).
    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        if self.fullscreen {
            let cur_size = self.canvas.window().size();
            if cur_size.0 > 0 && cur_size.1 > 0 {
                self.windowed_size = cur_size;
            }
            if let Err(e) = self.canvas.window_mut().set_fullscreen(FullscreenType::Desktop) {
                log::warn!("Failed to enter fullscreen: {}", e);
                self.fullscreen = false;
                return;
            }
        } else {
            if let Err(e) = self.canvas.window_mut().set_fullscreen(FullscreenType::Off) {
                log::warn!("Failed to exit fullscreen: {}", e);
            }
            let (w, h) = self.windowed_size;
            let _ = self.canvas.window_mut().set_size(w, h);
            self.canvas
                .window_mut()
                .set_position(WindowPos::Centered, WindowPos::Centered);
        }

        self.present_current_frame();
    }

    /// Pump SDL2 events and record any key-down edges so fast taps between frames are never missed.
    fn pump_events(&mut self) {
        let mut toggle_fs = false;
        let mut toggle_pause = false;
        let mut force_pause = false;
        let mut redraw_window = false;

        for event in self.event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => {
                    self.running = false;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    self.running = false;
                }
                Event::Window {
                    win_event: WindowEvent::Minimized,
                    ..
                } => {
                    force_pause = true;
                }
                Event::Window {
                    win_event:
                        WindowEvent::Restored
                        | WindowEvent::Shown
                        | WindowEvent::Exposed
                        | WindowEvent::SizeChanged(..),
                    ..
                } => {
                    // Do NOT auto-unpause on restore; simply redraw the pause screen.
                    redraw_window = true;
                }
                Event::MouseMotion { .. }
                | Event::MouseButtonDown { .. }
                | Event::MouseButtonUp { .. }
                | Event::MouseWheel { .. }
                | Event::Window {
                    win_event: WindowEvent::Enter | WindowEvent::Leave,
                    ..
                } => {
                    self.last_mouse_activity = Instant::now();
                    if !self.cursor_visible {
                        self.mouse.show_cursor(true);
                        self.cursor_visible = true;
                    }
                }
                Event::KeyDown {
                    scancode: Some(Scancode::Return | Scancode::KpEnter),
                    keymod,
                    repeat: false,
                    ..
                } if keymod.intersects(
                    if cfg!(target_os = "macos") {
                        Mod::LALTMOD | Mod::RALTMOD | Mod::LGUIMOD | Mod::RGUIMOD
                    } else {
                        Mod::LALTMOD | Mod::RALTMOD
                    },
                ) => {
                    self.suppress_enter = true;
                    self.latched_keys.remove(&0x1C);
                    self.latched_keys.remove(&0x38);
                    if self.pending_scan == 0x1C || self.pending_scan == 0x38 {
                        self.pending_scan = 0;
                    }
                    toggle_fs = true;
                }
                Event::KeyDown {
                    scancode: Some(Scancode::P),
                    repeat: false,
                    ..
                } => {
                    toggle_pause = true;
                }
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } => {
                    if !self.paused {
                        let dos_sc = sdl_to_game_scancode(sc);
                        if dos_sc != 0 {
                            self.latched_keys.insert(dos_sc);
                            self.pending_scan = dos_sc;
                        }
                        // Map Numpad directional arrows on KeyDown as well
                        match sc {
                            Scancode::Kp8 => {
                                self.latched_keys.insert(0x48);
                            }
                            Scancode::Kp2 => {
                                self.latched_keys.insert(0x50);
                            }
                            Scancode::Kp4 => {
                                self.latched_keys.insert(0x4B);
                            }
                            Scancode::Kp6 => {
                                self.latched_keys.insert(0x4D);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        // Also check the window's minimized flag directly in case the OS minimized without a separate event
        if (self.canvas.window().window_flags()
            & (sdl2::sys::SDL_WindowFlags::SDL_WINDOW_MINIMIZED as u32))
            != 0
        {
            force_pause = true;
        }

        if force_pause && !self.paused {
            self.set_paused(true);
        } else if toggle_pause {
            self.set_paused(!self.paused);
        } else if redraw_window && !self.last_rgb.is_empty() {
            self.present_current_frame();
        }

        if toggle_fs {
            self.toggle_fullscreen();
        }

        if self.cursor_visible && self.last_mouse_activity.elapsed() >= CURSOR_HIDE_TIMEOUT {
            self.mouse.show_cursor(false);
            self.cursor_visible = false;
        }
    }

    /// Write current keyboard state + latched key-down edges into DGROUP (`DS_BASE + 0x27F4 + sc`
    /// and `DS_BASE + 0x2874`) and poll the native audio engine.
    fn drive_input_and_audio(&mut self, state: &mut NativeGameState) {
        self.pump_events();
        if self.paused {
            self.wait_while_paused();
        }
        if !self.running {
            std::process::exit(0);
        }

        let mut held: HashSet<u8> = HashSet::new();
        {
            let keys = self.event_pump.keyboard_state();
            if keys.is_scancode_pressed(Scancode::Left) || keys.is_scancode_pressed(Scancode::Kp4) {
                held.insert(0x4B);
            }
            if keys.is_scancode_pressed(Scancode::Right) || keys.is_scancode_pressed(Scancode::Kp6) {
                held.insert(0x4D);
            }
            if keys.is_scancode_pressed(Scancode::Up) || keys.is_scancode_pressed(Scancode::Kp8) {
                held.insert(0x48);
            }
            if keys.is_scancode_pressed(Scancode::Down) || keys.is_scancode_pressed(Scancode::Kp2) {
                held.insert(0x50);
            }
            if keys.is_scancode_pressed(Scancode::Space) {
                held.insert(0x39);
            }
            if keys.is_scancode_pressed(Scancode::Return) || keys.is_scancode_pressed(Scancode::KpEnter) {
                held.insert(0x1C);
            }
            if keys.is_scancode_pressed(Scancode::Num1) || keys.is_scancode_pressed(Scancode::Kp1) {
                held.insert(0x02);
            }
            if keys.is_scancode_pressed(Scancode::Num2) {
                held.insert(0x03);
            }
            if keys.is_scancode_pressed(Scancode::LCtrl) || keys.is_scancode_pressed(Scancode::RCtrl) {
                held.insert(0x1D);
            }
            if keys.is_scancode_pressed(Scancode::LAlt) || keys.is_scancode_pressed(Scancode::RAlt) {
                held.insert(0x38);
            }
            if keys.is_scancode_pressed(Scancode::W) {
                held.insert(0x11);
            }
            if keys.is_scancode_pressed(Scancode::E) {
                held.insert(0x12);
            }
            if keys.is_scancode_pressed(Scancode::F1) {
                held.insert(0x3B);
            }
            if keys.is_scancode_pressed(Scancode::F2) {
                held.insert(0x3C);
            }
        }

        // Merge latched key-down events so sub-frame taps are guaranteed to be seen for 1 poll
        for &sc in &self.latched_keys {
            held.insert(sc);
        }
        self.latched_keys.clear();

        if self.suppress_enter {
            let keys = self.event_pump.keyboard_state();
            if !keys.is_scancode_pressed(Scancode::Return)
                && !keys.is_scancode_pressed(Scancode::KpEnter)
            {
                self.suppress_enter = false;
            } else {
                held.remove(&0x1C);
                held.remove(&0x38);
                if self.pending_scan == 0x1C || self.pending_scan == 0x38 {
                    self.pending_scan = 0;
                }
            }
        }

        // Standard scancodes that must be updated every frame in DGROUP [0x27F4 + sc]
        const STD_SCANCODES: &[u8] = &[
            0x4B, 0x4D, 0x48, 0x50, 0x39, 0x1C, 0x02, 0x03,
            0x1D, 0x38, 0x11, 0x12, 0x3B, 0x3C,
        ];
        for &sc in STD_SCANCODES {
            engine::input::set_key(state, sc, held.contains(&sc));
        }
        for &sc in &held {
            engine::input::set_key(state, sc, true);
        }

        // [0x2874] PENDING_KEY latch (used by ENTER CODE password entry and ATTRACT demo exit)
        let mut latch = self.pending_scan;
        self.pending_scan = 0;
        if state.rb(0x2879) == 1 && !held.is_empty() && latch == 0 {
            latch = *held.iter().next().unwrap();
        }
        state.wb(0x2874, latch);

        // Poll audio commands (StartSong, PlaySfx, StopSong)
        self.poll_audio(state);
    }

    fn poll_audio(&mut self, state: &mut NativeGameState) {
        if let Some(ref mut audio_out) = self.sdl_audio {
            self.native_audio.poll(state, audio_out);
            audio_out.pump();
        } else {
            let mut null = NullAudio;
            self.native_audio.poll(state, &mut null);
        }
    }

    fn draw_current_texture_to_canvas(&mut self) {
        self.canvas.set_draw_color(Color::RGB(0, 0, 0));
        self.canvas.clear();

        if self.fullscreen {
            let (out_w, out_h) = self.canvas.output_size().unwrap_or((1920, 1080));
            let screen_aspect = out_w as f64 / (out_h.max(1) as f64);
            let overlay_aspect = OVERLAY_W as f64 / OVERLAY_H as f64;

            // On ~16:9 displays (1.70..1.85), fill the entire screen; on ultrawide or 4:3/16:10,
            // preserve the overlay aspect ratio centered on screen.
            let (ov_x, ov_y, ov_w, ov_h) = if (1.70..=1.85).contains(&screen_aspect) {
                (0i32, 0i32, out_w, out_h)
            } else if screen_aspect > overlay_aspect {
                let w = (out_h as f64 * overlay_aspect).round() as u32;
                let x = ((out_w as i32) - (w as i32)) / 2;
                (x, 0, w, out_h)
            } else {
                let h = (out_w as f64 / overlay_aspect).round() as u32;
                let y = ((out_h as i32) - (h as i32)) / 2;
                (0, y, out_w, h)
            };

            let gx0 = ov_x
                + ((OVERLAY_GAME_X as f64 / OVERLAY_W as f64) * ov_w as f64).round() as i32;
            let gy0 = ov_y
                + ((OVERLAY_GAME_Y as f64 / OVERLAY_H as f64) * ov_h as f64).round() as i32;
            let gx1 = ov_x
                + (((OVERLAY_GAME_X + OVERLAY_GAME_W) as f64 / OVERLAY_W as f64) * ov_w as f64)
                    .round() as i32;
            let gy1 = ov_y
                + (((OVERLAY_GAME_Y + OVERLAY_GAME_H) as f64 / OVERLAY_H as f64) * ov_h as f64)
                    .round() as i32;
            let avail_w = (gx1 - gx0).max(1);
            let avail_h = (gy1 - gy0).max(1);

            // Fixed 960x600 game resolution centered inside the overlay viewport
            // (leaving black margins around the game inside the stone frame).
            const FIXED_W: i32 = 960;
            const FIXED_H: i32 = 600;
            let dst_x = gx0 + (avail_w - FIXED_W) / 2;
            let dst_y = gy0 + (avail_h - FIXED_H) / 2;

            let _ = self.canvas.copy(
                &self.texture,
                None,
                Some(Rect::new(dst_x, dst_y, FIXED_W as u32, FIXED_H as u32)),
            );
            if let Some(ref ov_tex) = self.overlay_texture {
                let _ = self
                    .canvas
                    .copy(ov_tex, None, Some(Rect::new(ov_x, ov_y, ov_w, ov_h)));
            }
        } else {
            let _ = self.canvas.copy(&self.texture, None, None);
        }

        self.canvas.present();
    }

    fn present_rgb(&mut self, rgb: &[u8]) {
        if self.last_rgb.len() != rgb.len() {
            self.last_rgb = rgb.to_vec();
        } else {
            self.last_rgb.copy_from_slice(rgb);
        }

        self.present_current_frame();
    }

    /// Wait precisely until `deadline` while pumping input/window events at ~1ms intervals.
    fn wait_until_deadline(&mut self, deadline: Instant) {
        loop {
            let now = Instant::now();
            if now >= deadline || !self.running {
                break;
            }
            let rem = deadline - now;
            if rem > Duration::from_millis(2) {
                std::thread::sleep(Duration::from_millis(1));
                self.pump_events();
                if self.paused {
                    self.wait_while_paused();
                    break;
                }
            } else {
                std::hint::spin_loop();
            }
        }
    }

    fn present_fe_scene(&mut self, scene: &FrontEndScene) {
        self.pump_events();
        if self.paused {
            self.wait_while_paused();
            self.fe_next_deadline = None;
        }
        if !self.running {
            std::process::exit(0);
        }
        let rgb = render_front_end_scene(scene, &self.fallback_pal);
        self.present_rgb(&rgb);

        // Front-end runs at 70Hz (~14.285ms), except game-paced scenes (Attract Title / Demo) at ~23.333Hz (42.857ms)
        let period = if scene.game_paced {
            GAME_TICK_DURATION
        } else {
            FE_TICK_DURATION
        };
        let now = Instant::now();
        let deadline = match self.fe_next_deadline {
            Some(d) if d > now => d,
            _ => now + period,
        };
        self.wait_until_deadline(deadline);
        let after = Instant::now();
        let next = deadline + period;
        self.fe_next_deadline = Some(if next > after { next } else { after + period });
    }
}

/// Cold-boot helper for `PRE2_SKIP_FE` mode (jumps straight into Level 1 without front-end).
fn cold_boot_direct_level(state: &mut NativeGameState, vga: &mut NativeVGA, game_root: &Path) {
    audio::native::native_load_sfx_bank(state, game_root);
    let front_seg = load_sqz(state, "FRONT.SQZ", game_root).expect("Failed to load FRONT.SQZ");
    state.ww(0x003B, front_seg);
    native_build_sprite_bank(state, game_root);
    let load_top = state.rw(0x2875);
    state.ww(0x0039, load_top);
    engine::level_init::native_level_start(state, game_root);
    engine::render::native_load_level_palette(state, vga);
    let song_name = engine::audio::native_level_song_name(state);
    engine::audio::native_load_song(state, &song_name, game_root);
}

/// Drive the front-end / menu flow on the main thread (handles `ExpertEater` CASTLE wall loop).
fn run_interactive_menu_flow(
    state: &mut NativeGameState,
    vga: &mut NativeVGA,
    game_root: &Path,
    cold_boot: bool,
) {
    let mut first = cold_boot;
    loop {
        let res = if first {
            first = false;
            let display_page_fe = state.rw(0x2DD8) as u16;
            engine::front_end::native_front_end(state, vga, display_page_fe, game_root, true)
                .map(|_| ())
        } else {
            let mut frames = Vec::new();
            engine::front_end::native_menu_flow(state, vga, game_root, &mut frames)
        };

        match res {
            Ok(()) => break,
            Err(FrontEndError::ExpertEater) => {
                // [asm 0163->0178] Beginner selected level 8/9 -> show CASTLE.SQZ wall, then back to menu
                let mut frames = Vec::new();
                engine::front_end::native_expert_eater(state, game_root, &mut frames);
                continue;
            }
            Err(e) => {
                log::warn!("Front-end returned {:?} — falling back to direct level load", e);
                cold_boot_direct_level(state, vga, game_root);
                break;
            }
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off")).init();
    log::info!("rustypre2 — Native Rust engine and SDL2 runtime");

    // ─── SDL2 init ──────────────────────────────────────────────
    sdl2::hint::set("SDL_WINDOWS_DPI_AWARENESS", "permonitorv2");
    let sdl = sdl2::init().expect("SDL2 init failed");
    let mouse = sdl.mouse();
    mouse.show_cursor(true);
    let video = sdl.video().expect("SDL2 video init failed");
    log::info!("SDL2 driver: {}", video.current_video_driver());

    // ─── SDL2 audio init ────────────────────────────────────────
    let game_root = find_game_data();
    log::info!("Game data: {}", game_root.display());

    let sdl_audio = match SdlAudioOutput::new(&sdl, game_root.clone()) {
        Ok(audio) => {
            log::info!("✓ SDL2 Audio initialized (44100 Hz stereo, Pure-Rust MOD + SFX)");
            Some(audio)
        }
        Err(e) => {
            log::warn!("⚠ Audio disabled: {}", e);
            None
        }
    };

    // ─── Cold boot ───────────────────────────────────────────────
    log::info!("Cold boot...");
    let boot = boot_data::build_boot_memory();
    let mut state = NativeGameState::new(boot);
    engine::r#loop::native_init_input(&mut state);
    log::info!("Boot image: {} bytes", state.data.len());

    let mut vga = NativeVGA::new();

    // ─── NativeAudio poller ─────────────────────────────────────
    let mut native_audio = NativeAudio::new(&game_root);
    native_audio.stereo = true;

    // Pre-warm static HUD chrome cache (ALLFONTS.SQZ → panel bitmap + glyph font)
    if views::render_state::load_hud_chrome_ref(&game_root).is_some() {
        log::info!("✓ HUD chrome loaded (ALLFONTS.SQZ)");
    } else {
        log::warn!("⚠ HUD chrome not found — HUD will not render");
    }

    // ─── SDL2 window ─────────────────────────────────────────────
    let scale = 3; // 320x200 → 960x600
    let init_w = render::sdl_view::WIDTH as u32 * scale;
    let init_h = render::sdl_view::HEIGHT as u32 * scale;
    let mut window = video
        .window("rustypre2", init_w, init_h)
        .position_centered()
        .resizable()
        .build()
        .expect("window init failed");
    if let Ok(img) = image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png) {
        let rgba = img.to_rgba8();
        let (iw, ih) = rgba.dimensions();
        let mut raw_icon = rgba.into_raw();
        if let Ok(surf) = sdl2::surface::Surface::from_data(
            &mut raw_icon,
            iw,
            ih,
            iw * 4,
            PixelFormatEnum::ABGR8888,
        ) {
            window.set_icon(&surf);
        }
    }
    let canvas = window.into_canvas().build().expect("canvas init failed");

    // Leak the single TextureCreator so `Texture<'static>` lives for the whole program without unsafe
    let texture_creator: &'static _ = Box::leak(Box::new(canvas.texture_creator()));

    // Nearest-neighbor scaling for crisp 320x200 game pixel art
    sdl2::hint::set("SDL_HINT_RENDER_SCALE_QUALITY", "0");
    let texture = texture_creator
        .create_texture_streaming(
            PixelFormatEnum::RGB24,
            render::sdl_view::WIDTH as u32,
            render::sdl_view::HEIGHT as u32,
        )
        .expect("texture init failed");

    // Linear scaling for the high-resolution 1920x1080 fullscreen bezel overlay
    sdl2::hint::set("SDL_HINT_RENDER_SCALE_QUALITY", "1");
    let overlay_texture = decode_overlay_abgr8888().and_then(|rgba| {
        let mut tex = texture_creator
            .create_texture_static(PixelFormatEnum::ABGR8888, OVERLAY_W, OVERLAY_H)
            .ok()?;
        tex.set_blend_mode(BlendMode::Blend);
        tex.update(None, &rgba, (OVERLAY_W * 4) as usize).ok()?;
        Some(tex)
    });
    sdl2::hint::set("SDL_HINT_RENDER_SCALE_QUALITY", "0");

    let event_pump = sdl.event_pump().expect("event pump init failed");

    let host = Rc::new(RefCell::new(HostContext {
        canvas,
        texture,
        overlay_texture,
        event_pump,
        mouse,
        last_mouse_activity: Instant::now(),
        cursor_visible: true,
        sdl_audio,
        native_audio,
        fallback_pal: [[0u8; 3]; 256],
        running: true,
        paused: false,
        fullscreen: false,
        windowed_size: (init_w, init_h),
        suppress_enter: false,
        last_rgb: vec![0u8; render::sdl_view::WIDTH * render::sdl_view::HEIGHT * 3],
        pause_rgb: vec![0u8; render::sdl_view::WIDTH * render::sdl_view::HEIGHT * 3],
        fe_next_deadline: None,
        pending_scan: 0,
        latched_keys: HashSet::new(),
    }));

    // Install main-thread callbacks for the front-end (active across cold boot, carte, and restarts)
    {
        let host_frame = Rc::clone(&host);
        let host_input = Rc::clone(&host);
        engine::front_end::set_front_end_callbacks(
            move |scene: &FrontEndScene| {
                host_frame.borrow_mut().present_fe_scene(scene);
            },
            move |st: &mut NativeGameState| {
                host_input.borrow_mut().drive_input_and_audio(st);
            },
        );
    }

    // ─── Front-end: interactive intro + menu (main thread) ──────────
    // Note: native_front_end loads SAMPLE.SQZ, FRONT.SQZ, SPRITES.SQZ, sets [0x0039],
    // runs the menu + carte, and calls native_level_start(state) exactly once!
    // Calling FRONT.SQZ / SPRITES.SQZ / native_level_start BEFORE or AFTER native_front_end
    // corrupts segment pointers [0x2875]/[0x0039] and breaks level collision/enemies.
    let skip_fe = std::env::var("PRE2_SKIP_FE").is_ok()
        || std::env::args().any(|a| a == "--skip-frontend");
    if !skip_fe {
        log::info!("Starting front-end (OLDIES -> TITUS -> PRESENT -> MENU -> CARTE)...");
        run_interactive_menu_flow(&mut state, &mut vga, &game_root, true);
    } else {
        log::info!("Skipping front-end — booting Level 1 directly");
        cold_boot_direct_level(&mut state, &mut vga, &game_root);
    }

    // Apply loaded level's DAC palette and sync render state before curtain reveal
    engine::render::native_load_level_palette(&state, &mut vga);
    host.borrow_mut().poll_audio(&mut state);

    // ─── Curtain reveal (3054 center-out, 10 steps) ─────────────────
    curtain_reveal(&mut state, &mut vga, &game_root, &host);

    // ─── Game loop ───────────────────────────────────────────────
    let mut frame_count = 0u64;
    let mut rgb_buf = vec![0u8; render::sdl_view::WIDTH * render::sdl_view::HEIGHT * 3];
    let game_root_str = game_root.to_str().unwrap_or("game_data").to_string();
    let mut next_tick = Instant::now() + GAME_TICK_DURATION;

    while host.borrow().running {
        // ── Input + Audio ────────────────────────────────────────
        host.borrow_mut().drive_input_and_audio(&mut state);
        if !host.borrow().running {
            break;
        }

        // TEST MODE: hold right to verify enemy spawn.
        if std::env::var("RUSTYPRE2_TEST_RIGHT").is_ok() || std::env::var("PRE2_TEST_RIGHT").is_ok() {
            engine::input::set_key(&mut state, 0x4D, true);
        }

        let mut had_transition = false;

        // ── Gameplay tick (exact 23.333 Hz = 70 Hz / 3 retrace cadence) ──────────
        match engine::r#loop::native_gameplay_frame(&mut state) {
            Ok(()) => {}
            Err(transition) => {
                had_transition = true;
                log::info!("Frame transition: {:?}", transition);
                match transition {
                    FrameTransition::Respawn => {
                        let disp_b = state.rw(0x2DD8) as usize;
                        let gr_str = game_root_str.clone();
                        let mut render_cb = |s: &mut NativeGameState| {
                            host.borrow_mut().drive_input_and_audio(s);
                            engine::render::native_sync_render_state(s);
                            engine::render::native_apply_palette_fade(s, &mut vga);
                            let mut opts = engine::render::RenderOptions::default();
                            opts.force_gameplay = true;
                            if let (Some(planes), pg) =
                                engine::render::native_render(s, &mut vga, disp_b, &gr_str, opts)
                            {
                                let _ = present_transition_frame(
                                    &planes,
                                    pg,
                                    &vga.vga_palette,
                                    &host,
                                );
                                views::game_visual_state::recycle_planes(planes);
                            }
                        };
                        match engine::level_state::native_4f6c(&mut state, &mut render_cb) {
                            Ok(()) => {
                                vertical_fade_out(&mut state, &mut vga, &game_root, &host);
                                engine::render::native_load_level_palette(&state, &mut vga);
                                curtain_reveal(&mut state, &mut vga, &game_root, &host);
                                log::info!("✓ Respawned to checkpoint");
                            }
                            Err(_sig) => {
                                log::info!("Respawn -> game over, restarting to menu");
                                restart_level(&mut state, &mut vga, &game_root, &host);
                                curtain_reveal(&mut state, &mut vga, &game_root, &host);
                            }
                        }
                    }
                    FrameTransition::GameOver => {
                        log::info!("Game over — death bounce, GAME OVER scene, restart to menu");
                        // [asm 5063] the death-bounce arc (60 frames rendered live with scream SFX)
                        let disp_b = state.rw(0x2DD8) as usize;
                        let gr_str = game_root_str.clone();
                        let mut render_cb = |s: &mut NativeGameState| {
                            host.borrow_mut().drive_input_and_audio(s);
                            engine::render::native_sync_render_state(s);
                            engine::render::native_apply_palette_fade(s, &mut vga);
                            let mut opts = engine::render::RenderOptions::default();
                            opts.force_gameplay = true;
                            if let (Some(planes), pg) =
                                engine::render::native_render(s, &mut vga, disp_b, &gr_str, opts)
                            {
                                let _ = present_transition_frame(
                                    &planes,
                                    pg,
                                    &vga.vga_palette,
                                    &host,
                                );
                                views::game_visual_state::recycle_planes(planes);
                            }
                        };
                        engine::level_state::native_5063(&mut state, &mut render_cb);

                        // [asm 5063: 02CC ax=0x11] the game-over song (BOULA.TRK)
                        engine::audio::native_load_song(&mut state, "BOULA.TRK", &game_root);
                        host.borrow_mut().poll_audio(&mut state);

                        // [asm 9B7F-9B8A] load the 16-colour DAC palette at DGROUP 0xAFE8
                        engine::render::native_load_dac_palette(
                            &state,
                            &mut vga,
                            engine::dgroup_offsets::GAMEOVER_PALETTE,
                            0x10,
                        );

                        // [asm 9B2C] demo playback ([0x2879]!=0) skips the scene entirely
                        if state.rb(0x2879) == 0 {
                            engine::gameover_scene::native_gameover_setup(&mut state);
                            state.ww(0x6BD5, 0);

                            let mut last_planes: Option<[Vec<u8>; 4]> = None;
                            let gameover_iters = 0x276usize / 3;
                            for _ in 0..gameover_iters {
                                engine::gameover_scene::native_gameover_tick(&mut state);
                                state.ww(0x6BD5, state.rw(0x6BD5).wrapping_add(1));
                                state.ww(0x27F0, state.rw(0x27F0).wrapping_add(3));

                                let (planes, _status) =
                                    views::gameover_scene::build_gameover_scene(
                                        &state, &game_root, 0, None,
                                    );
                                last_planes = Some(planes.clone());

                                render::sdl_view::render_planar_rgb_into(
                                    &planes,
                                    0,
                                    Some(&vga.vga_palette),
                                    0,
                                    320,
                                    0xFFFF,
                                    &mut rgb_buf,
                                );
                                host.borrow_mut().present_rgb(&rgb_buf);
                                host.borrow_mut().drive_input_and_audio(&mut state);

                                recovered::input_decode::decode_input(&mut state);
                                if !host.borrow().running {
                                    break;
                                }
                                if state.rb(0x27E8) != 0 {
                                    break;
                                }

                                std::thread::sleep(Duration::from_millis(43));
                            }

                            // [9C83] 9286: the 16-colour DAC fade-out over the frozen last frame
                            if host.borrow().running {
                                let ds = engine::state::DS_BASE;
                                let pal_base = ds + engine::dgroup_offsets::GAMEOVER_PALETTE;
                                let pal_end = (pal_base + 0x10 * 3).min(state.data.len());
                                let pal6: Vec<u8> = state.data[pal_base..pal_end]
                                    .iter()
                                    .map(|&b| b & 0x3F)
                                    .collect();
                                if let Some(ref last) = last_planes {
                                    for snap in
                                        recovered::front_end_fade::fade_out_frames(&pal6, 0x10)
                                    {
                                        for i in 0..0x10 {
                                            vga.vga_palette[i] = [
                                                dac8(snap[i * 3]),
                                                dac8(snap[i * 3 + 1]),
                                                dac8(snap[i * 3 + 2]),
                                            ];
                                        }
                                        render::sdl_view::render_planar_rgb_into(
                                            last,
                                            0,
                                            Some(&vga.vga_palette),
                                            0,
                                            320,
                                            0xFFFF,
                                            &mut rgb_buf,
                                        );
                                        host.borrow_mut().present_rgb(&rgb_buf);
                                        host.borrow_mut().pump_events();
                                        if !host.borrow().running {
                                            break;
                                        }
                                        std::thread::sleep(Duration::from_millis(14));
                                    }
                                }
                            }
                        }

                        if host.borrow().running {
                            log::info!("✓ GAME OVER scene complete — returning to menu");
                            restart_level(&mut state, &mut vga, &game_root, &host);
                            curtain_reveal(&mut state, &mut vga, &game_root, &host);
                        }
                    }
                    FrameTransition::LevelEnd => {
                        // Sync render state + palette fade on the final gameplay frame before
                        // starting the iris close / curtain transition so scroll ring indices
                        // (col_ring, row_ring, scroll_src) match the final camera position!
                        engine::render::native_sync_render_state(&mut state);
                        engine::render::native_apply_palette_fade(&state, &mut vga);

                        let display_page = state.rw(0x2DD8) as usize;
                        let pal = vga.vga_palette;
                        let mut quit = false;
                        let takes_tally = engine::level_state::level_end_takes_tally(
                            state.rb(0x6BE6),
                            state.rb(0x2D8A),
                        );

                        if takes_tally {
                            log::info!("Level end — iris close + tally cutscene + carte + advance");
                            // 1) Iris close
                            engine::runtime::native_iris_close(
                                &mut state,
                                &mut vga,
                                display_page,
                                &game_root_str,
                                |planes, page| {
                                    if present_transition_frame(planes, page, &pal, &host) {
                                        quit = true;
                                    }
                                },
                            );

                            // 2) Exit anim (tally cutscene with BRAVO.TRK)
                            if !quit {
                                engine::audio::native_load_song(
                                    &mut state,
                                    "BRAVO.TRK",
                                    &game_root,
                                );
                                host.borrow_mut().poll_audio(&mut state);
                                engine::runtime::native_exit_anim(
                                    &mut state,
                                    &mut vga,
                                    display_page,
                                    &game_root_str,
                                    false,
                                    |planes, page| {
                                        if let Some(p) = planes {
                                            if present_transition_frame(p, page, &pal, &host) {
                                                quit = true;
                                            }
                                        }
                                    },
                                );
                            }
                        } else {
                            log::info!("Bonus level warp — vertical curtain close + direct load");
                            vertical_fade_out(&mut state, &mut vga, &game_root, &host);
                        }

                        if quit {
                            host.borrow_mut().running = false;
                        } else {
                            // 3) Reset [0x2875] (LOAD_TOP) to [0x0039] (RESET_BASE = 0x5CC1)
                            //    before advancing/loading the next level or sub-level so
                            //    repeated level/tent/cave transitions never walk [0x2875]
                            //    into the 1 MB wrap or EGA plane memory.
                            let reset_base = state.rw(0x0039);
                            if reset_base != 0 {
                                state.ww(0x2875, reset_base);
                            }
                            match engine::level_state::native_level_end(&mut state, &game_root) {
                                Ok(()) => {
                                    if !takes_tally {
                                        // Bonus warp skips CARTE and starts the bonus level directly
                                        engine::render::native_load_level_palette(
                                            &state, &mut vga,
                                        );
                                        let song =
                                            engine::audio::native_level_song_name(&state);
                                        engine::audio::native_load_song(
                                            &mut state, &song, &game_root,
                                        );
                                        engine::render::native_sync_render_state(&mut state);
                                        host.borrow_mut().poll_audio(&mut state);
                                        curtain_reveal(&mut state, &mut vga, &game_root, &host);
                                    } else {
                                        // Show the world-map CARTE for the newly loaded level
                                        // without loading the level a second time!
                                        let mut fe_frames = Vec::new();
                                        match engine::front_end::native_carte(
                                            &mut state,
                                            &mut vga,
                                            &game_root,
                                            &mut fe_frames,
                                        ) {
                                            Ok(()) => {
                                                let song =
                                                    engine::audio::native_level_song_name(&state);
                                                engine::audio::native_load_song(
                                                    &mut state, &song, &game_root,
                                                );
                                                // Clear residual raw keys after CARTE confirmation
                                                let ds = engine::state::DS_BASE;
                                                for i in 0..0x80 {
                                                    state.data[ds + 0x27F4 + i] = 0;
                                                }
                                                engine::render::native_load_level_palette(
                                                    &state, &mut vga,
                                                );
                                                engine::render::native_sync_render_state(
                                                    &mut state,
                                                );
                                                host.borrow_mut().poll_audio(&mut state);
                                                curtain_reveal(
                                                    &mut state,
                                                    &mut vga,
                                                    &game_root,
                                                    &host,
                                                );
                                                log::info!(
                                                    "✓ World map shown + next level loaded (level {})",
                                                    state.rb(0x2D8A)
                                                );
                                            }
                                            Err(FrontEndError::ExpertEater) => {
                                                let mut wf = Vec::new();
                                                engine::front_end::native_expert_eater(
                                                    &mut state, &game_root, &mut wf,
                                                );
                                                restart_level(
                                                    &mut state, &mut vga, &game_root, &host,
                                                );
                                                curtain_reveal(
                                                    &mut state,
                                                    &mut vga,
                                                    &game_root,
                                                    &host,
                                                );
                                            }
                                            Err(e) => {
                                                log::warn!(
                                                    "Carte error: {:?} — stopping",
                                                    e
                                                );
                                                host.borrow_mut().running = false;
                                            }
                                        }
                                    }
                                }
                                Err(engine::level_state::LevelStateSignal::ExpertEater) => {
                                    let mut wf = Vec::new();
                                    engine::front_end::native_expert_eater(
                                        &mut state, &game_root, &mut wf,
                                    );
                                    restart_level(&mut state, &mut vga, &game_root, &host);
                                    curtain_reveal(&mut state, &mut vga, &game_root, &host);
                                }
                                Err(sig) => {
                                    log::warn!("Level end signal: {} — stopping", sig);
                                    host.borrow_mut().running = false;
                                }
                            }
                        }
                    }
                    FrameTransition::GameComplete => {
                        log::info!("Game complete! Showing THE END screen...");
                        engine::audio::native_load_song(&mut state, "FINAL.TRK", &game_root);
                        host.borrow_mut().poll_audio(&mut state);

                        // FRAME_CB and INPUT_CB stream THEEND.SQZ and CREATORS interactively
                        let mut fe_frames = Vec::new();
                        engine::front_end::native_the_end(
                            &mut state,
                            &game_root,
                            &mut fe_frames,
                        );

                        let ds = engine::state::DS_BASE;
                        state.data[ds + 0x6BE4] = 0;
                        state.data[ds + 0x6BE5] = 0;
                        state.data[ds + 0x6BE6] = 0;
                        log::info!("✓ THE END complete — returning to menu");
                        restart_level(&mut state, &mut vga, &game_root, &host);
                        curtain_reveal(&mut state, &mut vga, &game_root, &host);
                    }
                    FrameTransition::CaveTeleport(si) => {
                        log::info!("Cave teleport (entry {:#x})", si);
                        vertical_fade_out(&mut state, &mut vga, &game_root, &host);
                        if let Err(t) = engine::r#loop::native_cave_teleport(&mut state, si) {
                            log::warn!("Cave teleport raised: {:?}", t);
                        }
                        engine::render::native_sync_render_state(&mut state);
                        curtain_reveal(&mut state, &mut vga, &game_root, &host);
                        log::info!("✓ Cave teleport complete, render state synced");
                    }
                }
            }
        }

        // ── Audio: poll NativeAudio for SFX/song events ──
        host.borrow_mut().poll_audio(&mut state);

        // ── Sync render state (re-derive scroll ring indices + anim advance) ──
        engine::render::native_sync_render_state(&mut state);

        // [asm 6772] Apply the per-frame light-pickup DAC fade to the VGA palette.
        engine::render::native_apply_palette_fade(&state, &mut vga);

        // ── Render (core frame + foreground secret-passage tiles + particles + fireflies + hit-flash) ──
        let disp_cur = state.rw(0x2DD8) as usize;
        let mut opts = engine::render::RenderOptions::default();
        opts.force_gameplay = true;
        let (planes_opt, dest_page) = engine::render::native_render(
            &mut state,
            &mut vga,
            disp_cur,
            &game_root_str,
            opts,
        );
        let planes = planes_opt.unwrap_or_else(views::game_visual_state::take_clean_planes);

        render::sdl_view::render_planar_rgb_into(
            &planes,
            dest_page,
            Some(&vga.vga_palette),
            0,
            320,
            0xFFFF,
            &mut rgb_buf,
        );
        views::game_visual_state::recycle_planes(planes);

        host.borrow_mut().present_rgb(&rgb_buf);

        frame_count += 1;
        if frame_count % 24 == 0 {
            let mut active = 0u32;
            let mut off = 0x5720usize;
            while off > 0x4F0A {
                off -= 0x12;
                if state.rw(off + 4) != 0xFFFF {
                    active += 1;
                }
            }
            log::info!(
                "Tick {}: player=({},{}) cam=({},{}) sprites={}",
                frame_count,
                state.rw(0x4F1C),
                state.rw(0x4F1E),
                state.rb(0x2DE4),
                state.rb(0x2DE6),
                active
            );
        }

        if had_transition {
            next_tick = Instant::now() + GAME_TICK_DURATION;
        } else {
            host.borrow_mut().wait_until_deadline(next_tick);
            let now = Instant::now();
            next_tick += GAME_TICK_DURATION;
            if next_tick < now {
                next_tick = now + GAME_TICK_DURATION;
            }
        }
    }

    log::info!("Game loop ended after {} frames", frame_count);
}

// ── Curtain reveal ─────────────────────────────────────────────────────────────

/// [asm 30C6] Vertical fade-out curtain over the current gameplay frame (rows black
/// from top and bottom converging at row 88 in 9 steps, keeping the HUD band).
fn vertical_fade_out(
    state: &mut NativeGameState,
    vga: &mut NativeVGA,
    game_root: &Path,
    host: &Rc<RefCell<HostContext>>,
) {
    engine::render::native_sync_render_state(state);
    engine::render::native_apply_palette_fade(state, vga);
    let disp_cur = state.rw(0x2DD8) as usize;
    let mut opts = engine::render::RenderOptions::default();
    opts.force_gameplay = true;
    let (planes_opt, dest_page) = engine::render::native_render(
        state,
        vga,
        disp_cur,
        game_root.to_str().unwrap_or("game_data"),
        opts,
    );
    let planes = match planes_opt {
        Some(p) => p,
        None => return,
    };
    let full_rgb = render::sdl_view::render_planar_rgb_from_planes(
        &planes,
        dest_page,
        Some(&vga.vga_palette),
        0,
        320,
    );
    views::game_visual_state::recycle_planes(planes);

    const W: usize = 320;
    const VIEWPORT_H: usize = 176;

    for k in 1..=9usize {
        let top = (10 * k).min(88);
        let bot = (176usize.saturating_sub(10 * k)).max(88);
        let mut frame = full_rgb.clone();
        for y in 0..VIEWPORT_H {
            if y < top || y >= bot {
                let row = y * W * 3;
                for b in &mut frame[row..row + W * 3] {
                    *b = 0;
                }
            }
        }
        host.borrow_mut().present_rgb(&frame);
        host.borrow_mut().pump_events();
        if !host.borrow().running {
            break;
        }
        std::thread::sleep(Duration::from_millis(28));
    }
}

/// The level-start curtain reveal (the VM's 3054 center-out reveal):
/// render the loaded level once (including player, objects, foreground tiles),
/// then reveal it in 10 center-out steps over a black viewport (the HUD band always shows).
fn curtain_reveal(
    state: &mut NativeGameState,
    vga: &mut NativeVGA,
    game_root: &Path,
    host: &Rc<RefCell<HostContext>>,
) {
    engine::render::native_sync_render_state(state);
    engine::render::native_apply_palette_fade(state, vga);

    let disp_cur = state.rw(0x2DD8) as usize;
    let mut opts = engine::render::RenderOptions::default();
    opts.force_gameplay = true;
    let (planes_opt, dest_page) = engine::render::native_render(
        state,
        vga,
        disp_cur,
        game_root.to_str().unwrap_or("game_data"),
        opts,
    );
    let planes = planes_opt.unwrap_or_else(views::game_visual_state::take_clean_planes);
    let full_rgb = render::sdl_view::render_planar_rgb_from_planes(
        &planes,
        dest_page,
        Some(&vga.vga_palette),
        0,
        320,
    );
    views::game_visual_state::recycle_planes(planes);

    const W: usize = 320;
    const H: usize = 200;
    const VIEWPORT_H: usize = 176;

    let steps = 10usize;
    for k in 1..=steps {
        let mut frame = vec![0u8; W * H * 3];
        let half = k * W / 2 / steps;
        let lo = W / 2 - half;
        let hi = (W / 2 + half).min(W);
        for y in 0..H {
            if y < VIEWPORT_H {
                for x in lo..hi {
                    let off = (y * W + x) * 3;
                    frame[off..off + 3].copy_from_slice(&full_rgb[off..off + 3]);
                }
            } else {
                let row = y * W * 3;
                frame[row..row + W * 3].copy_from_slice(&full_rgb[row..row + W * 3]);
            }
        }
        host.borrow_mut().present_rgb(&frame);
        host.borrow_mut().pump_events();
        if !host.borrow().running {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    log::info!("✓ Curtain reveal complete");
}

/// Render one level-end transition frame (iris close / exit-anim tally) to the
/// SDL2 window and pump events so ESC/quit still works during the cutscene.
fn present_transition_frame(
    planes: &[Vec<u8>; 4],
    page: usize,
    pal: &[[u8; 3]; 256],
    host: &Rc<RefCell<HostContext>>,
) -> bool {
    let rgb = render::sdl_view::render_planar_rgb_from_planes(planes, page, Some(pal), 0, 320);
    let mut h = host.borrow_mut();
    h.present_rgb(&rgb);
    h.pump_events();
    let quit = !h.running;
    drop(h);
    std::thread::sleep(Duration::from_millis(33));
    quit
}

fn restart_level(
    state: &mut NativeGameState,
    vga: &mut NativeVGA,
    game_root: &Path,
    host: &Rc<RefCell<HostContext>>,
) {
    run_interactive_menu_flow(state, vga, game_root, false);
    engine::render::native_load_level_palette(state, vga);
    host.borrow_mut().poll_audio(state);
    log::info!("✓ Restarted via menu flow");
}

fn has_game_assets(dir: &Path) -> bool {
    engine::assets::resolve_game_path(dir, "SPRITES.SQZ").is_ok()
}

fn find_game_data() -> PathBuf {
    if let Ok(root) = std::env::var("RUSTYPRE2_GAME_ROOT").or_else(|_| std::env::var("PRE2_GAME_ROOT")) {
        let p = PathBuf::from(root);
        if has_game_assets(&p) {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("game_data").join("Prehist2");
            if has_game_assets(&p) {
                return p;
            }
            let p2 = dir.join("game_data");
            if has_game_assets(&p2) {
                return p2;
            }
            if has_game_assets(dir) {
                return dir.to_path_buf();
            }
        }
    }
    let local = PathBuf::from("game_data").join("Prehist2");
    if has_game_assets(&local) {
        return local;
    }
    let local2 = PathBuf::from("game_data");
    if has_game_assets(&local2) {
        return local2;
    }
    let assets = PathBuf::from("assets");
    if has_game_assets(&assets) {
        return assets;
    }
    let dev = PathBuf::from("../game_data/Prehist2");
    if has_game_assets(&dev) {
        return dev;
    }
    PathBuf::from("game_data")
}

/// Render a FrontEndScene to RGB using its own palette, pel_pan, active_width, and wrap mask.
fn render_front_end_scene(
    scene: &engine::front_end::FrontEndScene,
    fallback_palette: &[[u8; 3]; 256],
) -> Vec<u8> {
    const W: usize = 320;
    const H: usize = 200;

    let pal: &[[u8; 3]; 256] = if scene.palette.len() == 256 {
        scene.palette.as_slice().try_into().unwrap_or(fallback_palette)
    } else {
        fallback_palette
    };

    if scene.mode == 0x13 {
        // MODE_LINEAR: 320x200 256-colour image
        let mut rgb = vec![0u8; W * H * 3];
        if let Some(ref linear) = scene.linear {
            let npix = (W * H).min(linear.len());
            for i in 0..npix {
                let idx = linear[i] as usize;
                let c = pal[idx & 0xFF];
                let off = i * 3;
                rgb[off] = c[0];
                rgb[off + 1] = c[1];
                rgb[off + 2] = c[2];
            }
        }
        rgb
    } else if scene.mode == 0x12 {
        // MODE_CREATORS: 640x480 4-plane grayscale downsampled to 320x200
        let mut rgb = vec![0u8; W * H * 3];
        if let Some(ref planes) = scene.planes {
            for y in 0..H {
                let src_y = (y * 480) / H;
                let row_off = src_y * 80;
                for x in 0..W {
                    let src_x = (x * 640) / W;
                    let byte_off = row_off + (src_x >> 3);
                    let bit_mask = 0x80u8 >> (src_x & 7);
                    let mut color = 0usize;
                    for p in 0..4 {
                        if byte_off < planes[p].len() && (planes[p][byte_off] & bit_mask) != 0 {
                            color |= 1 << p;
                        }
                    }
                    let c = pal[color & 0xFF];
                    let off = (y * W + x) * 3;
                    rgb[off] = c[0];
                    rgb[off + 1] = c[1];
                    rgb[off + 2] = c[2];
                }
            }
        }
        rgb
    } else {
        // MODE_PLANAR: 4 EGA planes with pel_pan, active_width, and circular page wrap (0x1FFF / 0xFFFF)
        if let Some(ref planes) = scene.planes {
            render::sdl_view::render_planar_rgb_from_planes_wrapped(
                planes,
                scene.page,
                Some(pal),
                scene.pel,
                scene.active_width,
                scene.wrap,
            )
        } else {
            vec![0u8; W * H * 3]
        }
    }
}
