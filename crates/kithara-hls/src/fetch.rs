#![forbid(unsafe_code)]

use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_assets::{
    AssetResource, AssetStore, DiskAssetStore, EvictAssets, LeaseGuard, ResourceKey,
};
use kithara_net::{Headers, HttpClient, Net as _};
use kithara_storage::{
    Resource as _, ResourceStatus, StreamingResource, StreamingResourceExt, WaitOutcome,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};
use url::Url;

use crate::{
    HlsError, HlsEvent, HlsResult, KeyContext, abr::ThroughputSampleSource, playlist::MediaPlaylist,
};

pub type SegmentStream<'a> = Pin<Box<dyn Stream<Item = HlsResult<Bytes>> + Send + 'a>>;

pub type StreamingAssetResource =
    AssetResource<StreamingResource, LeaseGuard<EvictAssets<DiskAssetStore>>>;

#[derive(Clone, Debug)]
pub struct FetchBytes {
    pub bytes: Bytes,
    pub source: ThroughputSampleSource,
    pub duration: Duration,
}

#[derive(Clone)]
pub struct FetchManager {
    asset_root: String,
    assets: AssetStore,
    net: HttpClient,
}

impl FetchManager {
    pub fn new(asset_root: String, assets: AssetStore, net: HttpClient) -> Self {
        Self {
            asset_root,
            assets,
            net,
        }
    }

    pub fn net(&self) -> &HttpClient {
        &self.net
    }

    pub async fn probe_content_length(&self, url: &Url) -> HlsResult<Option<u64>> {
        let headers: Headers = self.net.head(url.clone(), None).await?;

        // Helper for case-insensitive lookup
        fn find_content_length(headers: &Headers) -> Option<u64> {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<u64>().ok())
        }

        Ok(find_content_length(&headers))
    }

    pub async fn fetch_segment(
        &self,
        media_playlist: &MediaPlaylist,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<Bytes> {
        let segment = media_playlist
            .segments
            .get(segment_index)
            .ok_or_else(|| HlsError::SegmentNotFound(format!("Index {}", segment_index)))?;

        let segment_uri = &segment.uri;
        let segment_url = base_url
            .join(segment_uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            base_url = %base_url,
            segment_uri = %segment_uri,
            segment_url = %segment_url,
            "kithara-hls fetch_segment begin"
        );

        if key_context.is_some() {
            return Err(HlsError::Unimplemented);
        }

        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &segment_url);
        let fetch = Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            &segment_url,
            &key.rel_path(),
            Some(segment_index),
        )
        .await?;

        let out = fetch.bytes;

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            bytes = out.len(),
            "kithara-hls fetch_segment done"
        );

        Ok(out)
    }

    pub async fn fetch_segment_with_meta(
        &self,
        media_playlist: &MediaPlaylist,
        segment_index: usize,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> HlsResult<FetchBytes> {
        let segment = media_playlist
            .segments
            .get(segment_index)
            .ok_or_else(|| HlsError::SegmentNotFound(format!("Index {}", segment_index)))?;

        let segment_uri = &segment.uri;
        let segment_url = base_url
            .join(segment_uri)
            .map_err(|e| HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)))?;

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            base_url = %base_url,
            segment_uri = %segment_uri,
            segment_url = %segment_url,
            "kithara-hls fetch_segment_with_meta begin"
        );

        if key_context.is_some() {
            return Err(HlsError::Unimplemented);
        }

        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &segment_url);
        let mut fetch = Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            &segment_url,
            &key.rel_path(),
            Some(segment_index),
        )
        .await?;

        trace!(
            asset_root = %self.asset_root,
            segment_index,
            bytes = fetch.bytes.len(),
            duration_ms = fetch.duration.as_millis(),
            "kithara-hls fetch_segment_with_meta done"
        );

        Ok(fetch)
    }

    pub async fn fetch_init_segment(&self, init_url: &Url) -> HlsResult<Bytes> {
        trace!(
            asset_root = %self.asset_root,
            init_url = %init_url,
            variant_id = 0usize,
            "kithara-hls fetch_init_segment begin"
        );
        let out = self.fetch_init_segment_resource(0, init_url).await?.bytes;
        trace!(
            asset_root = %self.asset_root,
            init_url = %init_url,
            variant_id = 0usize,
            bytes = out.len(),
            "kithara-hls fetch_init_segment done"
        );
        Ok(out)
    }

    pub async fn fetch_init_segment_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &url);

        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            rel_path = %key.rel_path(),
            "kithara-hls fetch_init_segment_resource"
        );

        Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            url,
            &key.rel_path(),
            None,
        )
        .await
    }

    pub async fn fetch_media_segment_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &url);

        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            rel_path = %key.rel_path(),
            "kithara-hls fetch_media_segment_resource"
        );

        Self::fetch_streaming_to_bytes_internal(
            &self.asset_root,
            &self.assets,
            &self.net,
            url,
            &key.rel_path(),
            None,
        )
        .await
    }

    pub async fn open_init_streaming_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<StreamingAssetResource> {
        self.open_init_streaming_resource_with_events(variant_id, url, None)
            .await
    }

    pub async fn open_init_streaming_resource_with_events(
        &self,
        variant_id: usize,
        url: &Url,
        events: Option<broadcast::Sender<HlsEvent>>,
    ) -> HlsResult<StreamingAssetResource> {
        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            "kithara-hls open_init_streaming_resource"
        );
        self.open_streaming_resource_with_writer(url, variant_id, events, None)
            .await
    }

    pub async fn open_media_streaming_resource(
        &self,
        variant_id: usize,
        url: &Url,
    ) -> HlsResult<StreamingAssetResource> {
        self.open_media_streaming_resource_with_events(variant_id, url, None, None)
            .await
    }

    pub async fn open_media_streaming_resource_with_events(
        &self,
        variant_id: usize,
        url: &Url,
        events: Option<broadcast::Sender<HlsEvent>>,
        segment_index: Option<usize>,
    ) -> HlsResult<StreamingAssetResource> {
        trace!(
            asset_root = %self.asset_root,
            variant_id,
            url = %url,
            "kithara-hls open_media_streaming_resource"
        );

        self.open_streaming_resource_with_writer(url, variant_id, events, segment_index)
            .await
    }

    async fn open_streaming_resource_with_writer(
        &self,
        url: &Url,
        variant_id: usize,
        events: Option<broadcast::Sender<HlsEvent>>,
        segment_index: Option<usize>,
    ) -> HlsResult<StreamingAssetResource> {
        let key = ResourceKey::from_url_with_asset_root(self.asset_root.clone(), &url);
        let cancel = CancellationToken::new();
        let res = self
            .assets
            .open_streaming_resource(&key, cancel.clone())
            .await?;

        let status = res.inner().status().await;

        // Spawn best-effort writer (net -> storage). Readers will observe availability via the same handle.
        if matches!(status, ResourceStatus::Committed { .. }) {
            return Ok(res);
        }

        let net = self.net.clone();
        let url = url.clone();
        let res_for_writer = res.clone();
        let events_for_writer = events.clone();
        let content_length_for_progress = self.probe_content_length(&url).await.ok().flatten();

        Self::spawn_stream_writer(
            net,
            url,
            res_for_writer,
            events_for_writer,
            variant_id,
            segment_index,
            content_length_for_progress,
        );

        Ok(res)
    }

    pub fn stream_segment_sequence(
        &self,
        media_playlist: MediaPlaylist,
        base_url: &Url,
        key_context: Option<&KeyContext>,
    ) -> SegmentStream<'static> {
        let asset_root = self.asset_root.clone();
        let assets = self.assets.clone();
        let net = self.net.clone();
        let base_url = base_url.clone();
        let key_ctx = key_context.cloned();

        Box::pin(async_stream::stream! {
            if key_ctx.is_some() {
                yield Err(HlsError::Unimplemented);
                return;
            }

            for (segment_index, segment) in media_playlist.segments.into_iter().enumerate() {
                let segment_url = match base_url.join(&segment.uri) {
                    Ok(url) => url,
                    Err(e) => {
                        yield Err(HlsError::InvalidUrl(format!("Failed to resolve segment URL: {}", e)));
                        continue;
                    }
                };

                // Use captured fields directly instead of creating new FetchManager
                let key = ResourceKey::from_url_with_asset_root(asset_root.clone(), &segment_url);
                let fetch_result = Self::fetch_streaming_to_bytes_internal(
                    &asset_root, &assets, &net, &segment_url, &key.rel_path(), Some(segment_index)
                ).await;

                match fetch_result {
                    Ok(fetch) => {
                        yield Ok(fetch.bytes);
                    }
                    Err(e) => yield Err(e),
                }
            }
        })
    }

    fn spawn_stream_writer(
        net: HttpClient,
        url: Url,
        res: StreamingAssetResource,
        events: Option<broadcast::Sender<HlsEvent>>,
        variant_id: usize,
        segment_index: Option<usize>,
        content_length_for_progress: Option<u64>,
    ) {
        let started_at = Instant::now();

        tokio::spawn(async move {
            if let (Some(ev), Some(segment_index)) = (&events, segment_index) {
                let _: Result<_, broadcast::error::SendError<HlsEvent>> =
                    ev.send(HlsEvent::SegmentStart {
                        variant: variant_id,
                        segment_index,
                        byte_offset: 0,
                    });
            }

            let mut stream = match net.stream(url.clone(), None).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(url = %url, error = %e, "kithara-hls streaming writer: net open error");
                    let _ = res.fail(format!("net error: {e}")).await;
                    return;
                }
            };

            let mut off: u64 = 0;
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk_bytes) => {
                        let chunk_len = chunk_bytes.len() as u64;
                        if let Err(e) = res.write_at(off, &chunk_bytes).await {
                            warn!(
                                url = %url,
                                off,
                                error = %e,
                                "kithara-hls streaming writer: storage write_at error"
                            );
                            let _ = res.fail(format!("storage write_at error: {e}")).await;
                            return;
                        }
                        off = off.saturating_add(chunk_len);
                        if let Some(ev) = &events {
                            let percent = content_length_for_progress
                                .map(|len| ((off as f64 / len as f64) * 100.0).min(100.0) as f32);
                            let _: Result<_, broadcast::error::SendError<HlsEvent>> =
                                ev.send(HlsEvent::DownloadProgress {
                                    offset: off,
                                    percent,
                                });
                        }
                    }
                    Err(e) => {
                        warn!(
                            url = %url,
                            off,
                            error = %e,
                            "kithara-hls streaming writer: net stream error"
                        );
                        let _ = res.fail(format!("net stream error: {e}")).await;
                        return;
                    }
                }
            }

            let _ = res.commit(Some(off)).await;

            if let (Some(ev), Some(segment_index)) = (&events, segment_index) {
                let _: Result<_, broadcast::error::SendError<HlsEvent>> =
                    ev.send(HlsEvent::SegmentComplete {
                        variant: variant_id,
                        segment_index,
                        bytes_transferred: off,
                        duration: started_at.elapsed(),
                    });
            }
        });
    }

    // Helper for stream_segment_sequence to avoid creating new FetchManager
    async fn fetch_streaming_to_bytes_internal(
        asset_root: &str,
        assets: &AssetStore,
        net: &HttpClient,
        url: &Url,
        rel_path: &str,
        segment_index: Option<usize>,
    ) -> HlsResult<FetchBytes> {
        let key = ResourceKey::from_url_with_asset_root(asset_root.to_string(), url);

        // Intentionally quiet: this path can be called a lot during playback.
        // Keep only warnings/errors and a single "done" summary.

        let cancel = CancellationToken::new();
        let res = assets.open_streaming_resource(&key, cancel.clone()).await?;

        // Spawn a best-effort background writer for this segment.
        // If multiple callers race, extra writers may occur; the resource contract handles `Sealed`
        // and failure propagation.
        let net = net.clone();
        let url = url.clone();

        let status = res.inner().status().await;
        if !matches!(status, ResourceStatus::Committed { .. }) {
            Self::spawn_stream_writer(net, url, res.clone(), None, 0, segment_index, None);
        }

        // Read-through to bytes: wait for full content once committed, then read all.
        let start = Instant::now();
        let mut offset: u64 = 0;
        let mut out = Vec::new();
        let read_chunk: u64 = 64 * 1024;

        loop {
            let end = offset.saturating_add(read_chunk);

            let outcome = res.wait_range(offset..end).await?;

            match outcome {
                WaitOutcome::Ready => {
                    let len: usize = (end - offset) as usize;
                    let bytes = res.read_at(offset, len).await?;

                    if bytes.is_empty() {
                        // Storage returned empty after reporting Ready: this is unexpected and can
                        // lead to "end of stream" at the decoder level.
                        warn!(
                            asset_root = %asset_root,
                            rel_path = %rel_path,
                            offset,
                            "kithara-hls segment reader: empty-after-ready"
                        );
                        break;
                    }
                    offset = offset.saturating_add(bytes.len() as u64);
                    out.extend_from_slice(&bytes);
                }
                WaitOutcome::Eof => {
                    break;
                }
            }
        }

        let duration = start.elapsed();
        trace!(
            asset_root = %asset_root,
            rel_path = %rel_path,
            bytes = out.len(),
            duration_ms = duration.as_millis(),
            "kithara-hls fetch_streaming_to_bytes done"
        );

        Ok(FetchBytes {
            bytes: Bytes::from(out),
            source: ThroughputSampleSource::Network,
            duration,
        })
    }
}
