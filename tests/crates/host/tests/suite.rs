#![forbid(unsafe_code)]
#![recursion_limit = "256"]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

use kithara_test_dylib as _;

#[cfg(not(target_arch = "wasm32"))]
mod deck_membership;
#[cfg(not(target_arch = "wasm32"))]
mod mix_tap;
mod mixing;
mod no_sync_real_media;
