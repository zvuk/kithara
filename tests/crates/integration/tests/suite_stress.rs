#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

#[cfg(not(target_arch = "wasm32"))]
use kithara_test_dylib as _;

mod hls_seek_middle_stress_long;
