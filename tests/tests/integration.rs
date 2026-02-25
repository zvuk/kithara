//! All integration tests for kithara
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod kithara_assets;
mod kithara_audio;
mod kithara_decode;
mod kithara_file;
mod kithara_hls;
mod kithara_net;
mod kithara_storage;
mod kithara_stream;
mod multi_instance;

// WASM + native test modules (platform-gated internally)
mod kithara_wasm;
