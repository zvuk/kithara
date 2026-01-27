use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use kanal::{AsyncReceiver, AsyncSender};
use kithara_assets::{AssetStore, Assets, ResourceKey};
use kithara_storage::{StreamingResourceExt, WaitOutcome};
use kithara_stream::{ContainerFormat, MediaInfo, Source, StreamResult, SyncSource, WaitOutcome as StreamWaitOutcome};
use kithara_worker::Fetch;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing::{debug, trace};
use url::Url;

use super::{HlsCommand, HlsMessage};
use crate::{HlsError, events::HlsEvent, fetch::StreamingAssetResource};

/// Entry tracking a loaded segment in the virtual stream.
#[derive(Debug, Clone)]
struct SegmentEntry {
    /// URL of the media segment (for opening resource).
    segment_url: Url,

    /// URL of the init segment (fMP4 only).
    init_url: Option<Url>,

    /// Start position in virtual stream.
    byte_offset: u64,

    /// Length of init segment in bytes.
    init_len: u64,

    /// Length of media segment in bytes.
    media_len: u64,

    /// Variant index.
    variant: usize,

    /// Container format.
    container: Option<ContainerFormat>,

    /// Audio codec.
    codec: Option<kithara_stream::AudioCodec>,
}

impl SegmentEntry {
    /// Total length of this entry (init + media).
    fn total_len(&self) -> u64 {
        self.init_len + self.media_len
    }

    /// End position in virtual stream.
    fn end_offset(&self) -> u64 {
        self.byte_offset + self.total_len()
    }

    /// Check if this entry contains the given offset.
    fn contains(&self, offset: u64) -> bool {
        offset >= self.byte_offset && offset < self.end_offset()
    }
}

/// Index of loaded segments for direct disk access.
#[derive(Debug, Default)]
struct SegmentIndex {
    /// Loaded segments in order.
    entries: Vec<SegmentEntry>,
}

impl SegmentIndex {
    fn new() -> Self {
        Self { entries: Vec::new() }
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn push(&mut self, entry: SegmentEntry) {
        self.entries.push(entry);
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn last(&self) -> Option<&SegmentEntry> {
        self.entries.last()
    }

    /// Find entry containing the given offset.
    fn find_at_offset(&self, offset: u64) -> Option<&SegmentEntry> {
        self.entries.iter().find(|e| e.contains(offset))
    }

    /// Check if range is covered by loaded segments.
    fn range_covered(&self, range: &Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }

        if self.entries.is_empty() {
            return false;
        }

        // Check continuous coverage
        let mut current_pos = range.start;

        for entry in &self.entries {
            if entry.byte_offset > current_pos {
                return false; // Gap
            }

            if entry.end_offset() > current_pos {
                current_pos = entry.end_offset();
            }

            if current_pos >= range.end {
                return true;
            }
        }

        current_pos >= range.end
    }

    /// Get total bytes loaded.
    fn total_bytes(&self) -> u64 {
        self.entries.last().map(|e| e.end_offset()).unwrap_or(0)
    }

    /// Get segment indices (for compatibility).
    fn segment_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, _)| i)
            .collect()
    }
}

/// Adapter from HLS worker to Source trait.
///
/// Receives segment metadata from worker, tracks loaded segments,
/// and reads data directly from disk via AssetStore.
pub struct HlsSourceAdapter {
    /// Receiver for segment metadata from worker.
    chunk_rx: AsyncReceiver<Fetch<HlsMessage>>,

    /// Sender for commands to worker.
    cmd_tx: AsyncSender<HlsCommand>,

    /// Asset store for reading segments from disk.
    assets: AssetStore,

    /// Index of loaded segments.
    segments: Arc<Mutex<SegmentIndex>>,

    /// Current epoch (for validation).
    current_epoch: Arc<Mutex<u64>>,

    /// Events channel.
    events_tx: broadcast::Sender<HlsEvent>,

    /// EOF flag.
    eof_reached: Arc<Mutex<bool>>,

    /// Tokio runtime handle for sync operations.
    runtime: tokio::runtime::Handle,
}

impl HlsSourceAdapter {
    /// Create a new adapter.
    pub fn new(
        chunk_rx: AsyncReceiver<Fetch<HlsMessage>>,
        cmd_tx: AsyncSender<HlsCommand>,
        assets: AssetStore,
        events_tx: broadcast::Sender<HlsEvent>,
    ) -> Self {
        Self {
            chunk_rx,
            cmd_tx,
            assets,
            segments: Arc::new(Mutex::new(SegmentIndex::new())),
            current_epoch: Arc::new(Mutex::new(0)),
            events_tx,
            eof_reached: Arc::new(Mutex::new(false)),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Subscribe to HLS events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events_tx.subscribe()
    }

    /// Ensure we have segments covering the given range.
    async fn ensure_segments_for_range(&self, range: Range<u64>) -> Result<(), HlsError> {
        loop {
            {
                let eof = *self.eof_reached.lock();
                if eof {
                    debug!("EOF reached");
                    return Ok(());
                }
            }

            {
                let segments = self.segments.lock();
                let covered = segments.range_covered(&range);
                if covered {
                    debug!(num_segments = segments.len(), "range already covered");
                    return Ok(());
                }
                debug!(
                    num_segments = segments.len(),
                    "range not covered, waiting for segment"
                );
            }

            debug!("waiting for segment from worker");
            let fetch = match self.chunk_rx.recv().await {
                Ok(f) => f,
                Err(_) => {
                    debug!("chunk channel closed");
                    *self.eof_reached.lock() = true;
                    return Ok(());
                }
            };
            debug!(?fetch.is_eof, epoch = fetch.epoch, "received message from worker");

            if fetch.is_eof {
                *self.eof_reached.lock() = true;
                return Ok(());
            }

            let current_epoch = *self.current_epoch.lock();

            if fetch.epoch != current_epoch {
                trace!(
                    fetch_epoch = fetch.epoch,
                    current_epoch, "discarding stale message"
                );
                continue;
            }

            // Add segment to index
            let msg = fetch.data;
            if msg.media_len > 0 || msg.init_len > 0 {
                let entry = SegmentEntry {
                    segment_url: msg.segment_url,
                    init_url: msg.init_url,
                    byte_offset: msg.byte_offset,
                    init_len: msg.init_len,
                    media_len: msg.media_len,
                    variant: msg.variant,
                    container: msg.container,
                    codec: msg.codec,
                };
                self.segments.lock().push(entry);
            } else {
                trace!("received empty message, continuing to wait");
            }
        }
    }

    /// Read bytes from a segment entry at given offset.
    async fn read_from_entry(
        &self,
        entry: &SegmentEntry,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let local_offset = offset - entry.byte_offset;

        // Determine if we're reading from init or media segment
        if local_offset < entry.init_len {
            // Reading from init segment
            let Some(ref init_url) = entry.init_url else {
                // No init segment but offset is in init range - shouldn't happen
                return Ok(0);
            };

            let key = ResourceKey::from_url(init_url);
            let resource: StreamingAssetResource =
                self.assets.open_streaming_resource(&key).await?;

            // Wait for data to be available
            let read_end = (local_offset + buf.len() as u64).min(entry.init_len);
            resource.wait_range(local_offset..read_end).await?;

            // Read from init
            let available_in_init = (entry.init_len - local_offset) as usize;
            let to_read_init = buf.len().min(available_in_init);
            let bytes_from_init = resource.read_at(local_offset, &mut buf[..to_read_init]).await?;

            // If we need more bytes from media segment
            if bytes_from_init < buf.len() && entry.media_len > 0 {
                let remaining = &mut buf[bytes_from_init..];
                let bytes_from_media = self
                    .read_from_media_segment(entry, 0, remaining)
                    .await?;
                Ok(bytes_from_init + bytes_from_media)
            } else {
                Ok(bytes_from_init)
            }
        } else {
            // Reading from media segment
            let media_offset = local_offset - entry.init_len;
            self.read_from_media_segment(entry, media_offset, buf).await
        }
    }

    /// Read from media segment at given offset.
    async fn read_from_media_segment(
        &self,
        entry: &SegmentEntry,
        media_offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, HlsError> {
        let key = ResourceKey::from_url(&entry.segment_url);
        let resource: StreamingAssetResource =
            self.assets.open_streaming_resource(&key).await?;

        // Wait for data to be available
        let read_end = (media_offset + buf.len() as u64).min(entry.media_len);
        resource.wait_range(media_offset..read_end).await?;

        // Read from media
        let bytes_read = resource.read_at(media_offset, buf).await?;
        Ok(bytes_read)
    }

    /// Send seek command.
    pub async fn seek(&self, segment_index: usize) -> Result<(), HlsError> {
        let new_epoch = {
            let mut epoch = self.current_epoch.lock();
            *epoch += 1;
            *epoch
        };

        self.segments.lock().clear();
        *self.eof_reached.lock() = false;

        self.cmd_tx
            .send(HlsCommand::Seek {
                segment_index,
                epoch: new_epoch,
            })
            .await
            .map_err(|_| HlsError::Driver("failed to send seek command".into()))?;

        Ok(())
    }

    /// Send force variant command.
    pub async fn force_variant(&self, variant_index: usize) -> Result<(), HlsError> {
        let new_epoch = {
            let mut epoch = self.current_epoch.lock();
            *epoch += 1;
            *epoch
        };

        self.segments.lock().clear();
        *self.eof_reached.lock() = false;

        self.cmd_tx
            .send(HlsCommand::ForceVariant {
                variant_index,
                epoch: new_epoch,
            })
            .await
            .map_err(|_| HlsError::Driver("failed to send force variant command".into()))?;

        Ok(())
    }

    /// Get current variant (compatibility method).
    pub fn current_variant(&self) -> usize {
        self.segments
            .lock()
            .last()
            .map(|e| e.variant)
            .unwrap_or(0)
    }

    /// Set current variant (compatibility method).
    pub fn set_current_variant(&self, variant: usize) {
        let _ = self.cmd_tx.try_send(HlsCommand::ForceVariant {
            variant_index: variant,
            epoch: *self.current_epoch.lock() + 1,
        });
    }

    /// Get loaded segment count (compatibility method).
    pub fn loaded_segment_count(&self) -> usize {
        self.segments.lock().len()
    }

    /// Get loaded segment indices (compatibility method).
    pub fn loaded_segment_indices(&self) -> Vec<usize> {
        self.segments.lock().segment_indices()
    }

    /// Check if segment is loaded (compatibility method).
    pub fn has_segment(&self, segment_index: usize) -> bool {
        self.segments.lock().entries.len() > segment_index
    }
}

#[async_trait]
impl Source for HlsSourceAdapter {
    type Item = u8;
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, HlsError> {
        use kithara_stream::StreamError;

        self.ensure_segments_for_range(range.clone())
            .await
            .map_err(StreamError::Source)?;

        let segments = self.segments.lock();
        let covered = segments.range_covered(&range);

        Ok(if covered {
            WaitOutcome::Ready
        } else {
            WaitOutcome::Eof
        })
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, HlsError> {
        use kithara_stream::StreamError;

        let range = offset..offset + buf.len() as u64;
        self.ensure_segments_for_range(range)
            .await
            .map_err(StreamError::Source)?;

        // Find segment containing offset
        let entry = {
            let segments = self.segments.lock();
            segments.find_at_offset(offset).cloned()
        };

        let Some(entry) = entry else {
            return Ok(0);
        };

        self.read_from_entry(&entry, offset, buf)
            .await
            .map_err(StreamError::Source)
    }

    fn len(&self) -> Option<u64> {
        let segments = self.segments.lock();
        if segments.is_empty() {
            return None;
        }
        Some(segments.total_bytes())
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let segments = self.segments.lock();
        let last = segments.last()?;

        debug!(container = ?last.container, codec = ?last.codec, "media_info called");

        Some(MediaInfo::new(last.codec, last.container))
    }
}

impl SyncSource for HlsSourceAdapter {
    fn wait_range(&self, range: std::ops::Range<u64>) -> std::io::Result<StreamWaitOutcome> {
        self.runtime
            .block_on(self.ensure_segments_for_range(range.clone()))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let segments = self.segments.lock();
        let covered = segments.range_covered(&range);

        Ok(if covered {
            StreamWaitOutcome::Ready
        } else {
            StreamWaitOutcome::Eof
        })
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let range = offset..offset + buf.len() as u64;
        self.runtime
            .block_on(self.ensure_segments_for_range(range))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Find segment containing offset
        let entry = {
            let segments = self.segments.lock();
            segments.find_at_offset(offset).cloned()
        };

        let Some(entry) = entry else {
            return Ok(0);
        };

        self.runtime
            .block_on(self.read_from_entry(&entry, offset, buf))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }

    fn len(&self) -> Option<u64> {
        let segments = self.segments.lock();
        if segments.is_empty() {
            return None;
        }
        Some(segments.total_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_entry() {
        let entry = SegmentEntry {
            segment_url: Url::parse("http://test.com/seg0.ts").unwrap(),
            init_url: Some(Url::parse("http://test.com/init.mp4").unwrap()),
            byte_offset: 100,
            init_len: 50,
            media_len: 200,
            variant: 0,
            container: Some(ContainerFormat::Fmp4),
            codec: None,
        };

        assert_eq!(entry.total_len(), 250);
        assert_eq!(entry.end_offset(), 350);
        assert!(entry.contains(100));
        assert!(entry.contains(349));
        assert!(!entry.contains(99));
        assert!(!entry.contains(350));
    }

    #[test]
    fn test_segment_index_range_covered() {
        let mut index = SegmentIndex::new();

        // Empty index doesn't cover anything
        assert!(!index.range_covered(&(0..100)));
        assert!(index.range_covered(&(0..0))); // Empty range is always covered

        // Add first segment
        index.push(SegmentEntry {
            segment_url: Url::parse("http://test.com/seg0.ts").unwrap(),
            init_url: None,
            byte_offset: 0,
            init_len: 0,
            media_len: 100,
            variant: 0,
            container: None,
            codec: None,
        });

        assert!(index.range_covered(&(0..50)));
        assert!(index.range_covered(&(0..100)));
        assert!(!index.range_covered(&(0..101))); // Beyond first segment

        // Add second segment
        index.push(SegmentEntry {
            segment_url: Url::parse("http://test.com/seg1.ts").unwrap(),
            init_url: None,
            byte_offset: 100,
            init_len: 0,
            media_len: 100,
            variant: 0,
            container: None,
            codec: None,
        });

        assert!(index.range_covered(&(0..200)));
        assert!(index.range_covered(&(50..150)));
        assert!(!index.range_covered(&(0..201)));
    }

    #[test]
    fn test_segment_index_find_at_offset() {
        let mut index = SegmentIndex::new();

        index.push(SegmentEntry {
            segment_url: Url::parse("http://test.com/seg0.ts").unwrap(),
            init_url: None,
            byte_offset: 0,
            init_len: 0,
            media_len: 100,
            variant: 0,
            container: None,
            codec: None,
        });

        index.push(SegmentEntry {
            segment_url: Url::parse("http://test.com/seg1.ts").unwrap(),
            init_url: None,
            byte_offset: 100,
            init_len: 0,
            media_len: 100,
            variant: 0,
            container: None,
            codec: None,
        });

        let found = index.find_at_offset(50);
        assert!(found.is_some());
        assert_eq!(found.unwrap().byte_offset, 0);

        let found = index.find_at_offset(150);
        assert!(found.is_some());
        assert_eq!(found.unwrap().byte_offset, 100);

        let found = index.find_at_offset(200);
        assert!(found.is_none());
    }
}
