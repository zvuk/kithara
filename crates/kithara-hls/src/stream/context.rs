//! Stream context and helper functions.

use std::{sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;
use url::Url;

use super::types::SegmentMeta;
use crate::{
    HlsError, HlsResult,
    abr::AbrController,
    fetch::FetchManager,
    keys::KeyManager,
    playlist::{MediaPlaylist, MediaSegment, PlaylistManager, VariantId},
};

/// Stream creation context.
pub struct StreamContext {
    pub master_url: Url,
    pub fetch: Arc<FetchManager>,
    pub playlist_manager: Arc<PlaylistManager>,
    pub _key_manager: Option<Arc<KeyManager>>,
    pub abr: AbrController,
    pub cancel: CancellationToken,
}

/// Loaded variant context.
pub struct VariantContext {
    pub media_url: Url,
    pub playlist: MediaPlaylist,
}

/// Load variant context (master + media playlist).
pub async fn load_variant_context(
    playlist_manager: &PlaylistManager,
    master_url: &Url,
    variant_index: usize,
) -> HlsResult<VariantContext> {
    let master = playlist_manager.master_playlist(master_url).await?;

    let variant = master
        .variants
        .get(variant_index)
        .ok_or_else(|| HlsError::VariantNotFound(format!("variant {}", variant_index)))?;

    let media_url = playlist_manager.resolve_url(master_url, &variant.uri)?;
    let playlist = playlist_manager
        .media_playlist(&media_url, VariantId(variant_index))
        .await?;

    Ok(VariantContext {
        media_url,
        playlist,
    })
}

/// Build segment metadata from playlist segment.
pub fn build_segment_meta(
    segment: &MediaSegment,
    media_url: &Url,
    variant: usize,
    idx: usize,
    duration: Duration,
    len: u64,
) -> HlsResult<SegmentMeta> {
    let segment_url = media_url
        .join(&segment.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

    Ok(SegmentMeta {
        variant,
        segment_index: idx,
        sequence: segment.sequence,
        url: segment_url,
        duration: Some(duration),
        key: segment.key.clone(),
        len,
    })
}

/// Fetch init segment for variant.
pub async fn fetch_init_segment(
    fetch: &FetchManager,
    ctx: &VariantContext,
    variant: usize,
) -> HlsResult<SegmentMeta> {
    let init = ctx
        .playlist
        .init_segment
        .as_ref()
        .ok_or_else(|| HlsError::SegmentNotFound("no init segment".to_string()))?;

    let init_url = ctx
        .media_url
        .join(&init.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve init URL: {}", e)))?;

    let fetch_result = fetch.fetch_init(&init_url).await?;

    Ok(SegmentMeta {
        variant,
        segment_index: usize::MAX,
        sequence: ctx.playlist.media_sequence,
        url: init_url,
        duration: Some(fetch_result.duration),
        key: init.key.clone(),
        len: fetch_result.bytes,
    })
}
