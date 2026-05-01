use std::{
    io::{Seek, SeekFrom},
    ops::Range,
};

use kithara_stream::{PendingReason, StreamReadError};

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
            .expect("segment range fits usize on supported targets")
    }
}

/// Drive a `BoxedSource` to fill `state.buffer` with all bytes in
/// `state.range`. Resumable across multiple calls.
pub(crate) fn fill_segment_buffer(
    source: &mut BoxedSource,
    state: &mut SegmentReadState,
) -> DecodeResult<FillStatus> {
    let total = state.total();
    if state.buffer.len() != total {
        state.buffer.clear();
        state.buffer.resize(total, 0);
    }

    while state.filled < total {
        let abs_offset = state.range.start + state.filled as u64;
        source.seek(SeekFrom::Start(abs_offset))?;
        match source
            .try_read(&mut state.buffer[state.filled..])
            .map_err(map_stream_err)?
        {
            InputReadOutcome::Bytes(n) => state.filled += n.get(),
            InputReadOutcome::Pending(reason) => return Ok(FillStatus::Pending(reason)),
            InputReadOutcome::Eof => {
                if state.filled == total {
                    break;
                }
                return Err(DecodeError::InvalidData(format!(
                    "unexpected EOF before segment buffer filled: {} / {}",
                    state.filled, total
                )));
            }
        }
    }
    Ok(FillStatus::Ready)
}
