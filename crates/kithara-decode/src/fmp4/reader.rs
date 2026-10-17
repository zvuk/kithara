use std::{
    io::{Seek, SeekFrom},
    ops::Range,
};

use kithara_bufpool::ByteBuffer;
use kithara_stream::{ByteMap, PendingReason, StreamPending, StreamReadError, VariantChangeError};
use kithara_test_utils::kithara;

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{BoxedSource, InputReadOutcome},
};

fn map_stream_err(err: StreamReadError) -> DecodeError {
    match err {
        StreamReadError::Source(io_err) => DecodeError::from(io_err),
        _ => DecodeError::parse("stream read", err),
    }
}

fn seek_input(source: &mut BoxedSource, position: u64) -> DecodeResult<FillStatus> {
    match source.seek(SeekFrom::Start(position)) {
        Ok(_) => Ok(FillStatus::Ready),
        Err(error) => {
            let payload = error.get_ref();
            if let Some(reason) = payload.and_then(|value| value.downcast_ref::<PendingReason>()) {
                return Ok(FillStatus::Pending(*reason));
            }
            if let Some(pending) = payload.and_then(|value| value.downcast_ref::<StreamPending>()) {
                return Ok(FillStatus::Pending(pending.reason()));
            }
            if let Some(value) = payload
                && value.is::<VariantChangeError>()
            {
                return Ok(FillStatus::Pending(PendingReason::VariantChange));
            }
            Err(error.into())
        }
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

/// Pool-backed segment read buffer. Drawn from the host pool region and
/// returned to it on drop, so a steady-state decode loop recycles one
/// high-water allocation across segments instead of mallocing per
/// segment.
pub(crate) type SegmentBuf = ByteBuffer;

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
    /// freshly drawn from the host pool region. The buffer's retained
    /// capacity (high-water mark carried over from a previous segment
    /// the pool recycled) is reused; [`Self::sync_buffer_ready`] only
    /// reallocates when this segment is larger than any seen before.
    pub(crate) const fn new(range: Range<u64>, buffer: SegmentBuf) -> Self {
        Self {
            range,
            buffer,
            filled: 0,
        }
    }

    /// Set `buffer` length to exactly `total`. Growth goes through the
    /// budget-tracked [`ByteBuffer::ensure_len`] (a plain `resize` via
    /// `DerefMut` would leak the pool's byte budget); shrink uses
    /// `truncate`, which keeps capacity so the high-water mark survives.
    fn resize_to(&mut self, total: usize) -> DecodeResult<()> {
        if self.buffer.len() == total {
            return Ok(());
        }
        self.buffer.ensure_len(total)?;
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
#[kithara::hang_watchdog]
pub(crate) fn fill_segment_buffer(
    source: &mut BoxedSource,
    state: &mut SegmentReadState,
    live: LiveRange<'_>,
) -> DecodeResult<FillStatus> {
    loop {
        hang_tick!();
        refresh_range(state, live);
        if state.sync_buffer_ready()? {
            return Ok(FillStatus::Ready);
        }

        let abs_offset = state.range.start + state.filled as u64;
        if let FillStatus::Pending(reason) = seek_input(source, abs_offset)? {
            return Ok(FillStatus::Pending(reason));
        }
        if refresh_range(state, live) {
            let total_after = state.total();
            state.resize_to(total_after)?;
            if state.filled >= total_after {
                return Ok(FillStatus::Ready);
            }
            let corrected = state.range.start + state.filled as u64;
            if let FillStatus::Pending(reason) = seek_input(source, corrected)? {
                return Ok(FillStatus::Pending(reason));
            }
        }

        match source
            .try_read(&mut state.buffer[state.filled..])
            .map_err(map_stream_err)?
        {
            InputReadOutcome::Bytes(n) => {
                state.filled += n.get();
                hang_reset!();
            }
            InputReadOutcome::Pending(reason) => return Ok(FillStatus::Pending(reason)),
            InputReadOutcome::Eof => return finish_segment_buffer(state, live),
        }
    }
}

fn finish_segment_buffer(
    state: &mut SegmentReadState,
    live: LiveRange<'_>,
) -> DecodeResult<FillStatus> {
    if state.filled == state.total() {
        return Ok(FillStatus::Ready);
    }
    if refresh_range(state, live) && state.sync_buffer_ready()? {
        return Ok(FillStatus::Ready);
    }
    Err(DecodeError::InvalidData {
        detail: "unexpected EOF before segment buffer filled",
    })
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
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use kithara_platform::time::Duration;
    use kithara_stream::{SegmentDescriptor, SourcePhase};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{test_pools::pools, traits::BoxedSource};

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
            SegmentDescriptor::builder()
                .byte_range(self.segments[idx].clone())
                .decode_time(Duration::ZERO)
                .duration(Duration::from_secs(1))
                .segment_index(u32::try_from(idx).unwrap_or(0))
                .variant_index(0)
                .build()
        }
    }

    /// R-fmp4buf: segment read buffers are drawn from the typed pool region and
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
        let pools = pools();

        {
            let mut warm = SegmentReadState::new(ranges[1].clone(), pools.get::<u8>());
            let status =
                fill_segment_buffer(&mut source, &mut warm, LiveRange::Segment(&layout, 1))
                    .expect("BUG: warm fill");
            assert_eq!(status, FillStatus::Ready);
        }

        for (idx, range) in ranges.iter().enumerate() {
            let mut state = SegmentReadState::new(range.clone(), pools.get::<u8>());
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
    }

    #[derive(Clone, Copy)]
    enum InputFault {
        Reason(PendingReason),
        Snapshot(PendingReason),
        Variant,
        Interrupted,
        Io,
    }

    impl InputFault {
        fn error(self) -> io::Error {
            match self {
                Self::Reason(reason) => io::Error::new(io::ErrorKind::Interrupted, reason),
                Self::Snapshot(reason) => io::Error::new(
                    io::ErrorKind::Interrupted,
                    StreamPending::new(reason, 6, 6, Some(12), SourcePhase::Waiting),
                ),
                Self::Variant => io::Error::new(io::ErrorKind::Interrupted, VariantChangeError),
                Self::Interrupted => io::ErrorKind::Interrupted.into(),
                Self::Io => io::Error::new(io::ErrorKind::PermissionDenied, "fixture read denied"),
            }
        }

        fn pending(self) -> Option<PendingReason> {
            match self {
                Self::Reason(reason) | Self::Snapshot(reason) => Some(reason),
                Self::Variant => Some(PendingReason::VariantChange),
                Self::Interrupted | Self::Io => None,
            }
        }
    }

    struct ChunkSource {
        data: Vec<u8>,
        pos: u64,
        chunk: usize,
        fault: Option<InputFault>,
        fault_on_seek: bool,
        reads: std::sync::Arc<Mutex<Vec<Range<u64>>>>,
    }

    impl ChunkSource {
        fn interrupt(&mut self, seek: bool) -> io::Result<()> {
            if self.pos == self.chunk as u64
                && seek == self.fault_on_seek
                && let Some(fault) = self.fault.take()
            {
                return Err(fault.error());
            }
            Ok(())
        }
    }

    impl Read for ChunkSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.interrupt(false)?;
            let start = usize::try_from(self.pos).unwrap_or(usize::MAX);
            if start >= self.data.len() {
                return Ok(0);
            }
            let n = self.chunk.min(buf.len()).min(self.data.len() - start);
            buf[..n].copy_from_slice(&self.data[start..start + n]);
            self.reads
                .lock()
                .expect("read ledger")
                .push(self.pos..self.pos + n as u64);
            self.pos += n as u64;
            Ok(n)
        }
    }

    impl Seek for ChunkSource {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.interrupt(true)?;
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
            SegmentDescriptor::builder()
                .byte_range(range)
                .decode_time(Duration::ZERO)
                .duration(Duration::from_secs(1))
                .segment_index(0)
                .variant_index(0)
                .build()
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
    #[case::ready(None, false)]
    #[case::read_retired(Some(InputFault::Reason(PendingReason::SessionRetired)), false)]
    #[case::seek_retired(Some(InputFault::Reason(PendingReason::SessionRetired)), true)]
    #[case::seek_snapshot_retired(Some(InputFault::Snapshot(PendingReason::SessionRetired)), true)]
    #[case::seek_variant(Some(InputFault::Variant), true)]
    #[case::seek_retry(Some(InputFault::Reason(PendingReason::Retry)), true)]
    #[case::seek_interrupted(Some(InputFault::Interrupted), true)]
    #[case::seek_io_error(Some(InputFault::Io), true)]
    #[case::read_io_error(Some(InputFault::Io), false)]
    fn mid_fill_end_shrink_keeps_already_read_prefix(
        #[case] fault: Option<InputFault>,
        #[case] fault_on_seek: bool,
    ) {
        let full = 16usize;
        let shrunk = 12usize;
        // Distinct non-zero payload so a zeroed prefix is unambiguous.
        let blob: Vec<u8> = (1..=u8::try_from(full).unwrap_or(0)).collect();
        let reads = std::sync::Arc::new(Mutex::new(Vec::new()));
        let mut source: BoxedSource = Box::new(ChunkSource {
            data: blob.clone(),
            pos: 0,
            chunk: 6,
            fault,
            fault_on_seek,
            reads: std::sync::Arc::clone(&reads),
        });
        // Shrink on the 4th poll: it is the post-seek `refresh_range` of the
        // second iteration, after 6 bytes are already buffered.
        let layout = ShrinkingLayout {
            full_end: full as u64,
            shrunk_end: shrunk as u64,
            shrink_after: 3,
            polls: AtomicUsize::new(0),
        };
        let pools = pools();
        let mut state = SegmentReadState::new(0..full as u64, pools.get::<u8>());

        let result = fill_segment_buffer(&mut source, &mut state, LiveRange::Segment(&layout, 0));
        if let Some(fault) = fault {
            assert_eq!(state.filled, 6);
            assert_eq!(&state.buffer[..6], &blob[..6]);
            if let Some(reason) = fault.pending() {
                assert_eq!(
                    result.expect("typed source status"),
                    FillStatus::Pending(reason)
                );
                assert_eq!(
                    fill_segment_buffer(&mut source, &mut state, LiveRange::Segment(&layout, 0))
                        .expect("resume same read"),
                    FillStatus::Ready
                );
            } else {
                match (fault, result) {
                    (InputFault::Interrupted, Err(DecodeError::Interrupted)) => {}
                    (InputFault::Io, Err(DecodeError::Io { source })) => {
                        assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
                        assert_eq!(source.to_string(), "fixture read denied");
                    }
                    _ => panic!("genuine source failures must retain their error semantics"),
                }
                return;
            }
        } else {
            assert_eq!(result.expect("fill must not error"), FillStatus::Ready);
        }
        assert_eq!(
            reads.lock().expect("read ledger").as_slice(),
            &[0..6, 6..12],
            "the completed prefix must be read only once"
        );
        assert_eq!(
            &state.buffer[..],
            &blob[..shrunk],
            "already-read prefix must survive the end-shrink resize, not be zeroed",
        );
    }
}
