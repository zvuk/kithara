use std::{
    any::Any,
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use delegate::delegate;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, ErrorClass,
    GaplessMode, PcmChunk, PcmSpec, duration_for_frames,
};
use kithara_events::{AudioEvent, AudioFormat, DeferredBus, SeekLifecycleStage, SegmentLocation};
use kithara_platform::Mutex;
use kithara_stream::{
    ContainerFormat, MediaInfo, PendingReason, SourcePhase, SourceSeekAnchor, Stream, StreamType,
    Timeline,
};
use kithara_test_utils::kithara;
use tracing::{debug, trace, warn};

use crate::{
    pipeline::{
        fetch::Fetch,
        gapless::GaplessStage,
        track_fsm::{
            ApplySeekState, ApplyingSeek, AwaitingResume, CurrentFsm, DecoderSession, Decoding,
            RecreateCause, RecreateNext, RecreateOutcome, RecreateState, RecreatingDecoder,
            ResumeState, SeekContext, SeekMode, SeekRequest, SeekRequested, Track, TrackFailure,
            TrackStep, WaitContext, WaitState, WaitingForSource, WaitingReason, map_source_phase,
        },
    },
    traits::AudioEffect,
    worker::{AudioWorkerSource, apply_effects, drain_effects, reset_effects},
};

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
pub(crate) struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
    /// Construction-phase read mode, shared across clones. When set, `Read`
    /// routes through the blocking off-RT [`Stream::read`] adapter (waits for
    /// the seeked range to download, cancel-bounded by its own timeout)
    /// instead of the non-blocking RT [`Stream::probe_read`]. `Audio::new`
    /// arms it for the single up-front decoder build (with the init body
    /// prefetched, the build read normally hits committed bytes; the blocking
    /// adapter is the bounded safety net for residual lateness) and disarms it
    /// before the worker is registered, so the RT decode loop the worker then
    /// drives always uses `probe_read`. See the crate `README.md`
    /// "Construction reads".
    blocking: Arc<std::sync::atomic::AtomicBool>,
}

impl<T: StreamType> SharedStream<T> {
    pub(crate) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
            blocking: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Arm/disarm the construction-phase blocking read mode (see the
    /// `blocking` field). Shared across all clones derived from this handle,
    /// so arming before the decoder build and disarming before worker
    /// registration is a staged construction → steady-state ownership
    /// transfer — never a live toggle once the RT worker runs.
    pub(crate) fn set_blocking(&self, on: bool) {
        self.blocking.store(on, Ordering::Release);
    }

    delegate! {
        to self.inner.lock_sync() {
            pub(crate) fn position(&self) -> u64;
            /// Absolute byte cursor set — forwards to the inner source's
            /// atomic, used post-seek when the audio FSM lands at a
            /// known byte position.
            pub(crate) fn set_position(&self, pos: u64);
            pub(crate) fn len(&self) -> Option<u64>;
            fn media_info(&self) -> Option<MediaInfo>;
            pub(crate) fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn current_segment_range(&self) -> Option<Range<u64>>;
            fn format_change_segment_range(&self) -> kithara_stream::StreamResult<Range<u64>>;
            pub(crate) fn clear_variant_fence(&self);
            pub(crate) fn has_variant_change_pending(&self) -> bool;
            fn seek_time_anchor(&self, position: Duration) -> Result<Option<SourceSeekAnchor>, io::Error>;
            fn commit_seek_landing(&self, anchor: Option<SourceSeekAnchor>);
            /// Build a fresh reader-side hooks instance from the inner source.
            pub(crate) fn take_reader_hooks(&self) -> Option<kithara_stream::BoxedHooks>;
            /// Pull a clone of the optional segment-layout handle from the
            /// inner source. Used by the decoder factory to activate the
            /// segment-by-segment fMP4 path on HLS.
            pub(crate) fn as_segment_layout(&self) -> Option<Arc<dyn kithara_stream::SegmentLayout>>;
            /// Get the shared timeline for flushing checks.
            pub(crate) fn timeline(&self) -> Timeline;
            /// Overall source readiness at current position.
            pub(crate) fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// The reader→peer wake handle — `Some` for segmented sources
            /// (HLS) that push a downloader peer. The FSM arms it on the
            /// produce core (seek-apply / finalize); the scheduler shell
            /// flushes it off the forbid-blocking path.
            pub(crate) fn peer_wake(&self) -> Option<Arc<kithara_stream::DeferredWake>>;
            /// Real-time on-core seek (FSM recreate/boundary, decoder
            /// `OffsetReader`): position math + cursor set, no `prime_seek_range`
            /// spin on the forbid-blocking produce core. See [`Stream::probe_seek`].
            pub(crate) fn probe_seek(&self, pos: SeekFrom) -> io::Result<u64>;
        }
    }
}

impl<T: StreamType> Clone for SharedStream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            blocking: Arc::clone(&self.blocking),
        }
    }
}

impl<T: StreamType> Read for SharedStream<T> {
    /// Steady state (RT worker / `OffsetReader`): the non-blocking
    /// [`Stream::probe_read`] — a not-ready range surfaces immediately so the
    /// scheduler parks and re-ticks. During construction only (the `blocking`
    /// flag armed by `Audio::new`), routes through the blocking off-RT
    /// [`Stream::read`] adapter so the single up-front decoder build waits for
    /// residual init lateness instead of erroring on the first not-ready probe.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut stream = self.inner.lock_sync();
        if self.blocking.load(Ordering::Acquire) {
            stream.read(buf)
        } else {
            stream.probe_read(buf)
        }
    }
}

impl<T: StreamType> Seek for SharedStream<T> {
    delegate! {
        to self.inner.lock_sync() {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
        }
    }
}

/// Reader that offsets all positions by a base offset.
///
/// When Symphonia seeks to position X, the real stream position is `base_offset + X`.
/// This is needed when recreating a decoder after ABR variant switch:
/// the new segment starts at `base_offset` in the virtual stream, but Symphonia
/// expects positions starting from 0.
pub(crate) struct OffsetReader<T: StreamType> {
    shared: SharedStream<T>,
    base_offset: u64,
}

impl<T: StreamType> OffsetReader<T> {
    pub(crate) fn new(shared: SharedStream<T>, base_offset: u64) -> Self {
        let _ = shared.probe_seek(SeekFrom::Start(base_offset));
        Self {
            shared,
            base_offset,
        }
    }
}

impl<T: StreamType> Read for OffsetReader<T> {
    delegate! {
        to self.shared {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
        }
    }
}

impl<T: StreamType> Seek for OffsetReader<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        // The decoder runs on the produce core, so seek through the real-time
        // `probe_seek` (no `prime_seek_range` spin on the forbid path).
        match pos {
            SeekFrom::Start(p) => {
                let abs = self.base_offset + p;
                let real_pos = self.shared.probe_seek(SeekFrom::Start(abs))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::Current(delta) => {
                let real_pos = self.shared.probe_seek(SeekFrom::Current(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::End(delta) => {
                let real_pos = self.shared.probe_seek(SeekFrom::End(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
        }
    }
}

/// Factory closure that creates a new decoder from stream, media info, and base offset.
///
/// Production: creates Symphonia `Decoder` via [`OffsetReader`].
/// Tests: returns `MockDecoder` without real I/O.
///
/// Returns `Result<_, DecodeError>` so the caller can distinguish a
/// **transient** failure (e.g. probe ran before the source buffered
/// `[0..PROBE)` of a freshly-switched variant — `ErrorClass::Interrupted`)
/// from a **hard** decoder/codec error. Transient errors must route to
/// `wait_for_source_on_recreate`, not `Failed(RecreateFailed)`.
pub(crate) type DecoderFactory<T> =
    Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Result<Box<dyn Decoder>, DecodeError> + Send>;

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(crate) struct StreamAudioSource<T: StreamType> {
    /// Decoder + `base_offset` + `media_info` as an atomic unit.
    pub(crate) session: DecoderSession,
    /// Cached timeline for lock-free flushing checks.
    pub(crate) timeline: Timeline,
    /// Explicit FSM state — single source of truth for track phase.
    pub(crate) state: CurrentFsm,
    epoch: Arc<AtomicU64>,
    decoder_factory: DecoderFactory<T>,
    /// Gapless trim mode applied per-track. Built once at construction;
    /// ABR variant switches inside one track keep the same trimmer
    /// (production semantics) so we never retrim audible content
    /// around a recreate boundary.
    gapless_mode: GaplessMode,
    /// Per-track gapless trimmer adapter.
    gapless: GaplessStage,
    /// Deferred sink for FSM lifecycle events ([`AudioEvent`]). The FSM runs on
    /// the produce core, so `emit_event` enqueues lock-free; the scheduler shell
    /// flushes via [`flush_deferred`](AudioWorkerSource::flush_deferred) and on
    /// `Drop`, keeping the cross-thread `broadcast::send` (a `kevent`) off the
    /// forbid path. `None` for sources built without an event bus.
    emit: Option<DeferredBus<AudioEvent>>,
    last_spec: Option<PcmSpec>,
    /// Reader→peer wake handle, resolved from [`Source::peer_wake`] at
    /// construction. The FSM runs on the produce core, so it `arm`s this
    /// (lock-free) at seek-apply / `finalize_seek_pending`; the scheduler shell
    /// flushes it off the forbid path. Holding the handle directly (not via the
    /// `SharedStream` mutex) keeps the arm out of the lock the FSM already holds
    /// at every `clear_seek_pending` callsite. `None` for file streams.
    peer_wake: Option<Arc<kithara_stream::DeferredWake>>,
    /// Buffered effect-chain tail produced by a single end-of-stream drain (`drain_effects`).
    /// `None` until true EOF arms it.
    eof_drain_queue: Option<VecDeque<PcmChunk>>,
    /// `(seek_epoch, target)` of the most recent applied seek.
    /// `committed_position` lags `target` until the seek's first
    /// (trim-aligned) chunk is consumed: the decoder lands at the
    /// containing segment's start and trims forward, so
    /// `commit_seek_landed` records the segment boundary, not the
    /// requested instant. A variant-switch recreate firing inside that
    /// window must resume at the real target, not at the lagging
    /// committed boundary — otherwise playback rewinds to the segment
    /// start. Tagged with the seek epoch so a later seek (especially a
    /// backward one) never resumes against a stale forward target. See
    /// `execute_recreation`.
    resume_target: Option<(u64, Duration)>,
    shared_stream: SharedStream<T>,
    effects: Vec<Box<dyn AudioEffect>>,
    chunks_decoded: u64,
    total_samples: u64,
    /// Absolute content frame offset just past the most recently emitted chunk
    /// (the producer's decode head), tagged with its epoch. A mid-playback
    /// variant-switch recreate continues the new decoder from here — NOT from
    /// the consumer's lagging `committed_position`: the chunks in
    /// `[committed..decode_head]` are already queued in the outlet ring (a
    /// `FormatBoundary` recreate neither flushes it nor bumps the seek epoch),
    /// so resuming at `committed` would re-emit them and rewind content. Stored
    /// as an exact frame (not a `Duration`) and converted back with
    /// `duration_for_frames`; the demuxer quantizes the seek landing to a
    /// sample and `frame_offset_for` rounds to the nearest frame, so the rebuilt
    /// decoder relabels its first chunk at exactly this frame. See
    /// `execute_recreation`.
    decode_head: Option<(u64, u64)>,
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Default read-ahead size in bytes when segment range is unknown.
    const DEFAULT_READ_AHEAD_BYTES: u64 = 32 * 1024;

    /// Nanoseconds per second for frame/duration conversion.
    const NANOS_PER_SEC: u128 = 1_000_000_000;

    pub(crate) fn new(
        shared_stream: SharedStream<T>,
        decoder: Box<dyn Decoder>,
        decoder_factory: DecoderFactory<T>,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
        effects: Vec<Box<dyn AudioEffect>>,
        gapless_mode: GaplessMode,
    ) -> Self {
        let timeline = shared_stream.timeline();
        let peer_wake = shared_stream.peer_wake();
        let gapless =
            GaplessStage::build(decoder.as_ref(), gapless_mode, initial_media_info.as_ref());
        let session = DecoderSession {
            decoder,
            base_offset: 0,
            media_info: initial_media_info,
            installed_at_seek_epoch: timeline.seek_epoch(),
        };
        timeline.set_playing(true);
        Self {
            shared_stream,
            session,
            decoder_factory,
            epoch,
            effects,
            eof_drain_queue: None,
            timeline,
            gapless_mode,
            gapless,
            peer_wake,
            state: CurrentFsm::decoding(),
            chunks_decoded: 0,
            total_samples: 0,
            last_spec: None,
            emit: None,
            resume_target: None,
            decode_head: None,
        }
    }

    fn active_seek_epoch(&self) -> Option<u64> {
        let epoch_from_recreate = |recreate: &RecreateState| match &recreate.next {
            RecreateNext::Decode => None,
            RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => {
                Some(request.seek.epoch)
            }
        };
        match &self.state {
            CurrentFsm::SeekRequested(h) => Some(h.data().seek.epoch),
            CurrentFsm::ApplyingSeek(h) => Some(h.data().request.seek.epoch),
            CurrentFsm::AwaitingResume(h) => Some(h.data().seek.epoch),
            CurrentFsm::RecreatingDecoder(h) => epoch_from_recreate(h.data()),
            CurrentFsm::WaitingForSource(h) => match &h.data().context {
                WaitContext::Seek(request) => Some(request.seek.epoch),
                WaitContext::ApplySeek(applying) => Some(applying.request.seek.epoch),
                WaitContext::Recreation(recreate) => epoch_from_recreate(recreate),
                WaitContext::Playback | WaitContext::PostSeek(_) => None,
            },
            CurrentFsm::Decoding(_) | CurrentFsm::AtEof(_) | CurrentFsm::Failed(_) => None,
        }
    }

    fn align_decoder_with_seek_anchor(
        &mut self,
        request: SeekRequest,
        anchor: SourceSeekAnchor,
    ) -> bool {
        let current_codec = self.session.media_info.as_ref().and_then(|info| info.codec);
        let stream_info = self.shared_stream.media_info();
        let target_codec = stream_info.as_ref().and_then(|info| info.codec);
        let current_variant: Option<usize> = self
            .session
            .media_info
            .as_ref()
            .and_then(|info| info.variant_index)
            .map(|v| v as usize);
        let target_variant: Option<usize> = anchor.variant_index.or_else(|| {
            stream_info
                .as_ref()
                .and_then(|info| info.variant_index)
                .map(|v| v as usize)
        });

        let codec_changed =
            matches!((current_codec, target_codec), (Some(from), Some(to)) if from != to);
        let variant_changed =
            matches!((current_variant, target_variant), (Some(from), Some(to)) if from != to);
        let target_container = stream_info
            .as_ref()
            .and_then(|info| info.container)
            .or_else(|| {
                self.session
                    .media_info
                    .as_ref()
                    .and_then(|info| info.container)
            });
        let needs_recreation = codec_changed || variant_changed;
        let recreate_offset = resolve_recreate_offset(
            &self.shared_stream,
            target_container,
            codec_changed,
            anchor.byte_offset,
        );
        trace!(
            ?current_codec,
            ?target_codec,
            ?current_variant,
            ?target_variant,
            anchor_variant = ?anchor.variant_index,
            codec_changed,
            variant_changed,
            needs_recreation,
            ?target_container,
            ?recreate_offset,
            base_offset = self.session.base_offset,
            "seek anchor alignment: compare format"
        );
        if !needs_recreation {
            return true;
        }

        let Some(recreate_offset) = recreate_offset else {
            self.fail_seek(
                request,
                DecodeError::InvalidData(format!(
                    "seek anchor alignment: {target_container:?} variant switch has \
                     no init segment range"
                )),
                "seek anchor alignment: no init segment range",
            );
            return false;
        };

        let target_variant_u32 = target_variant.and_then(|v| u32::try_from(v).ok());
        let target_info = stream_info.or_else(|| {
            self.session.media_info.clone().map(|mut info| {
                info.variant_index = target_variant_u32;
                info
            })
        });
        let Some(mut target_info) = target_info else {
            self.fail_seek(
                request,
                DecodeError::InvalidData(
                    "seek anchor alignment: variant/codec changed but media info unavailable"
                        .into(),
                ),
                "seek anchor alignment failed",
            );
            return false;
        };
        if let Some(v) = target_variant_u32 {
            target_info.variant_index = Some(v);
        }

        self.start_recreating_decoder(
            RecreateCause::VariantSwitch,
            target_info,
            RecreateNext::Seek(request),
            recreate_offset,
            request.attempt,
        );
        false
    }

    fn apply_aligned_anchor_seek(
        &mut self,
        request: SeekRequest,
        anchor: SourceSeekAnchor,
    ) -> bool {
        if !self.align_decoder_with_seek_anchor(request, anchor) {
            return false;
        }
        self.apply_time_anchor_seek(request, anchor)
    }

    /// Apply pending format change: clear fence, seek to segment start, recreate decoder.
    ///
    /// `Ok(())` = decoder recreated; `Err(DecodeError)` propagates the cause.
    /// Caller distinguishes transient (`ErrorClass::Interrupted`) from hard via
    /// [`DecodeError::classify`].
    #[kithara::probe(target_offset)]
    fn apply_format_change(
        &mut self,
        new_info: &MediaInfo,
        target_offset: u64,
    ) -> Result<(), DecodeError> {
        let current_pos = self.shared_stream.position();
        debug!(
            current_pos,
            target_offset,
            chunks_decoded = self.chunks_decoded,
            total_samples = self.total_samples,
            "apply_format_change: enter"
        );

        self.shared_stream.clear_variant_fence();

        self.shared_stream
            .probe_seek(SeekFrom::Start(target_offset))
            .map_err(|e| {
                warn!(?e, target_offset, "Failed to seek to segment boundary");
                DecodeError::from(e)
            })?;

        let pos_after_seek = self.shared_stream.position();
        debug!(
            target_offset,
            pos_after_seek, "apply_format_change: stream seeked, about to recreate decoder"
        );

        let result = self.recreate_decoder(new_info, target_offset);
        let pos_after_recreate = self.shared_stream.position();
        debug!(
            recreated = result.is_ok(),
            pos_after_recreate, "apply_format_change: exit"
        );
        result
    }

    fn apply_seek_applied(
        &mut self,
        epoch: u64,
        position: Duration,
        location: SegmentLocation,
        anchor_offset: Option<u64>,
    ) {
        self.reset_effect_chain();
        self.resume_target = Some((epoch, position));
        self.emit_seek_lifecycle(SeekLifecycleStage::SeekApplied, epoch, location);
        self.update_state(CurrentFsm::awaiting_resume(ResumeState {
            anchor_offset,
            seek: SeekContext {
                epoch,
                target: position,
            },
            ..Default::default()
        }));
    }

    fn apply_seek_from_decoder(&mut self, request: SeekRequest) -> bool {
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        let stream_pos = self.shared_stream.position();
        let segment_range = self.shared_stream.current_segment_range();
        debug!(
            ?position,
            epoch,
            attempt = request.attempt,
            stream_pos,
            ?segment_range,
            committed_position = ?self.timeline.committed_position(),
            variant = ?self
                .shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            "apply_seek_from_decoder: enter"
        );

        if let FormatChangeDetection::Applicable {
            target: new_info,
            target_offset,
        } = self.detect_format_change()
        {
            debug!(
                ?position,
                epoch,
                target_offset,
                current_stream_pos = stream_pos,
                ?segment_range,
                "seek: codec-changing format boundary pending, recreating decoder before seek"
            );
            self.start_recreating_decoder(
                RecreateCause::VariantSwitch,
                new_info,
                RecreateNext::ApplySeek(request),
                target_offset,
                request.attempt,
            );
            return false;
        }

        self.update_decoder_len_for_seek();

        let stream_len = self.shared_stream.len();
        debug!(
            ?position,
            epoch,
            stream_pos,
            ?segment_range,
            base_offset = self.session.base_offset,
            ?stream_len,
            "seek: about to call decoder.seek()"
        );
        if let Err(err) = self.decoder_seek_safe(position) {
            return self.recover_from_decoder_seek_error(
                request,
                err,
                position,
                epoch,
                self.session.base_offset,
                SeekMode::Direct {
                    target_byte: self.estimate_target_byte(position),
                },
            );
        }
        self.shared_stream.commit_seek_landing(None);

        self.apply_seek_applied(epoch, position, self.seek_context(), None);
        true
    }

    /// Trim or drop a freshly-decoded chunk against an in-flight seek
    /// skip. Returns the same [`DecoderChunkOutcome`] shape as
    /// [`Decoder::next_chunk`] so the decode loop carries one
    /// uniform three-way distinction across the whole pipeline:
    ///
    /// - [`DecoderChunkOutcome::Chunk`] — emit (no skip active, or skip
    ///   completed inside this chunk and the trimmed remainder is
    ///   ready to play).
    /// - [`DecoderChunkOutcome::Pending`] with
    ///   [`PendingReason::SeekPending`] — chunk was fully consumed by
    ///   the skip; caller must continue and fetch the next chunk.
    ///
    /// `Eof` is structurally impossible here (we don't observe stream
    /// termination from a chunk we just decoded).
    #[inline]
    fn apply_seek_skip(&mut self, epoch: u64, mut chunk: PcmChunk) -> DecoderChunkOutcome {
        let Some(remaining) = self.pending_skip_amount(epoch) else {
            return DecoderChunkOutcome::Chunk(chunk);
        };

        let spec = chunk.spec();
        let channels = usize::from(spec.channels.max(1));
        let chunk_frames = chunk.frames();
        if chunk_frames == 0 {
            return DecoderChunkOutcome::Pending(PendingReason::SeekPending);
        }

        let mut drop_frames = Self::frames_for_duration(spec, remaining);
        if drop_frames == 0 {
            drop_frames = 1;
        }

        if drop_frames >= chunk_frames {
            let dropped = Self::duration_for_frames(spec, chunk_frames);
            let next_remaining = remaining.saturating_sub(dropped);
            if let Some(state) = self.resume_state_mut() {
                state.skip = (!next_remaining.is_zero()).then_some(next_remaining);
            }
            return DecoderChunkOutcome::Pending(PendingReason::SeekPending);
        }

        let drop_samples = drop_frames.saturating_mul(channels);
        let len = chunk.pcm.len();
        chunk.pcm.copy_within(drop_samples..len, 0);
        chunk.pcm.truncate(len - drop_samples);

        chunk.meta.frame_offset = chunk.meta.frame_offset.saturating_add(drop_frames as u64);
        chunk.meta.timestamp = chunk
            .meta
            .timestamp
            .saturating_add(Self::duration_for_frames(spec, drop_frames));
        let dropped_u32 = u32::try_from(drop_frames).unwrap_or(u32::MAX);
        chunk.meta.frames = chunk.meta.frames.saturating_sub(dropped_u32);
        if let Some(state) = self.resume_state_mut() {
            state.skip = None;
        }
        DecoderChunkOutcome::Chunk(chunk)
    }

    fn apply_time_anchor_seek(&mut self, request: SeekRequest, anchor: SourceSeekAnchor) -> bool {
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        self.shared_stream.clear_variant_fence();
        self.update_decoder_len_for_seek();
        debug!(
            ?position,
            epoch,
            attempt = request.attempt,
            anchor_start = ?anchor.segment_start,
            anchor_byte_offset = anchor.byte_offset,
            anchor_variant = ?anchor.variant_index,
            stream_pos = self.shared_stream.position(),
            committed_position = ?self.timeline.committed_position(),
            variant = ?self
                .shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            "apply_time_anchor_seek: enter (anchor path)"
        );
        if let Err(err) = self.decoder_seek_safe(position) {
            return self.recover_from_decoder_seek_error(
                request,
                err,
                position,
                epoch,
                anchor.byte_offset,
                SeekMode::Anchor(anchor),
            );
        }
        trace!(
            ?position,
            anchor_start = ?anchor.segment_start,
            target_offset = anchor.byte_offset,
            "seek anchor path: exact decoder seek succeeded"
        );
        self.shared_stream.commit_seek_landing(Some(anchor));

        self.apply_seek_applied(
            epoch,
            position,
            self.seek_context(),
            Some(anchor.byte_offset),
        );
        true
    }

    /// Pin the timeline playhead to the decoder's actual landing point
    /// from a [`DecoderSeekOutcome`]. The decoder is the only source
    /// of ground truth — both for `landed_frame` (frame counter) and
    /// for the wall-clock position it parked at; we never recompute
    /// `frame * 1e9 / sample_rate` here.
    fn commit_decoder_seek_outcome(&self, outcome: &DecoderSeekOutcome) {
        let sample_rate = self.session.decoder.spec().sample_rate;
        if sample_rate == 0 {
            return;
        }
        let (frame_offset, end_position, applied_landed_byte) = match *outcome {
            DecoderSeekOutcome::Landed {
                landed_frame,
                landed_at,
                landed_byte,
                ..
            } => (landed_frame, landed_at, landed_byte),
            DecoderSeekOutcome::PastEof { duration } => {
                let end_frame = num_traits::cast::ToPrimitive::to_u64(
                    &(duration.as_secs_f64() * f64::from(sample_rate)),
                )
                .unwrap_or(u64::MAX);
                (end_frame, duration, None)
            }
        };
        let end_position_ns = u64::try_from(end_position.as_nanos()).unwrap_or(u64::MAX);
        let pos = kithara_stream::ChunkPosition {
            sample_rate,
            frame_offset,
            end_position_ns,
            frames: 0,
            source_bytes: 0,
            source_byte_offset: applied_landed_byte,
        };
        self.timeline.commit_seek_landed(&pos);
        if let Some(byte) = applied_landed_byte {
            self.shared_stream.set_position(byte);
        }
    }

    fn decode_panic_message(payload: Box<dyn Any + Send>) -> String {
        match payload.downcast::<String>() {
            Ok(msg) => *msg,
            Err(payload) => payload.downcast::<&'static str>().map_or_else(
                |_| "unknown panic payload".to_string(),
                |msg| (*msg).to_string(),
            ),
        }
    }

    // The single boundary into the upstream decoder subsystem. Every audio
    // codec we wrap allocates per packet/frame/segment *inside* the upstream
    // crate, with no pooled API to hook: `symphonia` format-readers box a
    // fresh slice per `next_packet` (`AdtsReader` → `read_boxed_slice_exact`),
    // its codecs (incl. the `fdk-aac` C decoder) allocate scratch inside
    // `decode`, and `re_mp4` re-parses the `(moof, mdat)` box tree per fMP4
    // segment. These are genuine upstream intrinsics — unavoidable without
    // forking the codec crates — so the permit carves the whole
    // `decoder.next_chunk()` call out of the forbid-blocking produce core. The
    // kithara-audio orchestration around it (scheduler, FSM, the
    // `decode_next_chunk` loop, gapless, seek-skip, variant-change, the
    // resampler/effects scratch, and the lock-free storage read) stays checked.
    #[kithara::rtsan_allow_blocking]
    fn decoder_next_chunk_safe(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let outcome: DecodeResult<DecoderChunkOutcome> =
            match catch_unwind(AssertUnwindSafe(|| self.session.decoder.next_chunk())) {
                Ok(result) => result,
                Err(payload) => Err(DecodeError::InvalidData(format!(
                    "decoder panic during next_chunk: {}",
                    Self::decode_panic_message(payload)
                ))),
            };
        match &outcome {
            Ok(DecoderChunkOutcome::Eof) => {
                debug!(
                    chunks = self.chunks_decoded,
                    samples = self.total_samples,
                    pos = self.shared_stream.position(),
                    "decoder_next_chunk_safe: Eof"
                );
            }
            Err(e) => {
                debug!(
                    error_class = ?e.classify(),
                    chunks = self.chunks_decoded,
                    samples = self.total_samples,
                    pos = self.shared_stream.position(),
                    "decoder_next_chunk_safe: Err {e}"
                );
            }
            Ok(DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Pending(_)) => {}
        }
        outcome
    }

    // The seek twin of [`decoder_next_chunk_safe`]: `decoder.seek()` resets the
    // upstream codec, which re-allocates its decoder state (e.g. symphonia's
    // `MpaDecoder::reset` rebuilds the MP3 `Layer3` tables). Like `next_chunk`,
    // this is an upstream intrinsic with no pooled API, so the permit carves the
    // `decoder.seek()` call out of the forbid-blocking produce core; the
    // kithara-audio seek orchestration around it stays checked.
    #[kithara::rtsan_allow_blocking]
    fn decoder_seek_safe(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let pos_before = self.shared_stream.position();
        debug!(?position, pos_before, "decoder_seek_safe: enter");
        let outcome = match catch_unwind(AssertUnwindSafe(|| self.session.decoder.seek(position))) {
            Ok(result) => result,
            Err(payload) => {
                return Err(DecodeError::InvalidData(format!(
                    "decoder panic during seek: {}",
                    Self::decode_panic_message(payload)
                )));
            }
        };
        let pos_after_seek = self.shared_stream.position();
        debug!(
            ?position,
            pos_before,
            pos_after_seek,
            ?outcome,
            "decoder_seek_safe: decoder returned"
        );
        if let Ok(ref outcome) = outcome {
            self.commit_decoder_seek_outcome(outcome);
        }
        let pos_after_commit = self.shared_stream.position();
        debug!(
            pos_before,
            pos_after_seek,
            pos_after_commit,
            stream_pos_changed = pos_after_commit != pos_before,
            "decoder_seek_safe: exit"
        );
        outcome
    }

    /// Detect `media_info` change and return the recovery anchor.
    ///
    /// Triggers on variant-index change. Codec/container are NOT
    /// re-derived from `current_info`: the source's `media_info()` may
    /// return a declarative container (e.g. `Fmp4` inferred from an
    /// `EXT-X-MAP` URL extension) that disagrees with the bytes the
    /// decoder is actually reading. The cached `session.media_info`
    /// reflects what was probed and built successfully — that's the
    /// authoritative decoder type.
    ///
    /// Two recovery anchors, picked in order:
    /// - cross-codec: init-segment offset via `format_change_segment_range`;
    /// - byte-shifted same-codec / non-init-bearing: current segment
    ///   start via `current_segment_range` (decoder re-parses format
    ///   markers from the new variant's segment boundary).
    /// `NoChange` when neither applies.
    #[kithara::probe]
    fn detect_format_change(&self) -> FormatChangeDetection {
        // NOTE: seek-epoch suppression (see README "Decoder recreate policy").
        let timeline = self.shared_stream.timeline();
        if timeline.is_seek_pending()
            && self.session.installed_at_seek_epoch == timeline.seek_epoch()
        {
            return FormatChangeDetection::NoChange;
        }
        let current_info = self.shared_stream.media_info();
        let session_info = self.session.media_info.as_ref();
        let Some(target) = current_info
            .as_ref()
            .and_then(|cur| resolve_format_change_target(session_info, cur))
        else {
            return FormatChangeDetection::NoChange;
        };
        let range = if let Ok(init) = self.shared_stream.format_change_segment_range() {
            init
        } else if let Some(current) = self.shared_stream.current_segment_range() {
            current
        } else {
            return FormatChangeDetection::NoChange;
        };
        FormatChangeDetection::Applicable {
            target,
            target_offset: range.start,
        }
    }

    fn duration_for_frames(spec: PcmSpec, frames: usize) -> Duration {
        if spec.sample_rate == 0 {
            return Duration::ZERO;
        }
        let nanos = (frames as u128)
            .saturating_mul(Self::NANOS_PER_SEC)
            .saturating_div(u128::from(spec.sample_rate));
        let nanos_u64 = num_traits::cast::ToPrimitive::to_u64(&nanos).unwrap_or(u64::MAX);
        Duration::from_nanos(nanos_u64)
    }

    /// Emit an audio event if the bus is set. Runs on the produce core, so it
    /// only *enqueues* (lock-free); the shell publishes via `flush_deferred` /
    /// `Drop` — the `broadcast::send` is a `kevent` the forbid path must not make.
    fn emit_event(&self, event: AudioEvent) {
        if let Some(ref emit) = self.emit {
            emit.enqueue(event);
        }
    }

    fn emit_seek_lifecycle(
        &self,
        stage: SeekLifecycleStage,
        seek_epoch: u64,
        location: SegmentLocation,
    ) {
        self.emit_event(AudioEvent::SeekLifecycle {
            stage,
            seek_epoch,
            location,
        });
    }

    /// Approximate the byte Symphonia will target for `position` before we
    /// issue the seek. Used to gate `apply_seek_from_decoder` on the byte
    /// range being downloaded — without this, `decoder.seek()` issues a
    /// read past the source's buffered tail and errors out.
    ///
    /// Returns `None` when we can't form a ratio (duration unknown, stream
    /// length unknown, or zero-length stream). Callers fall back to the
    /// historical read-head readiness check in that case.
    fn estimate_target_byte(&self, position: Duration) -> Option<u64> {
        let duration = self.session.decoder.duration()?;
        let stream_len = self.shared_stream.len()?;
        let base_offset = self.session.base_offset;
        if duration.is_zero() || stream_len <= base_offset {
            return None;
        }
        let payload = stream_len - base_offset;
        let pos_nanos = position.as_nanos();
        let dur_nanos = duration.as_nanos();
        let target_relative = u64::try_from(
            pos_nanos
                .saturating_mul(u128::from(payload))
                .saturating_div(dur_nanos.max(1)),
        )
        .expect("pos_nanos * payload / dur_nanos overflowed u64")
        .min(payload);
        Some(base_offset.saturating_add(target_relative))
    }

    fn fail_seek(&mut self, request: SeekRequest, err: DecodeError, context: &'static str) {
        warn!(
            ?err,
            epoch = request.seek.epoch,
            ?request.seek.target,
            attempts = request.attempt.saturating_add(1),
            "{context}"
        );
        self.emit_event(AudioEvent::SeekRejected {
            epoch: request.seek.epoch,
            target: request.seek.target,
            attempts: request.attempt.saturating_add(1),
        });
        self.finalize_seek_pending(request.seek.epoch);
        self.update_state(CurrentFsm::failed(TrackFailure::Decode(err)));
    }

    fn frames_for_duration(spec: PcmSpec, duration: Duration) -> usize {
        if spec.sample_rate == 0 {
            return 0;
        }
        let frames = duration
            .as_nanos()
            .saturating_mul(u128::from(spec.sample_rate))
            .saturating_div(Self::NANOS_PER_SEC);
        assert!(
            frames <= usize::MAX as u128,
            "source.rs:1036 frames_for_duration: frames={frames} \
             exceeds usize::MAX (duration={duration:?}, sample_rate={})",
            spec.sample_rate
        );
        frames as usize
    }

    fn install_recreated_session(
        &mut self,
        new_info: &MediaInfo,
        base_offset: u64,
        new_decoder: Box<dyn Decoder>,
    ) {
        let new_duration = new_decoder.duration();
        let variant = new_info.variant_index;
        self.session = DecoderSession {
            base_offset,
            decoder: new_decoder,
            media_info: Some(new_info.clone()),
            installed_at_seek_epoch: self.shared_stream.timeline().seek_epoch(),
        };
        debug!(?new_duration, base_offset, "Decoder recreated successfully");
        self.emit_event(AudioEvent::DecoderReady {
            base_offset,
            variant,
        });
    }

    /// Reset the effect chain and discard any armed `eof_drain_queue`.
    /// Used on seek / decoder recreation, so a buffering effect's stale tail never leaks past
    /// the discontinuity and a seek-after-EOF re-arms the drain from scratch.
    fn reset_effect_chain(&mut self) {
        reset_effects(&mut self.effects);
        self.eof_drain_queue = None;
    }

    /// Drain ready chunks from the gapless trimmer through the effect
    /// chain, returning the first chunk that survives effects.
    ///
    /// `apply_effects` may swallow a chunk (e.g. resampler buffering);
    /// in that case we keep pulling from `gapless.next()` until we hit
    /// either an emittable chunk or the trimmer is empty.
    fn next_gapless_output(&mut self) -> Option<PcmChunk> {
        while let Some(chunk) = self.gapless.next() {
            if let Some(processed) = apply_effects(&mut self.effects, chunk) {
                return Some(processed);
            }
        }
        None
    }

    /// Resolve the post-seek skip remainder for the given epoch in one
    /// pass. Returns `Some(remaining)` only when an active skip is still
    /// owed; otherwise clears any stale entry and returns `None`. Lets
    /// the caller short-circuit the per-chunk fast path with a single
    /// branch instead of a four-guard cascade (`guard_cascade.rs:60-76`).
    #[inline]
    fn pending_skip_amount(&mut self, epoch: u64) -> Option<Duration> {
        let resume = self.resume_state().copied()?;
        let remaining = resume.skip?;
        if resume.seek.epoch != epoch || remaining.is_zero() {
            if let Some(state) = self.resume_state_mut() {
                state.skip = None;
            }
            return None;
        }
        Some(remaining)
    }

    /// Resolve a seek-preemption target in one Option-chain so the per-tick
    /// hot path of `step_track` short-circuits with a single branch instead
    /// of four sequential predicates. Returns `Some(target)` only when a
    /// new timeline seek epoch must preempt the current state; otherwise
    /// `None` and the caller falls through to the phase dispatcher.
    ///
    /// Fast-path: `Timeline::take_seek_preempt` returns `true` exactly
    /// once per `initiate_seek` call. The typical no-seek tick reads a
    /// single Acquire bool and falls through, instead of dereferencing
    /// two `Arc<AtomicU64>`s. A spurious consume (e.g. seek already
    /// processed by an earlier tick) is harmless because the slow path
    /// below re-validates against `Timeline`.
    #[inline]
    fn preempt_seek_target(&self) -> Option<Duration> {
        if !self.timeline.did_take_seek_preempt() {
            return None;
        }
        let timeline_epoch = self.timeline.seek_epoch();
        if timeline_epoch <= self.epoch.load(Ordering::Acquire) {
            return None;
        }
        let target = self.timeline.seek_target()?;
        if self.state.is_terminal() {
            return None;
        }
        if self
            .active_seek_epoch()
            .is_some_and(|epoch| epoch >= timeline_epoch)
        {
            return None;
        }
        Some(target)
    }

    /// Shared recovery path for a failed `decoder.seek()`.
    ///
    /// Splits by [`DecodeError`] variant: [`DecodeError::SeekOutOfRange`]
    /// fails the seek (no recreate), anything else recreates at the
    /// init/offset range. Always returns `false`. See the crate
    /// `README.md` "Seek error recovery".
    fn recover_from_decoder_seek_error(
        &mut self,
        request: SeekRequest,
        err: DecodeError,
        position: Duration,
        epoch: u64,
        recreate_offset: u64,
        seek_mode: SeekMode,
    ) -> bool {
        let (warn_msg, fail_ctx) = match seek_mode {
            SeekMode::Direct { .. } => (
                "seek: decoder.seek failed, recreating decoder and retrying",
                "seek: decoder.seek failed",
            ),
            SeekMode::Anchor(_) => (
                "seek anchor path: decoder seek failed, recreating decoder",
                "seek anchor path: exact decoder seek failed",
            ),
        };
        warn!(
            ?err,
            epoch,
            ?position,
            recreate_offset,
            attempts = request.attempt.saturating_add(1),
            "{warn_msg}"
        );

        if matches!(err, DecodeError::SeekOutOfRange(_)) {
            self.reject_seek(request, &err, fail_ctx);
            return false;
        }

        if err.is_interrupted() {
            let applying = ApplySeekState {
                request,
                mode: seek_mode,
            };
            let phase = self.source_phase_for_wait_context(&WaitContext::ApplySeek(applying));
            let reason = map_source_phase(phase).unwrap_or(WaitingReason::Waiting);
            self.update_state(CurrentFsm::waiting(
                WaitContext::ApplySeek(applying),
                reason,
            ));
            return false;
        }

        let info = self
            .shared_stream
            .media_info()
            .or_else(|| self.session.media_info.clone());
        let Some(info) = info else {
            self.fail_seek(request, err, fail_ctx);
            return false;
        };
        let Some(recreate_offset) =
            resolve_recreate_offset(&self.shared_stream, info.container, false, recreate_offset)
        else {
            self.fail_seek(
                request,
                DecodeError::InvalidData(format!(
                    "{fail_ctx}: {:?} requires init segment range, none available",
                    info.container
                )),
                fail_ctx,
            );
            return false;
        };
        self.start_recreating_decoder(
            RecreateCause::VariantSwitch,
            info,
            RecreateNext::ApplySeek(request),
            recreate_offset,
            request.attempt.saturating_add(1),
        );
        false
    }

    /// Recreate decoder with new `MediaInfo` via factory.
    ///
    /// The factory handles `OffsetReader` creation and decoder instantiation.
    /// Returns true if decoder was recreated successfully.
    /// Recreate decoder with new `MediaInfo` via factory.
    ///
    /// On success, updates `session` atomically — all three fields
    /// (`decoder`, `base_offset`, `media_info`) change together.
    /// On failure, `session` is unchanged (fixes prior bug where
    /// `media_info` and `base_offset` were updated before factory call).
    fn recreate_decoder(
        &mut self,
        new_info: &MediaInfo,
        base_offset: u64,
    ) -> Result<(), DecodeError> {
        debug!(
            old = ?self.session.media_info,
            new = ?new_info,
            base_offset,
            "Recreating decoder for new format"
        );

        let new_decoder = (self.decoder_factory)(self.shared_stream.clone(), new_info, base_offset)
            .map_err(|e| {
                warn!(base_offset, ?e, "Failed to recreate decoder");
                e
            })?;
        self.install_recreated_session(new_info, base_offset, new_decoder);
        Ok(())
    }

    /// Soft seek rejection: the seek attempt cannot be honoured
    /// (target out-of-range, decoder.seek failed after a fresh
    /// recreate, etc.) but the existing decoder is still alive —
    /// the track keeps playing from its current position. Emits
    /// `SeekRejected`, clears the pending epoch, and parks the FSM
    /// back in `Decoding`. Used for both caller-side errors
    /// (`SeekOutOfRange`) and post-recreate seek failures, where a
    /// further recreate-and-retry would form a loop. The previous
    /// code marked the track `Failed` for these and broke
    /// auto-advance, seek-after-near-end, and stress reproducers.
    fn reject_seek(&mut self, request: SeekRequest, err: &DecodeError, context: &'static str) {
        warn!(
            ?err,
            epoch = request.seek.epoch,
            ?request.seek.target,
            attempts = request.attempt.saturating_add(1),
            "{context}"
        );
        self.emit_event(AudioEvent::SeekRejected {
            epoch: request.seek.epoch,
            target: request.seek.target,
            attempts: request.attempt.saturating_add(1),
        });
        self.epoch.store(request.seek.epoch, Ordering::Release);
        self.finalize_seek_pending(request.seek.epoch);
        self.update_state(CurrentFsm::decoding());
    }

    fn resume_state(&self) -> Option<&ResumeState> {
        match &self.state {
            CurrentFsm::AwaitingResume(h) => Some(h.data()),
            _ => None,
        }
    }

    fn resume_state_mut(&mut self) -> Option<&mut ResumeState> {
        match &mut self.state {
            CurrentFsm::AwaitingResume(h) => Some(h.data_mut()),
            _ => None,
        }
    }

    fn seek_context(&self) -> SegmentLocation {
        let segment_range = self.shared_stream.current_segment_range();
        SegmentLocation::new(
            self.shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            None,
            segment_range.as_ref().map(|range| range.start),
            segment_range.as_ref().map(|range| range.end),
        )
    }

    #[kithara::probe(offset)]
    fn start_recreating_decoder(
        &mut self,
        cause: RecreateCause,
        media_info: MediaInfo,
        next: RecreateNext,
        offset: u64,
        attempt: u8,
    ) {
        let pending_seek_target = match &next {
            RecreateNext::Seek(req) | RecreateNext::ApplySeek(req) => Some(req.seek.target),
            RecreateNext::Decode => None,
        };
        debug!(
            ?cause,
            codec = ?media_info.codec,
            container = ?media_info.container,
            target_offset = offset,
            attempt,
            next = ?std::mem::discriminant(&next),
            ?pending_seek_target,
            committed_position = ?self.timeline.committed_position(),
            stream_pos = self.shared_stream.position(),
            "start_recreating_decoder"
        );
        self.update_state(CurrentFsm::recreating(RecreateState {
            media_info,
            cause,
            next,
            offset,
            attempt,
        }));
    }

    /// Track chunk statistics and emit format events.
    fn track_chunk(&mut self, chunk: &PcmChunk) {
        self.chunks_decoded += 1;
        self.total_samples += chunk.pcm.len() as u64;

        if self.chunks_decoded == 1
            && let Some(ref emit) = self.emit
        {
            emit.enqueue(AudioEvent::FormatDetected {
                spec: AudioFormat::new(chunk.spec().channels, chunk.spec().sample_rate),
            });
            self.last_spec = Some(chunk.spec());
        }

        if let Some(old_spec) = self.last_spec
            && old_spec != chunk.spec()
        {
            self.emit_event(AudioEvent::FormatChanged {
                old: AudioFormat::new(old_spec.channels, old_spec.sample_rate),
                new: AudioFormat::new(chunk.spec().channels, chunk.spec().sample_rate),
            });
            self.last_spec = Some(chunk.spec());
        }
    }

    fn update_decoder_len_for_seek(&self) {
        if let Some(len) = self.shared_stream.len()
            && len > 0
        {
            let relative = len.saturating_sub(self.session.base_offset);
            self.session.decoder.update_byte_len(relative);
        }
    }

    /// Publish the current FSM phase to the shared Timeline and assign
    /// the new state.
    ///
    /// `PLAYING` mirrors "audio FSM has an active decode target on this
    /// Timeline": every non-terminal state keeps it set (`Decoding`,
    /// `SeekRequested`, `ApplyingSeek`, `AwaitingResume`,
    /// `WaitingForSource`, `RecreatingDecoder`), while terminal states
    /// (`AtEof`, `Failed`) clear it. The Downloader's peer
    /// `priority()` reads this flag to decide between High and Low
    /// priority slots — keeping PLAYING set through buffering and
    /// mid-seek windows is deliberate, because the listener is still
    /// attached to this track.
    fn update_state(&mut self, new: CurrentFsm) {
        self.timeline.set_playing(playing_for_state(&new));
        self.state = new;
    }

    pub(crate) fn with_emit(mut self, emit: DeferredBus<AudioEvent>) -> Self {
        self.emit = Some(emit);
        self
    }
}

/// Whether the decode loop should continue or return.
/// Three-state outcome from [`StreamAudioSource::detect_format_change`].
/// Each variant has a distinct caller action:
///
/// - [`NoChange`](Self::NoChange): decoder continues on the current
///   session — no recreate, no action.
/// - [`Applicable`](Self::Applicable): a format-change recovery target
///   was identified; caller should `start_recreating_decoder` with
///   `target` as the new info and `target_offset` as the byte position
///   to seek the stream to before the decoder factory probes init.
///
/// The third logical state — invariant violation — flows through the
/// outer `DecodeResult` as `Err`, not through this enum.
enum FormatChangeDetection {
    NoChange,
    Applicable {
        target: MediaInfo,
        target_offset: u64,
    },
}

enum DecodeAction {
    Yield,
    Return(DecodeResult<DecoderChunkOutcome>),
}

enum DecodeStep {
    Produced(Fetch<PcmChunk>),
    Interrupted,
    NotReady(WaitingReason),
    Eof,
    Failed,
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Handle decoder EOF: try format change recovery, then true EOF.
    #[cold]
    fn handle_decode_eof(&mut self) -> DecodeAction {
        let pos_at_eof = self.shared_stream.position();
        if let FormatChangeDetection::Applicable {
            target: new_info,
            target_offset,
        } = self.detect_format_change()
        {
            debug!(
                pos_at_eof,
                chunks = self.chunks_decoded,
                samples = self.total_samples,
                "Decoder EOF at format boundary, recreating decoder"
            );
            self.start_recreating_decoder(
                RecreateCause::FormatBoundary,
                new_info,
                RecreateNext::Decode,
                target_offset,
                0,
            );
            return DecodeAction::Yield;
        }

        debug!(
            chunks = self.chunks_decoded,
            samples = self.total_samples,
            pos_at_eof,
            "decode complete (true EOF)"
        );

        if self.eof_drain_queue.is_none() {
            let tail = drain_effects(&mut self.effects);
            self.eof_drain_queue = Some(VecDeque::from(tail));
        }

        self.eof_drain_queue
            .as_mut()
            .and_then(VecDeque::pop_front)
            .map_or_else(
                || {
                    // The source and the whole effect chain are fully drained - nothing left to hear.
                    self.emit_event(AudioEvent::EndOfStream);
                    DecodeAction::Return(Ok(DecoderChunkOutcome::Eof))
                },
                |chunk| DecodeAction::Return(Ok(DecoderChunkOutcome::Chunk(chunk))),
            )
    }

    /// Handle decode error without boundary fallback.
    #[cold]
    fn handle_decode_error(e: DecodeError) -> DecodeAction {
        warn!(?e, "decode error");
        DecodeAction::Return(Err(e))
    }

    /// Handle a variant-change signal from the source. Driven by both:
    /// - `Err(DecodeError)` classified as `VariantChange` from the
    ///   `Err`-side of `decode_next_chunk`, AND
    /// - `Ok(Pending(VariantChange))` polled directly on the
    ///   `Ok(Pending(_))` branch (Symphonia and some demuxers absorb
    ///   the underlying `VariantChangeError` as opaque retryable I/O
    ///   and surface only `Pending`).
    ///
    /// `no_change_err` is what the caller returns when
    /// `detect_format_change` reports `NoChange` — for the `Err` path
    /// it's the original decode error (proxied through); for the
    /// `Pending` path it's an explicit `InvalidData` contract violation
    /// because per [`HlsCoord::commit_variant_switch`] the fence
    /// closes BEFORE `abr.apply_decision`, so by the time the FSM
    /// reacts a format transition MUST be observable.
    #[cold]
    fn handle_variant_change(&mut self, no_change_err: DecodeError) -> DecodeAction {
        let FormatChangeDetection::Applicable {
            target: new_info,
            target_offset,
        } = self.detect_format_change()
        else {
            // A seek can race in between `decode_next_chunk`'s loop-top
            // `is_seek_pending` check and this `detect_format_change`,
            // whose seek-epoch suppression then reports `NoChange`. The
            // seek path owns repositioning, so defer to it (`Interrupted`
            // re-runs the loop, which handles the seek) instead of failing
            // the producer on this transient ordering.
            if self.timeline.is_seek_pending() {
                return DecodeAction::Return(Err(DecodeError::Interrupted));
            }
            warn!(
                ?no_change_err,
                chunks = self.chunks_decoded,
                samples = self.total_samples,
                "variant change signal without observable format transition"
            );
            return DecodeAction::Return(Err(no_change_err));
        };
        debug!(
            target_offset,
            chunks = self.chunks_decoded,
            samples = self.total_samples,
            "variant change — recreating decoder"
        );
        self.start_recreating_decoder(
            RecreateCause::FormatBoundary,
            new_info,
            RecreateNext::Decode,
            target_offset,
            0,
        );
        DecodeAction::Yield
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Core decode loop — produces one PCM chunk or signals EOF/error.
    ///
    /// Replaces the old `FallibleIterator::next` implementation.
    /// Called from `decode_one_fetch` to drive the decoder.
    #[kithara::hang_watchdog]
    fn decode_next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            if self.timeline.is_flushing() || self.timeline.is_seek_pending() {
                return Err(DecodeError::Interrupted);
            }

            if let Some(ready) = self.next_gapless_output() {
                return Ok(DecoderChunkOutcome::Chunk(ready));
            }

            match self.decoder_next_chunk_safe() {
                Ok(DecoderChunkOutcome::Pending(PendingReason::VariantChange)) => {
                    match self.handle_variant_change(DecodeError::InvalidData(
                        "variant change signal without observable format transition".into(),
                    )) {
                        DecodeAction::Yield => return Err(DecodeError::Interrupted),
                        DecodeAction::Return(result) => return result,
                    }
                }
                Ok(DecoderChunkOutcome::Pending(reason)) => {
                    if self.shared_stream.has_variant_change_pending() {
                        match self.handle_variant_change(DecodeError::InvalidData(
                            "variant change signal without observable format transition".into(),
                        )) {
                            DecodeAction::Yield => return Err(DecodeError::Interrupted),
                            DecodeAction::Return(result) => return result,
                        }
                    }
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
                Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                    let current_epoch = self.epoch.load(Ordering::Acquire);
                    let chunk = match self.apply_seek_skip(current_epoch, chunk) {
                        DecoderChunkOutcome::Chunk(c) => c,
                        DecoderChunkOutcome::Pending(_) => continue,
                        DecoderChunkOutcome::Eof => unreachable!(
                            "apply_seek_skip never produces Eof — it only trims/drops the chunk"
                        ),
                    };
                    if chunk.pcm.is_empty() {
                        continue;
                    }
                    hang_reset!();
                    self.track_chunk(&chunk);
                    self.gapless.push(chunk);
                    continue;
                }
                Ok(DecoderChunkOutcome::Eof) => {
                    self.gapless.flush();
                    if let Some(ready) = self.next_gapless_output() {
                        return Ok(DecoderChunkOutcome::Chunk(ready));
                    }
                    match self.handle_decode_eof() {
                        DecodeAction::Yield => return Err(DecodeError::Interrupted),
                        DecodeAction::Return(result) => return result,
                    }
                }
                Err(e) => match e.classify() {
                    ErrorClass::VariantChange => match self.handle_variant_change(e) {
                        DecodeAction::Yield => return Err(DecodeError::Interrupted),
                        DecodeAction::Return(result) => return result,
                    },
                    ErrorClass::Interrupted => continue,
                    _ => match Self::handle_decode_error(e) {
                        DecodeAction::Yield => return Err(DecodeError::Interrupted),
                        DecodeAction::Return(result) => return result,
                    },
                },
            }
        }
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Apply a pending seek from the Timeline.
    ///
    /// Reads epoch/target from Timeline and resolves the seek mode.
    fn apply_seek_from_timeline(&mut self, request: SeekRequest) {
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        debug!(
            ?position,
            epoch,
            attempt = request.attempt,
            current_epoch = self.epoch.load(Ordering::Acquire),
            timeline_seek_target = ?self.timeline.seek_target(),
            stream_pos = self.shared_stream.position(),
            variant = ?self
                .shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            "apply_seek_from_timeline: enter (TIMELINE seek picked up)"
        );
        if self.timeline.seek_target().is_none() {
            self.timeline.complete_seek(epoch);
            self.finalize_seek_pending(epoch);
            self.update_state(CurrentFsm::decoding());
            return;
        }

        let current_epoch = self.epoch.load(Ordering::Acquire);
        if epoch <= current_epoch {
            self.timeline.complete_seek(epoch);
            self.finalize_seek_pending(epoch);
            self.update_state(CurrentFsm::decoding());
            return;
        }

        if let Some(duration) = self.timeline.total_duration()
            && position >= duration
        {
            let sample_rate = self.session.decoder.spec().sample_rate;
            let end_frame = num_traits::cast::ToPrimitive::to_u64(
                &(duration.as_secs_f64() * f64::from(sample_rate)),
            )
            .unwrap_or(u64::MAX);
            let end_position_ns = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
            self.timeline
                .commit_seek_landed(&kithara_stream::ChunkPosition {
                    sample_rate,
                    end_position_ns,
                    frame_offset: end_frame,
                    frames: 0,
                    source_bytes: 0,
                    source_byte_offset: None,
                });
            self.timeline.complete_seek(epoch);
            self.finalize_seek_pending(epoch);
            self.epoch.store(epoch, Ordering::Release);
            self.update_state(CurrentFsm::at_eof());
            return;
        }

        if request.attempt == 0 {
            self.emit_seek_lifecycle(SeekLifecycleStage::SeekRequest, epoch, self.seek_context());
        }

        let anchor_result = self.shared_stream.seek_time_anchor(position);
        self.shared_stream.clear_variant_fence();
        self.timeline.complete_seek(epoch);
        // Seek applied on the produce core: arm the peer so it re-targets
        // fetches around the new reader position. The shell flushes it.
        self.arm_peer_wake();

        let mode = match anchor_result {
            Ok(Some(anchor)) => SeekMode::Anchor(anchor),
            Ok(None) => SeekMode::Direct {
                target_byte: self.estimate_target_byte(position),
            },
            Err(err) => {
                self.fail_seek(
                    request,
                    DecodeError::SeekFailed(format!("seek anchor resolution failed: {err}")),
                    "seek anchor resolution failed",
                );
                return;
            }
        };
        self.update_state(CurrentFsm::applying_seek(ApplySeekState { mode, request }));
    }

    fn boundary_end(&self, start: u64) -> u64 {
        self.shared_stream.len().map_or_else(
            || start.saturating_add(Self::DEFAULT_READ_AHEAD_BYTES),
            |len| {
                start
                    .saturating_add(Self::DEFAULT_READ_AHEAD_BYTES)
                    .min(len)
            },
        )
    }

    /// Decode one chunk using the decode loop.
    #[kithara::probe]
    fn decode_one_step(&mut self) -> DecodeStep {
        let decoder_duration = crate::pipeline::gapless::visible_duration(
            self.session.decoder.as_ref(),
            self.gapless_mode,
        );
        let timeline_duration = self.timeline.total_duration();
        if decoder_duration > timeline_duration {
            self.timeline.set_total_duration(decoder_duration);
        }
        let current_epoch = self.epoch.load(Ordering::Acquire);
        let result = self.decode_next_chunk();
        match result {
            Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                if self
                    .resume_state()
                    .is_some_and(|resume| resume.seek.epoch == current_epoch)
                {
                    let segment_range = self.shared_stream.current_segment_range();
                    self.emit_seek_lifecycle(
                        SeekLifecycleStage::DecodeStarted,
                        current_epoch,
                        SegmentLocation::new(
                            chunk.meta.variant_index,
                            chunk.meta.segment_index,
                            segment_range.as_ref().map(|range| range.start),
                            segment_range.as_ref().map(|range| range.end),
                        ),
                    );
                    self.update_state(CurrentFsm::decoding());
                }
                let fo = chunk.meta.frame_offset;
                let frames = chunk.meta.frames;
                self.decode_head = Some((current_epoch, fo.saturating_add(u64::from(frames))));
                DecodeStep::Produced(Fetch::new(chunk, false, current_epoch))
            }
            Ok(DecoderChunkOutcome::Eof) => {
                self.update_state(CurrentFsm::at_eof());
                DecodeStep::Eof
            }
            Ok(DecoderChunkOutcome::Pending(_reason)) => {
                DecodeStep::NotReady(WaitingReason::Waiting)
            }
            Err(e) if e.is_interrupted() => DecodeStep::Interrupted,
            Err(e) => {
                self.update_state(CurrentFsm::failed(TrackFailure::Decode(e)));
                DecodeStep::Failed
            }
        }
    }

    /// Clear the seek-pending flag and wake the source's peer in one step.
    ///
    /// `Timeline::clear_seek_pending` only flips the atomic flag; it does
    /// not wake anything. The HLS peer's `sync_abr_lock()` is invoked only
    /// inside `poll_next`, so when every requested segment is cached and
    /// the peer parks itself in `Poll::Pending`, the ABR lock acquired on
    /// seek-initiate stays held indefinitely. Subsequent `set_mode(Manual)`
    /// calls then hit `AbrReason::Locked` in `decide()` and silently fail
    /// to commit.
    ///
    /// Arming the source's peer wake (`Source::peer_wake`) after every
    /// seek-completion ensures the peer runs one more `poll_next` cycle, which
    /// sees `is_seek_pending() == false` and releases the ABR lock through
    /// `sync_abr_lock`.
    fn finalize_seek_pending(&self, epoch: u64) {
        self.timeline.clear_seek_pending(epoch);
        self.arm_peer_wake();
    }

    /// Arm the reader→peer wake on the produce core (lock-free). The scheduler
    /// shell flushes it off the forbid path, so the cross-thread `notify_one`
    /// (a `kevent`) never fires on the RT core. No-op for file streams.
    fn arm_peer_wake(&self) {
        if let Some(ref wake) = self.peer_wake {
            wake.arm();
        }
    }

    fn recreate_phase(&self, offset: u64) -> SourcePhase {
        self.shared_stream
            .phase_at(self.recreate_ready_range(offset))
    }

    /// Byte range whose readiness gates decoder recreation, shared by the
    /// gate and the wait path so the two never disagree (a mismatch
    /// livelocks the worker). Init range alone for a separate CMAF init,
    /// else the `[offset..offset+READ_AHEAD)` window. See the crate
    /// `README.md` "Recreate readiness gating".
    fn recreate_ready_range(&self, offset: u64) -> Range<u64> {
        if let Ok(init_range) = self.shared_stream.format_change_segment_range()
            && init_range.end.saturating_sub(init_range.start) <= Self::DEFAULT_READ_AHEAD_BYTES
        {
            return init_range;
        }
        offset..self.boundary_end(offset)
    }

    /// Compute the upper bound of the byte range required for the
    /// decoder to safely produce its first chunk after a seek landing
    /// at `byte`: the end of the segment containing `byte` (segmented
    /// sources) or the standard 32 KB look-ahead (raw sources). Always
    /// clamped to `Source::len()` so we don't gate on phantom bytes
    /// past EOF.
    fn seek_landing_end(&self, byte: u64) -> u64 {
        let segment_end = self
            .shared_stream
            .as_segment_layout()
            .and_then(|layout| layout.segment_at_byte(byte))
            .map(|seg| seg.byte_range.end);
        let end = segment_end.unwrap_or_else(|| self.boundary_end(byte));
        self.shared_stream.len().map_or(end, |len| end.min(len))
    }

    /// Check whether the underlying source has data ready for a non-blocking
    /// decode. Returns `true` for `Ready`, `Eof`, or `Seeking` phases.
    fn source_is_ready(&self) -> bool {
        let pos = self.shared_stream.position();
        let lookahead_end = pos.saturating_add(Self::DEFAULT_READ_AHEAD_BYTES);
        let check_end = self
            .shared_stream
            .as_segment_layout()
            .and_then(|layout| layout.segment_after_byte(pos))
            .map_or(lookahead_end, |next| {
                next.byte_range.start.min(lookahead_end)
            });
        let check_end = self
            .shared_stream
            .len()
            .map_or(check_end, |len| check_end.min(len));
        self.source_ready_for_range(pos..check_end)
    }

    /// Phase of the read-ahead window the decoder reads *through* during
    /// steady-state playback: `[pos, pos + READ_AHEAD)`, clamped to the
    /// stream length but — unlike [`source_is_ready`] — NOT clamped to the
    /// next segment boundary. The decoder's container parser reads across
    /// segment boundaries, so a boundary-clamped gate reports `Ready` while
    /// the decoder is actually blocked on the (withheld) next segment. The
    /// `WaitContext::Playback` wait path gates on this wider window so the
    /// gate and the decoder's real read never disagree (a mismatch hot-spins
    /// the worker; see crate `README.md` "Playback readiness gating").
    fn source_phase_forward(&self) -> SourcePhase {
        let pos = self.shared_stream.position();
        let end = pos.saturating_add(Self::DEFAULT_READ_AHEAD_BYTES);
        let end = self.shared_stream.len().map_or(end, |len| end.min(len));
        self.shared_stream.phase_at(pos..end)
    }

    fn source_is_ready_for_apply_seek(&self, applying: ApplySeekState) -> bool {
        match applying.mode {
            SeekMode::Anchor(anchor) => self.source_is_ready_for_seek_landing(anchor.byte_offset),
            SeekMode::Direct {
                target_byte: Some(byte),
            } => self.source_is_ready_for_seek_landing(byte),
            SeekMode::Direct { target_byte: None } => self.source_is_ready(),
        }
    }

    fn source_is_ready_for_boundary(&self, start: u64) -> bool {
        let end = self.boundary_end(start);
        self.source_ready_for_range(start..end)
    }

    /// Readiness check for the byte range the decoder will read first
    /// after a post-seek landing. Unlike [`source_is_ready_for_boundary`]
    /// (which gates on a fixed 32 KB window — enough for fmp4 init box
    /// probes), this gates on the **entire segment** containing the
    /// landing byte. A FLAC fmp4 chunk segment is ~700 KB; landing on
    /// its first byte with only 32 KB cached starves the decoder on the
    /// very next read - `wait_range` budget exceeds and the audio worker's
    /// `PassOutcome::Waiting` ticks the `HangDetector`. For sources without
    /// a segment layout (raw files), falls back to the boundary window.
    fn source_is_ready_for_seek_landing(&self, byte: u64) -> bool {
        let end = self.seek_landing_end(byte);
        self.source_ready_for_range(byte..end)
    }

    fn source_phase_for_boundary(&self, start: u64) -> SourcePhase {
        let end = self.boundary_end(start);
        self.shared_stream.phase_at(start..end)
    }

    /// Companion to [`source_is_ready_for_seek_landing`] used by the
    /// `WaitingForSource` branch — same byte range so the worker
    /// blocks on the same window it later gates ready on.
    fn source_phase_for_seek_landing(&self, byte: u64) -> SourcePhase {
        let end = self.seek_landing_end(byte);
        self.shared_stream.phase_at(byte..end)
    }

    fn source_phase_for_wait_context(&self, context: &WaitContext) -> SourcePhase {
        match context {
            WaitContext::ApplySeek(applying) => match applying.mode {
                SeekMode::Anchor(anchor) => self.source_phase_for_seek_landing(anchor.byte_offset),
                SeekMode::Direct {
                    target_byte: Some(byte),
                } => self.source_phase_for_seek_landing(byte),
                SeekMode::Direct { target_byte: None } => self.shared_stream.phase(),
            },
            WaitContext::Recreation(recreate) => self.recreate_phase(recreate.offset),
            WaitContext::PostSeek(resume) => resume.anchor_offset.map_or_else(
                || self.shared_stream.phase(),
                |byte| self.source_phase_for_boundary(byte),
            ),
            // Steady-state playback parks on the decoder's forward read-ahead
            // window (see `source_phase_forward`), not the single-byte phase at
            // `pos`: the decoder reads across the next segment boundary, so a
            // single-byte gate reports `Ready` while the decoder is blocked on
            // the withheld next segment, bouncing the worker straight back into
            // `Decoding` and hot-spinning the decode loop.
            WaitContext::Playback => self.source_phase_forward(),
            WaitContext::Seek(_) => self.shared_stream.phase(),
        }
    }

    fn source_ready_for_range(&self, range: Range<u64>) -> bool {
        matches!(
            self.shared_stream.phase_at(range),
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }

    fn source_ready_for_recreate(&self, recreate: &RecreateState) -> bool {
        matches!(
            self.recreate_phase(recreate.offset),
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Apply the `RecreateNext` action after a successful recreation.
    fn apply_recreate_next(&mut self, next: &RecreateNext) -> TrackStep<PcmChunk> {
        match *next {
            RecreateNext::Decode => {
                self.reset_effect_chain();
                self.update_state(CurrentFsm::decoding());
                TrackStep::StateChanged
            }
            RecreateNext::Seek(request) => {
                self.update_state(CurrentFsm::seek_requested(request));
                TrackStep::StateChanged
            }
            RecreateNext::ApplySeek(request) => self.finish_apply_seek_after_recreate(request),
        }
    }

    /// Map a recreate-path error to the FSM outcome. Transient
    /// `ErrorClass::Interrupted` (probe ran before the source buffered
    /// `[0..PROBE)` of a freshly-switched variant) maps to `NeedsSourceWait`
    /// so the caller retries after the source phase becomes Ready.
    /// Everything else is a hard fail.
    fn classify_recreate_err(e: &DecodeError, _offset: u64) -> RecreateOutcome {
        if e.classify() == ErrorClass::Interrupted {
            RecreateOutcome::NeedsSourceWait
        } else {
            RecreateOutcome::SoftFailed
        }
    }

    /// Execute the actual decoder recreation once readiness is confirmed.
    ///
    /// Returns `Some(RecreateOutcome::Done)` on success,
    /// `Some(RecreateOutcome::SoftFailed)` on hard failure (caller marks track failed),
    /// `Some(RecreateOutcome::NeedsSourceWait)` on transient `ErrorClass::Interrupted`
    /// (caller routes to `wait_for_source_on_recreate` for retry once the source
    /// has buffered the probe window), or `None` when the track was already
    /// terminated inside this helper (e.g. stream seek error).
    fn execute_recreation(&mut self, recreate: &RecreateState) -> Option<RecreateOutcome> {
        if recreate.cause == RecreateCause::FormatBoundary
            && matches!(recreate.next, RecreateNext::Decode)
        {
            debug!(
                offset = recreate.offset,
                cause = ?recreate.cause,
                next = ?recreate.next,
                committed = ?self.timeline.committed_position(),
                stream_pos = self.shared_stream.position(),
                stream_len = ?self.shared_stream.len(),
                "execute_recreation: FormatBoundary+Decode branch enter"
            );
            if let Err(e) = self.apply_format_change(&recreate.media_info, recreate.offset) {
                return Some(Self::classify_recreate_err(&e, recreate.offset));
            }
            let committed = self.timeline.committed_position();
            let epoch_now = self.epoch.load(Ordering::Acquire);
            let sample_rate = self.session.decoder.spec().sample_rate;
            // Continue the new decoder from the producer's decode head, not the
            // consumer's lagging `committed`: chunks in [committed..decode_head]
            // are already queued in the outlet ring (a FormatBoundary recreate
            // neither flushes it nor bumps the seek epoch), so resuming at
            // `committed` re-emits them — duplicated content, a backward phase
            // jump. The decode head is an exact frame; the demuxer quantizes the
            // seek landing to a sample, and `frame_offset_for` rounds that back
            // to the nearest frame (consistent with `frames_to_trim`), so the
            // rebuilt decoder relabels its first chunk at exactly `decode_head`.
            let decode_head = self
                .decode_head
                .filter(|&(epoch, _)| epoch == epoch_now && sample_rate > 0)
                .map(|(_, frame)| duration_for_frames(sample_rate, frame))
                .filter(|&head| head > committed)
                .unwrap_or(committed);
            let target_time = match self.resume_target {
                Some((seek_epoch, target)) if seek_epoch == epoch_now && target > committed => {
                    target
                }
                _ => decode_head,
            };
            debug!(
                ?target_time,
                stream_pos = self.shared_stream.position(),
                stream_len = ?self.shared_stream.len(),
                "execute_recreation: after apply_format_change, about to decoder_seek_safe"
            );
            if !target_time.is_zero()
                && let Err(e) = self.decoder_seek_safe(target_time)
            {
                warn!(
                    ?e,
                    ?target_time,
                    "Failed to seek decoder to timeline position after cross-codec recreate"
                );
                return Some(Self::classify_recreate_err(&e, recreate.offset));
            }
            debug!(
                ?target_time,
                stream_pos_final = self.shared_stream.position(),
                "execute_recreation: FormatBoundary+Decode branch exit"
            );
            return Some(RecreateOutcome::Done);
        }
        self.shared_stream.clear_variant_fence();
        if self
            .shared_stream
            .probe_seek(SeekFrom::Start(recreate.offset))
            .is_err()
        {
            self.update_state(CurrentFsm::failed(TrackFailure::RecreateFailed {
                offset: recreate.offset,
            }));
            return None;
        }
        self.shared_stream.clear_variant_fence();
        Some(
            match self.recreate_decoder(&recreate.media_info, recreate.offset) {
                Ok(()) => RecreateOutcome::Done,
                Err(e) => Self::classify_recreate_err(&e, recreate.offset),
            },
        )
    }

    fn finish_apply_seek_after_recreate(&mut self, request: SeekRequest) -> TrackStep<PcmChunk> {
        debug!(
            target = ?request.seek.target,
            epoch = request.seek.epoch,
            attempt = request.attempt,
            committed_position = ?self.timeline.committed_position(),
            stream_pos = self.shared_stream.position(),
            "finish_apply_seek_after_recreate: enter"
        );
        match self.decoder_seek_safe(request.seek.target) {
            Ok(_outcome) => {
                self.shared_stream.commit_seek_landing(None);
                self.apply_seek_applied(
                    request.seek.epoch,
                    request.seek.target,
                    self.seek_context(),
                    None,
                );
                self.epoch.store(request.seek.epoch, Ordering::Release);
                self.finalize_seek_pending(request.seek.epoch);
                TrackStep::StateChanged
            }
            Err(err) => {
                self.reject_seek(
                    request,
                    &err,
                    "step_recreating_decoder: recreated decoder seek failed",
                );
                TrackStep::StateChanged
            }
        }
    }

    /// Handle the "source not ready for boundary" branch of
    /// recreate. Transitions to `WaitingForSource` or terminates the
    /// track, depending on the source phase. Owns the `RecreateState`
    /// (moved out of the FSM by the caller) — no `self.state` re-read.
    fn wait_for_source_on_recreate(&mut self, recreate: RecreateState) -> TrackStep<PcmChunk> {
        let phase = self.recreate_phase(recreate.offset);
        if let Some(reason) = map_source_phase(phase) {
            self.update_state(CurrentFsm::waiting(
                WaitContext::Recreation(recreate),
                reason,
            ));
            return TrackStep::Blocked(reason);
        }
        if phase == SourcePhase::Cancelled {
            self.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
            return TrackStep::Failed;
        }
        self.update_state(CurrentFsm::recreating(recreate));
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

impl Track<ApplyingSeek> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let applying = self.into_inner();
        if !src.source_is_ready_for_apply_seek(applying) {
            let phase = src.source_phase_for_wait_context(&WaitContext::ApplySeek(applying));
            if let Some(reason) = map_source_phase(phase) {
                src.update_state(CurrentFsm::waiting(
                    WaitContext::ApplySeek(applying),
                    reason,
                ));
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                src.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            src.update_state(CurrentFsm::applying_seek(applying));
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        let request = applying.request;
        let applied = match applying.mode {
            SeekMode::Anchor(anchor) => src.apply_aligned_anchor_seek(request, anchor),
            SeekMode::Direct { .. } => src.apply_seek_from_decoder(request),
        };
        if applied {
            src.epoch.store(request.seek.epoch, Ordering::Release);
            src.finalize_seek_pending(request.seek.epoch);
            src.gapless.notify_seek();
        }
        TrackStep::StateChanged
    }
}

impl Track<AwaitingResume> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let resume = self.into_inner();
        let anchor_offset = resume.anchor_offset;
        let ready = anchor_offset.map_or_else(
            || src.source_is_ready(),
            |byte| src.source_is_ready_for_boundary(byte),
        );
        if !ready {
            let phase = anchor_offset.map_or_else(
                || src.shared_stream.phase(),
                |byte| src.source_phase_for_boundary(byte),
            );
            if let Some(reason) = map_source_phase(phase) {
                src.update_state(CurrentFsm::waiting(WaitContext::PostSeek(resume), reason));
                return TrackStep::Blocked(reason);
            }
        }
        // Restore the phase so the decode loop's `resume_state()` /
        // post-seek skip trimming sees the canonical `ResumeState`.
        src.update_state(CurrentFsm::awaiting_resume(resume));
        match src.decode_one_step() {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            DecodeStep::NotReady(reason) => TrackStep::Blocked(reason),
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }
}

impl Track<Decoding> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let () = self.into_inner();
        if !src.source_is_ready() {
            if !src.timeline.is_seek_pending()
                && let FormatChangeDetection::Applicable {
                    target: new_info,
                    target_offset,
                } = src.detect_format_change()
            {
                src.start_recreating_decoder(
                    RecreateCause::FormatBoundary,
                    new_info,
                    RecreateNext::Decode,
                    target_offset,
                    0,
                );
                return TrackStep::StateChanged;
            }
            let phase = src.shared_stream.phase();
            if let Some(reason) = map_source_phase(phase) {
                src.update_state(CurrentFsm::waiting(WaitContext::Playback, reason));
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                src.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            // Stay in Decoding — the dispatcher's sentinel is already
            // `Decoding`, so no restore is needed.
            return TrackStep::Blocked(WaitingReason::Waiting);
        }

        match src.decode_one_step() {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            // The decoder read across the current segment boundary into a
            // not-ready (withheld) byte. Park in `WaitingForSource(Playback)`
            // rather than re-running the full decode every tick: the wait
            // state re-checks the forward read-ahead window cheaply and only
            // re-enters `Decoding` once that window is ready. Staying in
            // `Decoding` here hot-spins the decode loop (`source_is_ready`
            // gates only the current segment, so it never reflects the
            // blocked forward read) — flake F5.
            DecodeStep::NotReady(reason) => {
                src.update_state(CurrentFsm::waiting(WaitContext::Playback, reason));
                TrackStep::Blocked(reason)
            }
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }
}

impl Track<RecreatingDecoder> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let recreate = self.into_inner();
        if !src.source_ready_for_recreate(&recreate) {
            return src.wait_for_source_on_recreate(recreate);
        }
        let Some(outcome) = src.execute_recreation(&recreate) else {
            return TrackStep::Failed;
        };
        match outcome {
            RecreateOutcome::Done => src.apply_recreate_next(&recreate.next),
            RecreateOutcome::SoftFailed => {
                src.update_state(CurrentFsm::failed(TrackFailure::RecreateFailed {
                    offset: recreate.offset,
                }));
                TrackStep::Failed
            }
            RecreateOutcome::NeedsSourceWait => src.wait_for_source_on_recreate(recreate),
        }
    }
}

impl Track<SeekRequested> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let request = self.into_inner();
        if !src.source_is_ready() {
            let phase = src.shared_stream.phase();
            if let Some(reason) = map_source_phase(phase) {
                src.update_state(CurrentFsm::waiting(WaitContext::Seek(request), reason));
                return TrackStep::Blocked(reason);
            }
        }
        src.apply_seek_from_timeline(request);
        TrackStep::StateChanged
    }
}

impl Track<WaitingForSource> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let WaitState {
            context,
            reason: stored_reason,
        } = self.into_inner();
        let phase = src.source_phase_for_wait_context(&context);

        if let Some(reason) = map_source_phase(phase) {
            // Still waiting — restore the phase with its stored reason.
            src.update_state(CurrentFsm::waiting(context, stored_reason));
            return TrackStep::Blocked(reason);
        }

        match phase {
            SourcePhase::Cancelled => {
                src.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            SourcePhase::Eof => {
                src.update_state(CurrentFsm::at_eof());
                return TrackStep::Eof;
            }
            _ => {}
        }

        // Source ready — resume into the phase that initiated the wait.
        match context {
            WaitContext::Playback => src.update_state(CurrentFsm::decoding()),
            WaitContext::Seek(ctx) => src.update_state(CurrentFsm::seek_requested(ctx)),
            WaitContext::ApplySeek(applying) => {
                src.update_state(CurrentFsm::applying_seek(applying));
            }
            WaitContext::Recreation(recreate) => src.update_state(CurrentFsm::recreating(recreate)),
            WaitContext::PostSeek(resume) => src.update_state(CurrentFsm::awaiting_resume(resume)),
        }
        TrackStep::StateChanged
    }
}

impl<T: StreamType> Drop for StreamAudioSource<T> {
    fn drop(&mut self) {
        // Publish any lifecycle event enqueued on the final produce pass before
        // the terminal node is dropped — `scheduler::run_loop` removes a
        // removable slot via `retain` without another `flush_deferred`, so a
        // terminal `EndOfStream` would otherwise be lost. Runs in the unchecked
        // shell (retain is outside `produce_pass`), off the forbid path.
        if let Some(ref emit) = self.emit {
            emit.flush();
        }
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        if let Some(target) = self.preempt_seek_target() {
            self.update_state(CurrentFsm::seek_requested(SeekRequest {
                seek: SeekContext {
                    target,
                    epoch: self.timeline.seek_epoch(),
                },
                ..Default::default()
            }));
            self.reset_effect_chain();
            self.gapless.notify_seek();
            return TrackStep::StateChanged;
        }

        // Move the typed handle out (sentinel = `Decoding`, the unit
        // phase, so the decode hot path that stays in `Decoding` needs
        // no restore). Each `step` either transitions via
        // `update_state` or restores its own phase before returning.
        match std::mem::replace(&mut self.state, CurrentFsm::decoding()) {
            CurrentFsm::Decoding(handle) => handle.step(self),
            CurrentFsm::SeekRequested(handle) => handle.step(self),
            CurrentFsm::WaitingForSource(handle) => handle.step(self),
            CurrentFsm::ApplyingSeek(handle) => handle.step(self),
            CurrentFsm::RecreatingDecoder(handle) => handle.step(self),
            CurrentFsm::AwaitingResume(handle) => handle.step(self),
            CurrentFsm::AtEof(handle) => {
                self.state = CurrentFsm::AtEof(handle);
                TrackStep::Eof
            }
            CurrentFsm::Failed(handle) => {
                emit_failure_log(handle.data());
                self.state = CurrentFsm::Failed(handle);
                TrackStep::Failed
            }
        }
    }

    fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    fn flush_deferred(&mut self) {
        self.session.decoder.flush_reader_signals();
        // Publish the FSM lifecycle events the produce core enqueued this pass,
        // off the forbid path (the `broadcast::send` is a `kevent`).
        if let Some(ref emit) = self.emit {
            emit.flush();
        }
        // Deliver the peer wake the produce core armed this pass (a blocked
        // `probe_read`, a seek-apply / finalize). The `notify_one` is a
        // cross-thread `kevent` the forbid-blocking core must not make, so it
        // lands here in the shell. Same `Arc<DeferredWake>` the reader drivers
        // and the FSM arm, so one flush covers both. `None` for file streams.
        if let Some(ref wake) = self.peer_wake {
            wake.flush();
        }
    }

    fn warm_up(&mut self) {
        // The storage committed-read fast path (`MemDriver::committed_len` /
        // `read_committed` behind an `arc_swap::ArcSwapOption`) lazily
        // `Box`-allocates this thread's `arc_swap` debt node on its FIRST load.
        // Left to the produce core, that one-time alloc lands inside the
        // forbid-blocking region (the first committed `len`/`contains_range`/
        // read after the resource opens). The debt node is process-global per
        // thread and shared by every `ArcSwap` regardless of payload type, so a
        // throwaway load here — in the scheduler shell, before any checked
        // `tick` — allocates it off the RT path and warms every real storage
        // read. It is resource-independent, so it works even before this
        // source's resource has been opened (the `len()` path only reaches
        // `committed_len` once the resource is live).
        let warm = ArcSwap::from_pointee(());
        let _ = warm.load();
        let _ = self.shared_stream.len();
    }
}

/// Classify a [`CurrentFsm`] phase for the shared Timeline `PLAYING` flag.
///
/// The Downloader peers read `Timeline::is_playing()` in their
/// `priority()` method. Every non-terminal phase keeps this track
/// "listened to" from the user's perspective — buffering, seek-in-
/// progress, and decoder recreation are all transient windows inside
/// an otherwise-active track. Only `AtEof` (natural end) and `Failed`
/// (terminal error) clear the flag.
fn playing_for_state(state: &CurrentFsm) -> bool {
    !matches!(state, CurrentFsm::AtEof(_) | CurrentFsm::Failed(_))
}

fn emit_failure_log(failure: &TrackFailure) {
    match failure {
        TrackFailure::Decode(err) => warn!(?err, "track failed: decode error"),
        TrackFailure::RecreateFailed { offset } => {
            warn!(offset = *offset, "track failed: decoder recreation failed");
        }
        TrackFailure::SourceCancelled => warn!("track failed: source cancelled"),
    }
}

/// Build the recreate target `MediaInfo` for a format boundary.
///
/// Returns `None` when there is no boundary to act on. A boundary
/// triggers on either:
/// - variant-index change (ABR switched to a different variant), or
/// - explicit codec change with both sides specified (rare cross-codec
///   transitions where the source has actually probed a new codec).
///
/// The returned target preserves cached `container` (the decoder's
/// truth — see below) and updates `variant_index` from `current`.
/// `codec` is taken from `current` only on an explicit codec change;
/// otherwise cached `codec` is preserved.
///
/// Why preserve cached `container`: `Source::media_info()` may report
/// a declarative container (e.g. `Fmp4` inferred from an `EXT-X-MAP`
/// URL extension) that disagrees with the bytes actually being read.
/// The cached value reflects what was probed and built successfully —
/// that's the authoritative decoder type. True container transitions
/// (very rare in real HLS) are surfaced through decode errors and
/// recovered via the seek-error recovery path.
fn resolve_format_change_target(
    cached: Option<&MediaInfo>,
    current: &MediaInfo,
) -> Option<MediaInfo> {
    let variant_changed = cached.map_or_else(
        || current.variant_index.is_some(),
        |c| c.variant_index != current.variant_index,
    );
    let codec_changed = matches!(
        (cached.and_then(|c| c.codec), current.codec),
        (Some(a), Some(b)) if a != b
    );
    if !variant_changed && !codec_changed {
        return None;
    }
    let target = cached.map_or_else(
        || current.clone(),
        |c| {
            let mut t = c.clone();
            t.variant_index = current.variant_index;
            if codec_changed || t.codec.is_none() {
                t.codec = current.codec;
            }
            if t.container.is_none() {
                t.container = current.container;
            }
            t
        },
    );
    Some(target)
}

/// Whether the container requires an init header (ftyp/moov/RIFF/EBML…)
/// at byte 0 of the decoder input. Such containers cannot be parsed
/// from a mid-stream offset, so a variant-switch recreate must land on
/// the init segment range rather than on a seek anchor's byte target.
/// Mid-stream-decodable containers (MPEG-ES, ADTS, native FLAC, Ogg,
/// MPEG-TS) accept any valid packet start.
fn container_needs_init_range(container: ContainerFormat) -> bool {
    match container {
        ContainerFormat::Fmp4
        | ContainerFormat::Mp4
        | ContainerFormat::Wav
        | ContainerFormat::Mkv
        | ContainerFormat::Caf => true,
        ContainerFormat::MpegAudio
        | ContainerFormat::Adts
        | ContainerFormat::Flac
        | ContainerFormat::Ogg
        | ContainerFormat::MpegTs => false,
    }
}

/// Pick the byte offset to hand the decoder factory on a variant/codec
/// boundary.
///
/// - Init-bearing containers (fMP4, MP4, WAV, MKV, CAF): the decoder
///   must start at the init header, so return
///   `format_change_segment_range().start` — or `None` when the source
///   cannot yet locate it, so the caller fails the seek instead of
///   handing the decoder a mid-segment offset (produces
///   `"missing ftyp atom"`).
/// - Codec change with a non-init-bearing (or unknown) container:
///   prefer `format_change_segment_range()` when available — the new
///   codec's first packet is the cleanest resync point — but fall back
///   to `anchor_byte_offset` so legacy flows (no `format_change_range`
///   yet committed) keep working.
/// - Variant-only change with a non-init-bearing container: return
///   `anchor_byte_offset` — mid-stream resync is valid for
///   MPEG-ES/ADTS/FLAC/Ogg/MPEG-TS.
fn resolve_recreate_offset<T: StreamType>(
    shared: &SharedStream<T>,
    target_container: Option<ContainerFormat>,
    codec_changed: bool,
    anchor_byte_offset: u64,
) -> Option<u64> {
    let needs_init = target_container.is_some_and(container_needs_init_range);
    let init_offset = shared
        .format_change_segment_range()
        .ok()
        .map(|range| range.start);
    if needs_init {
        init_offset
    } else if codec_changed {
        Some(init_offset.unwrap_or(anchor_byte_offset))
    } else {
        Some(anchor_byte_offset)
    }
}

#[cfg(test)]
mod playing_flag_tests {
    use kithara_stream::MediaInfo;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pipeline::track_fsm::{
        ApplySeekState, RecreateCause, RecreateNext, RecreateState, ResumeState, SeekContext,
        SeekMode, SeekRequest, TrackFailure, WaitContext, WaitingReason,
    };

    fn seek_ctx() -> SeekContext {
        SeekContext {
            epoch: 1,
            target: Duration::from_secs(5),
        }
    }

    fn seek_req() -> SeekRequest {
        SeekRequest {
            seek: seek_ctx(),
            ..Default::default()
        }
    }

    #[kithara::test]
    fn playing_for_state_active_states_are_true() {
        assert!(playing_for_state(&CurrentFsm::decoding()));
        assert!(playing_for_state(&CurrentFsm::seek_requested(seek_req())));
        assert!(playing_for_state(&CurrentFsm::applying_seek(
            ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: seek_req(),
            }
        )));
        assert!(playing_for_state(&CurrentFsm::waiting(
            WaitContext::Playback,
            WaitingReason::Waiting,
        )));
        assert!(playing_for_state(&CurrentFsm::recreating(RecreateState {
            attempt: 0,
            cause: RecreateCause::FormatBoundary,
            media_info: MediaInfo::default(),
            next: RecreateNext::Decode,
            offset: 0,
        })));
        assert!(playing_for_state(&CurrentFsm::awaiting_resume(
            ResumeState {
                seek: seek_ctx(),
                ..Default::default()
            }
        )));
    }

    #[kithara::test]
    fn playing_for_state_terminal_states_are_false() {
        assert!(!playing_for_state(&CurrentFsm::at_eof()));
        assert!(!playing_for_state(&CurrentFsm::failed(
            TrackFailure::SourceCancelled
        )));
        assert!(!playing_for_state(&CurrentFsm::failed(
            TrackFailure::RecreateFailed { offset: 0 }
        )));
        assert!(!playing_for_state(&CurrentFsm::failed(
            TrackFailure::Decode(DecodeError::Interrupted)
        )));
    }

    #[kithara::test]
    fn playing_matrix_covers_every_transition_endpoint() {
        let transitions: &[(CurrentFsm, bool)] = &[
            (CurrentFsm::decoding(), true),
            (CurrentFsm::seek_requested(seek_req()), true),
            (
                CurrentFsm::applying_seek(ApplySeekState {
                    mode: SeekMode::Direct { target_byte: None },
                    request: seek_req(),
                }),
                true,
            ),
            (
                CurrentFsm::waiting(WaitContext::Playback, WaitingReason::Waiting),
                true,
            ),
            (
                CurrentFsm::recreating(RecreateState {
                    attempt: 0,
                    cause: RecreateCause::VariantSwitch,
                    media_info: MediaInfo::default(),
                    next: RecreateNext::Decode,
                    offset: 0,
                }),
                true,
            ),
            (
                CurrentFsm::awaiting_resume(ResumeState {
                    seek: seek_ctx(),
                    ..Default::default()
                }),
                true,
            ),
            (CurrentFsm::at_eof(), false),
            (CurrentFsm::failed(TrackFailure::SourceCancelled), false),
        ];
        for (state, expected) in transitions {
            assert_eq!(playing_for_state(state), *expected);
        }
    }

    #[kithara::test]
    fn no_spurious_flip_across_100_decoding_transitions() {
        for _ in 0..100 {
            assert!(
                playing_for_state(&CurrentFsm::decoding()),
                "PLAYING must stay true across a long Decoding → Decoding loop"
            );
        }
    }
}

#[cfg(test)]
mod resolve_format_change_target_tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;

    use super::resolve_format_change_target;

    fn info(
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
        variant: Option<u32>,
    ) -> MediaInfo {
        let mut info = MediaInfo::new(codec, container);
        info.variant_index = variant;
        info
    }

    #[kithara::test]
    fn no_change_when_variant_index_matches() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        assert!(resolve_format_change_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn variant_change_keeps_cached_codec_and_container_when_current_disagrees() {
        let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
        let current = info(None, Some(ContainerFormat::Fmp4), Some(1));
        let target = resolve_format_change_target(Some(&cached), &current)
            .expect("variant change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::Pcm));
        assert_eq!(target.container, Some(ContainerFormat::Wav));
        assert_eq!(target.variant_index, Some(1));
    }

    #[kithara::test]
    fn variant_change_falls_back_to_current_when_cached_lacks_codec_or_container() {
        let cached = info(None, None, Some(0));
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(2),
        );
        let target = resolve_format_change_target(Some(&cached), &current)
            .expect("variant change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::AacLc));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
        assert_eq!(target.variant_index, Some(2));
    }

    #[kithara::test]
    fn no_cached_uses_current_directly() {
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(1),
        );
        let target = resolve_format_change_target(None, &current)
            .expect("None cached + Some(variant) must trigger");
        assert_eq!(target, current);
    }

    #[kithara::test]
    fn explicit_codec_change_takes_current_codec() {
        let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        let current = info(Some(AudioCodec::Flac), Some(ContainerFormat::Fmp4), None);
        let target = resolve_format_change_target(Some(&cached), &current)
            .expect("codec change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::Flac));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn current_codec_none_is_not_a_codec_change() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(None, Some(ContainerFormat::Fmp4), Some(0));
        assert!(resolve_format_change_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn no_change_when_neither_side_has_variant() {
        let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        let current = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        assert!(resolve_format_change_target(Some(&cached), &current).is_none());
    }
}
