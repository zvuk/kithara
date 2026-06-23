#![forbid(unsafe_code)]

//! Playlist fetch, parse, and parsed-state subsystem.

pub(crate) mod atomic_fetch;
pub(crate) mod keys;
pub(crate) mod parse;
pub(crate) mod playlist_cache;
pub(crate) mod size_estimation;
pub(crate) mod state;

pub use keys::KeyStore;
pub use parse::{
    MediaPlaylist, ParsedMaster, VariantId, VariantStream, parse_master_playlist,
    parse_media_playlist, variant_info_from_master,
};
pub use playlist_cache::PlaylistCache;
pub(crate) use state::PlaylistAccess;
pub use state::{PlaylistState, SegmentState, VariantSizeMap, VariantState};
