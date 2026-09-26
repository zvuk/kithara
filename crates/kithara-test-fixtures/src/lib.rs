#![forbid(unsafe_code)]

//! Build-time generated audio test assets.
//!
//! Asset declarations live in `src/defs/`, compile only into this crate's build
//! script, and never enter the library. Fixture providers read prepared inputs
//! for test parameters; signal primitives also support assertions and their own
//! tests.

/// Native fixture generation is opt-in through `native-fixtures`: every native
/// accessor reads its prepared entry from the host store, which the browser
/// cannot reach. Without the feature only the portable signal naming compiles,
/// and the wasm lane reaches fixtures through `SignalAsset` over HTTP.
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub mod asset;
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub mod assets;
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub mod fixtures;
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub mod hls;
#[cfg(all(test, feature = "library", not(target_arch = "wasm32")))]
use hls::hydrate as hls_hydrate;
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub(crate) use hls::manifest as hls_manifest;
/// Shared build support is declared here for its unit tests. The hydrator,
/// the verified download and the build context are one chain, and `library` is
/// the family that carries all of it.
#[cfg(all(test, feature = "library", not(target_arch = "wasm32")))]
mod context;
/// The gapless request shape is shared with wasm; the native-only fMP4 muxer is
/// gated inside the module with the encoder types it consumes.
pub mod fmp4;
#[cfg(all(test, feature = "native-fixtures", not(target_arch = "wasm32")))]
mod graph;
/// The two shapes an MP3 fixture takes: as encoded, and with its Xing/Info
/// frame dropped so the byte length is the only record of duration.
pub mod mp3;
#[cfg(all(test, feature = "library", not(target_arch = "wasm32")))]
mod remote_file;
pub mod signal;
pub mod signal_asset;
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub mod store;

pub use mp3::{Mp3Shape, headerless_bitrate_change, without_xing_frame};
pub use signal_asset::SignalAsset;

#[cfg(all(feature = "hls-inputs", not(target_arch = "wasm32")))]
pub mod variant_input;

#[cfg(all(feature = "hls-inputs", not(target_arch = "wasm32")))]
pub use fixtures::hls as hls_fixtures;
#[cfg(all(feature = "native-fixtures", not(target_arch = "wasm32")))]
pub use fixtures::{
    analysis as analysis_fixtures, beat as analysis_beat_fixtures,
    integration as integration_fixtures, mock as mock_fixtures, play as play_fixtures,
    stretch as stretch_fixtures, unit as unit_fixtures,
};
