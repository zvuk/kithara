#![deny(unsafe_code)]

pub mod config;
pub mod error;

mod decrypt_processor;
mod event;
mod handle;
mod ids;
mod logging;
mod peer;
mod playlist;
mod reader;
mod segment;
mod signal;
mod stream;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod variant;

pub use config::{HlsConfig, HlsConfigPatch, KeyOptions, SizeProbeMethod};
pub use error::{HlsError, HlsResult};
pub use event::{DrmEvent, HlsEvent, HlsFailure, KeyFailureStage, KeySource};
pub use ids::VariantIndex;
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyRequestResolver, PreparedKeyRequest};
pub use kithara_platform::traits::FromWithParams;
pub use playlist::{
    KeyStore, MediaPlaylist, ParsedMaster, PlaylistCache, PlaylistState, SegmentState, VariantId,
    VariantState, VariantStream, parse_master_playlist, parse_media_playlist,
};
pub use stream::Hls;
mod consts;
