//! HlsSource: Source adapter for random-access over HLS stream.
//!
//! Single spawn reads from SegmentStream, builds segment index,
//! and provides random-access by reading from cached segment files.

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use kithara_assets::{Assets, ResourceKey};
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{Source, StreamError as KitharaIoError, StreamResult as KitharaIoResult};
use tokio::sync::{Notify, RwLock, broadcast};

use crate::{
    HlsError,
    events::HlsEvent,
    fetch::FetchManager,
    index::SegmentIndex,
    stream::SegmentStream,
};

/// HLS source for random-access reading.
///
/// Created by `Hls::open()`. Implements `Source` trait for reading segment data
/// and provides event subscription.
pub struct HlsSource {
    index: Arc<RwLock<SegmentIndex>>,
    fetch: Arc<FetchManager>,
    notify: Arc<Notify>,
    events_tx: broadcast::Sender<HlsEvent>,
}

impl HlsSource {
    /// Create source from SegmentStream. Single spawn reads stream and builds index.
    pub(crate) fn new(
        base_stream: SegmentStream,
        fetch: Arc<FetchManager>,
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
                        let mut idx = index_clone.write().await;
                        idx.add(meta.url, meta.len);
                        drop(idx);
                        notify_clone.notify_waiters();
                    }
                    Err(e) => {
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
            fetch,
            notify,
            events_tx,
        }
    }

    /// Subscribe to HLS events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events_tx.subscribe()
    }
}

#[async_trait]
impl Source for HlsSource {
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        // First wait for segment to be in index.
        let (segment_url, local_range) = loop {
            {
                let idx = self.index.read().await;

                if let Some(e) = idx.error() {
                    return Err(KitharaIoError::Source(HlsError::Driver(e.to_string())));
                }

                // Check if segment covering range.start exists.
                if let Some(segment) = idx.find(range.start) {
                    let local_start = range.start - segment.global_start;
                    let local_end = (range.end - segment.global_start)
                        .min(segment.global_end - segment.global_start);
                    break (segment.url.clone(), local_start..local_end);
                }

                if idx.is_finished() {
                    return Ok(WaitOutcome::Eof);
                }
            }

            self.notify.notified().await;
        };

        // Now delegate to StreamingResource.wait_range for byte-level waiting.
        let key = ResourceKey::from_url(&segment_url);
        let res = self
            .fetch
            .assets()
            .open_streaming_resource(&key)
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

        res.inner()
            .wait_range(local_range)
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> KitharaIoResult<usize, HlsError> {
        let (segment_url, local_offset, segment_len) = {
            let idx = self.index.read().await;

            if let Some(e) = idx.error() {
                return Err(KitharaIoError::Source(HlsError::Driver(e.to_string())));
            }

            if offset >= idx.total_len() {
                return Ok(0);
            }

            let segment = idx.find(offset).ok_or_else(|| {
                KitharaIoError::Source(HlsError::SegmentNotFound(format!(
                    "No segment for offset {}",
                    offset
                )))
            })?;

            let local_offset = offset - segment.global_start;
            let segment_len = segment.global_end - segment.global_start;
            (segment.url.clone(), local_offset, segment_len)
        };

        let key = ResourceKey::from_url(&segment_url);
        let res = self
            .fetch
            .assets()
            .open_streaming_resource(&key)
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

        let available_in_segment = segment_len.saturating_sub(local_offset);
        let read_len = (buf.len() as u64).min(available_in_segment) as usize;

        let bytes_read = res
            .read_at(local_offset, &mut buf[..read_len])
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))?;

        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        None
    }
}
