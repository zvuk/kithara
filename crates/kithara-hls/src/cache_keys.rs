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
//! - `master_hash` is computed from the master playlist URL using `DefaultHasher`,
//!   matching legacy behavior. This is deterministic within a Rust version, but not
//!   guaranteed stable across Rust versions.
//! - Basename extraction ignores query string.
//! - This module only *derives keys*; it does not perform any I/O.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use url::Url;

/// Computes a deterministic identifier for a stream from the master playlist URL.
///
/// NOTE: uses the standard library hasher, so the result is not guaranteed to be stable across Rust versions.
pub fn master_hash_from_url(url: &Url) -> String {
    let mut hasher = DefaultHasher::new();
    url.as_str().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

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
            master_hash: master_hash_from_url(master_url),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_basename_no_query_works() {
        assert_eq!(
            CacheKeyGenerator::uri_basename_no_query("https://a/b/master.m3u8?token=1"),
            Some("master.m3u8")
        );
        assert_eq!(
            CacheKeyGenerator::uri_basename_no_query("https://a/b/"),
            None
        );
        assert_eq!(
            CacheKeyGenerator::uri_basename_no_query("master.m3u8?x=1"),
            Some("master.m3u8")
        );
    }

    #[test]
    fn rel_path_formats_match_contract() {
        let master = Url::parse("https://example.com/hls/master.m3u8").unwrap();
        let g = CacheKeyGenerator::new(&master);

        let playlist = Url::parse("https://x/y/index.m3u8?z=1").unwrap();
        let key = Url::parse("https://x/y/key.bin?z=1").unwrap();
        let init = Url::parse("https://x/y/init.mp4?z=1").unwrap();
        let seg = Url::parse("https://x/y/seg_0001.ts?z=1").unwrap();

        let mh = g.master_hash().to_string();

        assert_eq!(
            g.playlist_rel_path_from_url(&playlist).unwrap(),
            format!("{}/index.m3u8", mh)
        );
        assert_eq!(
            g.key_rel_path_from_url(3, &key).unwrap(),
            format!("{}/3/key.bin", mh)
        );
        assert_eq!(
            g.init_segment_rel_path_from_url(3, &init).unwrap(),
            format!("{}/3/init_init.mp4", mh)
        );
        assert_eq!(
            g.media_segment_rel_path_from_url(3, &seg).unwrap(),
            format!("{}/3/seg_seg_0001.ts", mh)
        );
    }
}
