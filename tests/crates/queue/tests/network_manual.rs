#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

#[path = "source_helper.rs"]
mod source_helper;

#[path = "cold_seek_cpal.rs"]
mod cold_seek_cpal;
#[path = "zvuk_drm_trace.rs"]
mod zvuk_drm_trace;
#[path = "zvuk_stage_drm_e2e.rs"]
mod zvuk_stage_drm_e2e;
