#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]
#![recursion_limit = "256"]

pub use kithara_integration_tests::bufpool_ext;

#[cfg(not(target_arch = "wasm32"))]
mod no_sync_passthrough;
mod rate_response;
