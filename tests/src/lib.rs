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

#[cfg(feature = "all")]
pub mod abr_fixtures;
#[cfg(feature = "all")]
pub use abr_fixtures::auto;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod analysis_pass;
#[cfg(all(
    any(feature = "audio", feature = "all"),
    any(target_os = "macos", target_os = "ios")
))]
pub mod apple_warmup;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod architecture_trace;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod asset_fixture;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod assets_ext;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod audio_artifact;
#[cfg(feature = "all")]
pub mod audio_mock;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use kithara::bufpool::testing as bufpool_ext;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod cochlea;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod consts;
#[cfg(feature = "all")]
pub mod decode_ext;
#[cfg(all(
    any(feature = "all", feature = "audio", feature = "wasm"),
    not(target_arch = "wasm32")
))]
pub mod decode_mock;
#[cfg(feature = "all")]
pub mod e2e;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod encode_ext;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod fixture_protocol;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod fixtures;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod flash_pace;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod gapless;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod hls_blob_store;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod hls_fixture;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod hls_server;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod hls_spec;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod hls_test_helpers;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod hls_url;
#[cfg(feature = "all")]
pub mod log_filter;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod memory_source;
#[cfg(all(
    any(feature = "all", feature = "audio", feature = "wasm"),
    not(target_arch = "wasm32")
))]
mod native;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod net_fixture;
#[cfg(any(feature = "all", feature = "wasm"))]
pub mod offline;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod packed_audio;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod reads;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod rfc6381;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod ring;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod rng;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod server_url;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod storage_ext;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod swallow_detector;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod test_defaults;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod test_server;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod token_store;
/// Scenario machinery for the user-simulation suites: the action vocabulary,
/// the scripted scenarios built from it, and the harness that applies them to a
/// `Queue`. It lives here rather than beside one suite because two suites drive
/// it — the fixture-backed one and the production one — and each uses a
/// different part.
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod user_sim;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod waits;

#[cfg(feature = "all")]
pub use abr_fixtures::{abr_fast, abr_initial_mode, abr_switch_trigger};
#[cfg(all(any(feature = "audio", feature = "all"), not(target_arch = "wasm32")))]
pub use assets_ext::disk_asset_store;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use assets_ext::memory_asset_store;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use fixtures::*;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use hls_server::{
    AbrTestServer, EncryptionConfig, HlsTestServer, HlsTestServerConfig, PackagedTestServer,
    TestServer, abr, compat, master_playlist, mixed_codec_ladder, mixed_codec_ladder_encrypted,
    mixed_codec_ladder_url, packaged, packaged_test_server, test_master_playlist,
    test_master_playlist_encrypted, test_master_playlist_with_init, test_media_playlist_encrypted,
    test_segment_data, test_server,
};
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use hls_url::{
    HlsSpec, encode_hls_spec, hls_init_path, hls_key_path, hls_master_path, hls_media_path,
    hls_segment_path,
};
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use kithara;
#[cfg(feature = "all")]
pub use log_filter::rust_log_filter;
#[cfg(all(
    any(feature = "all", feature = "audio", feature = "wasm"),
    not(target_arch = "wasm32")
))]
pub use native::*;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use rng::*;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use server_url::join_server_url;
#[cfg(all(
    any(feature = "all", feature = "audio", feature = "wasm"),
    not(target_arch = "wasm32")
))]
pub use test_server::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, InitGateHandle, PrivateTestServer,
    SegmentGateHandle,
};
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use test_server::{CreateHlsError, CreatedHls, HlsFixtureBuilder, TestServerHelper};

#[cfg(any(feature = "all", feature = "wasm"))]
pub mod event;
