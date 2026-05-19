#![deny(unsafe_code)]

pub mod config;
pub mod error;

mod coord;
mod ids;
mod loading;
mod parsing;
mod peer;
mod playlist;
mod reader;
mod source;
mod stream;
mod variant;

pub use config::{HlsConfig, KeyOptions};
pub use error::{HlsError, HlsResult};
pub use ids::VariantIndex;
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};
pub use loading::{KeyManager, PlaylistCache};
pub use parsing::{
    MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
    parse_media_playlist, variant_info_from_master,
};
pub use playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState};
pub use source::HlsSource;
pub use stream::Hls;
