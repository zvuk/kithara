#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

mod engine_e2e;
mod hls_conformance;
mod origin;
mod origin_tests;
mod packaging_tests;
mod route_handover;
mod vod_tail;
