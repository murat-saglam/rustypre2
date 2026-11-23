// SPDX-License-Identifier: GPL-3.0-only
//! Symbolic DGROUP offset constants.
//!
//! Every constant here is a DS-relative offset into the game's global data segment (`DGROUP`).

// ---- camera / scroll / render-mirror ---------------------------------
pub const CAM_COL: usize             = 0x2DE4;
pub const CAM_ROW: usize             = 0x2DE6;
pub const COL_RING: usize            = 0x2DE8;
pub const ROW_RING: usize            = 0x2DEA;
pub const SCROLL_COPY_SRC: usize     = 0x2DBA;
pub const SCROLL_SCRIPT_PTR: usize   = 0x2DBC;
pub const SCROLL_SCRIPT_PTR2: usize  = 0x2DBE;
pub const SCROLL_SCRIPT_TABLE: usize = 0x2D40;

// ---- per-level metadata + asset-addressing --------------------------
pub const WARP_TABLE: usize          = 0x2CF6;
pub const LEVEL_HEADER_TABLE: usize  = 0x2D30;
pub const FILENAME_DIGIT: usize      = 0x2D90;
pub const GFX_GROUP_TABLE: usize     = 0x2D96;
pub const GFX_GROUP: usize           = 0x2DAA;
pub const LEVEL_DATA_SEG: usize      = 0x2DDA;
pub const UNION_BANK_SEG: usize      = 0x2DDC;
pub const LOAD_TOP: usize            = 0x2875;

// ---- collectible / score / item -------------------------------------
pub const COLLECT_TOTAL_MAIN: usize  = 0x2A74;
pub const COLLECT_TOTAL_DECOR: usize = 0x2A78;
pub const ITEM_QUEUE: usize          = 0x6C12;
pub const ITEM_TOTAL: usize          = 0x6C9E;

// ---- password / level-warp seed -------------------------------------
pub const BIOS_SEED: usize           = 0xA333;
pub const SEED_COMPUTED_FLAG: usize  = 0xA335;
pub const DECOR_PTR_LIST: usize      = 0x6A88;

// ---- font + property-header words -----------------------------------
pub const LEVEL_PROP_HEADER: usize   = 0x815E;
pub const PLAYER_START_X: usize      = 0x8160;
pub const PLAYER_START_Y: usize      = 0x8162;

// ---- per-level index / mode / loader flags ---------------------------
pub const LEVEL_INDEX: usize         = 0x2D8A;
pub const MODE_COPY: usize           = 0xB198;
pub const ANY_ANIMATED_FLAG: usize   = 0x6BBD;
pub const LEVEL_BOTTOM_LIMIT: usize  = 0x2CF5;
pub const SPRITE_BANK_LO: usize      = 0x8C89;
pub const SPRITE_BANK_HI: usize      = 0x8C8B;

// ---- horizontal-scroll follower -------------------------------------
pub const SCROLL_ACCUM: usize        = 0x8164;

// ---- render / RNG scratch -------------------------------------------
pub const TILE_FLAGS_ACC: usize      = 0x2DF2;
pub const GRID_DIRTY: usize          = 0x2DF4;
pub const BG_RESTORE_PTR: usize      = 0x2DF6;

// ---- bulk state blocks + lookup tables ------------------------------
pub const DBL_BUFFER_BACKUP: usize   = 0x9203;
pub const TILE_TYPE_TABLE: usize     = 0x4DF8;
pub const TILE_MASK_TABLE: usize     = 0x2DF8;
pub const GAMEOVER_PALETTE: usize    = 0xAFE8;

// ---- arena bases walked by index ------------------------------------
pub const ENTITY_LIST_2NDPASS: usize = 0x8489;
pub const BONUS_CELL_LIST: usize     = 0x8C8D;
pub const ACTIVE_FLAG_SNAPSHOT: usize = 0xA2A8;
pub const BIRD_PAIR_SLOTS: usize     = 0x4FD0;

// ---- render / effect slots -----------------------------------------
pub const RENDER_SLOTS_BASE: usize   = 0x4F0A;
pub const PLAYER_SLOT: usize         = 0x4F1C;
pub const EFFECT_SPRITE_SRC: usize   = 0x8F1D;
pub const CRY_CAVEMAN_SPRITE: usize  = 0x5088;
