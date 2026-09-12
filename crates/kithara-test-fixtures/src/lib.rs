#![forbid(unsafe_code)]

//! Build-time generated audio test assets.
//!
//! Asset declarations live in `src/defs/`, compile only into this crate's build
//! script, and never enter the library. Fixture providers read prepared inputs
//! for test parameters; signal primitives also support assertions and their own
//! tests.

/// Every native accessor reads its prepared entry from the store. An accessor
/// marked `embed` carries its bytes on wasm, where the store is unavailable; the
/// remaining accessors compile only for native targets. The wasm lane reaches
/// those fixtures through `SignalAsset` over HTTP.
pub mod asset;
pub mod assets;
pub mod fixtures;
#[cfg(not(target_arch = "wasm32"))]
pub mod hls;
#[cfg(all(test, not(target_arch = "wasm32")))]
use hls::hydrate as hls_hydrate;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use hls::manifest as hls_manifest;
/// Shared build support is declared here for its unit tests.
#[cfg(test)]
mod context;
/// The gapless request shape is shared with wasm; the native-only fMP4 muxer is
/// gated inside the module with the encoder types it consumes.
pub mod fmp4;
#[cfg(test)]
mod graph;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod remote_file;
pub mod signal;
pub mod signal_asset;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;

pub use signal_asset::SignalAsset;

#[cfg(not(target_arch = "wasm32"))]
pub mod variant_input;

#[cfg(not(target_arch = "wasm32"))]
pub use fixtures::hls as hls_fixtures;
pub use fixtures::{
    analysis as analysis_fixtures, beat as analysis_beat_fixtures,
    integration as integration_fixtures, mock as mock_fixtures, play as play_fixtures,
    stretch as stretch_fixtures, unit as unit_fixtures,
};
