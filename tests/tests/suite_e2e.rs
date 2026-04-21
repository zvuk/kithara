#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

//! End-to-end integration tests that hit real networks and real audio hardware.
//!
//! This suite is gated behind the `e2e` cargo feature so `just test` never
//! compiles or runs it. Every test is `#[ignore]` because it requires live
//! external services (silvercomet.top, zvq.me) or a real cpal audio device.
//!
//! Run with:
//!   just test-e2e
//! or directly:
//!   cargo nextest run -p kithara-integration-tests \
//!       --features e2e --cargo-profile test-release \
//!       --test suite_e2e --run-ignored all

mod common;

#[cfg(not(target_arch = "wasm32"))]
#[path = "common/continuity.rs"]
pub(crate) mod continuity;

#[cfg(not(target_arch = "wasm32"))]
mod kithara_play {
    #[path = "../kithara_play/engine_tests.rs"]
    mod engine_tests;

    #[path = "../kithara_play/resource_regressions.rs"]
    mod resource_regressions;

    #[path = "../kithara_play/silvercomet_seek_hang.rs"]
    mod silvercomet_seek_hang;
}

#[cfg(not(target_arch = "wasm32"))]
mod kithara_queue {
    #[path = "../kithara_queue/cold_seek_cpal.rs"]
    mod cold_seek_cpal;

    #[path = "../kithara_queue/real_playlist.rs"]
    mod real_playlist;
}
