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

pub mod fixture_client;
pub mod fixture_protocol;
pub mod fixtures;
#[cfg(not(target_arch = "wasm32"))]
pub mod http_server;
mod log_filter;
pub mod memory_source;
pub mod rng;
#[cfg(not(target_arch = "wasm32"))]
pub mod routes;
pub mod server_url;
pub mod signal_pcm;
pub mod signal_source;
mod signal_source_utils;
pub(crate) mod signal_spec;
#[cfg(not(target_arch = "wasm32"))]
pub mod test_server;
pub mod wav;

/// Re-export of `kithara_test_macros::test` under the `kithara` namespace.
///
/// Allows `use kithara_test_utils::kithara` and then `#[kithara::test]`
/// or `#[kithara::test(tokio)]` in test modules.
pub mod kithara {
    pub use kithara_test_macros::{fixture, test};
}

pub use fixtures::*;
#[cfg(not(target_arch = "wasm32"))]
pub use http_server::TestHttpServer;
pub use log_filter::rust_log_filter;
pub use rng::*;
pub use server_url::join_server_url;
pub use signal_source_utils::*;
#[cfg(not(target_arch = "wasm32"))]
pub use test_server::{TestServerHelper, run_test_server};
pub use wav::{create_test_wav, create_wav_exact_bytes};
