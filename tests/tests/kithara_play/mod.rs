#[cfg(not(target_arch = "wasm32"))]
#[path = "../common/offline_player_harness.rs"]
mod offline_player_harness;

mod engine_tests;
mod gapless_offline_e2e;
mod gapless_startup_regressions;
mod offline_harness_smoke;
mod player_queue_api_regressions;
mod red_crossfade_hls_to_mp3_blocks_render;
mod resource_regressions;
mod seamless_queue_advance;
mod silvercomet_seek_hang;
