#[cfg(not(target_arch = "wasm32"))]
#[path = "../common/offline_player_harness.rs"]
mod offline_player_harness;

mod auto_advance;
mod cold_seek_middle;
mod cpal_cold_seek_synthetic;
mod hls_seek_cancels_stale_fetches;
mod hls_seek_near_end_stress;
mod local_track_plays;
mod real_playlist;
mod track_replay_after_switch;
mod track_switch_race;
mod zvuk_cipher_check;
mod zvuk_drm_trace;
mod zvuk_prod_drm_e2e;
mod zvuk_stage_drm_e2e;
mod zvuk_stage_seed_brute_force;

// Mirror crate so the test binary can resolve `aes::cipher::*` directly.
// `cbc` already brings AES, but for ECB diagnostic we need the bare
// block cipher.
