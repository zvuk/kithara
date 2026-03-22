#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "test utility crate — unwraps are acceptable"
)]
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "test utility crate — numeric casts are acceptable for WAV generation"
)]
#![expect(
    clippy::missing_panics_doc,
    reason = "test utility crate — panic documentation not needed"
)]

//! Shared test utilities for the kithara workspace.

pub mod asset_server;
pub mod fixture_client;
pub mod fixture_protocol;
pub mod fixtures;
#[cfg(not(target_arch = "wasm32"))]
pub mod http_server;
mod log_filter;
pub mod memory_source;
pub mod rng;
pub mod server_url;
pub mod wav;

/// Re-export of `kithara_test_macros::test` under the `kithara` namespace.
///
/// Allows `use kithara_test_utils::kithara;` and then `#[kithara::test]`
/// or `#[kithara::test(tokio)]` in test modules.
pub mod kithara {
    pub use kithara_test_macros::{fixture, test};
}

pub use asset_server::{AssetServer, serve_assets};
pub use fixtures::*;
#[cfg(not(target_arch = "wasm32"))]
pub use http_server::TestHttpServer;
pub use log_filter::rust_log_filter;
pub use rng::*;
pub use server_url::join_server_url;
pub use wav::{create_saw_wav, create_test_wav};
