#[cfg(not(target_arch = "wasm32"))]
mod source_helper;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use source_helper::{app_disk_asset_store, app_track_source};

mod advance_boundary_provenance;
mod architecture_flow;
mod auto_advance;
mod cold_seek_middle;
mod cpal_cold_seek_synthetic;
mod duplicate_src_in_queue;
mod early_seek_size_withheld_advance;
mod file_replay_from_warm_cache;
mod flac_swallow_fixture;
mod hls_seek_cancels_stale_fetches;
mod hls_seek_near_end_stress;
mod hls_variant_playlists_concurrent;
mod loader_lanes;
mod loader_starvation;
mod local_track_plays;
mod non_leading_track_completion;
mod packaged_drm_seek;
mod play_before_the_load_lands;
mod playback_warms_its_own_analysis;
mod playlist_stall_fails_load;
mod rapid_scrub_decode_failure;
mod select_after_eof;
mod track_replay_after_switch;
mod track_switch_race;
mod user_simulation;
mod zvuk_cipher_check;

// Mirror crate so the test binary can resolve `aes::cipher::*` directly.
// `cbc` already brings AES, but for ECB diagnostic we need the bare
// block cipher.
