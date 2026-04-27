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

// engine_tests / resource_regressions содержат большинство не-ignored
// тестов и наши local_* зеркала — подключаются в suite_light через
// kithara_play/mod.rs. Здесь не дублируем.
//
// silvercomet_seek_hang / cold_seek_cpal / real_playlist оставлены здесь
// как именованный entry-point для `just test-e2e` — все их кейсы
// `#[ignore]`'d, и `--run-ignored all` поднимает их.
#[cfg(not(target_arch = "wasm32"))]
mod kithara_play {
    #[path = "../kithara_play/engine_cpal_tests.rs"]
    mod engine_cpal_tests;

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
