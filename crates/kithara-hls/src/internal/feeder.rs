#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use bytes::Bytes;
use kithara_io::{IoError as KitharaIoError, IoResult as KitharaIoResult, WaitOutcome};
use kithara_storage::StreamingResourceExt;
use tracing::debug;
use url::Url;

use super::Fetcher;

/// Internal "read loop" abstraction for HLS segments.
///
/// Key point:
/// `kithara-storage::StreamingResource` tracks available ranges in-memory per handle. Re-opening
/// the same disk path yields a new handle with an empty `available` set, which will never become
/// ready unless *that handle* is written to. Therefore, this feeder must operate on the shared
/// streaming resource handle returned by `Fetcher` (writer + reader share the same handle).
pub struct Feeder {
    fetcher: Arc<Fetcher>,
}

impl Feeder {
    pub fn new(fetcher: Arc<Fetcher>) -> Self {
        Self { fetcher }
    }

    pub fn fetcher(&self) -> &Fetcher {
        self.fetcher.as_ref()
    }

    /// Ensure a byte range inside a single segment is available.
    ///
    /// `rel_path` must match the handle managed by `Fetcher`. This function will:
    /// - start the writer (best-effort) for this segment (idempotent via `Fetcher`),
    /// - obtain the shared streaming resource handle,
    /// - wait for `range_in_segment` to become available.
    pub async fn wait_in_segment(
        &self,
        segment_url: &Url,
        rel_path: &str,
        _is_init: bool,
        range_in_segment: Range<u64>,
    ) -> KitharaIoResult<WaitOutcome> {
        // Ensure writer is started + obtain shared handle.
        let res = self
            .fetcher
            .get_or_start(segment_url, rel_path)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        // Intentionally quiet: called very frequently during playback.

        match res
            .wait_range(range_in_segment)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?
        {
            kithara_storage::WaitOutcome::Ready => Ok(WaitOutcome::Ready),
            kithara_storage::WaitOutcome::Eof => Ok(WaitOutcome::Eof),
        }
    }

    /// Read bytes from a single segment at a specific offset.
    ///
    /// Note: callers may call this without `wait_in_segment` first; we still ensure the writer is
    /// started and then read from the shared handle.
    pub async fn read_in_segment(
        &self,
        segment_url: &Url,
        rel_path: &str,
        _is_init: bool,
        offset_in_segment: u64,
        len: usize,
    ) -> KitharaIoResult<Bytes> {
        if len == 0 {
            return Ok(Bytes::new());
        }

        // Ensure writer is started + obtain shared handle.
        let res = self
            .fetcher
            .get_or_start(segment_url, rel_path)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        // Intentionally quiet: called very frequently during playback.

        let bytes = res
            .read_at(offset_in_segment, len)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        // Intentionally quiet: called very frequently during playback.

        Ok(bytes)
    }

    /// Convenience helper: wait+read a range and return bytes (single segment).
    pub async fn wait_then_read(
        &self,
        segment_url: &Url,
        rel_path: &str,
        is_init: bool,
        offset_in_segment: u64,
        len: usize,
    ) -> KitharaIoResult<Bytes> {
        let end = offset_in_segment.saturating_add(len as u64);

        match self
            .wait_in_segment(segment_url, rel_path, is_init, offset_in_segment..end)
            .await?
        {
            WaitOutcome::Ready => {
                self.read_in_segment(segment_url, rel_path, is_init, offset_in_segment, len)
                    .await
            }
            WaitOutcome::Eof => {
                debug!(
                    rel_path,
                    is_init, offset_in_segment, len, "kithara-hls feeder: wait_then_read saw EOF"
                );
                Ok(Bytes::new())
            }
        }
    }
}
