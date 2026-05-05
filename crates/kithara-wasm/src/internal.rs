#![forbid(unsafe_code)]

//! Internal re-exports for the WASM crate's own integration tests.
//!
//! Wasm-only; gated under a single `cfg(target_arch = "wasm32")` on
//! `mod re_exports` so the rest of this file is unconditional.

#[cfg(target_arch = "wasm32")]
mod re_exports {
    pub use crate::wasm::{
        bindings::{build_info, setup},
        player::{
            player_eq_band_count, player_eq_gain, player_get_crossfade_seconds,
            player_get_duration_ms, player_get_position_ms, player_get_session_ducking,
            player_get_volume, player_is_playing, player_new, player_pause, player_play,
            player_process_count, player_reset_eq, player_seek, player_select_track,
            player_set_crossfade_seconds, player_set_eq_gain, player_set_session_ducking,
            player_set_volume, player_stop, player_take_events, player_tick, player_warm_up_audio,
        },
    };
}

#[cfg(target_arch = "wasm32")]
pub use re_exports::*;
