#![forbid(unsafe_code)]

//! Deterministic cache key helpers for `kithara-hls`.
//!
//! This mirrors the *on-disk layout contract* used by `stream-download-hls`:
//! - playlists: `"<master_hash>/<playlist_basename>"`
//! - keys:      `"<master_hash>/<variant_id>/<key_basename>"`
//! - init segments: `"<master_hash>/<variant_id>/init_<basename>"`
//! - media segments: `"<master_hash>/<variant_id>/seg_<basename>"`
//!
//! Notes:
//! - `master_hash` is computed from the master playlist URL using `ResourceKey::asset_root_for_url`
//!   (SHA-256 truncated to 16 bytes, hex-encoded), stable across runs.
//! - Basename extraction ignores query string.
//! - This module only *derives keys*; it does not perform any I/O.

use kithara_assets::ResourceKey;
use url::Url;

/// Generator for cache keys based on the master URL.
///
/// This is intentionally small and allocation-friendly.
#[derive(Debug, Clone)]
pub struct CacheKeyGenerator {
    master_hash: String,
}

impl CacheKeyGenerator {
    /// Create a new generator from a master playlist URL.
    pub fn new(master_url: &Url) -> Self {
        Self {
            master_hash: ResourceKey::asset_root_for_url(master_url),
        }
    }

    /// Create a new generator from a precomputed master hash.
    pub fn with_master_hash(master_hash: impl Into<String>) -> Self {
        Self {
            master_hash: master_hash.into(),
        }
    }

    /// Returns the master hash used for cache keys.
    pub fn master_hash(&self) -> &str {
        &self.master_hash
    }

    /// Extract basename from a URI-like string ignoring the query string.
    ///
    /// Returns `None` if no basename can be derived.
    fn uri_basename_no_query(uri: &str) -> Option<&str> {
        let no_query = uri.split('?').next().unwrap_or(uri);
        let base = no_query.rsplit('/').next().unwrap_or(no_query);
        if base.is_empty() { None } else { Some(base) }
    }

    /// Construct playlist key rel_path: `"<master_hash>/<playlist_basename>"`.
    pub fn playlist_rel_path_from_url(&self, playlist_url: &Url) -> Option<String> {
        let basename = Self::uri_basename_no_query(playlist_url.as_str())?;
        Some(self.playlist_rel_path_from_basename(basename))
    }

    /// Construct playlist key rel_path from basename: `"<master_hash>/<playlist_basename>"`.
    pub fn playlist_rel_path_from_basename(&self, playlist_basename: &str) -> String {
        format!("{}/{}", self.master_hash, playlist_basename)
    }

    /// Construct key rel_path: `"<master_hash>/<variant_id>/<key_basename>"`.
    pub fn key_rel_path_from_url(&self, variant_id: usize, key_url: &Url) -> Option<String> {
        let basename = Self::uri_basename_no_query(key_url.as_str())?;
        Some(self.key_rel_path_from_basename(variant_id, basename))
    }

    /// Construct key rel_path from basename: `"<master_hash>/<variant_id>/<key_basename>"`.
    pub fn key_rel_path_from_basename(&self, variant_id: usize, key_basename: &str) -> String {
        format!("{}/{}/{}", self.master_hash, variant_id, key_basename)
    }

    /// Construct init segment rel_path: `"<master_hash>/<variant_id>/init_<basename>"`.
    pub fn init_segment_rel_path_from_url(
        &self,
        variant_id: usize,
        init_url: &Url,
    ) -> Option<String> {
        let basename = Self::uri_basename_no_query(init_url.as_str())?;
        Some(self.init_segment_rel_path_from_basename(variant_id, basename))
    }

    /// Construct init segment rel_path from basename: `"<master_hash>/<variant_id>/init_<basename>"`.
    pub fn init_segment_rel_path_from_basename(
        &self,
        variant_id: usize,
        init_basename: &str,
    ) -> String {
        format!("{}/{}/init_{}", self.master_hash, variant_id, init_basename)
    }

    /// Construct media segment rel_path: `"<master_hash>/<variant_id>/seg_<basename>"`.
    pub fn media_segment_rel_path_from_url(
        &self,
        variant_id: usize,
        segment_url: &Url,
    ) -> Option<String> {
        let basename = Self::uri_basename_no_query(segment_url.as_str())?;
        Some(self.media_segment_rel_path_from_basename(variant_id, basename))
    }

    /// Construct media segment rel_path from basename: `"<master_hash>/<variant_id>/seg_<basename>"`.
    pub fn media_segment_rel_path_from_basename(
        &self,
        variant_id: usize,
        segment_basename: &str,
    ) -> String {
        format!(
            "{}/{}/seg_{}",
            self.master_hash, variant_id, segment_basename
        )
    }
}
