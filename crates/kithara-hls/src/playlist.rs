//! Playlist fetching and caching.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use tokio::sync::OnceCell;
use url::Url;

// Re-export parsing types and functions for external use
pub use crate::parsing::{
    CodecInfo, ContainerFormat, EncryptionMethod, InitSegment, KeyInfo, MasterPlaylist,
    MediaPlaylist, MediaSegment, SegmentKey, VariantId, VariantStream, parse_master_playlist,
    parse_media_playlist,
};
use kithara_abr::{Variant, VariantInfo, VariantSource};

use crate::{HlsError, HlsResult, fetch::DefaultFetchManager};

fn uri_basename_no_query(uri: &str) -> Option<&str> {
    let no_query = uri.split('?').next().unwrap_or(uri);
    let base = no_query.rsplit('/').next().unwrap_or(no_query);
    if base.is_empty() { None } else { Some(base) }
}

/// Convert HLS master playlist variants to ABR variant list.
pub fn variants_from_master(master: &MasterPlaylist) -> Vec<Variant> {
    master
        .variants
        .iter()
        .map(|v| Variant {
            variant_index: v.id.0,
            bandwidth_bps: v.bandwidth.unwrap_or(0),
        })
        .collect()
}

/// Extract extended variant metadata from master playlist.
///
/// This creates `VariantInfo` structures with all available metadata
/// for each variant, including bandwidth, name, codecs, and container format.
pub fn variant_info_from_master(master: &MasterPlaylist) -> Vec<VariantInfo> {
    master
        .variants
        .iter()
        .map(|v| VariantInfo {
            index: v.id.0,
            bandwidth_bps: v.bandwidth,
            name: v.name.clone(),
            codecs: v.codec.as_ref().and_then(|c| c.codecs.clone()),
            container: v
                .codec
                .as_ref()
                .and_then(|c| c.container)
                .map(|fmt| format!("{:?}", fmt)),
        })
        .collect()
}

/// Implement VariantSource for MasterPlaylist.
///
/// This allows ABR controller to work directly with HLS master playlists
/// without intermediate Vec<Variant> allocation.
impl VariantSource for MasterPlaylist {
    fn variant_count(&self) -> usize {
        self.variants.len()
    }

    fn variant_bandwidth(&self, index: usize) -> Option<u64> {
        self.variants
            .iter()
            .find(|v| v.id.0 == index)
            .and_then(|v| v.bandwidth)
    }
}

/// Thin wrapper: fetches + parses playlists with caching via `kithara-assets`.
#[derive(Clone)]
pub struct PlaylistManager {
    fetch: Arc<DefaultFetchManager>,
    base_url: Option<Url>,
    master: Arc<OnceCell<MasterPlaylist>>,
    variants: Arc<OnceCell<Vec<Variant>>>,
    media: Arc<RwLock<HashMap<VariantId, Arc<OnceCell<MediaPlaylist>>>>>,
}

impl PlaylistManager {
    pub fn new(fetch: Arc<DefaultFetchManager>, base_url: Option<Url>) -> Self {
        Self {
            fetch,
            base_url,
            master: Arc::new(OnceCell::new()),
            variants: Arc::new(OnceCell::new()),
            media: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn master_playlist(&self, url: &Url) -> HlsResult<MasterPlaylist> {
        let master = self
            .master
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "master_playlist", parse_master_playlist)
                    .await
            })
            .await?;

        Ok(master.clone())
    }

    pub async fn variants(&self, url: &Url) -> HlsResult<Vec<Variant>> {
        if let Some(cached) = self.variants.get() {
            return Ok(cached.clone());
        }

        let computed = {
            let master = self.master_playlist(url).await?;
            variants_from_master(&master)
        };

        let _ = self.variants.set(computed.clone());
        Ok(computed)
    }

    pub fn master_variants(&self) -> Option<Vec<VariantStream>> {
        self.master.get().map(|m| m.variants.clone())
    }

    pub async fn media_playlist(
        &self,
        url: &Url,
        variant_id: VariantId,
    ) -> HlsResult<MediaPlaylist> {
        let cell = {
            let mut guard = self
                .media
                .write()
                .map_err(|_| HlsError::PlaylistParse("playlist cache poisoned".to_string()))?;
            guard
                .entry(variant_id)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let playlist = cell
            .get_or_try_init(|| async {
                self.fetch_and_parse(url, "media_playlist", |bytes| {
                    parse_media_playlist(bytes, variant_id)
                })
                .await
            })
            .await?;

        Ok(playlist.clone())
    }

    pub fn resolve_url(&self, base: &Url, target: &str) -> HlsResult<Url> {
        let resolved = if let Some(ref base_url) = self.base_url {
            base_url.join(target).map_err(|e| {
                HlsError::InvalidUrl(format!("Failed to resolve URL with base override: {e}"))
            })?
        } else {
            base.join(target)
                .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve URL: {e}")))?
        };

        Ok(resolved)
    }

    async fn fetch_and_parse<T, F>(&self, url: &Url, label: &str, parse: F) -> HlsResult<T>
    where
        F: Fn(&[u8]) -> HlsResult<T>,
    {
        let basename = uri_basename_no_query(url.as_str())
            .ok_or_else(|| HlsError::InvalidUrl(format!("Failed to derive {label} basename")))?;
        let bytes = Arc::clone(&self.fetch)
            .fetch_playlist(url, basename)
            .await?;

        parse(&bytes)
    }
}
