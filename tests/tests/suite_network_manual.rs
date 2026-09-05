#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]
//! The network tests no runner can serve.
//!
//! Two reasons put a test here, and both are about the machine rather than the
//! code. `*.zvq.me` resolves only inside the corporate network, which the CI
//! fleet is not on and will not be — no certificate is installed for it and
//! none will be. And a test that opens a real output device needs a sound card,
//! which a container does not have.
//!
//! Everything reachable from the public internet lives in `suite_network`
//! instead, where CI runs it. The split is by what a runner can reach, not by
//! what the test is about: keeping them together meant the whole set stayed off
//! CI because part of it could not run there.
//!
//! `just test run --lane=network-manual`, from a machine that has both.

pub use kithara_integration_tests::bufpool_ext;

#[cfg(not(target_arch = "wasm32"))]
mod kithara_play {
    mod live_remote_network;
}

#[cfg(not(target_arch = "wasm32"))]
mod kithara_queue {
    mod source_helper;

    mod cold_seek_cpal;
    mod zvuk_drm_trace;
    mod zvuk_stage_drm_e2e;
}
