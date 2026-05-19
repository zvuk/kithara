use std::{
    io::{Seek, SeekFrom},
    ops::Range,
};

use kithara_stream::{PendingReason, SegmentLayout, StreamReadError};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{BoxedSource, InputReadOutcome},
};

fn map_stream_err(err: StreamReadError) -> DecodeError {
    match err {
        StreamReadError::Source(io_err) => DecodeError::from(io_err),
        _ => DecodeError::InvalidData(format!("unknown stream read error: {err:?}")),
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
    pub(crate) buffer: Vec<u8>,
    pub(crate) filled: usize,
}

impl SegmentReadState {
    pub(crate) fn new(range: Range<u64>) -> Self {
        Self {
            range,
            buffer: Vec::new(),
            filled: 0,
        }
    }

    pub(crate) fn total(&self) -> usize {
        usize::try_from(self.range.end - self.range.start)
            .expect("BUG: segment range fits usize on supported targets")
    }
}

/// Which range in the live layout to re-resolve `state.range` against
/// on each iteration. Init reads (no `segment_index`) must re-query
/// [`SegmentLayout::init_segment_range`] because the post-decrypt init
/// size on DRM streams can shrink between cursor setup and the actual
/// read (PKCS7 padding strips up to 16 bytes off the encrypted estimate).
/// Media reads must re-query [`SegmentLayout::segment_at_index`] for the
/// same reason on individual segment sizes.
#[derive(Clone, Copy)]
pub(crate) enum LiveRange<'a> {
    Init(&'a dyn SegmentLayout),
    Segment(&'a dyn SegmentLayout, u32),
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
pub(crate) fn fill_segment_buffer(
    source: &mut BoxedSource,
    state: &mut SegmentReadState,
    live: LiveRange<'_>,
) -> DecodeResult<FillStatus> {
    loop {
        refresh_range(state, live);
        let total = state.total();
        if state.buffer.len() != total {
            state.buffer.resize(total, 0);
        }
        if state.filled >= total {
            return Ok(FillStatus::Ready);
        }

        let abs_offset = state.range.start + state.filled as u64;
        source.seek(SeekFrom::Start(abs_offset))?;
        if refresh_range(state, live) {
            let total_after = state.total();
            if state.buffer.len() != total_after {
                state.buffer.clear();
                state.buffer.resize(total_after, 0);
            }
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
                if refresh_range(state, live) {
                    let new_total = state.total();
                    if state.buffer.len() != new_total {
                        state.buffer.resize(new_total, 0);
                    }
                    if state.filled >= new_total {
                        return Ok(FillStatus::Ready);
                    }
                }
                return Err(DecodeError::InvalidData(format!(
                    "unexpected EOF before segment buffer filled: {} / {}",
                    state.filled,
                    state.total()
                )));
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
