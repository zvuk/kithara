#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

//! Stress tests that require exclusive CPU access.
//!
//! These tests use `recv_outcome_blocking` (blocking audio reads after seek)
//! and are sensitive to CPU contention. They run one at a time via nextest
//! `threads-required = "num-cpus"` override.

mod common;

#[cfg(not(target_arch = "wasm32"))]
#[path = "common/continuity.rs"]
pub(crate) mod continuity;

mod kithara_hls {
    mod abr_auto_switch;
    mod abr_mode_switch;
    mod abr_switch_playback;
    mod live_stress_real_stream;
    mod red_flaky_small_cache_hot_refetch;
    mod red_leak_native_drm_seek_resume;
    mod stress_seek_abr;
    mod stress_seek_abr_audio;
    mod stress_seek_audio;
    mod stress_seek_lifecycle;
}
