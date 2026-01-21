//! FetchLoader: Adapter from FetchManager to Loader trait.

use std::sync::Arc;
use async_trait::async_trait;
use parking_lot::RwLock;
use url::Url;

use super::{Loader, types::SegmentMeta};
use crate::{
    HlsResult, HlsError,
    fetch::{ActiveFetchResult, DefaultFetchManager},
    playlist::{MediaPlaylist, PlaylistManager, VariantId},
};

/// Adapter: FetchManager + PlaylistManager → Loader trait.
///
/// Provides segment loading for CachedLoader by:
/// - Fetching media playlists for variants
/// - Loading segments via FetchManager
/// - Returning SegmentMeta with real lengths after processing
pub struct FetchLoader {
    master_url: Url,
    fetch: Arc<DefaultFetchManager>,
    playlists: Arc<PlaylistManager>,
    /// Cached number of variants (read-only after first load).
    num_variants_cache: RwLock<Option<usize>>,
}

impl FetchLoader {
    pub fn new(
        master_url: Url,
        fetch: Arc<DefaultFetchManager>,
        playlists: Arc<PlaylistManager>,
    ) -> Self {
        Self {
            master_url,
            fetch,
            playlists,
            num_variants_cache: RwLock::new(None),
        }
    }

    /// Load media playlist for variant.
    async fn load_media_playlist(&self, variant: usize) -> HlsResult<(Url, MediaPlaylist)> {
        let master = self.playlists.master_playlist(&self.master_url).await?;

        let variant_stream = master
            .variants
            .get(variant)
            .ok_or_else(|| HlsError::VariantNotFound(format!("variant {}", variant)))?;

        let media_url = self.playlists.resolve_url(&self.master_url, &variant_stream.uri)?;
        let playlist = self
            .playlists
            .media_playlist(&media_url, VariantId(variant))
            .await?;

        Ok((media_url, playlist))
    }
}

#[async_trait]
impl Loader for FetchLoader {
    async fn load_segment(
        &self,
        variant: usize,
        segment_index: usize,
    ) -> HlsResult<SegmentMeta> {
        // Load media playlist for variant
        let (media_url, playlist) = self.load_media_playlist(variant).await?;

        // Get segment from playlist
        let segment = playlist
            .segments
            .get(segment_index)
            .ok_or_else(|| {
                HlsError::SegmentNotFound(format!(
                    "segment {} not found in variant {} playlist",
                    segment_index, variant
                ))
            })?;

        // Resolve segment URL
        let segment_url = media_url
            .join(&segment.uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        // Fetch segment
        let fetch_result = self.fetch.start_fetch(&segment_url).await?;

        // Determine segment length after fetch/decryption
        let segment_len = match fetch_result {
            ActiveFetchResult::Cached { bytes } => bytes,
            ActiveFetchResult::Active(mut active_fetch) => {
                let mut total_bytes = 0u64;

                // Consume all chunks to complete the download
                while let Some(chunk_bytes) = active_fetch.next_chunk().await? {
                    total_bytes += chunk_bytes;
                }

                total_bytes
            }
        };

        // Build SegmentMeta
        Ok(SegmentMeta {
            variant,
            segment_index,
            sequence: segment.sequence,
            url: segment_url,
            duration: Some(segment.duration),
            key: segment.key.clone(),
            len: segment_len,
        })
    }

    fn num_variants(&self) -> usize {
        // Check cache first
        if let Some(cached) = *self.num_variants_cache.read() {
            return cached;
        }

        // Try to get from already-loaded master playlist
        if let Some(variants) = self.playlists.master_variants() {
            let count = variants.len();
            *self.num_variants_cache.write() = Some(count);
            return count;
        }

        // Master playlist not loaded yet - return 0
        // Will be populated when first segment is loaded
        0
    }

    async fn num_segments(&self, variant: usize) -> HlsResult<usize> {
        let (_media_url, playlist) = self.load_media_playlist(variant).await?;
        Ok(playlist.segments.len())
    }
}
