#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}

#[path = "aac_priming_regression.rs"]
mod aac_priming_regression;
#[path = "apple_mp3_priming_probe.rs"]
mod apple_mp3_priming_probe;
#[path = "decoder_seek_tests.rs"]
mod decoder_seek_tests;
#[path = "decoder_tests.rs"]
mod decoder_tests;
#[path = "factory_tests.rs"]
mod factory_tests;
#[path = "protocol_tests.rs"]
mod protocol_tests;
#[path = "symphonia_seek_stale_duration.rs"]
mod symphonia_seek_stale_duration;
#[path = "symphonia_tests.rs"]
mod symphonia_tests;
#[path = "timeline_tests.rs"]
mod timeline_tests;
