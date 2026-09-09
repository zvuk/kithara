#![forbid(unsafe_code)]
#![recursion_limit = "256"]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

#[path = "no_sync_real_media.rs"]
mod no_sync_real_media;
