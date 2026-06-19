#![deny(unsafe_code)]

pub mod config;
pub mod error;

mod coord;
mod ids;
mod invalidation;
mod loading;
mod naming;
mod parsing;
mod peer;
mod playlist;
mod reader;
mod source;
mod stream;
mod variant;

pub use config::{HlsConfig, KeyOptions, SizeProbeMethod};
pub use error::{HlsError, HlsResult};
pub use ids::VariantIndex;
pub use invalidation::HlsStore;
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};
pub use loading::{KeyStore, PlaylistCache};
#[doc(hidden)]
pub use naming::HlsAssetScopeDelegate;
pub use parsing::{
    MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
    parse_media_playlist, variant_info_from_master,
};
pub use playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState};
pub use source::HlsSource;
pub use stream::{Hls, build_shared_asset_store};
