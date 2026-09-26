#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

mod matrix;
mod oracle;
mod reference;
mod runtime;

use matrix::{
    BLOCK_FRAMES, BOUNDARY_OUTLIER_RATIO, CHANNELS, CapturedAudio, Case,
    EXACT_ZERO_RUN_LIMIT_FRAMES, MAX_DECK_GAIN_DELTA, MAX_MATCHED_RMS_DELTA_DB, MIN_BOUNDARY_JUMP,
    MIN_DECK_CONTRIBUTION_RATIO, MIN_FIXED_STEM_RMS_DBFS, MIX_HEADROOM, PRELOAD_TIMEOUT,
    SOURCE_RATE,
};
use runtime::Deck;
