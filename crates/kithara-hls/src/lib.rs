#![deny(unsafe_code)]

pub mod config;
pub mod error;

mod conv;
mod handle;
mod ids;
mod invalidation;
mod naming;
mod peer;
mod playlist;
mod reader;
mod segment;
mod signal;
mod stream;
mod variant;

pub use config::{HlsConfig, KeyOptions, SizeProbeMethod};
pub use conv::FromWithParams;
pub use error::{HlsError, HlsResult};
pub use ids::VariantIndex;
pub use invalidation::HlsStore;
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};
#[doc(hidden)]
pub use naming::HlsAssetScopeDelegate;
pub use playlist::{
    KeyStore, MediaPlaylist, ParsedMaster, PlaylistCache, PlaylistState, SegmentState, VariantId,
    VariantSizeMap, VariantState, VariantStream, parse_master_playlist, parse_media_playlist,
};
pub use stream::{Hls, HlsSource, build_shared_asset_store};
