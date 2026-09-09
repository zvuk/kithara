#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}
pub use kithara_integration_tests::gapless as gapless_common;

#[path = "fixture_integration.rs"]
mod fixture_integration;
#[path = "gapless_encoding_parity.rs"]
mod gapless_encoding_parity;
#[path = "gapless_parity.rs"]
mod gapless_parity;
#[path = "hls_abr_variant_switch.rs"]
mod hls_abr_variant_switch;
#[path = "stress_seek_random.rs"]
mod stress_seek_random;
#[path = "stress_timeline.rs"]
mod stress_timeline;
