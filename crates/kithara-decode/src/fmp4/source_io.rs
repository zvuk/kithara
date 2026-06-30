use std::{
    io::{Seek, SeekFrom},
    ops::Range,
};

use kithara_bufpool::PooledOwned;
use kithara_stream::{ByteMap, PendingReason, StreamReadError};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{BoxedSource, InputReadOutcome},
};

fn map_stream_err(err: StreamReadError) -> DecodeError {
    match err {
        StreamReadError::Source(io_err) => DecodeError::from(io_err),
        _ => DecodeError::InvalidData(format!("unknown stream read error: {err:?}").into()),
    }
}

/// Status returned by [`fill_segment_buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FillStatus {
    /// Buffer fully populated.
    Ready,
    /// Source signalled a typed pending reason — caller should
    /// surface this as `DecoderChunkOutcome::Pending(reason)` and
    /// retry on the next `next_chunk` call.
    Pending(PendingReason),
}

/// Pool-backed segment read buffer. Drawn from the host `BytePool` and
/// returned to it on drop, so a steady-state decode loop recycles one
/// high-water allocation across segments instead of mallocing per
/// segment. Holds 32 shards to match the workspace `BytePool`.
pub(crate) type SegmentBuf = PooledOwned<32, Vec<u8>>;

/// Resumable read of a contiguous byte range from a `Read + Seek`
/// source into an in-memory buffer.
///
/// The buffer is sized once (when `filled == 0`) to match
/// `range.end - range.start`. Subsequent calls resume from
/// `state.filled`, so a `Pending` interrupt is recoverable: just call
/// `fill_segment_buffer` again with the same `state` once the audio
/// FSM unblocks.
#[derive(Debug)]
pub(crate) struct SegmentReadState {
    pub(crate) range: Range<u64>,
    pub(crate) buffer: SegmentBuf,
    pub(crate) filled: usize,
}

impl SegmentReadState {
    /// Build a read state over `range` backed by `buffer`, a buffer
    /// freshly drawn from the host `BytePool`. The buffer's retained
    /// capacity (high-water mark carried over from a previous segment
    /// the pool recycled) is reused; [`Self::sync_buffer_ready`] only
    /// reallocates when this segment is larger than any seen before.
    pub(crate) fn new(range: Range<u64>, buffer: SegmentBuf) -> Self {
        Self {
            range,
            buffer,
            filled: 0,
        }
    }

    /// Set `buffer` length to exactly `total`. Growth goes through the
    /// budget-tracked [`PooledOwned::ensure_len`] (a plain `resize` via
    /// `DerefMut` would leak the pool's byte budget); shrink uses
    /// `truncate`, which keeps capacity so the high-water mark survives.
    fn resize_to(&mut self, total: usize) -> DecodeResult<()> {
        if self.buffer.len() == total {
            return Ok(());
        }
        self.buffer.ensure_len(total).map_err(|_| {
            DecodeError::InvalidData(
                format!("byte-pool budget exhausted sizing segment buffer to {total} bytes").into(),
            )
        })?;
        self.buffer.truncate(total);
        Ok(())
    }

    /// Resize `buffer` to the current `total()` and report whether the
    /// segment is already fully filled. Every fill checkpoint runs this
    /// after a live-range re-resolve that may have grown or shrunk the
    /// target.
    fn sync_buffer_ready(&mut self) -> DecodeResult<bool> {
        let total = self.total();
        self.resize_to(total)?;
        Ok(self.filled >= total)
    }

    pub(crate) fn total(&self) -> usize {
        usize::try_from(self.range.end - self.range.start)
            .expect("BUG: segment range fits usize on supported targets")
    }
}

/// Which range in the live layout to re-resolve `state.range` against
/// on each iteration. Init reads (no `segment_index`) must re-query
/// [`ByteMap::init_segment_range`] because the post-decrypt init
/// size on DRM streams can shrink between cursor setup and the actual
/// read (PKCS7 padding strips up to 16 bytes off the encrypted estimate).
/// Media reads must re-query [`ByteMap::segment_at_index`] for the
/// same reason on individual segment sizes.
#[derive(Clone, Copy)]
pub(crate) enum LiveRange<'a> {
    Init(&'a dyn ByteMap),
    Segment(&'a dyn ByteMap, u32),
}

impl<'a> LiveRange<'a> {
    // ast-grep-ignore: idioms.match-self-conversion
    fn resolve(self) -> Option<Range<u64>> {
        match self {
            LiveRange::Init(layout) => {
                let range = layout.init_segment_range();
                if range.is_empty() { None } else { Some(range) }
            }
            LiveRange::Segment(layout, idx) => layout.segment_at_index(idx).map(|d| d.byte_range),
        }
    }
}

/// Drive a `BoxedSource` to fill `state.buffer` with all bytes in
/// `state.range`. Resumable across multiple calls.
///
/// `live` lets the loop re-resolve `state.range` against the live layout
/// on each iteration. When a DRM init or media segment commits with a
/// smaller post-decrypt size than the HEAD estimate, the layout's
/// reported byte range shrinks; without this re-resolve, `state.range`
/// (captured at cursor-setup time) extends past the segment's actual
/// end, and `HlsSource::read_at` happily fills the buffer's tail with
/// bytes from the next segment (or from the seg-0 moof after the init).
/// `re_mp4` then parses the trailing splice as a malformed box and
/// errors with "failed to fill whole buffer".
pub(crate) fn fill_segment_buffer(
    source: &mut BoxedSource,
    state: &mut SegmentReadState,
    live: LiveRange<'_>,
) -> DecodeResult<FillStatus> {
    loop {
        refresh_range(state, live);
        if state.sync_buffer_ready()? {
            return Ok(FillStatus::Ready);
        }

        let abs_offset = state.range.start + state.filled as u64;
        source.seek(SeekFrom::Start(abs_offset))?;
        if refresh_range(state, live) {
            let total_after = state.total();
            // Resize WITHOUT clearing: `refresh_range` already reset
            // `state.filled` to 0 on a start shift (whole prefix invalid,
            // re-read from scratch) and left it intact on an end-only
            // shrink (prefix still valid). `resize_to` truncates the latter
            // — keeping the read prefix — and grows the former; a
            // `buffer.clear()` here would zero a valid prefix and feed
            // `re_mp4` a 0x00000000 box size.
            state.resize_to(total_after)?;
            if state.filled >= total_after {
                return Ok(FillStatus::Ready);
            }
            let corrected = state.range.start + state.filled as u64;
            source.seek(SeekFrom::Start(corrected))?;
        }

        match source
            .try_read(&mut state.buffer[state.filled..])
            .map_err(map_stream_err)?
        {
            InputReadOutcome::Bytes(n) => state.filled += n.get(),
            InputReadOutcome::Pending(reason) => return Ok(FillStatus::Pending(reason)),
            InputReadOutcome::Eof => {
                if state.filled == state.total() {
                    return Ok(FillStatus::Ready);
                }
                if refresh_range(state, live) && state.sync_buffer_ready()? {
                    return Ok(FillStatus::Ready);
                }
                return Err(DecodeError::InvalidData(
                    format!(
                        "unexpected EOF before segment buffer filled: {} / {}",
                        state.filled,
                        state.total()
                    )
                    .into(),
                ));
            }
        }
    }
}

/// Re-resolve `state.range` against the live layout. Returns `true` if
/// the range moved. When `state.range.start` shifts, any bytes already
/// accumulated in `state.buffer` describe the OLD position in the
/// virtual byte map (the underlying segment data now lives at a
/// different virtual address) — we reset `state.filled = 0` and re-read
/// from the new start. When only `state.range.end` shrinks, the prefix
/// stays valid; cap `state.filled` so the loop doesn't try to re-read
/// trimmed-off bytes.
fn refresh_range(state: &mut SegmentReadState, live: LiveRange<'_>) -> bool {
    let Some(new_range) = live.resolve() else {
        return false;
    };
    if new_range == state.range {
        return false;
    }
    let start_changed = new_range.start != state.range.start;
    state.range = new_range;
    if start_changed {
        state.filled = 0;
    } else {
        let new_total = state.total();
        if state.filled > new_total {
            state.filled = new_total;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Cursor, Read, Seek, SeekFrom},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use kithara_bufpool::BytePool;
    use kithara_platform::time::Duration;
    use kithara_stream::SegmentDescriptor;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::traits::BoxedSource;

    /// Fixed multi-segment layout whose `segment_at_index` returns each
    /// segment's byte range unchanged — `fill_segment_buffer`'s live
    /// re-resolve is a no-op, isolating the buffer-capacity behaviour.
    struct FixedLayout {
        segments: Vec<Range<u64>>,
    }

    impl ByteMap for FixedLayout {
        fn init_segment_range(&self) -> Range<u64> {
            0..0
        }

        fn len(&self) -> Option<u64> {
            self.segments.last().map(|r| r.end)
        }

        fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
            self.segments
                .iter()
                .position(|r| r.start >= byte_offset)
                .map(|i| self.desc(i))
        }

        fn segment_at_index(&self, idx: u32) -> Option<SegmentDescriptor> {
            self.segments
                .get(idx as usize)
                .map(|_| self.desc(idx as usize))
        }

        fn segment_at_time(&self, _t: Duration) -> Option<SegmentDescriptor> {
            self.segments.first().map(|_| self.desc(0))
        }

        fn segment_count(&self) -> Option<u32> {
            u32::try_from(self.segments.len()).ok()
        }
    }

    impl FixedLayout {
        fn desc(&self, idx: usize) -> SegmentDescriptor {
            SegmentDescriptor::new(
                self.segments[idx].clone(),
                Duration::ZERO,
                Duration::from_secs(1),
                u32::try_from(idx).unwrap_or(0),
                0,
            )
        }
    }

    /// R-fmp4buf: segment read buffers are drawn from the [`BytePool`] and
    /// returned to it on drop, so a warm decode loop pays no per-segment
    /// `malloc`. Once one warm-up read sizes a pooled buffer to the max
    /// segment, every subsequent segment — larger or smaller — must reuse
    /// it: the pool's `alloc_misses` must not grow and the recycled buffer
    /// keeps the high-water capacity, with payload intact.
    #[kithara::test]
    fn pooled_segment_buffer_recycles_without_per_segment_malloc() {
        let max_size = 400usize;
        let sizes = [120usize, max_size, 80, max_size, 360];
        let mut blob = Vec::new();
        let mut ranges = Vec::new();
        let mut at = 0u64;
        for (i, &size) in sizes.iter().enumerate() {
            let start = at;
            blob.extend(std::iter::repeat_n(u8::try_from(i + 1).unwrap_or(0), size));
            at += size as u64;
            ranges.push(start..at);
        }
        let layout = FixedLayout {
            segments: ranges.clone(),
        };
        let mut source: BoxedSource = Box::new(Cursor::new(blob));
        // Dedicated pool (trim disabled) so `alloc_misses` is deterministic
        // and recycled buffers keep their high-water capacity.
        let pool = BytePool::new(32, 0);

        // Warm the home shard with a buffer grown to the high-water size,
        // then drop it back into the pool.
        {
            let mut warm = SegmentReadState::new(ranges[1].clone(), pool.get());
            let status =
                fill_segment_buffer(&mut source, &mut warm, LiveRange::Segment(&layout, 1))
                    .expect("BUG: warm fill");
            assert_eq!(status, FillStatus::Ready);
        }
        let warm_misses = pool.stats().alloc_misses;

        for (idx, range) in ranges.iter().enumerate() {
            let mut state = SegmentReadState::new(range.clone(), pool.get());
            let status = fill_segment_buffer(
                &mut source,
                &mut state,
                LiveRange::Segment(&layout, u32::try_from(idx).unwrap_or(0)),
            )
            .expect("BUG: fill segment");
            assert_eq!(status, FillStatus::Ready);
            assert_eq!(state.buffer.len(), sizes[idx]);
            assert_eq!(
                state.buffer[0],
                u8::try_from(idx + 1).unwrap_or(0),
                "segment {idx} payload must be intact after recycling",
            );
            assert!(
                state.buffer.capacity() >= max_size,
                "segment {idx} must reuse the high-water capacity, not realloc",
            );
        }

        assert_eq!(
            pool.stats().alloc_misses,
            warm_misses,
            "warm pool must serve every segment buffer without a fresh malloc",
        );
    }

    /// A `Read + Seek` source that hands back at most `chunk` bytes per
    /// `read`, so draining one segment takes several fill iterations — the
    /// partial-read shape the live HLS source produces, and the
    /// precondition for an end-shrink to land *mid-fill* (after some bytes
    /// are already buffered).
    struct ChunkSource {
        data: Vec<u8>,
        pos: u64,
        chunk: usize,
    }

    impl Read for ChunkSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let start = usize::try_from(self.pos).unwrap_or(usize::MAX);
            if start >= self.data.len() {
                return Ok(0);
            }
            let n = self.chunk.min(buf.len()).min(self.data.len() - start);
            buf[..n].copy_from_slice(&self.data[start..start + n]);
            self.pos += n as u64;
            Ok(n)
        }
    }

    impl Seek for ChunkSource {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            let abs = match from {
                SeekFrom::Start(s) => s,
                SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
                SeekFrom::End(d) => (self.data.len() as u64).saturating_add_signed(d),
            };
            self.pos = abs;
            Ok(abs)
        }
    }

    /// Single-segment layout whose end shrinks by a few bytes once
    /// `segment_at_index` has been polled `shrink_after` times — the
    /// post-decrypt PKCS7 size correction (`S0 -> S0'`) seen through the
    /// fill loop's live re-resolve, timed to land between the two
    /// `refresh_range` calls of one fill iteration.
    struct ShrinkingLayout {
        polls: AtomicUsize,
        full_end: u64,
        shrunk_end: u64,
        shrink_after: usize,
    }

    impl ShrinkingLayout {
        fn desc(range: Range<u64>) -> SegmentDescriptor {
            SegmentDescriptor::new(range, Duration::ZERO, Duration::from_secs(1), 0, 0)
        }

        fn range(&self) -> Range<u64> {
            let n = self.polls.fetch_add(1, Ordering::SeqCst);
            let end = if n >= self.shrink_after {
                self.shrunk_end
            } else {
                self.full_end
            };
            0..end
        }
    }

    impl ByteMap for ShrinkingLayout {
        fn init_segment_range(&self) -> Range<u64> {
            0..0
        }

        fn len(&self) -> Option<u64> {
            Some(self.full_end)
        }

        fn segment_after_byte(&self, _byte_offset: u64) -> Option<SegmentDescriptor> {
            None
        }

        fn segment_at_index(&self, _idx: u32) -> Option<SegmentDescriptor> {
            Some(Self::desc(self.range()))
        }

        fn segment_at_time(&self, _t: Duration) -> Option<SegmentDescriptor> {
            Some(Self::desc(0..self.full_end))
        }

        fn segment_count(&self) -> Option<u32> {
            Some(1)
        }
    }

    /// R-fmp4shrink: when a DRM segment's end shrinks mid-fill (PKCS7 trims
    /// the HEAD estimate after some bytes are already read), the
    /// already-read prefix `[0, filled)` is valid — `refresh_range` keeps
    /// `state.filled`, so the prefix MUST survive the buffer resize.
    /// Regression for the `fill_segment_buffer` bug where the post-seek
    /// re-resolve called `buffer.clear()` and zeroed that prefix, handing
    /// `re_mp4` a buffer whose box-size field read `0x00000000` ("invalid
    /// box size 0 at offset 0" -> decode failure -> producer failed).
    #[kithara::test]
    fn mid_fill_end_shrink_keeps_already_read_prefix() {
        let full = 16usize;
        let shrunk = 12usize;
        // Distinct non-zero payload so a zeroed prefix is unambiguous.
        let blob: Vec<u8> = (1..=u8::try_from(full).unwrap_or(0)).collect();
        let mut source: BoxedSource = Box::new(ChunkSource {
            data: blob.clone(),
            pos: 0,
            chunk: 6,
        });
        // Shrink on the 4th poll: it is the post-seek `refresh_range` of the
        // second iteration, after 6 bytes are already buffered.
        let layout = ShrinkingLayout {
            full_end: full as u64,
            shrunk_end: shrunk as u64,
            shrink_after: 3,
            polls: AtomicUsize::new(0),
        };
        let pool = BytePool::new(32, 0);
        let mut state = SegmentReadState::new(0..full as u64, pool.get());

        let status = fill_segment_buffer(&mut source, &mut state, LiveRange::Segment(&layout, 0))
            .expect("fill must not error");

        assert_eq!(status, FillStatus::Ready);
        assert_eq!(
            &state.buffer[..],
            &blob[..shrunk],
            "already-read prefix must survive the end-shrink resize, not be zeroed",
        );
    }
}
