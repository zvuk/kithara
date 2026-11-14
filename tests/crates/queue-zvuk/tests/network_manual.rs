#![cfg(not(target_os = "android"))]
#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;
use kithara_test_dylib as _;

mod zvuk_drm_trace;
mod zvuk_stage_drm_e2e;
