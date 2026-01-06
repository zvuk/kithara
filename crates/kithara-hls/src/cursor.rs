#![forbid(unsafe_code)]

use std::ops::Range;

use bytes::Bytes;
use kithara_storage::StreamingResourceExt;
use kithara_stream::io::{IoError as KitharaIoError, IoResult as KitharaIoResult, WaitOutcome};
use tracing::debug;
use url::Url;

use crate::fetch::FetchManager;

/// Fixed-variant, byte-addressable cursor over an HLS VOD playlist.
///
/// This type is intentionally "file-like" (contiguous byte stream), but it does **not** prefetch
/// nor "stitch" anything in memory. It maps a global byte offset (`pos`) to a specific segment and
/// offset inside that segment, then uses:
/// - [`FetchManager`] to trigger fetch and wait/read from storage,
/// - the provided segment list (init + media segments) to define the byte layout.
///
/// Contract:
/// - Variant is fixed for the whole lifetime of this cursor.
/// - Segment list is treated as stable (VOD).
/// - `seek()` is supported within the byte space of this fixed variant.
///
/// Note: This is async and returns `Bytes`. A sync `Read+Seek` adapter can be built on top
/// (e.g. for `rodio`), but that is out of scope for this file.
pub struct SegmentCursor {
    fetch: FetchManager,
    variant_index: usize,
    segments: Vec<SegmentDesc>,
    prefix: Vec<u64>,
    total_len: u64,

    pos: u64,
    chunk_size: usize,
}

/// One segment in the fixed-variant virtual byte space.
#[derive(Clone, Debug)]
pub struct SegmentDesc {
    pub url: Url,
    /// Path under `<asset_root>/...` (without duplicating the root).
    pub rel_path: String,
    pub len: u64,
    pub is_init: bool,
}

impl SegmentCursor {
    /// Create a new cursor for a fixed variant.
    ///
    /// `segments` must be in the exact byte order:
    /// - optional init segment first (if present),
    /// - then media segments in playback order.
    ///
    /// `len` fields must be correct. They are used for mapping and EOF decisions.
    pub fn new(
        fetch: FetchManager,
        variant_index: usize,
        segments: Vec<SegmentDesc>,
        chunk_size: usize,
    ) -> Self {
        let mut prefix: Vec<u64> = Vec::with_capacity(segments.len() + 1);
        prefix.push(0);
        let mut total_len: u64 = 0;
        for s in &segments {
            total_len = total_len.saturating_add(s.len);
            prefix.push(total_len);
        }

        Self {
            fetch,
            variant_index,
            segments,
            prefix,
            total_len,
            pos: 0,
            chunk_size: chunk_size.max(1),
        }
    }

    pub fn len(&self) -> u64 {
        self.total_len
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn segments(&self) -> &[SegmentDesc] {
        &self.segments
    }

    /// Clamp and set the current position.
    pub fn seek(&mut self, pos: u64) {
        self.pos = pos.min(self.total_len);
        debug!(pos = self.pos, "segment_cursor: seek");
    }

    /// Map a global byte position to `(segment_index, offset_in_segment)`.
    fn locate(&self, pos: u64) -> Option<(usize, u64)> {
        if pos >= self.total_len {
            return None;
        }

        // Binary search on prefix sums.
        // Find largest i where prefix[i] <= pos, then segment = i, offset = pos - prefix[i].
        let mut lo: usize = 0;
        let mut hi: usize = self.prefix.len(); // = segments.len()+1

        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.prefix[mid] <= pos {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        if lo >= self.segments.len() {
            return None;
        }

        let seg_off = pos - self.prefix[lo];
        Some((lo, seg_off))
    }

    /// Ensure that a contiguous byte range is available (across segment boundaries).
    ///
    /// This does not read bytes; it just blocks until the data is available (or EOF).
    pub async fn wait_range(&self, range: Range<u64>) -> KitharaIoResult<WaitOutcome> {
        if range.start >= self.total_len {
            return Ok(WaitOutcome::Eof);
        }

        let mut pos = range.start;
        let end = range.end.min(self.total_len);

        while pos < end {
            let Some((seg_idx, seg_off)) = self.locate(pos) else {
                return Ok(WaitOutcome::Eof);
            };

            let seg = &self.segments[seg_idx];
            let seg_remaining = seg.len.saturating_sub(seg_off);
            if seg_remaining == 0 {
                return Ok(WaitOutcome::Eof);
            }

            let need = (end - pos).min(seg_remaining);

            // Intentionally quiet: this mapping is on the hot path during playback.

            let res = self
                .fetch
                .open_media_streaming_resource(self.variant_index, &seg.url)
                .await
                .map_err(|e| KitharaIoError::Source(e.to_string()))?;
            let outcome = res
                .wait_range(seg_off..seg_off.saturating_add(need))
                .await
                .map_err(|e| KitharaIoError::Source(e.to_string()))?;

            match outcome {
                kithara_storage::WaitOutcome::Ready => {
                    pos = pos.saturating_add(need);
                }
                kithara_storage::WaitOutcome::Eof => return Ok(WaitOutcome::Eof),
            }
        }

        Ok(WaitOutcome::Ready)
    }

    /// Read at most `len` bytes at `offset`.
    ///
    /// This reads within a single segment only. Callers that need contiguous buffers across
    /// boundaries should loop.
    pub async fn read_at(&self, offset: u64, len: usize) -> KitharaIoResult<Bytes> {
        if len == 0 {
            return Ok(Bytes::new());
        }
        if offset >= self.total_len {
            return Ok(Bytes::new());
        }

        let Some((seg_idx, seg_off)) = self.locate(offset) else {
            return Ok(Bytes::new());
        };

        let seg = &self.segments[seg_idx];
        let seg_remaining = seg.len.saturating_sub(seg_off);
        if seg_remaining == 0 {
            return Ok(Bytes::new());
        }

        let want = (len as u64).min(seg_remaining);
        let want_usize: usize = want
            .try_into()
            .map_err(|_| KitharaIoError::Source("read length too large".into()))?;

        // Intentionally quiet: this mapping is on the hot path during playback.

        let res = self
            .fetch
            .open_media_streaming_resource(self.variant_index, &seg.url)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))?;

        res.read_at(seg_off, want_usize)
            .await
            .map_err(|e| KitharaIoError::Source(e.to_string()))
    }

    /// Yield the next sequential chunk from the current cursor position.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` with non-empty bytes on progress,
    /// - `Ok(None)` on EOF,
    /// - `Err(...)` on I/O/storage/network errors.
    pub async fn next_chunk(&mut self) -> KitharaIoResult<Option<Bytes>> {
        if self.pos >= self.total_len {
            return Ok(None);
        }

        let end = self
            .pos
            .saturating_add(self.chunk_size as u64)
            .min(self.total_len);

        match self.wait_range(self.pos..end).await? {
            WaitOutcome::Eof => Ok(None),
            WaitOutcome::Ready => {
                let want = (end - self.pos) as usize;
                let bytes = self.read_at(self.pos, want).await?;

                if bytes.is_empty() {
                    return Err(KitharaIoError::Source(
                        "segment_cursor: storage returned empty after Ready".into(),
                    ));
                }

                self.pos = self.pos.saturating_add(bytes.len() as u64);
                Ok(Some(bytes))
            }
        }
    }
}
