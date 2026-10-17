use std::sync::atomic::AtomicU64;

use kithara_bufpool::{PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioSpec;
use kithara_stream::{BoxedEventSink, PendingReason};

use crate::{
    PcmOrigin,
    codec::FrameCodec,
    demuxer::{Demuxer, Frame},
    error::DecodeResult,
    traits::DecoderChunkOutcome,
};

/// Frames the decoder drops from the head of its decoded signal, counted as they are
/// dropped rather than declared up front.
///
/// `supplied` comes from the packet's own duration converted at the output
/// sample rate, so it is already in the domain the decoder emits in: an
/// SBR codec whose access unit is 1024 core frames but 2048 output frames
/// is measured against 2048, not against a core-rate constant.
#[derive(Clone, Copy, Debug, Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(super) struct HeadStrip {
    pub(super) settled: bool,
    #[field(get, vis = "pub(super)", copy)]
    pub(super) frames: u64,
}

#[derive(Clone, Copy)]
pub(super) enum SeekTrim {
    Nearest(Duration),
    Prefix(Duration),
}

/// Generic decoder built by composition: a [`Demuxer`] feeds raw frames
/// into a [`FrameCodec`] which produces PCM. One implementation, one
/// dispatch path - no per-backend duplication.
pub(crate) struct ComposedDecoder<D: Demuxer, C: FrameCodec, S> {
    pub(super) spec: AudioSpec,
    pub(super) codec: C,
    pub(super) demuxer: D,
    pub(super) head_strip: HeadStrip,
    pub(super) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(super) duration: Option<crate::DecoderDuration>,
    pub(super) origin: PcmOriginState,
    /// Reader-side event sink. Single-owner `Box<dyn ReaderEventSink>` -
    /// `None` skips emission entirely; `Some(_)` calls `on_chunk` /
    /// `on_seek` directly via `&mut` after the inner outcome resolves.
    /// No lock on the produce-core. Folded in from the former
    /// `HookedDecoder` decorator - every decoder is hookable now.
    pub(super) hooks: Option<BoxedEventSink>,
    pub(super) output: Option<DecodeResult<SampleBuffer>>,
    /// When `Some`, frames whose decode-time end is `<= target` are
    /// dropped before the next chunk is emitted. Cleared after the
    /// first frame past the target is consumed. The intent selects nearest
    /// PCM for ordinary seeks or preserves a native prefix for rendering.
    pub(super) pending_seek_target: Option<SeekTrim>,
    pub(super) pools: PoolRegion<S>,
    pub(super) prefill: Prefill,
    /// Set on every seek; the next emitted chunk may re-anchor the PCM cursor.
    pub(super) resync_frame_offset_to_pts: bool,
    pub(super) zero_frame_count: u32,
    /// Cumulative frame counter. Anchored to `landed_at` on seek (so the
    /// next chunk's `frame_offset / sample_rate ~= timestamp`) and
    /// incremented by `decoded.frames` per emitted chunk. Tracking it
    /// cumulatively avoids the precision loss that sneaks in if every
    /// chunk recomputes `floor(pts * sample_rate)` from a `Duration`
    /// nanosecond value.
    pub(super) frame_offset: u64,
    pub(super) timeline_gap_frames: u64,
}

/// Runtime wiring required to build a [`ComposedDecoder`].
///
/// The host must provide its configured pool region explicitly.
pub(crate) struct DecoderRuntime<S> {
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    pub(crate) hooks: Option<BoxedEventSink>,
    pub(crate) pools: PoolRegion<S>,
}

#[derive(Clone, Copy)]
pub(super) enum PcmOriginState {
    Observing(Option<Duration>),
    Captured(PcmOrigin),
    Sealed,
}

pub(super) enum Prefill {
    Seeking(PendingReason),
    Needed,
    Buffered(DecodeResult<DecoderChunkOutcome>),
    Complete,
}

impl PcmOriginState {
    pub(super) fn observe_packet(&mut self, frame: Option<Frame<'_>>, at_eof: bool) {
        if !at_eof
            && let Some(frame) = frame
            && let Self::Observing(packet) = self
            && packet.is_none()
        {
            *packet = Some(frame.pts);
        }
    }
}
