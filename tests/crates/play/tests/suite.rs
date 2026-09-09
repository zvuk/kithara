#![forbid(unsafe_code)]
#![recursion_limit = "256"]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}
#[path = "../../integration/tests/common/continuity.rs"]
mod continuity;
pub use kithara_integration_tests::gapless as gapless_common;

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
mod rate_response;
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
mod sync_fixture_census;
mod sync_listening;
#[cfg(not(target_arch = "wasm32"))]
mod sync_oracle;
mod sync_product_matrix;
mod sync_runtime_oracles;
