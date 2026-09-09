#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

mod common {
    pub(crate) use kithara_integration_tests::test_defaults;
}
pub use kithara_integration_tests::gapless as gapless_common;

mod audio_tests;
mod dsp_properties;
mod file_ephemeral_mp3;
#[cfg(not(target_arch = "wasm32"))]
mod gapless_crossfade;
#[cfg(not(target_arch = "wasm32"))]
mod gapless_pipeline;
#[cfg(not(target_arch = "wasm32"))]
mod no_sync_passthrough;
mod stream_source_tests;
