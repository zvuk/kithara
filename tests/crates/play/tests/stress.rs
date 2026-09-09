#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}
#[path = "../../integration/tests/phase_continuity/mod.rs"]
mod phase_continuity;

#[path = "flac_realtime_player_continuity.rs"]
mod flac_realtime_player_continuity;
#[path = "hls_seek_middle_stress_long.rs"]
mod hls_seek_middle_stress_long;
