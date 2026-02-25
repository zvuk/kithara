//! Integration tests for kithara-decode

pub(crate) mod fixture;

// Cross-platform: pure in-memory tests (Cursor + EmbeddedAudio, no HTTP)
mod decoder_tests;
mod stress_timeline;
mod timeline_tests;

// Cross-platform: AudioTestServer / AbrTestServer (native axum / WASM fixture server)
mod decoder_seek_tests;
mod fixture_integration;
mod hls_abr_variant_switch;

// Native-only: uses local filesystem (FileSrc::Local + tempfile)
#[cfg(not(target_arch = "wasm32"))]
mod stress_seek_random;
