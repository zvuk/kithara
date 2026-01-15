#![forbid(unsafe_code)]

//! HLS session and Source adapter.
//!
//! HlsSessionSource reads from BaseStream directly (single spawn), builds segment index,
//! and provides random-access by reading from cached segment files.

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use kithara_assets::{Assets, ResourceKey};
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{Source, StreamError as KitharaIoError, StreamResult as KitharaIoResult};
use tokio::sync::{Notify, RwLock, broadcast};
use url::Url;

use crate::{
    HlsError,
    events::{EventEmitter, HlsEvent},
    fetch::FetchManager,
    options::HlsOptions,
    pipeline::{BaseStream, PipelineEvent, PipelineStream},
    playlist::MasterPlaylist,
};

/// Selects the effective variant index to use for this session.
pub fn select_variant_index(master: &MasterPlaylist, options: &HlsOptions) -> usize {
    if let Some(selector) = &options.variant_stream_selector {
        selector(master).unwrap_or_else(|| options.abr_initial_variant_index.unwrap_or(0))
    } else {
        options.abr_initial_variant_index.unwrap_or(0)
    }
}

/// Entry in segment index: maps global byte range to segment file.
#[derive(Debug, Clone)]
struct SegmentEntry {
    global_start: u64,
    global_end: u64,
    url: Url,
}

/// Segment index state for random-access over HLS stream.
struct SegmentIndex {
    segments: Vec<SegmentEntry>,
    total_len: u64,
    finished: bool,
    error: Option<String>,
}

impl SegmentIndex {
    fn new() -> Self {
        Self {
            segments: Vec::new(),
            total_len: 0,
            finished: false,
            error: None,
        }
    }

    fn add_segment(&mut self, url: Url, len: u64) {
        let global_start = self.total_len;
        let global_end = global_start + len;
        self.segments.push(SegmentEntry {
            global_start,
            global_end,
            url,
        });
        self.total_len = global_end;
    }

    fn find_segment(&self, offset: u64) -> Option<&SegmentEntry> {
        self.segments
            .iter()
            .find(|s| offset >= s.global_start && offset < s.global_end)
    }
}

/// Source adapter: single spawn reads from BaseStream, builds index, forwards events.
pub struct HlsSessionSource {
    index: Arc<RwLock<SegmentIndex>>,
    fetch: Arc<FetchManager>,
    notify: Arc<Notify>,
}

impl HlsSessionSource {
    /// Create source from BaseStream. Single spawn handles everything.
    pub(crate) fn new(
        base_stream: BaseStream,
        fetch: Arc<FetchManager>,
        events: Arc<EventEmitter>,
    ) -> Self {
        let index = Arc::new(RwLock::new(SegmentIndex::new()));
        let notify = Arc::new(Notify::new());

        let index_clone = Arc::clone(&index);
        let notify_clone = Arc::clone(&notify);
        let events_clone = Arc::clone(&events);

        // Single spawn: reads BaseStream, builds index, forwards events.
        tokio::spawn(async move {
            let mut ev_rx = base_stream.event_sender().subscribe();
            let mut stream = Box::pin(base_stream);

            loop {
                tokio::select! {
                    biased;

                    // Forward pipeline events to HlsEvent.
                    ev = ev_rx.recv() => {
                        if let Ok(ev) = ev {
                            match ev {
                                PipelineEvent::VariantApplied { from, to, reason } => {
                                    events_clone.emit_variant_applied(from, to, reason);
                                }
                                PipelineEvent::SegmentReady { variant, segment_index } => {
                                    events_clone.emit_segment_start(variant, segment_index, 0);
                                }
                                PipelineEvent::Decrypted { .. } | PipelineEvent::Prefetched { .. } => {}
                            }
                        }
                    }

                    // Read segments and build index.
                    item = stream.next() => {
                        match item {
                            Some(Ok(meta)) => {
                                let mut idx = index_clone.write().await;
                                idx.add_segment(meta.url, meta.len);
                                drop(idx);
                                notify_clone.notify_waiters();
                            }
                            Some(Err(e)) => {
                                let mut idx = index_clone.write().await;
                                idx.error = Some(e.to_string());
                                idx.finished = true;
                                drop(idx);
                                notify_clone.notify_waiters();
                                break;
                            }
                            None => {
                                let mut idx = index_clone.write().await;
                                idx.finished = true;
                                drop(idx);
                                notify_clone.notify_waiters();
                                events_clone.emit_end_of_stream();
                                break;
                            }
                        }
                    }
                }
            }
        });

        Self { index, fetch, notify }
    }
}

#[async_trait]
impl Source for HlsSessionSource {
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        // First wait for segment to be in index
        let (segment_url, local_range) = loop {
            {
                let idx = self.index.read().await;

                if let Some(ref e) = idx.error {
                    return Err(KitharaIoError::Source(HlsError::Driver(e.clone())));
                }

                // Check if segment covering range.start exists
                if let Some(segment) = idx.find_segment(range.start) {
                    let local_start = range.start - segment.global_start;
                    let local_end = (range.end - segment.global_start)
                        .min(segment.global_end - segment.global_start);
                    break (segment.url.clone(), local_start..local_end);
                }

                if idx.finished {
                    return Ok(WaitOutcome::Eof);
                }
            }

            self.notify.notified().await;
        };

        // Now delegate to StreamingResource.wait_range for byte-level waiting
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

            if let Some(ref e) = idx.error {
                return Err(KitharaIoError::Source(HlsError::Driver(e.clone())));
            }

            if offset >= idx.total_len {
                return Ok(0);
            }

            let segment = idx.find_segment(offset).ok_or_else(|| {
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

// =============================================================================
// HlsSession
// =============================================================================

pub struct HlsSession {
    base_stream: Option<BaseStream>,
    fetch: Arc<FetchManager>,
    events: Arc<EventEmitter>,
}

impl HlsSession {
    pub(crate) fn new(
        base_stream: BaseStream,
        fetch: Arc<FetchManager>,
        events: Arc<EventEmitter>,
    ) -> Self {
        Self {
            base_stream: Some(base_stream),
            fetch,
            events,
        }
    }

    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events.subscribe()
    }

    /// Create Source for random-access reading. Consumes the session.
    pub fn source(mut self) -> HlsSessionSource {
        let base_stream = self.base_stream.take().expect("source() called twice");
        HlsSessionSource::new(base_stream, self.fetch, self.events)
    }
}
