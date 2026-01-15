//! Stream context and helper functions.

use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    commands::SwitchDecision,
    types::{PipelineEvent, SegmentMeta},
};
use crate::{
    HlsError, HlsResult,
    abr::{AbrController, ThroughputSample, Variant},
    fetch::{DownloadContext, FetchManager},
    keys::KeyManager,
    playlist::{MediaPlaylist, MediaSegment, PlaylistManager, VariantId},
};

/// Stream creation context.
pub struct StreamContext {
    pub master_url: Url,
    pub fetch: Arc<FetchManager>,
    pub playlist_manager: Arc<PlaylistManager>,
    pub key_manager: Option<Arc<KeyManager>>,
    pub abr: AbrController,
    pub cancel: CancellationToken,
    pub throughput_rx: mpsc::Receiver<ThroughputSample>,
}

/// Loaded variant context.
pub struct VariantContext {
    pub media_url: Url,
    pub playlist: MediaPlaylist,
}

/// Result of streaming segment preparation.
pub struct StreamingSegmentResult {
    pub meta: SegmentMeta,
    pub download: Option<DownloadContext>,
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

    let fetch_meta = fetch.download_streaming(&init_url).await?;

    Ok(SegmentMeta {
        variant,
        segment_index: usize::MAX,
        sequence: ctx.playlist.media_sequence,
        url: init_url,
        duration: Some(fetch_meta.duration),
        key: init.key.clone(),
        len: fetch_meta.len,
    })
}

/// Prepare segment for streaming (get expected length, don't wait for download).
pub async fn prepare_segment_streaming(
    fetch: &FetchManager,
    segment: &MediaSegment,
    ctx: &VariantContext,
    variant: usize,
    idx: usize,
    _key_manager: &Option<Arc<KeyManager>>,
    _events: &broadcast::Sender<PipelineEvent>,
) -> HlsResult<StreamingSegmentResult> {
    let segment_url = ctx
        .media_url
        .join(&segment.uri)
        .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

    let prepare_result = fetch.prepare_streaming(&segment_url).await?;
    let expected_len = prepare_result.expected_len.unwrap_or(0);

    let duration = segment.duration;
    let meta = build_segment_meta(
        segment,
        &ctx.media_url,
        variant,
        idx,
        duration,
        expected_len,
    )?;

    // Note: Decryption is handled by KeyManager::decrypt() when reading the segment.
    // The decrypted event will be emitted by the consumer when decryption is performed.

    Ok(StreamingSegmentResult {
        meta,
        download: prepare_result.download,
    })
}

/// Process pending throughput samples from background downloads.
pub fn process_throughput_samples(
    throughput_rx: &mut mpsc::Receiver<ThroughputSample>,
    abr: &mut AbrController,
    variants: &[Variant],
    events: &broadcast::Sender<PipelineEvent>,
    last_segment_index: usize,
) -> Option<SwitchDecision> {
    let mut switch = None;
    while let Ok(sample) = throughput_rx.try_recv() {
        let duration = sample.duration;
        abr.push_throughput_sample(sample);

        let decision = abr.decide(variants, duration.as_secs_f64(), std::time::Instant::now());
        if decision.changed {
            let from = abr.current_variant().load(Ordering::Acquire);
            abr.apply(&decision, std::time::Instant::now());
            let _ = events.send(PipelineEvent::VariantApplied {
                from,
                to: decision.target_variant_index,
                reason: decision.reason,
            });
            let next_segment = if last_segment_index == usize::MAX {
                0
            } else {
                last_segment_index.saturating_add(1)
            };
            switch = Some(SwitchDecision {
                from,
                to: decision.target_variant_index,
                start_segment: next_segment,
                reason: decision.reason,
            });
        }
    }
    switch
}

/// Drive download to completion and send throughput sample.
pub async fn drive_download_and_report(
    fetch: &FetchManager,
    download_ctx: DownloadContext,
    throughput_tx: &mpsc::Sender<ThroughputSample>,
) {
    let sample = fetch.drive_download(download_ctx).await;
    let _ = throughput_tx.send(sample).await;
}
