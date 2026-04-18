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

#[cfg(test)]
extern crate self as kithara_test_utils;

pub(crate) mod consts;
pub mod fixture_protocol;
pub mod fixtures;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod fmp4;
mod hls_blob_store;
pub mod hls_fixture;
pub(crate) mod hls_spec;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod hls_stream;
pub mod hls_url;
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
pub mod signal_url;
pub mod test_server;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod test_server_state;
mod token_store;
pub mod wav;

/// Re-export of `kithara_test_macros::test` under the `kithara` namespace.
///
/// Allows `use kithara_test_utils::kithara` and then `#[kithara::test]`
/// or `#[kithara::test(tokio)]` in test modules.
pub mod kithara {
    pub use kithara_test_macros::{fixture, test};
}

pub use fixtures::*;
pub use hls_fixture::{
    AbrTestServer, EncryptionConfig, HlsTestServer, HlsTestServerConfig, PackagedTestServer,
    TestServer, abr, compat, master_playlist, packaged, packaged_test_server, test_master_playlist,
    test_master_playlist_encrypted, test_master_playlist_with_init, test_media_playlist,
    test_media_playlist_encrypted, test_media_playlist_with_init, test_segment_data, test_server,
};
pub use hls_url::{
    HlsSpec, encode_hls_spec, hls_init_path, hls_key_path, hls_master_path, hls_media_path,
    hls_segment_path,
};
#[cfg(not(target_arch = "wasm32"))]
pub use http_server::TestHttpServer;
pub use log_filter::rust_log_filter;
pub use rng::*;
pub use server_url::join_server_url;
pub use signal_source_utils::*;
pub use signal_url::{SignalFormat, SignalKind, SignalSpec, SignalSpecLength, signal_path};
#[cfg(not(target_arch = "wasm32"))]
pub use test_server::run_test_server;
pub use test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder, TestServerHelper};
pub use wav::{create_test_wav, create_wav_exact_bytes};
