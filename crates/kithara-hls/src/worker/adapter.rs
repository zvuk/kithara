use std::{ops::Range, sync::Arc};

use async_trait::async_trait;
use kanal::{AsyncReceiver, AsyncSender};
use kithara_storage::WaitOutcome;
use kithara_stream::{ContainerFormat as StreamContainerFormat, MediaInfo, Source, StreamResult};
use kithara_worker::Fetch;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing::trace;

use super::{HlsCommand, HlsMessage};
use crate::{HlsError, events::HlsEvent, parsing::ContainerFormat};

/// Adapter from HLS worker chunks to Source trait.
///
/// Buffers chunks in memory and provides random-access read interface.
pub struct HlsSourceAdapter {
    /// Receiver for chunks from worker.
    chunk_rx: AsyncReceiver<Fetch<HlsMessage>>,

    /// Sender for commands to worker.
    cmd_tx: AsyncSender<HlsCommand>,

    /// Buffered chunks (in order).
    buffered_chunks: Arc<Mutex<Vec<HlsMessage>>>,

    /// Current epoch (for validation).
    current_epoch: Arc<Mutex<u64>>,

    /// Events channel.
    events_tx: broadcast::Sender<HlsEvent>,

    /// EOF flag.
    eof_reached: Arc<Mutex<bool>>,
}

impl HlsSourceAdapter {
    /// Create a new adapter.
    pub fn new(
        chunk_rx: AsyncReceiver<Fetch<HlsMessage>>,
        cmd_tx: AsyncSender<HlsCommand>,
        events_tx: broadcast::Sender<HlsEvent>,
    ) -> Self {
        Self {
            chunk_rx,
            cmd_tx,
            buffered_chunks: Arc::new(Mutex::new(Vec::new())),
            current_epoch: Arc::new(Mutex::new(0)),
            events_tx,
            eof_reached: Arc::new(Mutex::new(false)),
        }
    }

    /// Subscribe to HLS events.
    pub fn events(&self) -> broadcast::Receiver<HlsEvent> {
        self.events_tx.subscribe()
    }

    /// Get events sender.
    pub(crate) fn events_tx(&self) -> &broadcast::Sender<HlsEvent> {
        &self.events_tx
    }

    /// Release chunks that are fully consumed (before given offset).
    /// Keeps at least one chunk for media_info().
    fn release_consumed_chunks(&self, read_offset: u64) {
        let mut chunks = self.buffered_chunks.lock();

        // Keep at least 1 chunk for media_info and avoiding edge cases
        if chunks.len() <= 1 {
            return;
        }

        // Find how many chunks we can safely remove
        let mut remove_count = 0;
        for chunk in chunks.iter() {
            let chunk_end = chunk.byte_offset + chunk.len() as u64;

            // Only remove if chunk is fully behind read position
            if chunk_end <= read_offset {
                remove_count += 1;
            } else {
                break;
            }
        }

        // Always keep at least 1 chunk
        if remove_count >= chunks.len() {
            remove_count = chunks.len() - 1;
        }

        if remove_count > 0 {
            chunks.drain(0..remove_count);
            tracing::debug!(
                removed = remove_count,
                remaining = chunks.len(),
                "released consumed chunks"
            );
        }
    }

    /// Ensure we have chunks covering the given range.
    async fn ensure_chunks_for_range(&self, range: Range<u64>) -> Result<(), HlsError> {
        use tracing::debug;
        debug!(?range, "ensure_chunks_for_range called");
        loop {
            {
                let eof = *self.eof_reached.lock();
                if eof {
                    debug!("EOF reached");
                    return Ok(());
                }
            }

            {
                let chunks = self.buffered_chunks.lock();
                let covered = self.range_covered_by_chunks(&chunks, range.clone());
                if covered {
                    debug!(num_chunks = chunks.len(), "range already covered");
                    return Ok(());
                }
                debug!(
                    num_chunks = chunks.len(),
                    "range not covered, waiting for chunk"
                );
            }

            debug!("waiting for chunk from worker");
            let fetch = match self.chunk_rx.recv().await {
                Ok(f) => f,
                Err(_) => {
                    debug!("chunk channel closed");
                    {
                        *self.eof_reached.lock() = true;
                    }
                    return Ok(());
                }
            };
            debug!(?fetch.is_eof, epoch = fetch.epoch, "received chunk from worker");

            if fetch.is_eof {
                {
                    *self.eof_reached.lock() = true;
                }
                return Ok(());
            }

            let current_epoch = {
                let epoch = self.current_epoch.lock();
                *epoch
            };

            if fetch.epoch != current_epoch {
                trace!(
                    fetch_epoch = fetch.epoch,
                    current_epoch, "discarding stale chunk"
                );
                continue;
            }

            if !fetch.data.is_empty() {
                {
                    self.buffered_chunks.lock().push(fetch.data);
                }
            } else {
                // Empty chunk without EOF - worker might be paused or sending empty data
                // Continue waiting, but log for debugging
                trace!("received empty chunk without EOF, continuing to wait");
            }
        }
    }

    /// Check if buffered chunks cover the given range.
    fn range_covered_by_chunks(&self, chunks: &[HlsMessage], range: Range<u64>) -> bool {
        if range.is_empty() {
            return true;
        }

        if chunks.is_empty() {
            return false;
        }

        // Sort chunks by byte_offset to check continuous coverage
        let mut sorted_chunks: Vec<_> = chunks.iter().collect();
        sorted_chunks.sort_by_key(|c| c.byte_offset);

        let mut current_pos = range.start;

        for chunk in sorted_chunks {
            let chunk_start = chunk.byte_offset;
            let chunk_end = chunk.byte_offset + chunk.len() as u64;

            // If there's a gap before this chunk, range is not covered
            if chunk_start > current_pos {
                return false;
            }

            // If this chunk extends our coverage, update position
            if chunk_end > current_pos {
                current_pos = chunk_end;
            }

            // If we've covered the entire range, we're done
            if current_pos >= range.end {
                return true;
            }
        }

        // Check if we covered the entire range
        current_pos >= range.end
    }

    /// Find chunk containing the given offset and return (chunk_index, offset_in_chunk).
    fn find_chunk_at_offset(&self, chunks: &[HlsMessage], offset: u64) -> Option<(usize, usize)> {
        for (idx, chunk) in chunks.iter().enumerate() {
            let chunk_start = chunk.byte_offset;
            let chunk_end = chunk.byte_offset + chunk.len() as u64;

            if offset >= chunk_start && offset < chunk_end {
                let offset_in_chunk = (offset - chunk_start) as usize;
                return Some((idx, offset_in_chunk));
            }
        }

        None
    }

    /// Send seek command.
    pub async fn seek(&self, segment_index: usize) -> Result<(), HlsError> {
        let new_epoch = {
            let mut epoch = self.current_epoch.lock();
            *epoch += 1;
            *epoch
        };

        self.buffered_chunks.lock().clear();
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

        self.buffered_chunks.lock().clear();
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
    ///
    /// Note: In worker architecture, variant can change asynchronously.
    /// This returns the variant from the last received chunk.
    pub fn current_variant(&self) -> usize {
        self.buffered_chunks
            .lock()
            .last()
            .map(|chunk| chunk.variant)
            .unwrap_or(0)
    }

    /// Set current variant (compatibility method).
    ///
    /// Note: This is an async operation in worker architecture.
    /// For synchronous API compatibility, we just send the command without waiting.
    pub fn set_current_variant(&self, variant: usize) {
        let _ = self.cmd_tx.try_send(HlsCommand::ForceVariant {
            variant_index: variant,
            epoch: *self.current_epoch.lock() + 1,
        });
    }

    /// Get loaded segment count (compatibility method).
    ///
    /// Note: In worker architecture this is approximate.
    pub fn loaded_segment_count(&self) -> usize {
        self.buffered_chunks.lock().len()
    }

    /// Get loaded segment indices (compatibility method).
    ///
    /// Note: In worker architecture this is approximate.
    pub fn loaded_segment_indices(&self) -> Vec<usize> {
        self.buffered_chunks
            .lock()
            .iter()
            .filter_map(|chunk| chunk.segment_type.media_index())
            .collect()
    }

    /// Check if segment is loaded (compatibility method).
    ///
    /// Note: In worker architecture this is approximate.
    pub fn has_segment(&self, segment_index: usize) -> bool {
        self.buffered_chunks
            .lock()
            .iter()
            .any(|chunk| chunk.segment_type.media_index() == Some(segment_index))
    }
}

#[async_trait]
impl Source for HlsSourceAdapter {
    type Item = u8;
    type Error = HlsError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, HlsError> {
        use kithara_stream::StreamError;

        self.ensure_chunks_for_range(range.clone())
            .await
            .map_err(StreamError::Source)?;

        let chunks = self.buffered_chunks.lock();
        let covered = self.range_covered_by_chunks(&chunks, range);

        Ok(if covered {
            WaitOutcome::Ready
        } else {
            WaitOutcome::Eof
        })
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, HlsError> {
        use kithara_stream::StreamError;

        let range = offset..offset + buf.len() as u64;
        self.ensure_chunks_for_range(range)
            .await
            .map_err(StreamError::Source)?;

        let chunks = self.buffered_chunks.lock();

        let Some((chunk_idx, offset_in_chunk)) = self.find_chunk_at_offset(&chunks, offset) else {
            return Ok(0);
        };

        let chunk = &chunks[chunk_idx];
        let available = chunk.len() - offset_in_chunk;
        let to_copy = available.min(buf.len());

        buf[..to_copy].copy_from_slice(&chunk.bytes[offset_in_chunk..offset_in_chunk + to_copy]);

        // Release lock before cleanup
        drop(chunks);

        // Release consumed chunks to free memory (sequential reads)
        self.release_consumed_chunks(offset);

        Ok(to_copy)
    }

    fn len(&self) -> Option<u64> {
        let chunks = self.buffered_chunks.lock();
        if chunks.is_empty() {
            return None;
        }

        let last_chunk = chunks.last()?;
        Some(last_chunk.byte_offset + last_chunk.len() as u64)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        use tracing::debug;
        let chunks = self.buffered_chunks.lock();
        let last_chunk = chunks.last()?;

        let container = match last_chunk.container {
            Some(ContainerFormat::Fmp4) => Some(StreamContainerFormat::Fmp4),
            Some(ContainerFormat::Ts) => Some(StreamContainerFormat::MpegTs),
            Some(ContainerFormat::Other) | None => None,
        };

        debug!(?container, codec = ?last_chunk.codec, "media_info called");

        Some(MediaInfo {
            container,
            codec: last_chunk.codec,
            sample_rate: None,
            channels: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use url::Url;

    use super::*;
    use crate::cache::SegmentType;

    fn create_test_chunk(offset: u64, len: usize) -> HlsMessage {
        HlsMessage {
            bytes: Bytes::from(vec![0u8; len]),
            byte_offset: offset,
            variant: 0,
            segment_type: SegmentType::Media(0),
            segment_url: Url::parse("http://test.com/seg0.ts").expect("valid url"),
            segment_duration: None,
            codec: None,
            container: None,
            bitrate: None,
            encryption: None,
            is_segment_start: true,
            is_segment_end: true,
            is_variant_switch: false,
        }
    }

    #[test]
    fn test_range_covered_by_chunks() {
        let (chunk_tx, chunk_rx) = kanal::bounded_async(10);
        let (cmd_tx, _cmd_rx) = kanal::bounded_async(10);
        let (events_tx, _) = broadcast::channel(10);

        let adapter = HlsSourceAdapter::new(chunk_rx, cmd_tx, events_tx);

        let chunks = vec![
            create_test_chunk(0, 100),
            create_test_chunk(100, 100),
            create_test_chunk(200, 100),
        ];

        // Single chunks
        assert!(adapter.range_covered_by_chunks(&chunks, 0..50));
        assert!(adapter.range_covered_by_chunks(&chunks, 50..100));
        assert!(adapter.range_covered_by_chunks(&chunks, 100..200));
        assert!(adapter.range_covered_by_chunks(&chunks, 200..250));

        // Multiple chunks (collective coverage)
        assert!(adapter.range_covered_by_chunks(&chunks, 0..101));
        assert!(adapter.range_covered_by_chunks(&chunks, 0..200));
        assert!(adapter.range_covered_by_chunks(&chunks, 50..150));
        assert!(adapter.range_covered_by_chunks(&chunks, 150..250));

        // Gaps (not covered)
        assert!(!adapter.range_covered_by_chunks(&chunks, 300..400));

        drop(chunk_tx);
    }

    #[test]
    fn test_find_chunk_at_offset() {
        let (_chunk_tx, chunk_rx) = kanal::bounded_async(10);
        let (cmd_tx, _cmd_rx) = kanal::bounded_async(10);
        let (events_tx, _) = broadcast::channel(10);

        let adapter = HlsSourceAdapter::new(chunk_rx, cmd_tx, events_tx);

        let chunks = vec![
            create_test_chunk(0, 100),
            create_test_chunk(100, 100),
            create_test_chunk(200, 100),
        ];

        assert_eq!(adapter.find_chunk_at_offset(&chunks, 0), Some((0, 0)));
        assert_eq!(adapter.find_chunk_at_offset(&chunks, 50), Some((0, 50)));
        assert_eq!(adapter.find_chunk_at_offset(&chunks, 100), Some((1, 0)));
        assert_eq!(adapter.find_chunk_at_offset(&chunks, 150), Some((1, 50)));
        assert_eq!(adapter.find_chunk_at_offset(&chunks, 200), Some((2, 0)));
        assert_eq!(adapter.find_chunk_at_offset(&chunks, 300), None);
    }
}
