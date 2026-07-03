#![deny(unsafe_code)]

pub mod config;
pub mod error;

mod decrypt_processor;
mod handle;
mod ids;
mod peer;
mod playlist;
mod reader;
mod segment;
mod signal;
mod stream;
mod variant;

pub use config::{HlsConfig, KeyOptions, SizeProbeMethod};
pub use error::{HlsError, HlsResult};
pub use ids::VariantIndex;
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};
pub use kithara_platform::traits::FromWithParams;
pub use playlist::{
    KeyStore, MediaPlaylist, ParsedMaster, PlaylistCache, PlaylistState, SegmentState, VariantId,
    VariantState, VariantStream, parse_master_playlist, parse_media_playlist,
};
pub use stream::{Hls, HlsSource, build_shared_asset_store};
