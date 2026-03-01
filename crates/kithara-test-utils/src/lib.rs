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

#[cfg(not(target_arch = "wasm32"))]
pub mod asset_server;
pub mod fixture_client;
pub mod fixture_protocol;
pub mod fixtures;
#[cfg(not(target_arch = "wasm32"))]
pub mod http_server;
pub mod memory_source;
pub mod rng;
pub mod wav;

/// Re-export of `kithara_test_macros::test` under the `kithara` namespace.
///
/// Allows `use kithara_test_utils::kithara;` and then `#[kithara::test]`
/// or `#[kithara::test(tokio)]` in test modules.
pub mod kithara {
    pub use kithara_test_macros::{fixture, test};
}

#[cfg(not(target_arch = "wasm32"))]
pub use asset_server::serve_assets;
pub use fixtures::*;
#[cfg(not(target_arch = "wasm32"))]
pub use http_server::TestHttpServer;
pub use rng::*;
pub use wav::{create_saw_wav, create_test_wav};

/// Get a `tracing` filter string for tests with wasm-safe fallback.
#[inline]
#[must_use]
pub fn rust_log_filter(default: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("RUST_LOG").unwrap_or_else(|_| default.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        default.to_string()
    }
}
