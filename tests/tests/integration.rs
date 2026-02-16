//! All integration tests for kithara
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

// Native-only test modules
#[cfg(not(target_arch = "wasm32"))]
mod common;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_assets;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_audio;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_decode;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_file;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_hls;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_net;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_storage;
#[cfg(not(target_arch = "wasm32"))]
mod kithara_stream;
#[cfg(not(target_arch = "wasm32"))]
mod multi_instance;

// WASM test modules
#[cfg(target_arch = "wasm32")]
mod kithara_wasm;
