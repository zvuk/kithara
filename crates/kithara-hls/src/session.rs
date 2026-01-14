#![forbid(unsafe_code)]

//! `kithara-stream::io::Source` adapter for HLS.
//!
//! HlsSessionSource builds a segment index from driver.stream() and provides random-access
//! by reading directly from cached segment files (via FetchManager/AssetStore).
//! Data flows: driver.stream() → segment index → wait_range/read_at → file read → SyncReader.

use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use kithara_assets::ResourceKey;
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{Source, StreamError as KitharaIoError, StreamResult as KitharaIoResult};
use tokio::sync::{Notify, RwLock, broadcast};
use url::Url;

use crate::{
    HlsError, driver::HlsDriver, events::HlsEvent, fetch::FetchManager,
    options::HlsOptions, pipeline::SegmentPayload, playlist::MasterPlaylist,
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
    /// Start offset in the global stream.
    global_start: u64,
    /// End offset in the global stream (exclusive).
    global_end: u64,
    /// Segment URL for ResourceKey lookup.
    url: Url,
}

/// Segment index state for random-access over HLS stream.
struct SegmentIndex {
    /// Segments in order (sorted by global_start).
    segments: Vec<SegmentEntry>,
    /// Current total length (grows as segments arrive).
    total_len: u64,
    /// True when stream has ended.
    finished: bool,
    /// Error message if stream failed.
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

    /// Find segment containing the given global offset.
    fn find_segment(&self, offset: u64) -> Option<&SegmentEntry> {
        self.segments
            .iter()
            .find(|s| offset >= s.global_start && offset < s.global_end)
    }
}

/// Source adapter that reads from cached segment files via FetchManager.
pub struct HlsSessionSource {
    index: Arc<RwLock<SegmentIndex>>,
    fetch: Arc<FetchManager>,
    notify: Arc<Notify>,
}

impl HlsSessionSource {
    /// Create a new source that reads from the given driver's stream.
    pub fn new(driver: HlsDriver, fetch: Arc<FetchManager>) -> Self {
        let index = Arc::new(RwLock::new(SegmentIndex::new()));
        let notify = Arc::new(Notify::new());

        let index_clone = Arc::clone(&index);
        let notify_clone = Arc::clone(&notify);

        tokio::spawn(async move {
            let mut stream = Box::pin(driver.stream());

            while let Some(result) = stream.next().await {
                match result {
                    Ok(payload) => {
                        let SegmentPayload { meta, bytes } = payload;
                        let len = bytes.len() as u64;

                        let mut idx = index_clone.write().await;
                        idx.add_segment(meta.url, len);
                        drop(idx);
                        notify_clone.notify_waiters();
                    }
                    Err(e) => {
                        let mut idx = index_clone.write().await;
                        idx.error = Some(e.to_string());
                        idx.finished = true;
                        drop(idx);
                        notify_clone.notify_waiters();
                        return;
                    }
                }
            }

            let mut idx = index_clone.write().await;
            idx.finished = true;
            drop(idx);
            notify_clone.notify_waiters();
        });

        Self { index, fetch, notify }
    }
}

#[async_trait]
impl Source for HlsSessionSource {
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome, HlsError> {
        loop {
            {
                let idx = self.index.read().await;

                if let Some(ref e) = idx.error {
                    return Err(KitharaIoError::Source(HlsError::Driver(e.clone())));
                }

                // Check if we have data covering range.start.
                if range.start < idx.total_len {
                    return Ok(WaitOutcome::Ready);
                }

                if idx.finished {
                    return Ok(WaitOutcome::Eof);
                }
            }

            self.notify.notified().await;
        }
    }

    async fn read_at(&self, offset: u64, len: usize) -> KitharaIoResult<Bytes, HlsError> {
        let (segment_url, local_offset, segment_len) = {
            let idx = self.index.read().await;

            if let Some(ref e) = idx.error {
                return Err(KitharaIoError::Source(HlsError::Driver(e.clone())));
            }

            if offset >= idx.total_len {
                return Ok(Bytes::new());
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

        // Open segment file from cache and read.
        let key = ResourceKey::from_url(&segment_url);
        let res = self
            .fetch
            .assets()
            .open_streaming_resource(&key)
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Assets(e)))?;

        // Calculate how much to read (don't exceed segment bounds).
        let available_in_segment = segment_len.saturating_sub(local_offset);
        let read_len = (len as u64).min(available_in_segment) as usize;

        let bytes = res
            .read_at(local_offset, read_len)
            .await
            .map_err(|e| KitharaIoError::Source(HlsError::Storage(e)))?;

        Ok(bytes)
    }

    fn len(&self) -> Option<u64> {
        // Length is unknown for streaming HLS.
        None
    }
}

// =============================================================================
// HlsSession - main entry point
// =============================================================================

pub struct HlsSession {
    driver: HlsDriver,
    fetch: Arc<FetchManager>,
}

impl HlsSession {
    pub(crate) fn new(driver: HlsDriver, fetch: Arc<FetchManager>) -> Self {
        Self { driver, fetch }
    }

    /// Get event receiver for stream events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.driver.events()
    }

    /// Create a Source for random-access reading (used with SyncReader for rodio).
    pub fn source(self) -> HlsSessionSource {
        HlsSessionSource::new(self.driver, self.fetch)
    }
}
