#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

#[cfg(target_os = "android")]
use kithara_test_dylib as _;

mod engine_tests;
#[cfg(not(target_arch = "wasm32"))]
mod no_sync_deadline;
mod player_internal;
mod player_processor_internal;
mod player_resource_internal;
mod player_track_internal;
mod resource_internal;
mod rt_click;
mod rt_metrics;
mod support;
