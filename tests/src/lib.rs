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

#[cfg(any(feature = "all", feature = "wasm"))]
pub mod abr_fixtures;
#[cfg(any(feature = "all", feature = "wasm"))]
pub use abr_fixtures::auto;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod analysis_pass;
#[cfg(all(
    any(feature = "audio", feature = "all"),
    any(target_os = "macos", target_os = "ios")
))]
pub mod apple_warmup;
#[cfg(all(
    feature = "all",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
pub mod architecture_trace;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod artifact_timeline;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod assets_ext;
#[cfg(all(
    feature = "all",
    not(target_arch = "wasm32"),
    not(target_os = "android")
))]
pub mod audio_artifact;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub use kithara_test_utils::bufpool as bufpool_ext;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod cochlea;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod consts;
#[cfg(all(
    any(feature = "all", feature = "audio", feature = "wasm"),
    not(target_arch = "wasm32")
))]
pub mod decode_mock;
#[cfg(feature = "all")]
pub mod e2e;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod fixture_protocol;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod fixtures;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod gapless;
#[cfg(all(feature = "analysis", not(target_arch = "wasm32")))]
pub mod grid;
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
#[cfg(any(feature = "all", feature = "wasm"))]
pub mod offline;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod output_continuity;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod packed_audio;
#[cfg(all(any(feature = "all", feature = "audio"), not(target_arch = "wasm32")))]
pub mod pcm_oracle;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod phase_continuity;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod reads;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod rfc6381;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod server_url;
#[cfg(all(any(feature = "all", feature = "wasm"), not(target_arch = "wasm32")))]
pub mod smoothing;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod swallow_detector;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod test_defaults;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod test_server;
#[cfg(any(feature = "all", feature = "audio", feature = "wasm"))]
pub mod token_store;
#[cfg(all(feature = "all", not(target_arch = "wasm32")))]
pub mod underrun_ledger;
/// The probe recorder the native suites read. `kithara-test-utils` compiles
/// the USDT module only off wasm, so the re-export follows it there rather
/// than breaking every wasm test binary on an import that cannot resolve.
#[cfg(not(target_arch = "wasm32"))]
pub mod usdt_trace;
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
    abr_binary_ladder, aes128_encryption, aes128_segment, mixed_codec_ladder,
    mixed_codec_ladder_encrypted, mixed_codec_ladder_url, packaged_hls, packaged_ladder,
    packaged_ladder_encrypted, test_pattern_hls, test_pattern_ladder,
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

#[cfg(any(feature = "audio", feature = "wasm"))]
pub mod event;
