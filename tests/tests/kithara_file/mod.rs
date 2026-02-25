//! Integration tests for kithara-file

pub(crate) mod fixture;

// Native-only: use axum directly for in-process HTTP servers
#[cfg(not(target_arch = "wasm32"))]
mod early_stream_close;
#[cfg(not(target_arch = "wasm32"))]
mod file_source;

mod live_stress_real_mp3;
