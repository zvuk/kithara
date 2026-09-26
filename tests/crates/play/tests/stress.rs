#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;
use kithara_test_dylib as _;

#[path = "flac_realtime_player_continuity.rs"]
mod flac_realtime_player_continuity;
