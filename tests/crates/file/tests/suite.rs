//! Integration tests for kithara-file

#[cfg(not(target_arch = "wasm32"))]
mod early_stream_close;
#[cfg(not(target_arch = "wasm32"))]
mod file_source;

mod live_stress_real_mp3;
