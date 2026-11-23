// SPDX-License-Identifier: GPL-3.0-only
//! Scene video mode identifiers.

/// EGA 320x200x16 planar video mode (`0x0D` — menus, world map, score tally).
pub const MODE_PLANAR: u8 = 0x0D;
/// VGA 320x200x256 linear video mode (`0x13` — intro and title screens).
pub const MODE_LINEAR: u8 = 0x13;
