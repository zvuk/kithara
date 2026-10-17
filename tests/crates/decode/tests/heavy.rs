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

mod fixture_integration;
mod gapless_encoding_parity;
mod gapless_offline_e2e;
mod gapless_parity;
mod gapless_startup_regressions;
mod generated_gapless_hls;
mod initial_visible_duration;
mod mp3_grid_clock;
mod phase_continuity;
mod stress_seek_random;
mod stress_timeline;
