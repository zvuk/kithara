#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

//! Integration tests for kithara-assets

mod eviction_bytes_integration;
mod eviction_integration;
mod index_persistence;
mod integration_storage_assets;
#[cfg(not(target_arch = "wasm32"))]
mod pins_index_integration;
mod processing_integration;
mod resource_path_test;
#[cfg(not(target_arch = "wasm32"))]
mod resource_state;
mod streaming_resources_comprehensive;
mod support;
