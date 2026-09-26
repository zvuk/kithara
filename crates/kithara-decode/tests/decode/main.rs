#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

#[cfg(target_os = "android")]
use kithara_test_dylib as _;

mod decoder_tests;
mod factory_tests;
mod protocol_tests;
mod symphonia_seek_stale_duration;
mod symphonia_tests;
mod timeline_tests;
