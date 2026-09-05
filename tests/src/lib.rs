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
#[cfg(not(target_arch = "wasm32"))]
pub mod analysis_pass;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple_warmup;
#[cfg(not(target_arch = "wasm32"))]
pub mod architecture_trace;
#[cfg(not(target_arch = "wasm32"))]
pub mod asset_fixture;
pub mod assets_ext;
#[cfg(not(target_arch = "wasm32"))]
pub mod audio_artifact;
pub mod audio_mock;
pub use kithara::bufpool::testing as bufpool_ext;
#[cfg(not(target_arch = "wasm32"))]
pub mod cochlea;
pub mod consts;
pub mod decode_ext;
#[cfg(not(target_arch = "wasm32"))]
pub mod decode_mock;
pub mod e2e;
#[cfg(not(target_arch = "wasm32"))]
pub mod encode_ext;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod fixture_cache;
pub mod fixture_protocol;
pub mod fixtures;
#[cfg(not(target_arch = "wasm32"))]
pub mod flash_pace;
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
pub mod packed_audio;
pub mod reads;
pub mod rfc6381;
#[cfg(not(target_arch = "wasm32"))]
pub mod ring;
pub mod rng;
pub mod server_url;
pub mod storage_ext;
#[cfg(not(target_arch = "wasm32"))]
pub mod swallow_detector;
pub mod test_defaults;
pub mod test_server;
pub mod token_store;
/// Scenario machinery for the user-simulation suites: the action vocabulary,
/// the scripted scenarios built from it, and the harness that applies them to a
/// `Queue`. It lives here rather than beside one suite because two suites drive
/// it — the fixture-backed one and the production one — and each uses a
/// different part.
#[cfg(not(target_arch = "wasm32"))]
pub mod user_sim;
#[cfg(not(target_arch = "wasm32"))]
pub mod waits;

pub use abr_fixtures::{abr_fast, abr_initial_mode, abr_switch_trigger};
#[cfg(not(target_arch = "wasm32"))]
pub use assets_ext::disk_asset_store;
pub use assets_ext::memory_asset_store;
pub use fixtures::*;
pub use hls_server::{
    AbrTestServer, EncryptionConfig, HlsTestServer, HlsTestServerConfig, PackagedTestServer,
    TestServer, abr, compat, master_playlist, mixed_codec_ladder, mixed_codec_ladder_encrypted,
    mixed_codec_ladder_url, packaged, packaged_test_server, test_master_playlist,
    test_master_playlist_encrypted, test_master_playlist_with_init, test_media_playlist_encrypted,
    test_segment_data, test_server,
};
pub use hls_url::{
    HlsSpec, encode_hls_spec, hls_init_path, hls_key_path, hls_master_path, hls_media_path,
    hls_segment_path,
};
pub use kithara;
pub use log_filter::rust_log_filter;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
pub use rng::*;
pub use server_url::join_server_url;
#[cfg(not(target_arch = "wasm32"))]
pub use test_server::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, InitGateHandle, PrivateTestServer,
    SegmentGateHandle,
};
pub use test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder, TestServerHelper};
