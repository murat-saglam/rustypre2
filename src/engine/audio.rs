// SPDX-License-Identifier: GPL-3.0-only
//! Native audio bridge — drive the enhanced (.TRK music + SFX) player from a NativeGameState, VM-free.
//!
//! The recovered gameplay/front-end writes the audio-COMMAND state into the DGROUP byte-exact,
//! exactly as the VM does: the loaded song's order table ([0xDC7] / length [0xDC2]) and the
//! active SFX descriptor ([0x1004] src / [0x1006] remaining / [0x0B59] seg). The `--audio enhanced`
//! player is COMMAND-DRIVEN — it plays the identified standard .TRK module + the SFX PCM,
//! NOT the VM's software mixer (no SB / DMA / IRQ / 8237 / 8259). So a per-frame poll of the
//! native state emits the SAME StartSong / PlaySfx events the VM's
//! `install_command_observers` emits from its CPU hooks — with no VM in the loop.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::engine::state::{NativeGameState, DS_BASE};
use crate::codecs::sqz::unpack_sqz;

// ---- constants from audio.py ----

/// code segment — the SFX device-flag reads (native_play_sfx) are CS-relative, not DGROUP
const CS_BASE: usize = 0x1030 << 4;
/// DGROUP paragraph base
const DS: usize = DS_BASE; // 0x1A0F0
/// [asm 22B3] order table (pattern sequence)
const ORDER_TABLE: usize = 0xDC7;
/// max MOD order positions
const ORDER_TABLE_LEN: usize = 0x100;
/// [asm 22FE] number of order positions
const SONG_LENGTH: usize = 0xDC2;
/// cs: digital-device-present flags (either == 1 -> the SB/digital path)
const SFX_DEV_FLAGS: [usize; 2] = [0x1D6C, 0x1D6D];
/// native home for the SFX PCM bank (upper memory, above the 0xA000 video aperture)
const SFX_BANK_SEG: usize = 0xC000;
/// DS:0x1009 + dl*4 -> {src word, len word}
const SFX_TABLE: usize = 0x1009;
/// the SFX PCM sample-bank segment [asm 07C9]
const SFX_SEG_PTR: usize = 0x0B59;
/// the active SFX descriptor: PCM source offset [asm 02b7]
const SFX_DESC_SRC: usize = 0x1004;
/// ... PCM length [asm 02be]
const SFX_DESC_LEN: usize = 0x1006;
/// the PC-speaker active-note pointer [asm 0292]
const SFX_PCSPK_NOTE: usize = 0x1035;

/// Write the active SFX descriptor src ([0x1004]).
#[inline]
fn set_sfx_src(state: &mut NativeGameState, v: u16) { state.ww(SFX_DESC_SRC, v); }
/// Write the active SFX descriptor len ([0x1006]).
#[inline]
fn set_sfx_len(state: &mut NativeGameState, v: u16) { state.ww(SFX_DESC_LEN, v); }
/// Write the PC-speaker active-note pointer ([0x1035]).
#[inline]
fn set_sfx_pcspk_note(state: &mut NativeGameState, v: u16) { state.ww(SFX_PCSPK_NOTE, v); }
/// Write the SFX PCM sample-bank segment ([0x0B59]).
#[inline]
fn set_sfx_sample_seg(state: &mut NativeGameState, v: u16) { state.ww(SFX_SEG_PTR, v); }
/// Write the loaded song's order-table length ([0xDC2]).
#[inline]
fn set_song_length(state: &mut NativeGameState, v: u8) { state.wb(SONG_LENGTH, v); }

/// Read one SFX table entry's src (at 0x1009 + dl*4 + 0).
#[inline]
fn sfx_entry_src(state: &NativeGameState, dl: usize) -> u16 { state.rw(SFX_TABLE + dl * 4) }
/// Write one SFX table entry's src (at 0x1009 + dl*4 + 0).
#[inline]
fn set_sfx_entry_src(state: &mut NativeGameState, dl: usize, v: u16) { state.ww(SFX_TABLE + dl * 4, v); }
/// Read one SFX table entry's length (at 0x1009 + dl*4 + 2).
#[inline]
fn sfx_entry_length(state: &NativeGameState, dl: usize) -> u16 { state.rw(SFX_TABLE + dl * 4 + 2) }

// ---- sfx_screen_x / player_sfx_x ----

/// The on-screen X (0..320-ish) of a world-space X for the stereo pan: `world_x - camera`.
/// The camera pixel X is `[0x2DE4]` (tiles) * 16. May land <0 or >320 (off-screen); the poll clamps the pan.
pub fn sfx_screen_x(state: &NativeGameState, world_x: i32) -> i32 {
    let cam_col = state.rw(0x2DE4) as i32;
    let cam_px = cam_col * 16;
    world_x - cam_px
}

/// The player's on-screen X — the pan for the player's own sounds (jump, throw, hurt, death scream, glider).
pub fn player_sfx_x(state: &NativeGameState) -> i32 {
    let player_x = state.rw(0x4F1C) as i32;
    sfx_screen_x(state, player_x)
}

// ---- native_play_sfx ----

/// [asm 0282] Emit sound effect `dl` the way `play_sfx` does — reproduce its device dispatch.
///
/// `x` is the OPTIONAL trigger screen X (0..320) for the enhanced STEREO pan (None = centre);
/// it only rides the native `sfx_queue` side-channel, never the byte-exact `play_sfx` writes below.
///
/// * a digital device is present (`cs:[0x1D6C]` or `cs:[0x1D6D]` == 1) -> write the SB descriptor
///   `[0x1004]` (src) / `[0x1006]` (len) from the table entry `[0x1009 + dl*4]` (0x2A9).
/// * otherwise -> write the PC-speaker active-note pointer `[0x1035] = 0x1037 + dl*0xA` (0x292).
///
/// Every CALL is a distinct trigger (a repeated identical effect re-fires), so each call appends
/// to the sfx_queue. The queue is capped at 64 entries for a consumer-less run.
pub fn native_play_sfx(state: &mut NativeGameState, dl: u8, x: Option<i32>) {
    let dl = dl & 0xFF;
    let dl_usize = dl as usize;

    // [asm 0282/028a] digital present?
    let digital = state.data[(CS_BASE + SFX_DEV_FLAGS[0]) & 0xFFFFF] == 1
               || state.data[(CS_BASE + SFX_DEV_FLAGS[1]) & 0xFFFFF] == 1;

    if digital {
        // [asm 02b7-02bb] the SB {src,len} descriptor table
        let entry_src = sfx_entry_src(state, dl_usize);
        let entry_len = sfx_entry_length(state, dl_usize);
        set_sfx_src(state, entry_src);
        set_sfx_len(state, entry_len);
    } else {
        // [asm 0292-02a1] PC-speaker note ptr
        let ptr = (0x1037u16 + dl as u16 * 0xA) & 0xFFFF;
        set_sfx_pcspk_note(state, ptr);
    }

    // every CALL is a distinct trigger (a repeated identical effect re-fires)
    state.sfx_queue.push(dl as u16);
    while state.sfx_queue_x.len() < state.sfx_queue.len() {
        state.sfx_queue_x.push(x);
    }
    if state.sfx_queue.len() > 64 {
        let excess = state.sfx_queue.len() - 64;
        state.sfx_queue.drain(0..excess);
        let x_excess = excess.min(state.sfx_queue_x.len());
        state.sfx_queue_x.drain(0..x_excess);
    }
}

// ---- native_load_sfx_bank ----

/// [asm 07C9] Reproduce the sound-init's SFX sample-bank load so the VM-less runtime can PLAY effects.
///
/// Decodes `SAMPLE.SQZ` (the 11 concatenated 8-bit-PCM @ 8000 Hz effects, 0xED60 bytes), places it
/// at `seg`, points the sample segment `[0x0B59]` there, fills each effect's source offset in the
/// `[0x1009 + dl*4]` `{src,len}` table (`src` = prefix-sum of the lengths already loaded there),
/// and flags a digital device present (`cs:[0x1D6C]`) so `native_play_sfx` takes the digital path
/// (writes `[0x1004]`/`[0x1006]`) and the poll fires PlaySfx. Idempotent; call once at boot.
pub fn native_load_sfx_bank(state: &mut NativeGameState, game_root: &Path) {
    let path = match crate::engine::assets::resolve_game_path(game_root, "SAMPLE.SQZ") {
        Ok(p) => p,
        Err(_) => {
            log::warn!("native_load_sfx_bank: SAMPLE.SQZ not found in {}", game_root.display());
            return;
        }
    };
    let raw = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("native_load_sfx_bank: read error: {e}");
            return;
        }
    };
    let pcm = unpack_sqz(&raw);

    let base = (SFX_BANK_SEG << 4) & 0xFFFFF;
    let n = pcm.len().min(state.data.len() - base);
    state.data[base..base + n].copy_from_slice(&pcm[..n]);

    // [0x0B59] = sample segment
    set_sfx_sample_seg(state, SFX_BANK_SEG as u16);

    // fill each effect's src = running offset into the bank
    let mut src: u16 = 0;
    for dl in 0..11 {
        let length = sfx_entry_length(state, dl);
        set_sfx_entry_src(state, dl, src);
        src = src.wrapping_add(length);
    }

    // digital device present -> the SB/digital path
    state.data[(CS_BASE + SFX_DEV_FLAGS[0]) & 0xFFFFF] = 1;
}

// ---- _SONG_INDEX_TO_FILE ----

/// [asm 01AB-01B7] the per-level song: `bl=[0x2d8a]; al=[bx+0x2d20]; call 0x2cc` — the level
/// indexes the song-index table [0x2D20], and the loader (0x2cc/0x492) maps that index to the
/// in-EMS module. Native streams the .TRK from disk, so we map the song index -> filename here.
pub fn song_index_to_file(idx: u8) -> Option<&'static str> {
    match idx {
        0  => Some("PRES.TRK"),       // L3-L5
        1  => Some("CARTE.TRK"),      // the carte scroll-in [asm 96E3 ax=1]
        2  => Some("CODE.TRK"),       // the mode-select menu [asm 952D ax=2]
        3  => Some("PRESENTA.TRK"),   // the title/MENU song [asm 0107/015a ax=3]
        4  => Some("GLACE.TRK"),      // L7/L8
        9  => Some("MINES.TRK"),      // L1/L2
        10 => Some("MYSTERY.TRK"),    // L9
        13 => Some("MONSTER.TRK"),    // L6/L10 (the boss levels)
        14 => Some("FINAL.TRK"),      // L0xE = the final room
        15 => Some("BRAVO.TRK"),      // the level-end tally [asm 4CE7 ax=0xf]
        _  => None,
    }
}

/// [asm 01AB-01B7] The music for the CURRENT level: song index = `[0x2D20 + [0x2d8a]]` mapped to
/// its .TRK. The VM loads this right after the carte (main 01B7), which native's flow must
/// reproduce so the level music replaces the carte song. Falls back to MINES for an unmapped
/// index (any level still gets level music).
pub fn native_level_song_name(state: &NativeGameState) -> String {
    let level = state.rb(0x2D8A) as usize;
    let idx = state.rb(0x2D20 + level);
    song_index_to_file(idx).unwrap_or("MINES.TRK").to_string()
}

// ---- .TRK parsing ----

/// MOD format constants
const MOD_TITLE_LEN: usize = 20;
const MOD_NUM_SAMPLES: usize = 31;
const MOD_SAMPLE_HDR_LEN: usize = 30;
const MOD_ORDER_LEN: usize = 128;
const MOD_SIGNATURE_LEN: usize = 4;

/// A parsed ProTracker module (minimal — just what native_load_song needs).
#[derive(Clone, Debug)]
pub struct ModModule {
    pub title: String,
    pub order: Vec<u8>,
    pub song_length: u8,
    pub restart: u8,
    pub signature: String,
}

/// Decompress a .TRK file (SQZ-LZSS) and parse it as a ProTracker module.
pub fn load_trk(raw: &[u8]) -> ModModule {
    let data = unpack_sqz(raw);
    parse_mod(&data)
}

/// Parse a decoded ProTracker MOD (the payload of a decompressed .TRK).
fn parse_mod(data: &[u8]) -> ModModule {
    let title = String::from_utf8_lossy(
        &data[..MOD_TITLE_LEN].iter().take_while(|&&b| b != 0).copied().collect::<Vec<u8>>()
    ).to_string();

    // 31 sample headers, each 30 bytes (we only need to skip past them)
    let off = MOD_TITLE_LEN + MOD_NUM_SAMPLES * MOD_SAMPLE_HDR_LEN;

    let song_len = if off < data.len() { data[off] } else { 0 };
    let restart = if off + 1 < data.len() { data[off + 1] } else { 0 };
    let order_start = off + 2;
    let order_end = (order_start + MOD_ORDER_LEN).min(data.len());
    let order_full: Vec<u8> = data[order_start..order_end].to_vec();

    let sig_start = order_end;
    let sig_end = (sig_start + MOD_SIGNATURE_LEN).min(data.len());
    let signature = String::from_utf8_lossy(&data[sig_start..sig_end]).to_string();

    // order is truncated to song_length
    let order = order_full[..song_len as usize].to_vec();

    ModModule {
        title,
        order,
        song_length: song_len,
        restart,
        signature,
    }
}

// ---- native_load_song ----

/// Reproduce the song loader (1030:02cc) for the VM-less runtime: parse the standard .TRK module's
/// order list into [0xDC7] and its length into [0xDC2] — the fingerprint the poll matches to
/// identify + play the song. (The enhanced player streams the .TRK from disk, so only the
/// order/length state is needed; no module PCM has to be placed in memory.)
pub fn native_load_song(state: &mut NativeGameState, name: &str, game_root: &Path) {
    let path = match crate::engine::assets::resolve_game_path(game_root, name) {
        Ok(p) => p,
        Err(_) => {
            log::warn!("native_load_song: {} not found in {}", name, game_root.display());
            return;
        }
    };
    let raw = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("native_load_song: read error: {e}");
            return;
        }
    };
    let mod_ = load_trk(&raw);
    let order = &mod_.order;

    set_song_length(state, (order.len() & 0xFF) as u8);

    let order_bytes: Vec<u8> = order.iter().take(ORDER_TABLE_LEN).map(|&o| o & 0xFF).collect();
    let base = DS + ORDER_TABLE;
    let n = order_bytes.len().min(state.data.len() - base);
    state.data[base..base + n].copy_from_slice(&order_bytes[..n]);
}

// ---- audio events ----

/// A `StartSong` event: the identified `.TRK` module name and loop flag.
#[derive(Clone, Debug)]
pub struct StartSong {
    pub name: String,
    pub r#loop: bool,
}

/// A `PlaySfx` event: the resolved PCM data for a sound effect.
#[derive(Clone, Debug)]
pub struct PlaySfx {
    pub sfx_id: u8,
    pub pcm: Vec<u8>,
    pub volume: u8,
    pub source_rate: u32,
    pub pan: f32,
}

// ---- audio command layer ----

/// Read the order table from DGROUP [0xDC7] (up to ORDER_TABLE_LEN bytes).
pub fn read_order_table(state: &NativeGameState) -> Vec<u8> {
    let base = DS + ORDER_TABLE;
    state.data[base..base + ORDER_TABLE_LEN].to_vec()
}

/// Read the song length from DGROUP [0xDC2].
pub fn read_song_length(state: &NativeGameState) -> u8 {
    state.rb(SONG_LENGTH)
}

/// Whether the digital-SFX path is active (a digital device was detected).
pub fn sfx_enabled(state: &NativeGameState) -> bool {
    state.data[(CS_BASE + SFX_DEV_FLAGS[0]) & 0xFFFFF] == 1
    || state.data[(CS_BASE + SFX_DEV_FLAGS[1]) & 0xFFFFF] == 1
}

/// A content hash of the loaded song's whole static data — order, speed, all patterns, and all
/// sample bytes/loops. The song loader fills the order first, then the patterns and the sample
/// PCM, then the playback state, over several frames. A caller waits for two equal readings,
/// so capture happens only once EVERYTHING has stopped changing. `None` when no song is loaded
/// / speed not yet set.
pub fn song_load_fingerprint(state: &NativeGameState) -> Option<u64> {
    let order = read_order_table(state);
    let song_len = read_song_length(state);
    if song_len == 0 {
        return None;
    }
    // Check that the order has at least one non-zero entry in the first song_length+1 positions
    let check_len = (song_len as usize + 1).min(order.len());
    if !order[..check_len].iter().any(|&b| b != 0) {
        return None;
    }
    // Hash the order table (the first song_length entries are the reliable "which song" signal)
    let mut hash: u64 = 0x811C9DC5; // FNV offset basis
    for i in 0..check_len {
        hash ^= order[i] as u64;
        hash = hash.wrapping_mul(0x0100019B); // FNV prime
    }
    hash ^= song_len as u64;
    hash = hash.wrapping_mul(0x0100019B);
    Some(hash)
}

/// Cache of parsed .TRK modules (filename, ModModule) for song identification.
static TRK_INDEX: OnceLock<Vec<(String, ModModule)>> = OnceLock::new();

/// (filename, parsed module) for every .TRK asset, parsed once + cached.
fn trk_index(game_root: &Path) -> &'static Vec<(String, ModModule)> {
    TRK_INDEX.get_or_init(|| {
        let mut index = Vec::new();
        let entries = match std::fs::read_dir(game_root) {
            Ok(e) => e,
            Err(_) => return index,
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext.eq_ignore_ascii_case("TRK")))
            .collect();
        paths.sort();
        for path in paths {
            if let Ok(raw) = std::fs::read(&path) {
                let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string());
                match std::panic::catch_unwind(|| load_trk(&raw)) {
                    Ok(mod_) => index.push((name, mod_)),
                    Err(_) => log::warn!("load_trk failed for {}", name),
                }
            }
        }
        if index.is_empty() {
            log::warn!("[audio-obs] WARNING: no .TRK songs parsed from {} -> StartSong can never identify a song (no music).", game_root.display());
        }
        index
    })
}

/// Match the loaded in-memory module to its standard .TRK by order table.
/// The song loader (0x02cc) copies the module's order list to [0xDC7] and its length to [0xDC2];
/// the order sequence is a strong fingerprint of the song.
pub fn identify_song(state: &NativeGameState, game_root: &Path) -> Option<(String, ModModule)> {
    let order = read_order_table(state);
    let song_len = read_song_length(state) as usize;
    if song_len == 0 {
        return None;
    }
    let target = &order[..song_len];
    if target.is_empty() {
        return None;
    }
    let idx = trk_index(game_root);

    // exact order match
    for (name, mod_) in idx {
        if mod_.order == target {
            return Some((name.clone(), ModModule {
                title: mod_.title.clone(),
                order: mod_.order.clone(),
                song_length: mod_.song_length,
                restart: mod_.restart,
                signature: mod_.signature.clone(),
            }));
        }
    }
    // tolerant prefix match
    for (name, mod_) in idx {
        let n = mod_.order.len().min(target.len());
        if n >= 4 && mod_.order[..n] == target[..n] {
            return Some((name.clone(), ModModule {
                title: mod_.title.clone(),
                order: mod_.order.clone(),
                song_length: mod_.song_length,
                restart: mod_.restart,
                signature: mod_.signature.clone(),
            }));
        }
    }
    None
}

/// Build a StartSong for the song just loaded into VM memory, or None if no song is loaded.
/// Carries the standard .TRK module (module/name) identified from the loaded order table.
pub fn make_start_song(state: &NativeGameState, game_root: &Path) -> Option<StartSong> {
    let order = read_order_table(state);
    let song_len = read_song_length(state);
    if song_len == 0 {
        return None;
    }
    let check_len = (song_len as usize + 1).min(order.len());
    if !order[..check_len].iter().any(|&b| b != 0) {
        return None;
    }
    match identify_song(state, game_root) {
        Some((name, _mod)) => Some(StartSong {
            name,
            r#loop: true,
        }),
        None => Some(StartSong {
            name: String::new(),
            r#loop: true,
        }),
    }
}

/// Resolve the `play_sfx(dl)` digital command into a self-contained event.
/// Mirrors 0x2a9: `bx = dl*4`; `src = [bx+0x1009]`, `len = [bx+0x100b]`; the sample bytes come
/// from segment `[0x0b59]:src`. `pan` (-1 left .. +1 right) is the ENHANCED stereo position.
pub fn resolve_sfx(state: &NativeGameState, dl: u8, pan: f32) -> PlaySfx {
    let dl = dl as usize & 0xFF;
    let base = SFX_TABLE + dl * 4;
    let src = state.rw(base);
    let length = state.rw(base + 2);
    let seg = state.rw(SFX_SEG_PTR);
    let flat = ((seg as usize) << 4) + src as usize;
    let end = (flat + length as usize).min(state.data.len());
    let pcm = state.data[flat..end].to_vec();
    PlaySfx {
        sfx_id: dl as u8,
        pcm,
        volume: 0x40,
        source_rate: 8403,
        pan,
    }
}

// ---- NativeAudio: per-frame audio-command poller ----

/// Per-frame audio-command poller over a NativeGameState.
///
/// `emit` receives each GameAudioEvent (StartSong / PlaySfx) — pass a closure.
/// `game_root` is where the .TRK songs live (to root StartSong in the asset).
pub struct NativeAudio {
    game_root: PathBuf,
    /// last song order-signature emitted (fire once per change)
    song: Option<u64>,
    /// last song_request .TRK loaded (gate the per-frame boss re-request)
    req_file: Option<String>,
    /// ENHANCED: pan SFX by their on-screen trigger X (else all centre / faithful)
    pub stereo: bool,
}

/// Callback trait for audio events emitted by `NativeAudio::poll`.
pub trait AudioEmit {
    fn emit_start_song(&mut self, cmd: StartSong);
    fn emit_play_sfx(&mut self, cmd: PlaySfx);
}

impl NativeAudio {
    /// Create a new poller. `game_root` is where the .TRK songs live.
    pub fn new(game_root: &Path) -> Self {
        Self {
            game_root: game_root.to_path_buf(),
            song: None,
            req_file: None,
            stereo: false,
        }
    }

    /// Emit the audio commands the native frame just produced. Call once per displayed frame.
    pub fn poll<E: AudioEmit>(&mut self, state: &mut NativeGameState, emit: &mut E) {
        // --- a recovered routine requested a song by 02CC INDEX (the 7585 boss-music seam):
        //     load its .TRK order table once per change (the request repeats every boss frame) ---
        let req = state.song_request.take();
        let mut loaded_by_req = false;
        if let Some(req_idx) = req {
            if let Some(name) = song_index_to_file(req_idx as u8) {
                if self.req_file.as_deref() != Some(name) {
                    // the per-frame boss-request de-dup (load once)
                    self.req_file = Some(name.to_string());
                    native_load_song(state, name, &self.game_root);
                    loaded_by_req = true;
                }
            }
        }

        // --- music: the recovered loader wrote the order table; emit StartSong once it changes ---
        let fp = song_load_fingerprint(state);
        if fp != self.song {
            self.song = fp;
            if !loaded_by_req {
                // An EXTERNAL song load (native_load_song at a level start / tally / carte)
                // overwrote the order table — the "already loaded this request" latch (_req_file)
                // is now STALE. Clear it, else a LATER boss re-request of the SAME index is
                // wrongly suppressed and the boss music never switches back.
                self.req_file = None;
            }
            if let Some(_fp) = fp {
                if let Some(cmd) = make_start_song(state, &self.game_root) {
                    emit.emit_start_song(cmd);
                }
            }
        }

        // --- SFX: native_play_sfx queued each TRIGGER this frame (one entry per call, so a
        //     repeated identical effect — a held attack hitting each frame — fires each time).
        //     Emit + drain. ---
        if !state.sfx_queue.is_empty() {
            if sfx_enabled(state) {
                let fallback_x = Some(player_sfx_x(state));
                let queue: Vec<(u16, Option<i32>)> = state
                    .sfx_queue
                    .iter()
                    .enumerate()
                    .map(|(i, &dl)| {
                        let x = state.sfx_queue_x.get(i).copied().flatten().or(fallback_x);
                        (dl, x)
                    })
                    .collect();
                for (dl, x) in &queue {
                    let pan = if self.stereo {
                        if let Some(xv) = x {
                            let p = (*xv as f32 - 160.0) / 160.0;
                            p.max(-1.0).min(1.0)
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };
                    // resolve_sfx reads from the sample bank — if not loaded yet, the PCM is
                    // empty (the bank not loaded yet), which we just emit as an empty PlaySfx.
                    let cmd = resolve_sfx(state, *dl as u8, pan);
                    if cmd.pcm.is_empty() {
                        // bank not loaded yet — stop draining 
                        break;
                    }
                    emit.emit_play_sfx(cmd);
                }
            }
            state.sfx_queue.clear();
            state.sfx_queue_x.clear();
        }
    }
}
