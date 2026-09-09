#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod common;

#[cfg(not(target_arch = "wasm32"))]
mod multi_instance;

mod offline_browser;
