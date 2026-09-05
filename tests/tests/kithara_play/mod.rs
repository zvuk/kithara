mod cochlea_continuity_oracle;
mod engine_tests;
mod gapless_offline_e2e;
mod gapless_startup_regressions;
mod generated_gapless_hls;
mod hls_seek_middle_no_queue;
mod hls_seek_middle_stress;
mod hls_seek_past_end_terminates;
mod local_seek_hang_iters;
#[cfg(not(target_arch = "wasm32"))]
mod mix_tap;
mod mixing;
#[cfg(not(target_arch = "wasm32"))]
mod no_sync_deadline;
mod offline_harness_smoke;
mod player_internal;
mod player_processor_internal;
mod player_queue_api_regressions;
mod player_resource_internal;
mod player_track_internal;
mod quality_switch_continuity;
mod red_crossfade_hls_to_mp3_blocks_render;
mod resource_internal;
mod resource_regressions;
#[cfg(not(target_arch = "wasm32"))]
mod ring_admission;
mod rt_click;
mod rt_metrics;
mod seamless_queue_advance;
#[cfg(not(target_arch = "wasm32"))]
mod session_transport;
mod sync_listening;
#[cfg(not(target_arch = "wasm32"))]
mod sync_oracle;
mod sync_product_matrix;
mod sync_runtime_oracles;
