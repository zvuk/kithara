#![forbid(unsafe_code)]

//! Playlist fetch, parse, and parsed-state subsystem.

pub(crate) mod keys;
pub(crate) mod master;
pub(crate) mod parse;
pub(crate) mod playlist_cache;
pub(crate) mod state;
pub(crate) mod variant_playlist;

pub use keys::KeyStore;
pub(crate) use keys::{resolve_init_decrypt_ctx, resolve_variant_decrypt_contexts};
pub(crate) use master::MasterPlaylist;
pub use parse::{
    MediaPlaylist, ParsedMaster, VariantId, VariantStream, parse_master_playlist,
    parse_media_playlist,
};
pub use playlist_cache::PlaylistCache;
pub(crate) use state::PlaylistAccess;
pub use state::{PlaylistState, SegmentState, VariantState};
pub(crate) use variant_playlist::load_variant_playlists;
