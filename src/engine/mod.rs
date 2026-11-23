// SPDX-License-Identifier: GPL-3.0-only
//! Engine module — core game state, boot memory, VGA, assets, audio, and runtime drivers.

pub mod audio;
pub mod assets;
pub mod boot_data;
pub mod camera_scroll;
pub mod dgroup_offsets;
pub mod front_end;
pub mod gameover_scene;
pub mod input;
pub mod level_init;
pub mod level_load;
pub mod level_state;
pub mod render;
pub mod r#loop;
pub mod runtime;
pub mod state;
pub mod vga;
pub mod sprite_bank;

pub use state::NativeGameState;
pub use vga::NativeVGA;
