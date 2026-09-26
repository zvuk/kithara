#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

use kithara_test_dylib as _;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}
pub use kithara_integration_tests::gapless as gapless_common;

#[path = "fixture_integration.rs"]
mod fixture_integration;
#[path = "gapless_encoding_parity.rs"]
mod gapless_encoding_parity;
#[path = "gapless_offline_e2e.rs"]
mod gapless_offline_e2e;
#[path = "gapless_parity.rs"]
mod gapless_parity;
#[path = "gapless_startup_regressions.rs"]
mod gapless_startup_regressions;
#[path = "generated_gapless_hls.rs"]
mod generated_gapless_hls;
#[path = "phase_continuity.rs"]
mod phase_continuity;
#[path = "stress_seek_random.rs"]
mod stress_seek_random;
#[path = "stress_timeline.rs"]
mod stress_timeline;
