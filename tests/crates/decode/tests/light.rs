#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

#[path = "aac_he_v2_hls_decode.rs"]
mod aac_he_v2_hls_decode;
#[path = "aac_priming_regression.rs"]
mod aac_priming_regression;
#[path = "apple_mp3_priming_probe.rs"]
mod apple_mp3_priming_probe;
#[path = "decoder_seek_tests.rs"]
mod decoder_seek_tests;
#[path = "timeline_tests.rs"]
mod timeline_tests;
