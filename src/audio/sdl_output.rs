// SPDX-License-Identifier: GPL-3.0-only
//! SDL2 real-time audio output backend for Prehistorik 2.
//!
//! Uses a single SDL2 `AudioDevice` callback at 44100 Hz stereo `i16` to mix:
//!   - Real-time pure-Rust ProTracker `.MOD` music (`ModSynth`, decoded from `.TRK` via `unpack_sqz`)
//!   - Digital SFX (8000 Hz unsigned 8-bit PCM from `SAMPLE.SQZ`, resampled to 44100 Hz stereo)
//!
//! No external `libxmp.dll` or temp files required — works 100% self-contained on Windows, Linux, and macOS.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use crate::engine::audio::{AudioEmit, PlaySfx, StartSong};
use crate::codecs::sqz::unpack_sqz;
use crate::audio::mod_player::{ModFile, ModSynth};

const OUTPUT_RATE: u32 = 44100;

struct ActiveSfx {
    sfx_id: u8,
    samples: Vec<i16>, // interleaved L, R
    pos: usize,
}

#[inline]
fn soft_clip_i16(v: i32) -> i16 {
    // Clean linear pass up to ±24576 (75% full scale), smooth rational soft-knee above that
    // so simultaneous MOD music + high-amplitude SFX (such as SFX 1 "ugaga" whose second half
    // hits ±127 in 8-bit PCM) never suffer harsh digital square-wave clipping.
    const KNEE: f32 = 24576.0;
    const HEADROOM: f32 = 8191.0; // 32767 - 24576
    let vf = v as f32;
    if vf.abs() <= KNEE {
        v as i16
    } else {
        let sign = vf.signum();
        let excess = vf.abs() - KNEE;
        let compressed = KNEE + HEADROOM * (excess / (excess + HEADROOM));
        (sign * compressed).clamp(-32768.0, 32767.0) as i16
    }
}

struct MixerSharedState {
    mod_synth: Option<ModSynth>,
    active_sfx: Vec<ActiveSfx>,
    music_enabled: bool,
    sfx_enabled: bool,
    paused: bool,
    music_vol: f32,
    sfx_vol: f32,
}

struct RealTimeAudioCallback {
    shared: Arc<Mutex<MixerSharedState>>,
}

impl AudioCallback for RealTimeAudioCallback {
    type Channel = i16;

    fn callback(&mut self, out: &mut [i16]) {
        for s in out.iter_mut() {
            *s = 0;
        }
        let mut guard = match self.shared.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.paused {
            return;
        }

        if guard.music_enabled {
            let gain = guard.music_vol;
            if let Some(ref mut synth) = guard.mod_synth {
                synth.mix_stereo_i16(out, gain);
            }
        }

        if guard.sfx_enabled {
            let active_count = guard.active_sfx.len();
            let norm = if active_count > 1 {
                1.0 / (active_count as f32).sqrt()
            } else {
                1.0
            };
            let mut sfx_sum = vec![0i32; out.len()];
            for sfx in guard.active_sfx.iter_mut() {
                let rem = sfx.samples.len().saturating_sub(sfx.pos);
                let n = rem.min(out.len());
                for i in 0..n {
                    sfx_sum[i] += (sfx.samples[sfx.pos + i] as f32 * norm) as i32;
                }
                sfx.pos += n;
            }
            for i in 0..out.len() {
                if sfx_sum[i] != 0 {
                    out[i] = soft_clip_i16(out[i] as i32 + sfx_sum[i]);
                }
            }
            guard.active_sfx.retain(|s| s.pos < s.samples.len());
        } else {
            guard.active_sfx.clear();
        }
    }
}

/// Real SDL2 audio output — mixes pure-Rust MOD music + resampled SFX in one real-time callback.
pub struct SdlAudioOutput {
    _device: AudioDevice<RealTimeAudioCallback>,
    shared: Arc<Mutex<MixerSharedState>>,
    game_root: PathBuf,
    current_song: Option<String>,
}

impl SdlAudioOutput {
    /// Initialize SDL2 audio playback device at 44100 Hz stereo.
    pub fn new(sdl_context: &sdl2::Sdl, game_root: PathBuf) -> Result<Self, String> {
        let audio_subsys = sdl_context.audio()?;

        let desired_spec = AudioSpecDesired {
            freq: Some(OUTPUT_RATE as i32),
            channels: Some(2),
            samples: Some(1024),
        };

        let shared = Arc::new(Mutex::new(MixerSharedState {
            mod_synth: None,
            active_sfx: Vec::new(),
            music_enabled: true,
            sfx_enabled: true,
            paused: false,
            music_vol: 0.85,
            sfx_vol: 1.0,
        }));

        let shared_cb = Arc::clone(&shared);
        let device = audio_subsys.open_playback(None, &desired_spec, |_spec| {
            RealTimeAudioCallback { shared: shared_cb }
        })?;
        device.resume();

        Ok(SdlAudioOutput {
            _device: device,
            shared,
            game_root,
            current_song: None,
        })
    }

    /// Resample 8-bit signed PCM (`i8`, `-128..127`, center `0`) at `src_rate` (~8403 Hz)
    /// to 16-bit signed stereo PCM at `OUTPUT_RATE` (44100 Hz) using linear interpolation
    /// (matching `scripts/sdl_view.py` `np.interp`) and a smooth tail release ramp so samples
    /// that end at a non-zero offset (such as SFX 1 "ugaga" ending at `-54`) never click or distort.
    fn resample_sfx(pcm: &[u8], src_rate: u32, volume: f32, pan: f32) -> Vec<i16> {
        if pcm.is_empty() || src_rate == 0 {
            return Vec::new();
        }

        // Convert to normalized f32 [-1.0..1.0] and gently smooth harsh 8-bit rail saturation
        // (where the original 8-bit recording hits ±125..127 in the second half of SFX 1).
        let n_src = pcm.len();
        let mut src_f32 = Vec::with_capacity(n_src);
        for i in 0..n_src {
            let c = pcm[i] as i8 as f32;
            let val = if c.abs() >= 122.0 && i > 0 && i + 1 < n_src {
                let prev = pcm[i - 1] as i8 as f32;
                let next = pcm[i + 1] as i8 as f32;
                0.25 * prev + 0.50 * c + 0.25 * next
            } else {
                c
            };
            src_f32.push(val / 128.0);
        }

        let ratio = OUTPUT_RATE as f64 / src_rate as f64;
        let out_frames = ((n_src as f64) * ratio).ceil() as usize;
        let mut out = Vec::with_capacity(out_frames * 2);

        let p = pan.clamp(-1.0, 1.0);
        let left_gain = (1.0 - p).min(1.0) * volume;
        let right_gain = (1.0 + p).min(1.0) * volume;

        // Smooth fade-out over the last ~8 ms (350 output frames at 44.1 kHz) to eliminate
        // any abrupt cutoff click when a sample terminates mid-wave (e.g. SFX 1 ends at -54).
        let fade_out_frames = 350usize.min(out_frames / 8).max(1);
        let fade_start = out_frames.saturating_sub(fade_out_frames);

        for i in 0..out_frames {
            let src_pos = (i as f64) / ratio;
            let idx0 = (src_pos.floor() as usize).min(n_src - 1);
            let idx1 = (idx0 + 1).min(n_src - 1);
            let frac = (src_pos - idx0 as f64) as f32;

            let mut sample = src_f32[idx0] * (1.0 - frac) + src_f32[idx1] * frac;
            if i >= fade_start {
                let env = (out_frames - i) as f32 / (fade_out_frames as f32);
                sample *= env;
            }

            let left = (sample * left_gain * 22000.0).clamp(-32768.0, 32767.0) as i16;
            let right = (sample * right_gain * 22000.0).clamp(-32768.0, 32767.0) as i16;

            out.push(left);
            out.push(right);
        }

        out
    }
}

impl AudioEmit for SdlAudioOutput {
    fn emit_start_song(&mut self, cmd: StartSong) {
        self.on_start_song(cmd);
    }
    fn emit_play_sfx(&mut self, cmd: PlaySfx) {
        self.on_play_sfx(cmd);
    }
}

impl SdlAudioOutput {
    fn on_play_sfx(&mut self, cmd: PlaySfx) {
        let sfx_vol = match self.shared.lock() {
            Ok(g) => {
                if !g.sfx_enabled {
                    return;
                }
                g.sfx_vol
            }
            Err(_) => return,
        };
        let vol = (cmd.volume as f32 / 64.0).clamp(0.0, 1.0) * sfx_vol;
        let samples = Self::resample_sfx(&cmd.pcm, cmd.source_rate, vol, cmd.pan);
        if !samples.is_empty() {
            if let Ok(mut g) = self.shared.lock() {
                // Re-triggering the same SFX replaces the previous instance instead of stacking
                g.active_sfx.retain(|s| s.sfx_id != cmd.sfx_id);
                if g.active_sfx.len() >= 8 {
                    g.active_sfx.remove(0);
                }
                g.active_sfx.push(ActiveSfx {
                    sfx_id: cmd.sfx_id,
                    samples,
                    pos: 0,
                });
            }
        }
    }

    fn on_start_song(&mut self, cmd: StartSong) {
        let trk_file = if cmd.name.to_ascii_uppercase().ends_with(".TRK") {
            cmd.name.clone()
        } else {
            format!("{}.TRK", cmd.name)
        };

        if self.current_song.as_deref() == Some(&trk_file) {
            return;
        }

        let trk_path = crate::engine::assets::resolve_game_path(&self.game_root, &trk_file)
            .unwrap_or_else(|_| self.game_root.join(&trk_file));

        match std::fs::read(&trk_path) {
            Ok(raw) => {
                let mod_data = unpack_sqz(&raw);
                if let Some(mod_file) = ModFile::from_bytes(&mod_data) {
                    let synth = ModSynth::new(mod_file, OUTPUT_RATE, cmd.r#loop);
                    if let Ok(mut g) = self.shared.lock() {
                        g.mod_synth = Some(synth);
                    }
                    self.current_song = Some(trk_file.clone());
                    log::debug!("StartSong: {} ({} bytes MOD)", trk_file, mod_data.len());
                } else {
                    log::warn!("Failed to parse MOD data from {}", trk_file);
                }
            }
            Err(e) => {
                log::warn!("TRK not found: {:?}: {}", trk_path, e);
            }
        }
    }

    /// Pause or resume real-time audio mixing while preserving playback position.
    pub fn set_paused(&mut self, paused: bool) {
        if let Ok(mut g) = self.shared.lock() {
            g.paused = paused;
        }
    }

    /// Compatibility no-op (audio is mixed in real-time by the SDL2 callback thread).
    pub fn pump(&mut self) {}
}

/// No-op audio sink when no audio device is available.
pub struct NullAudio;

impl AudioEmit for NullAudio {
    fn emit_start_song(&mut self, _cmd: StartSong) {}
    fn emit_play_sfx(&mut self, _cmd: PlaySfx) {}
}
