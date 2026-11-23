# Changelog

All notable changes to **`rustypre2`** are documented in this file.

## [v0.1.0] - 2026-11-19

### Added
- **Initial Native Rust Release (`v0.1.0`):**
  - Complete, VM-less native Rust reimplementation of *Prehistorik 2* (1993, Titus Interactive) across Windows, macOS, and Linux.
  - Full cold-boot front-end sequence (`TITUS.SQZ` logo, `PRESENT.SQZ`, `CASTLE.SQZ`, interactive Mode-Select menu, `ENTER CODE` hexadecimal password & cheat validator, `EXPERT EATER` / `YEAR >= 1994` *CREATORS* screens, and deterministic Attract-Mode demos).
  - All 13 recovered enemy and object AI handlers, level/boss state machines, cave/tent teleports, iris-close & tally cutscenes, and `GAME OVER` / `THE END` sequences.
  - Pure-Rust 44.1 kHz stereo ProTracker `.MOD` synthesizer (`ModSynth` with 4-point Catmull-Rom cubic spline interpolation and Amiga PAL clock timing) and real-time 8 kHz PCM digital SFX mixer (`SAMPLE.SQZ`).
- **`Alt+Enter` Fullscreen Bezel Overlay Mode:**
  - Press `Alt+Enter` (or `Cmd+Enter` on macOS) to toggle between windowed mode (`960×600`) and borderless desktop fullscreen mode with an embedded `1920×1080` prehistoric stone bezel overlay (`assets/overlay.png`).
  - Centers the game at an integer `960×600` (`3×`) resolution inside the overlay viewport without stretching or fractional scaling artifacts, suppressing `Alt`/`Enter` key bleed into the game.
- **`P` Key & Window Minimize Pause System:**
  - Press `P` at any time to pause or resume gameplay simulation and real-time audio mixing while preserving exact sample positions.
  - Automatically pauses the game when the window is minimized; restoring the window keeps the game paused until the player explicitly presses `P`.
  - Custom darkened pause scene (`render_pause_scene_into`) displaying crisp bright-white text and a 3D pixel-art `[P]` keyboard keycap (`Press [P] to continue`).
- **Cross-Platform Polish & Desktop Integration:**
  - Auto-hiding mouse cursor after 1.5 seconds of inactivity in both windowed and fullscreen modes on Windows, macOS, and Linux.
  - Windows GUI subsystem (`#![windows_subsystem = "windows"]`) so the game opens directly into a window without spawning a background console terminal.
  - Custom caveman icon (`assets/icon.ico` & `assets/icon.png`) embedded both into the Windows PE `.rsrc` section via a pure-Rust COFF builder in `build.rs` and set as the runtime SDL2 window/dock icon across all platforms.
  - Case-insensitive game asset path resolver (`resolve_game_path`) for Linux (`ext4`/`btrfs`) and macOS (`APFS`) compatibility.

### Optimized
- **Deadline-Based Tick Runtime (`GAME_TICK_DURATION = 42.857 ms`):**
  - Replaced fixed `sleep(16ms)` polling with an `Instant` deadline pacer locked to the exact DOS `70 Hz / 3 = 23.333 Hz` cadence, eliminating slowdowns when many enemies, projectiles, and particles are on screen simultaneously.
- **Zero-Allocation Render Pipeline & Memory Management:**
  - Added a thread-local `PLANE_POOL` to recycle the four 64 KB EGA plane buffers every frame instead of allocating and dropping 256 KB per tick.
  - Eliminated 128 KB/frame `grid`/`gfx` cloning in `read_foreground_state` by pre-checking live memory slices for secret foreground tiles (`flag & 0x40`).
  - Cached `ALLFONTS.SQZ` HUD chrome as a static reference (`load_hud_chrome_ref`) and replaced per-row heap allocations in planar-to-RGB conversion with stack buffers.
  - Unrolled `paint_sprite` into dedicated 4-plane mask and single-plane OR blitters.
- **Vertical Camera Follow (`camera_scroll.rs`):**
  - Prevented landing (`yvel == 0`) from prematurely aborting upward camera re-centering and tuned the live vertical follow dead-zone (`rows 6..8`) so the camera never lags below the character after climbing platforms.
