//! Body of `kithara-test-utils` — gated as a whole behind
//! `feature = "test-utils"` from `lib.rs`.

pub mod consts;
pub mod fixture_protocol;
pub mod fixtures;
pub mod hls_blob_store;
pub mod hls_fixture;
pub mod hls_spec;
pub mod hls_url;
pub mod log_filter;
pub mod rng;
pub mod server_url;
pub mod signal_pcm;
pub mod signal_source_utils;
pub mod signal_spec;
pub mod signal_url;
pub mod test_server;
pub mod token_store;
pub mod wav;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub mod probe_capture;
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
pub use log_filter::rust_log_filter;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
pub use rng::*;
pub use server_url::join_server_url;
pub use signal_source_utils::*;
pub use signal_url::{
    SignalFormat, SignalKind, SignalSpec, SignalSpecLength, SweepMode, signal_path,
};
pub use test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder, TestServerHelper};
pub use wav::{create_test_wav, create_wav_exact_bytes};
