#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

mod common;

mod kithara_broadcast {
    #[path = "../common/offline_player_harness.rs"]
    mod offline_player_harness;

    mod engine_e2e;
    mod hls_conformance;
    mod origin;
    mod origin_tests;
    mod packaging_tests;
    mod vod_tail;
}
