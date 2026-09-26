#![forbid(unsafe_code)]
#![recursion_limit = "256"]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;
use kithara_test_dylib as _;

#[cfg(not(target_arch = "wasm32"))]
mod append_while_playing;
mod architecture_flow;
mod auto_advance;
mod clear_then_replay;
mod duplicate_src_in_queue;
mod early_seek_size_withheld_advance;
mod file_replay_from_warm_cache;
mod full_playthrough_census;
mod loader_lanes;
mod loader_starvation;
mod local_track_plays;
mod mp3_plays_to_its_end;
mod packaged_drm_seek;
mod play_before_the_load_lands;
mod player_queue_api_regressions;
mod playlist_stall_fails_load;
mod select_after_eof;
mod track_switch_race;
mod user_simulation;
mod zvuk_cipher_check;

// Mirror crate so the test binary can resolve `aes::cipher::*` directly.
// `cbc` already brings AES, but for ECB diagnostic we need the bare
// block cipher.
