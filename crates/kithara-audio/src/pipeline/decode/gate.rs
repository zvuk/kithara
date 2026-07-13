use std::ops::Range;

use kithara_decode::InputRequirement;
use kithara_platform::sync::Arc;
use kithara_stream::{DeferredWake, SeekControl, SourcePhase, StreamType};
use tracing::trace;

use crate::pipeline::{
    stream::shared::SharedStream,
    track_fsm::{
        ApplySeekState, RecreateState, ResumeState, SeekMode, WaitContext, WaitingReason,
        map_source_phase,
    },
};

const DEFAULT_READ_AHEAD_BYTES: u64 = 32 * 1024;

/// Owns source-readiness ranges and the lock-free peer-wake contract.
///
/// Gate and wait callers resolve ranges exclusively through this API. The
/// range helpers remain private to this module so playback cannot clone byte
/// policy outside `ReadinessGate`; `SharedStream` remains byte-space ground
/// truth. Peer wake is armed on the forbid-blocking produce core and flushed
/// by the scheduler shell, keeping the cross-thread `notify_one` off RT.
/// Holding the handle directly instead of resolving it through the stream
/// mutex also keeps `arm` out of the lock held at seek-finalization sites.
pub(crate) struct ReadinessGate {
    peer_wake: Option<Arc<DeferredWake>>,
}

impl ReadinessGate {
    pub(crate) fn new(peer_wake: Option<Arc<DeferredWake>>) -> Self {
        Self { peer_wake }
    }

    pub(crate) fn arm_peer_wake(&self) {
        if let Some(ref wake) = self.peer_wake {
            wake.arm();
        }
    }

    pub(crate) fn flush_peer_wake(&self) {
        if let Some(ref wake) = self.peer_wake {
            wake.flush();
        }
    }

    /// Clear seek-pending and arm the peer in one wait-free produce-core step.
    ///
    /// The peer needs one more `poll_next` after completion to release its ABR
    /// lock; the scheduler shell delivers the armed wake outside RT.
    pub(crate) fn finalize_seek_pending(&self, seek: &dyn SeekControl, epoch: u64) {
        seek.clear_pending(epoch);
        self.arm_peer_wake();
    }

    pub(crate) fn source_is_ready<T: StreamType>(&self, stream: &SharedStream<T>) -> bool {
        self.source_ready_for_range(stream, chunk_lookahead_range(stream, stream.position()))
    }

    pub(crate) fn source_is_ready_for_chunk<T: StreamType>(
        &self,
        stream: &SharedStream<T>,
        byte: u64,
    ) -> bool {
        self.source_ready_for_range(stream, chunk_lookahead_range(stream, byte))
    }

    pub(crate) fn source_is_ready_for_apply_seek<T: StreamType>(
        &self,
        stream: &SharedStream<T>,
        applying: ApplySeekState,
    ) -> bool {
        match applying.mode {
            SeekMode::Anchor(anchor) => {
                self.source_is_ready_for_seek_landing(stream, anchor.byte_offset)
            }
            SeekMode::Direct {
                target_byte: Some(byte),
            } => self.source_is_ready_for_seek_landing(stream, byte),
            SeekMode::Direct { target_byte: None } => self.source_is_ready(stream),
        }
    }

    pub(crate) fn source_park<T: StreamType>(
        &self,
        stream: &SharedStream<T>,
        phase: SourcePhase,
    ) -> Option<WaitingReason> {
        let reason = map_source_phase(phase)?;
        trace!(
            ?phase,
            ?reason,
            pos = stream.position(),
            "source_park: parking on source phase"
        );
        self.arm_peer_wake();
        Some(reason)
    }

    pub(crate) fn source_ready_for_recreate<T: StreamType>(
        &self,
        stream: &SharedStream<T>,
        recreate: &RecreateState,
    ) -> bool {
        let phase = recreate_phase(stream, recreate);
        if self.source_park(stream, phase).is_some() {
            return false;
        }
        matches!(
            phase,
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }

    fn source_is_ready_for_seek_landing<T: StreamType>(
        &self,
        stream: &SharedStream<T>,
        byte: u64,
    ) -> bool {
        let end = seek_landing_end(stream, byte);
        self.source_ready_for_range(stream, byte..end)
    }

    fn source_ready_for_range<T: StreamType>(
        &self,
        stream: &SharedStream<T>,
        range: Range<u64>,
    ) -> bool {
        let phase = stream.phase_at(range);
        if self.source_park(stream, phase).is_some() {
            return false;
        }
        matches!(
            phase,
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }
}

pub(crate) fn post_seek_anchor_offset<T: StreamType>(
    stream: &SharedStream<T>,
    resume: &ResumeState,
) -> Option<u64> {
    let offset = resume.anchor_offset?;
    let Some(anchor_variant) = resume.anchor_variant_index else {
        return Some(offset.max(stream.position()));
    };
    let current_variant = stream
        .abr_handle()
        .and_then(|handle| handle.current_variant_index());
    match current_variant {
        Some(current) if current == anchor_variant => Some(offset.max(stream.position())),
        _ => None,
    }
}

pub(crate) fn recreate_phase<T: StreamType>(
    stream: &SharedStream<T>,
    recreate: &RecreateState,
) -> SourcePhase {
    stream.phase_at(recreate_ready_range(stream, recreate))
}

fn source_phase_for_seek_landing<T: StreamType>(
    stream: &SharedStream<T>,
    byte: u64,
) -> SourcePhase {
    let end = seek_landing_end(stream, byte);
    stream.phase_at(byte..end)
}

pub(crate) fn source_phase_for_wait_context<T: StreamType>(
    stream: &SharedStream<T>,
    context: &WaitContext,
) -> SourcePhase {
    match context {
        WaitContext::ApplySeek(applying) => match applying.mode {
            SeekMode::Anchor(anchor) => source_phase_for_seek_landing(stream, anchor.byte_offset),
            SeekMode::Direct {
                target_byte: Some(byte),
            } => source_phase_for_seek_landing(stream, byte),
            SeekMode::Direct { target_byte: None } => stream.phase(),
        },
        WaitContext::Recreation(recreate) => recreate_phase(stream, recreate),
        WaitContext::PostSeek(resume) => post_seek_anchor_offset(stream, resume).map_or_else(
            || stream.phase(),
            |byte| stream.phase_at(chunk_lookahead_range(stream, byte)),
        ),
        WaitContext::Playback => source_phase_forward(stream),
        WaitContext::Seek(_) => stream.phase(),
    }
}

/// Byte range whose readiness gates decoder recreation, shared by the gate
/// and wait paths so the two never disagree. The demuxer contract picks the
/// shape; this layer resolves it in virtual byte-space. An init-bearing
/// recreate gates on the init header when it remains addressable and bounded;
/// otherwise it uses the standard offset read-ahead window.
fn recreate_ready_range<T: StreamType>(
    stream: &SharedStream<T>,
    recreate: &RecreateState,
) -> Range<u64> {
    let byte_map = stream.byte_map();
    let input = kithara_decode::DecoderFactory::input_requirement(
        &recreate.media_info,
        byte_map.as_deref(),
    );
    if matches!(input, InputRequirement::Incremental) {
        return recreate.offset..boundary_end(stream, recreate.offset);
    }
    if let Ok(init_range) = stream.format_change_segment_range()
        && init_range.end.saturating_sub(init_range.start) <= DEFAULT_READ_AHEAD_BYTES
    {
        return init_range;
    }
    recreate.offset..boundary_end(stream, recreate.offset)
}

/// End of the byte range required for the first chunk after a seek landing:
/// the containing segment end, or standard look-ahead for raw sources, always
/// clamped to source length so the gate never waits beyond EOF.
fn seek_landing_end<T: StreamType>(stream: &SharedStream<T>, byte: u64) -> u64 {
    let segment_end = stream
        .byte_map()
        .and_then(|layout| layout.segment_at_byte(byte))
        .map(|seg| seg.byte_range.end);
    let end = segment_end.unwrap_or_else(|| boundary_end(stream, byte));
    stream.len().map_or(end, |len| end.min(len))
}

/// Segment-clamped look-ahead used by the next-chunk readiness gate.
///
/// Exactly on a segment boundary this range may be empty. That is deliberate:
/// the demuxer may still have buffered input from the previous segment and
/// must be allowed to drain it before a pending decode moves to the wider
/// playback wait window.
fn chunk_lookahead_range<T: StreamType>(stream: &SharedStream<T>, byte: u64) -> Range<u64> {
    let lookahead_end = byte.saturating_add(DEFAULT_READ_AHEAD_BYTES);
    let check_end = stream
        .byte_map()
        .and_then(|layout| layout.segment_after_byte(byte))
        .map_or(lookahead_end, |next| {
            next.byte_range.start.min(lookahead_end)
        });
    let check_end = stream.len().map_or(check_end, |len| check_end.min(len));
    byte..check_end
}

/// Playback wait phase for the decoder's forward read-ahead window.
///
/// Unlike the next-chunk gate, this is not segment-clamped: container parsing
/// crosses the boundary, so a single-byte or current-segment wait would report
/// ready while the decoder is blocked on the withheld next segment and would
/// hot-spin the worker.
fn source_phase_forward<T: StreamType>(stream: &SharedStream<T>) -> SourcePhase {
    let pos = stream.position();
    let end = pos.saturating_add(DEFAULT_READ_AHEAD_BYTES);
    let end = stream.len().map_or(end, |len| end.min(len));
    stream.phase_at(pos..end)
}

fn boundary_end<T: StreamType>(stream: &SharedStream<T>, start: u64) -> u64 {
    stream.len().map_or_else(
        || start.saturating_add(DEFAULT_READ_AHEAD_BYTES),
        |len| start.saturating_add(DEFAULT_READ_AHEAD_BYTES).min(len),
    )
}
