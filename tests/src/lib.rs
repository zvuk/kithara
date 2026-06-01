#![deny(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::impl_trait_in_params,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::option_if_let_else,
    clippy::unwrap_used
)]

pub mod abr_fixtures;
pub use abr_fixtures::auto;
pub mod alac_fixture;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple_warmup;
pub mod asset_fixture;
pub mod assets_ext;
pub mod audio_fixture;
pub mod audio_mock;
pub mod bufpool_ext;
pub mod consts;
pub mod decode_ext;
#[cfg(not(target_arch = "wasm32"))]
pub mod decode_mock;
#[cfg(not(target_arch = "wasm32"))]
pub mod encode_ext;
pub mod encode_test_pcm;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod fixture_cache;
pub mod fixture_protocol;
pub mod fixtures;
pub mod hls_blob_store;
pub mod hls_fixture;
pub mod hls_server;
pub mod hls_spec;
#[cfg(not(target_arch = "wasm32"))]
pub mod hls_test_helpers;
pub mod hls_url;
pub mod log_filter;
pub mod memory_source;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub mod net_fixture;
#[cfg(not(target_arch = "wasm32"))]
pub mod offline;
pub mod rfc6381;
pub mod rng;
pub mod server_url;
pub mod signal_pcm;
pub mod signal_source;
pub mod signal_source_utils;
pub mod signal_spec;
pub mod signal_url;
pub mod storage_ext;
pub mod swallow_detector;
pub mod test_server;
pub mod token_store;
pub mod wav;

pub use abr_fixtures::{abr_fast, abr_initial_mode, abr_switch_trigger};
pub use alac_fixture::ensure_silence_1s_alac_m4a;
pub use fixtures::*;
pub use hls_server::{
    AbrTestServer, EncryptionConfig, HlsTestServer, HlsTestServerConfig, PackagedTestServer,
    TestServer, abr, compat, master_playlist, packaged, packaged_test_server, test_master_playlist,
    test_master_playlist_encrypted, test_master_playlist_with_init, test_media_playlist,
    test_media_playlist_encrypted, test_media_playlist_with_init, test_segment_data, test_server,
};
pub use hls_url::{
    HlsSpec, encode_hls_spec, hls_init_path, hls_key_path, hls_master_path, hls_media_path,
    hls_segment_path,
};
pub use kithara_test_utils::kithara;
pub use log_filter::rust_log_filter;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
pub use rng::*;
pub use server_url::join_server_url;
pub use signal_source_utils::*;
pub use signal_url::{
    SignalFormat, SignalKind, SignalSpec, SignalSpecLength, SweepMode, signal_path,
};
pub use test_server::{
    BehaviorHandle, Content, CreateHlsError, CreatedHls, Delivery, FixtureBehavior,
    HlsFixtureBuilder, TestServerHelper,
};
pub use wav::{create_test_wav, create_wav_exact_bytes};
