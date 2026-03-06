#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
pub use crate::bindings::{build_info, setup};
#[cfg(target_arch = "wasm32")]
pub use crate::player::{
    player_eq_band_count, player_eq_gain, player_get_crossfade_seconds, player_get_duration_ms,
    player_get_position_ms, player_get_session_ducking, player_get_volume, player_is_playing,
    player_new, player_pause, player_play, player_process_count, player_reset_eq, player_seek,
    player_select_track, player_set_crossfade_seconds, player_set_eq_gain,
    player_set_session_ducking, player_set_volume, player_stop, player_take_events, player_tick,
    player_warm_up_audio,
};
