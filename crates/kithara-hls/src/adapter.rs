//! HlsSource: Source adapter for random-access over HLS stream.
//!
//! Single spawn reads from SegmentStream, builds segment index,
//! and provides random-access by reading from cached segment files.

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use kithara_assets::{AssetStore, Assets, ResourceKey};
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{Source, StreamError as KitharaIoError, StreamResult as KitharaIoResult};
use tokio::sync::{Notify, RwLock, broadcast};
use url::Url;

use crate::{
    HlsError,
    events::HlsEvent,
    index::{EncryptionInfo, SegmentIndex},
    playlist::EncryptionMethod,
    stream::{PipelineHandle, SegmentMeta, SegmentStream},
};

/// HLS source for random-access reading.
///
/// Created by `Hls::open()`. Implements `Source` trait for reading segment data
/// and provides event subscription.
pub struct HlsSource {
    index: Arc<RwLock<SegmentIndex>>,
    assets: AssetStore<EncryptionInfo>,
    notify: Arc<Notify>,
    events_tx: broadcast::Sender<HlsEvent>,
    handle: PipelineHandle,
}

impl HlsSource {
    /// Create source from SegmentStream. Single spawn reads stream and builds index.
    pub(crate) fn new(
        handle: PipelineHandle,
        base_stream: SegmentStream,
        assets: AssetStore<EncryptionInfo>,
        events_tx: broadcast::Sender<HlsEvent>,
    ) -> Self {
        let index = Arc::new(RwLock::new(SegmentIndex::new()));
        let notify = Arc::new(Notify::new());

        let index_clone = Arc::clone(&index);
        let notify_clone = Arc::clone(&notify);
        let events_tx_clone = events_tx.clone();

        // Single spawn: reads SegmentStream and builds index.
        // Events are already emitted directly by pipeline into events_tx.
        tokio::spawn(async move {
            let mut stream = Box::pin(base_stream);

            while let Some(item) = stream.next().await {
                match item {
                    Ok(meta) => {
                        let encryption = extract_encryption_info(&meta);
                        let mut idx = index_clone.write().await;
                        idx.add(
                            meta.url,
                            meta.len,
                            meta.variant,
                            meta.segment_index,
                            encryption,
                        );
                        drop(idx);
                        notify_clone.notify_waiters();
                    }
                    Err(e) => {
                        // Error event is already emitted by pipeline.
                        let mut idx = index_clone.write().await;
                        idx.set_error(e.to_string());
                        drop(idx);
                        notify_clone.notify_waiters();
                        break;
                    }
                }
            }

            // Stream finished normally.
            let mut idx = index_clone.write().await;
            if !idx.is_finished() {
                idx.set_finished();
                drop(idx);
                notify_clone.notify_waiters();
                let _ = events_tx_clone.send(HlsEvent::EndOfStream);
            }
        });

        Self {
            index,
            assets,
            notify,
            events_tx,
            handle,
        }
    }

    /// Subscribe to HLS events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events_tx.subscribe()
    }

    /// Get the events sender (for StreamSource implementation).
    pub(crate) fn events_tx(&self) -> &broadcast::Sender<HlsEvent> {
        &self.events_tx
    }

    /// Get pipeline handle for controlling playback.
    pub fn handle(&self) -> &PipelineHandle {
        &self.handle
    }
}

/// Extract encryption info from segment metadata.
fn extract_encryption_info(meta: &SegmentMeta) -> Option<EncryptionInfo> {
    let key = meta.key.as_ref()?;

    if !matches!(key.method, EncryptionMethod::Aes128) {
        return None;
    }

    let key_info = key.key_info.as_ref()?;
    let key_uri = key_info.uri.as_ref()?;

    // Resolve key URL
    let key_url = if key_uri.starts_with("http://") || key_uri.starts_with("https://") {
        Url::parse(key_uri).ok()?
    } else {
        meta.url.join(key_uri).ok()?
    };

    // Derive IV: explicit or from sequence number
    let iv = key_info.iv.unwrap_or_else(|| {
        let mut iv = [0u8; 16];
        iv[8..].copy_from_slice(&meta.sequence.to_be_bytes());
        iv
    });

    Some(EncryptionInfo { key_url, iv })
}

#[async_trait]
impl Source for HlsSource {
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        // Wait for segment to be in index for the current variant.
        let (segment_url, local_range, encryption) = loop {
            let current_var = self.handle.current_variant();

            {
                let idx = self.index.read().await;

                if let Some(e) = idx.error() {
                    return Err(KitharaIoError::Source(HlsError::Driver(e.to_string())));
                }

                // Check if segment covering range.start exists for current variant.
                if let Some(entry) = idx.find(range.start, current_var) {
                    let local_start = range.start - entry.global_start;
                    let local_end =
                        (range.end - entry.global_start).min(entry.global_end - entry.global_start);
                    break (entry.url, local_start..local_end, entry.encryption);
                }

                // Segment not found for current variant.
                // Try to find segment_index hint from any loaded variant.
                let seg_idx_hint = idx.find_segment_index_for_offset(range.start);

                if idx.is_finished() && seg_idx_hint.is_none() {
                    return Ok(WaitOutcome::Eof);
                }

                // If we have a hint, send seek command to pipeline.
                if let Some(seg_idx) = seg_idx_hint {
                    self.handle.seek(seg_idx);
                }
            }

            self.notify.notified().await;
        };

        let key = ResourceKey::from_url(&segment_url);

        if let Some(enc) = encryption {
            // Encrypted: use open_processed
            let ctx = EncryptionInfo {
                key_url: enc.key_url.clone(),
                iv: enc.iv,
            };

            // Emit KeyFetch event.
            let _ = self.events_tx.send(HlsEvent::KeyFetch {
                key_url: enc.key_url.to_string(),
                success: true,
                cached: false,
            });

            let res = self
                .assets
                .open_processed(&key, ctx)
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

            res.wait_range(local_range)
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))
        } else {
            // Not encrypted: use regular streaming resource
            let res = self
                .assets
                .open_streaming_resource(&key)
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

            res.inner()
                .wait_range(local_range)
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))
        }
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> KitharaIoResult<usize, HlsError> {
        let current_var = self.handle.current_variant();

        let (segment_url, local_offset, segment_len, encryption, total_len) = {
            let idx = self.index.read().await;

            if let Some(e) = idx.error() {
                return Err(KitharaIoError::Source(HlsError::Driver(e.to_string())));
            }

            let total = idx.total_len(current_var);
            if offset >= total && total > 0 {
                return Ok(0);
            }

            // Find segment for current variant.
            let entry = match idx.find(offset, current_var) {
                Some(e) => e,
                None => {
                    // Segment not found for current variant - need to trigger seek.
                    // First check if we have a hint from another variant.
                    if let Some(seg_idx) = idx.find_segment_index_for_offset(offset) {
                        drop(idx);
                        self.handle.seek(seg_idx);
                        // Wait for segment to be loaded and retry.
                        self.notify.notified().await;
                        return self.read_at(offset, buf).await;
                    }

                    return Err(KitharaIoError::Source(HlsError::SegmentNotFound(format!(
                        "No segment for offset {} in variant {}",
                        offset, current_var
                    ))));
                }
            };

            let local_offset = offset - entry.global_start;
            let segment_len = entry.global_end - entry.global_start;
            (
                entry.url,
                local_offset,
                segment_len,
                entry.encryption,
                total,
            )
        };

        let key = ResourceKey::from_url(&segment_url);
        let available_in_segment = segment_len.saturating_sub(local_offset);
        let read_len = (buf.len() as u64).min(available_in_segment) as usize;

        let bytes_read = if let Some(enc) = encryption {
            // Encrypted: use open_processed
            let ctx = EncryptionInfo {
                key_url: enc.key_url.clone(),
                iv: enc.iv,
            };

            // Emit KeyFetch event (we don't know if cached, so assume success after open).
            let _ = self.events_tx.send(HlsEvent::KeyFetch {
                key_url: enc.key_url.to_string(),
                success: true,
                cached: false, // We don't track this currently
            });

            let res = self
                .assets
                .open_processed(&key, ctx)
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

            res.read_at(local_offset, &mut buf[..read_len])
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))?
        } else {
            // Not encrypted: use regular streaming resource
            let res = self
                .assets
                .open_streaming_resource(&key)
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

            res.read_at(local_offset, &mut buf[..read_len])
                .await
                .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))?
        };

        // Emit playback progress.
        let new_pos = offset.saturating_add(bytes_read as u64);
        let percent = if total_len > 0 {
            Some(((new_pos as f64 / total_len as f64) * 100.0).min(100.0) as f32)
        } else {
            None
        };
        let _ = self.events_tx.send(HlsEvent::PlaybackProgress {
            position: new_pos,
            percent,
        });

        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        None
    }
}
