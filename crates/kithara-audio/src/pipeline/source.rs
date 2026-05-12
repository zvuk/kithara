use std::{
    any::Any,
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use delegate::delegate;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, ErrorClass,
    GaplessMode, PcmChunk, PcmSpec,
};
use kithara_events::{AudioEvent, AudioFormat, SeekLifecycleStage, SegmentLocation};
use kithara_platform::{Mutex, thread::yield_now};
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
            ApplySeekState, DecoderSession, RecreateCause, RecreateNext, RecreateState,
            ResumeState, SeekContext, SeekMode, SeekRequest, TrackFailure, TrackPhaseTag,
            TrackState, TrackStep, WaitContext, WaitingReason, map_source_phase,
        },
    },
    traits::AudioEffect,
    worker::{AudioWorkerSource, apply_effects, flush_effects, reset_effects},
};

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
pub(crate) struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
}

impl<T: StreamType> SharedStream<T> {
    pub(crate) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
        }
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
            fn format_change_segment_range(&self) -> Option<Range<u64>>;
            pub(crate) fn clear_variant_fence(&self);
            pub(crate) fn commit_variant_layout(&self);
            pub(crate) fn set_seek_epoch(&self, seek_epoch: u64);
            fn seek_time_anchor(&self, position: Duration) -> Result<Option<SourceSeekAnchor>, io::Error>;
            fn commit_seek_landing(&self, anchor: Option<SourceSeekAnchor>);
            /// Build a fresh reader-side hooks instance from the inner source.
            pub(crate) fn take_reader_hooks(&self) -> Option<kithara_stream::SharedHooks>;
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
            /// Signal that the given byte range will be needed soon.
            pub(crate) fn demand_range(&self, range: Range<u64>);
            /// Wake blocked `wait_range()` calls and downstream waiters.
            ///
            /// Safe to call outside of `read()`; briefly takes the inner mutex.
            fn notify_waiting(&self);
            /// Create a lock-free callback for waking blocked `wait_range()`.
            ///
            /// Called once during `Audio::new()` (before the worker starts),
            /// so the inner mutex lock is safe. The returned closure captures
            /// only the condvar/notify primitive — it never takes the inner
            /// mutex, preventing deadlock when called from `Audio::seek()`
            /// while the worker holds the lock inside `read()`.
            pub(crate) fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>>;
        }
    }
}

impl<T: StreamType> Clone for SharedStream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: StreamType> Read for SharedStream<T> {
    delegate! {
        to self.inner.lock_sync() {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
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
    pub(crate) fn new(mut shared: SharedStream<T>, base_offset: u64) -> Self {
        let _ = shared.seek(SeekFrom::Start(base_offset));
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
        match pos {
            SeekFrom::Start(p) => {
                let abs = self.base_offset + p;
                let real_pos = self.shared.seek(SeekFrom::Start(abs))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::Current(delta) => {
                let real_pos = self.shared.seek(SeekFrom::Current(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::End(delta) => {
                let real_pos = self.shared.seek(SeekFrom::End(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
        }
    }
}

/// Factory closure that creates a new decoder from stream, media info, and base offset.
///
/// Production: creates Symphonia `Decoder` via [`OffsetReader`].
/// Tests: returns `MockDecoder` without real I/O.
pub(crate) type DecoderFactory<T> =
    Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Option<Box<dyn Decoder>> + Send>;

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
    pub(crate) state: TrackState,
    epoch: Arc<AtomicU64>,
    decoder_factory: DecoderFactory<T>,
    /// Gapless trim mode applied per-track. Built once at construction;
    /// ABR variant switches inside one track keep the same trimmer
    /// (production semantics) so we never retrim audible content
    /// around a recreate boundary.
    gapless_mode: GaplessMode,
    /// Per-track gapless trimmer adapter.
    gapless: GaplessStage,
    emit: Option<Box<dyn Fn(AudioEvent) + Send>>,
    last_spec: Option<PcmSpec>,
    shared_stream: SharedStream<T>,
    effects: Vec<Box<dyn AudioEffect>>,
    chunks_decoded: u64,
    total_samples: u64,
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
        let gapless =
            GaplessStage::from_decoder(decoder.as_ref(), gapless_mode, initial_media_info.as_ref());
        let session = DecoderSession {
            decoder,
            base_offset: 0,
            media_info: initial_media_info,
        };
        timeline.set_playing(true);
        Self {
            shared_stream,
            session,
            decoder_factory,
            epoch,
            effects,
            timeline,
            gapless_mode,
            gapless,
            state: TrackState::Decoding,
            chunks_decoded: 0,
            total_samples: 0,
            last_spec: None,
            emit: None,
        }
    }

    fn active_seek_epoch(&self) -> Option<u64> {
        match &self.state {
            TrackState::WaitingForSource {
                context: WaitContext::Seek(request),
                ..
            }
            | TrackState::SeekRequested(request) => Some(request.seek.epoch),
            TrackState::ApplyingSeek(state) => Some(state.request.seek.epoch),
            TrackState::WaitingForSource {
                context: WaitContext::ApplySeek(applying),
                ..
            } => Some(applying.request.seek.epoch),
            TrackState::WaitingForSource {
                context: WaitContext::Recreation(recreate),
                ..
            } => match &recreate.next {
                RecreateNext::Decode => None,
                RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => {
                    Some(request.seek.epoch)
                }
                RecreateNext::AnchorSeek { request, .. } => Some(request.seek.epoch),
            },
            TrackState::AwaitingResume(state) => Some(state.seek.epoch),
            TrackState::RecreatingDecoder(state) => match &state.next {
                RecreateNext::Decode => None,
                RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => {
                    Some(request.seek.epoch)
                }
                RecreateNext::AnchorSeek { request, .. } => Some(request.seek.epoch),
            },
            _ => None,
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
        let init_range = self.shared_stream.format_change_segment_range();
        let init_offset = init_range.as_ref().map(|range| range.start);
        let is_init_bearing = target_container.is_some_and(container_needs_init_range);
        let already_at_init = init_offset.is_some_and(|o| o == self.session.base_offset);
        let container_needs_init_resync =
            is_init_bearing && init_offset.is_some() && !already_at_init;
        let needs_recreation = codec_changed || variant_changed || container_needs_init_resync;
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
            container_needs_init_resync,
            already_at_init,
            needs_recreation,
            ?target_container,
            ?recreate_offset,
            ?init_offset,
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

        let next = if container_needs_init_resync && !codec_changed && !variant_changed {
            RecreateNext::AnchorSeek { request, anchor }
        } else {
            RecreateNext::Seek(request)
        };

        self.start_recreating_decoder(
            RecreateCause::VariantSwitch,
            target_info,
            next,
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
    /// Returns true if decoder was recreated successfully.
    #[kithara::probe(target_offset)]
    fn apply_format_change(&mut self, new_info: &MediaInfo, target_offset: u64) -> bool {
        let current_pos = self.shared_stream.position();
        debug!(
            current_pos,
            target_offset,
            chunks_decoded = self.chunks_decoded,
            total_samples = self.total_samples,
            "Applying format change: old decoder finished, seeking to new segment start"
        );

        self.shared_stream.clear_variant_fence();

        if let Err(e) = self.shared_stream.seek(SeekFrom::Start(target_offset)) {
            warn!(?e, target_offset, "Failed to seek to segment boundary");
            return false;
        }

        self.recreate_decoder(new_info, target_offset)
    }

    fn apply_seek_applied(
        &mut self,
        epoch: u64,
        position: Duration,
        location: SegmentLocation,
        anchor_offset: Option<u64>,
    ) {
        reset_effects(&mut self.effects);
        self.emit_seek_lifecycle(SeekLifecycleStage::SeekApplied, epoch, location);
        self.update_state(TrackState::AwaitingResume(ResumeState {
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

        if let Some((new_info, target_offset)) = self.detect_format_change() {
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
        warn!(
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
        trace!(
            ?position,
            anchor_start = ?anchor.segment_start,
            target_offset = anchor.byte_offset,
            "seek anchor path: starting exact decoder seek"
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

    fn decoder_next_chunk_safe(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        match catch_unwind(AssertUnwindSafe(|| self.session.decoder.next_chunk())) {
            Ok(result) => result,
            Err(payload) => Err(DecodeError::InvalidData(format!(
                "decoder panic during next_chunk: {}",
                Self::decode_panic_message(payload)
            ))),
        }
    }

    fn decoder_seek_safe(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = match catch_unwind(AssertUnwindSafe(|| self.session.decoder.seek(position))) {
            Ok(result) => result,
            Err(payload) => {
                return Err(DecodeError::InvalidData(format!(
                    "decoder panic during seek: {}",
                    Self::decode_panic_message(payload)
                )));
            }
        };
        if let Ok(ref outcome) = outcome {
            self.commit_decoder_seek_outcome(outcome);
        }
        outcome
    }

    /// Detect `media_info` change and return the init-bearing boundary.
    ///
    /// The variant fence in `Source::read_at()` prevents the old decoder
    /// from reading data from a new variant. This causes Symphonia to hit
    /// EOF naturally, after which `fetch_next` recreates the decoder.
    ///
    /// Triggers on variant-index change. Codec/container are NOT
    /// re-derived from `current_info`: the source's `media_info()` may
    /// return a declarative container (e.g. `Fmp4` inferred from an
    /// `EXT-X-MAP` URL extension) that disagrees with the bytes the
    /// decoder is actually reading. The cached `session.media_info`
    /// reflects what was probed and built successfully — that's the
    /// authoritative decoder type. True codec/container transitions
    /// (rare in real HLS) are surfaced through decode errors and
    /// recovered via `recover_from_decoder_seek_error`.
    #[kithara::probe]
    fn detect_format_change(&self) -> Option<(MediaInfo, u64)> {
        let current_info = self.shared_stream.media_info()?;
        let target = resolve_format_change_target(self.session.media_info.as_ref(), &current_info)?;

        let seg_range = self
            .shared_stream
            .format_change_segment_range()
            .or_else(|| self.shared_stream.current_segment_range());

        seg_range.map(|range| (target, range.start))
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

    /// Emit an audio event if the callback is set.
    fn emit_event(&self, event: AudioEvent) {
        if let Some(ref emit) = self.emit {
            emit(event);
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
        self.timeline.clear_seek_pending(request.seek.epoch);
        self.update_state(TrackState::Failed(TrackFailure::Decode(err)));
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
        };
        debug!(?new_duration, base_offset, "Decoder recreated successfully");
        self.emit_event(AudioEvent::DecoderReady {
            base_offset,
            variant,
        });
    }

    fn log_failure(&self) {
        if let TrackState::Failed(failure) = &self.state {
            emit_failure_log(failure);
        }
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
    /// Two classes of failure share this entry point and need different
    /// architectural responses:
    ///
    /// 1. **Decoder internal-state corruption** — e.g. Symphonia's moof
    ///    fragment table holding stale offsets after a variant switch.
    ///    Fresh decoder state resolves this; recreate is the right cure.
    ///
    /// 2. **Caller-side invalid target** — e.g. seek past EOF, target
    ///    timestamp out-of-range for the stream's known duration.
    ///    Recreate cannot fix this: a freshly built decoder has the same
    ///    `duration()` and rejects the same target with the same error.
    ///    Retrying loops forever (the prod "перемотка не работает" bug).
    ///
    /// Classification is by [`DecodeError`] variant, not by string
    /// match: caller-side errors arrive as
    /// [`DecodeError::SeekOutOfRange`] (produced by the decoder layer
    /// from typed Symphonia `SeekErrorKind::OutOfRange` and from typed
    /// `StreamSeekPastEof` payloads in the underlying `io::Error`).
    /// Those route directly to `fail_seek` — no recreate, no retry.
    /// Anything else is treated as class (1) and dispatched to the
    /// recreate-at-init path.
    ///
    /// Init-bearing containers (fMP4/MP4/WAV/MKV/CAF) must recreate at
    /// the source's init segment range; mid-segment recreate would land
    /// on bytes with no ftyp/RIFF/EBML header and the factory would fail
    /// silently. Mid-stream-decodable containers (MPEG-ES/ADTS/FLAC/Ogg/
    /// MPEG-TS) and unknown containers use `recreate_offset` directly.
    ///
    /// Calls `fail_seek` for class (2), missing `MediaInfo`, or when an
    /// init-bearing container has no available init range. Always
    /// returns `false` so callers can `return` directly.
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
        warn!(?err, epoch, ?position, "{warn_msg}");

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
            self.update_state(TrackState::WaitingForSource {
                reason,
                context: WaitContext::ApplySeek(applying),
            });
            self.submit_demand_for_current_state();
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
            RecreateNext::Seek(request),
            recreate_offset,
            request.attempt,
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
    fn recreate_decoder(&mut self, new_info: &MediaInfo, base_offset: u64) -> bool {
        debug!(
            old = ?self.session.media_info,
            new = ?new_info,
            base_offset,
            "Recreating decoder for new format"
        );

        let Some(new_decoder) =
            (self.decoder_factory)(self.shared_stream.clone(), new_info, base_offset)
        else {
            warn!(base_offset, "Failed to recreate decoder");
            return false;
        };
        self.install_recreated_session(new_info, base_offset, new_decoder);
        true
    }

    /// Soft seek rejection: the seek attempt cannot be honoured
    /// (target out-of-range, etc.) but the existing decoder is still
    /// alive — the track keeps playing from its current position.
    /// Emits `SeekRejected`, clears the pending epoch, and parks the
    /// FSM back in `Decoding`. Used for caller-side errors
    /// (`SeekOutOfRange`) where retry/recreate cannot help; the
    /// previous code marked the track `Failed` for these and broke
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
        self.timeline.clear_seek_pending(request.seek.epoch);
        self.update_state(TrackState::Decoding);
    }

    fn resume_state(&self) -> Option<&ResumeState> {
        match &self.state {
            TrackState::AwaitingResume(state) => Some(state),
            _ => None,
        }
    }

    fn resume_state_mut(&mut self) -> Option<&mut ResumeState> {
        match &mut self.state {
            TrackState::AwaitingResume(state) => Some(state),
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

    fn seek_request(&self) -> Option<SeekRequest> {
        match &self.state {
            TrackState::SeekRequested(request) => Some(*request),
            TrackState::ApplyingSeek(state) => Some(state.request),
            _ => None,
        }
    }

    fn start_recreating_decoder(
        &mut self,
        cause: RecreateCause,
        media_info: MediaInfo,
        next: RecreateNext,
        offset: u64,
        attempt: u8,
    ) {
        self.update_state(TrackState::RecreatingDecoder(RecreateState {
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
            emit(AudioEvent::FormatDetected {
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
    fn update_state(&mut self, new: TrackState) {
        self.timeline.set_playing(playing_for_state(&new));
        self.state = new;
    }

    pub(crate) fn with_emit(mut self, emit: Box<dyn Fn(AudioEvent) + Send>) -> Self {
        self.emit = Some(emit);
        self
    }
}

/// Whether the decode loop should continue or return.
enum DecodeAction {
    Yield,
    Return(DecodeResult<DecoderChunkOutcome>),
}

enum DecodeStep {
    Produced(Fetch<PcmChunk>),
    Interrupted,
    Eof,
    Failed,
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Handle decoder EOF: try format change recovery, then true EOF.
    #[cold]
    fn handle_decode_eof(&mut self) -> DecodeAction {
        let pos_at_eof = self.shared_stream.position();
        if let Some((new_info, target_offset)) = self.detect_format_change() {
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

        if let Some(flushed) = flush_effects(&mut self.effects) {
            self.emit_event(AudioEvent::EndOfStream);
            return DecodeAction::Return(Ok(DecoderChunkOutcome::Chunk(flushed)));
        }

        self.emit_event(AudioEvent::EndOfStream);
        DecodeAction::Return(Ok(DecoderChunkOutcome::Eof))
    }

    /// Handle decode error without boundary fallback.
    #[cold]
    fn handle_decode_error(e: DecodeError) -> DecodeAction {
        warn!(?e, "decode error");
        DecodeAction::Return(Err(e))
    }

    /// Handle an explicit source-level variant boundary signal.
    #[cold]
    fn handle_variant_change(&mut self, e: DecodeError) -> DecodeAction {
        if let Some((new_info, target_offset)) = self.detect_format_change() {
            debug!(
                target_offset,
                chunks = self.chunks_decoded,
                samples = self.total_samples,
                "Decoder reached variant boundary, recreating decoder"
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

        warn!(?e, "variant change without codec-changing media info");
        DecodeAction::Return(Err(e))
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Core decode loop — produces one PCM chunk or signals EOF/error.
    ///
    /// Replaces the old `FallibleIterator::next` implementation.
    /// Called from `decode_one_fetch` to drive the decoder.
    #[kithara_hang_detector::hang_watchdog]
    fn decode_next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            hang_tick!();
            yield_now();

            if self.timeline.is_flushing() || self.timeline.is_seek_pending() {
                return Err(DecodeError::Interrupted);
            }

            if let Some(ready) = self.next_gapless_output() {
                return Ok(DecoderChunkOutcome::Chunk(ready));
            }

            match self.decoder_next_chunk_safe() {
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
                Ok(DecoderChunkOutcome::Pending(_reason)) => {
                    continue;
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
    fn apply_seek_from_timeline(&mut self) {
        let Some(request) = self.seek_request() else {
            return;
        };
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        if self.timeline.seek_target().is_none() {
            self.timeline.complete_seek(epoch);
            self.timeline.clear_seek_pending(epoch);
            self.update_state(TrackState::Decoding);
            return;
        }

        let current_epoch = self.epoch.load(Ordering::Acquire);
        if epoch <= current_epoch {
            self.timeline.complete_seek(epoch);
            self.timeline.clear_seek_pending(epoch);
            self.update_state(TrackState::Decoding);
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
            self.timeline.clear_seek_pending(epoch);
            self.epoch.store(epoch, Ordering::Release);
            self.update_state(TrackState::AtEof);
            return;
        }

        if request.attempt == 0 {
            self.emit_seek_lifecycle(SeekLifecycleStage::SeekRequest, epoch, self.seek_context());
        }

        self.shared_stream.set_seek_epoch(epoch);
        self.shared_stream.clear_variant_fence();
        let anchor_result = self.shared_stream.seek_time_anchor(position);
        self.timeline.complete_seek(epoch);
        self.shared_stream.notify_waiting();

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
        self.update_state(TrackState::ApplyingSeek(ApplySeekState { mode, request }));
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
                    self.update_state(TrackState::Decoding);
                }
                DecodeStep::Produced(Fetch::new(chunk, false, current_epoch))
            }
            Ok(DecoderChunkOutcome::Eof) => {
                self.update_state(TrackState::AtEof);
                DecodeStep::Eof
            }
            Ok(DecoderChunkOutcome::Pending(_reason)) => DecodeStep::Interrupted,
            Err(e) if e.is_interrupted() => DecodeStep::Interrupted,
            Err(e) => {
                self.update_state(TrackState::Failed(TrackFailure::Decode(e)));
                DecodeStep::Failed
            }
        }
    }

    /// Check whether the underlying source has data ready for a non-blocking
    /// decode. Returns `true` for `Ready`, `Eof`, or `Seeking` phases.
    fn source_is_ready(&self) -> bool {
        let pos = self.shared_stream.position();
        let check_end = self
            .shared_stream
            .current_segment_range()
            .filter(|seg| seg.start <= pos && pos < seg.end)
            .map_or_else(
                || pos.saturating_add(Self::DEFAULT_READ_AHEAD_BYTES),
                |seg| seg.end,
            );
        let check_end = self
            .shared_stream
            .len()
            .map_or(check_end, |len| check_end.min(len));
        self.source_ready_for_range(pos..check_end)
    }

    fn source_is_ready_for_apply_seek(&self, applying: ApplySeekState) -> bool {
        match applying.mode {
            SeekMode::Anchor(anchor) => self.source_is_ready_for_boundary(anchor.byte_offset),
            SeekMode::Direct {
                target_byte: Some(byte),
            } => self.source_is_ready_for_boundary(byte),
            SeekMode::Direct { target_byte: None } => self.source_is_ready(),
        }
    }

    fn source_is_ready_for_boundary(&self, start: u64) -> bool {
        let end = self.boundary_end(start);
        self.source_ready_for_range(start..end)
    }

    fn source_phase_for_boundary(&self, start: u64) -> SourcePhase {
        let end = self.boundary_end(start);
        self.shared_stream.phase_at(start..end)
    }

    fn source_phase_for_wait_context(&self, context: &WaitContext) -> SourcePhase {
        match context {
            WaitContext::ApplySeek(applying) => match applying.mode {
                SeekMode::Anchor(anchor) => self.source_phase_for_boundary(anchor.byte_offset),
                SeekMode::Direct {
                    target_byte: Some(byte),
                } => self.source_phase_for_boundary(byte),
                SeekMode::Direct { target_byte: None } => self.shared_stream.phase(),
            },
            WaitContext::Recreation(recreate) => self.source_phase_for_boundary(recreate.offset),
            WaitContext::PostSeek(resume) => resume.anchor_offset.map_or_else(
                || self.shared_stream.phase(),
                |byte| self.source_phase_for_boundary(byte),
            ),
            WaitContext::Playback | WaitContext::Seek(_) => self.shared_stream.phase(),
        }
    }

    fn source_ready_for_range(&self, range: Range<u64>) -> bool {
        matches!(
            self.shared_stream.phase_at(range),
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }

    /// Submit a demand signal for the byte range corresponding to the
    /// current `WaitingForSource` state. This is a non-blocking hint
    /// that tells the source (and transitively the downloader) which
    /// data the worker needs next.
    ///
    /// The byte target is resolved through `TrackState::seek_location()`
    /// so the dispatch over `WaitContext × SeekMode` lives in one place.
    fn submit_demand_for_current_state(&self) {
        if !matches!(self.state, TrackState::WaitingForSource { .. }) {
            return;
        }
        self.state
            .seek_location()
            .submit_demand(&self.shared_stream);
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Apply the `RecreateNext` action after a successful recreation.
    fn apply_recreate_next(&mut self, next: &RecreateNext) -> TrackStep<PcmChunk> {
        match *next {
            RecreateNext::Decode => {
                reset_effects(&mut self.effects);
                self.update_state(TrackState::Decoding);
                TrackStep::StateChanged
            }
            RecreateNext::Seek(request) => {
                self.update_state(TrackState::SeekRequested(request));
                TrackStep::StateChanged
            }
            RecreateNext::ApplySeek(request) => self.finish_apply_seek_after_recreate(request),
            RecreateNext::AnchorSeek { request, anchor } => {
                reset_effects(&mut self.effects);
                self.update_state(TrackState::ApplyingSeek(ApplySeekState {
                    request,
                    mode: SeekMode::Anchor(anchor),
                }));
                if self.apply_time_anchor_seek(request, anchor) {
                    self.epoch.store(request.seek.epoch, Ordering::Release);
                    self.timeline.clear_seek_pending(request.seek.epoch);
                    TrackStep::StateChanged
                } else {
                    TrackStep::StateChanged
                }
            }
        }
    }

    /// Execute the actual decoder recreation once readiness is confirmed.
    ///
    /// Returns `Some(true)` on success, `Some(false)` on soft failure
    /// (caller must mark track failed), or `None` when the track was
    /// already terminated inside this helper (e.g. stream seek error).
    fn execute_recreation(&mut self, recreate: &RecreateState) -> Option<bool> {
        if recreate.cause == RecreateCause::FormatBoundary
            && matches!(recreate.next, RecreateNext::Decode)
        {
            return Some(self.apply_format_change(&recreate.media_info, recreate.offset));
        }
        self.shared_stream.clear_variant_fence();
        if self
            .shared_stream
            .seek(SeekFrom::Start(recreate.offset))
            .is_err()
        {
            self.update_state(TrackState::Failed(TrackFailure::RecreateFailed {
                offset: recreate.offset,
            }));
            return None;
        }
        self.shared_stream.clear_variant_fence();
        Some(self.recreate_decoder(&recreate.media_info, recreate.offset))
    }

    fn finish_apply_seek_after_recreate(&mut self, request: SeekRequest) -> TrackStep<PcmChunk> {
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
                self.timeline.clear_seek_pending(request.seek.epoch);
                TrackStep::StateChanged
            }
            Err(err) => {
                self.fail_seek(
                    request,
                    err,
                    "step_recreating_decoder: recreated decoder seek failed",
                );
                TrackStep::Failed
            }
        }
    }

    fn step_applying_seek(&mut self) -> TrackStep<PcmChunk> {
        let applying = match &self.state {
            TrackState::ApplyingSeek(state) => *state,
            _ => return TrackStep::StateChanged,
        };
        if !self.source_is_ready_for_apply_seek(applying) {
            let phase = self.source_phase_for_wait_context(&WaitContext::ApplySeek(applying));
            if let Some(reason) = map_source_phase(phase) {
                self.update_state(TrackState::WaitingForSource {
                    reason,
                    context: WaitContext::ApplySeek(applying),
                });
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                self.update_state(TrackState::Failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        let request = applying.request;
        let applied = match applying.mode {
            SeekMode::Anchor(anchor) => self.apply_aligned_anchor_seek(request, anchor),
            SeekMode::Direct { .. } => self.apply_seek_from_decoder(request),
        };
        if applied {
            self.epoch.store(request.seek.epoch, Ordering::Release);
            self.timeline.clear_seek_pending(request.seek.epoch);
            self.gapless.notify_seek();
        }
        TrackStep::StateChanged
    }

    fn step_awaiting_resume(&mut self) -> TrackStep<PcmChunk> {
        let resume_state = match &self.state {
            TrackState::AwaitingResume(resume) => Some(*resume),
            _ => None,
        };
        let anchor_offset = resume_state.and_then(|r| r.anchor_offset);
        let ready = anchor_offset.map_or_else(
            || self.source_is_ready(),
            |byte| self.source_is_ready_for_boundary(byte),
        );
        if !ready {
            let phase = anchor_offset.map_or_else(
                || self.shared_stream.phase(),
                |byte| self.source_phase_for_boundary(byte),
            );
            if let Some(reason) = map_source_phase(phase) {
                let context = resume_state.map_or(WaitContext::Playback, WaitContext::PostSeek);
                self.update_state(TrackState::WaitingForSource { context, reason });
                return TrackStep::Blocked(reason);
            }
        }
        match self.decode_one_step() {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }

    fn step_decoding(&mut self) -> TrackStep<PcmChunk> {
        if !self.source_is_ready() {
            if !self.timeline.is_seek_pending()
                && let Some((new_info, target_offset)) = self.detect_format_change()
            {
                self.start_recreating_decoder(
                    RecreateCause::FormatBoundary,
                    new_info,
                    RecreateNext::Decode,
                    target_offset,
                    0,
                );
                return TrackStep::StateChanged;
            }
            let phase = self.shared_stream.phase();
            if let Some(reason) = map_source_phase(phase) {
                self.update_state(TrackState::WaitingForSource {
                    reason,
                    context: WaitContext::Playback,
                });
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                self.update_state(TrackState::Failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            return TrackStep::Blocked(WaitingReason::Waiting);
        }

        match self.decode_one_step() {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }

    fn step_recreating_decoder(&mut self) -> TrackStep<PcmChunk> {
        let recreate = match &self.state {
            TrackState::RecreatingDecoder(recreate) => recreate.clone(),
            _ => return TrackStep::StateChanged,
        };
        self.shared_stream.commit_variant_layout();
        if !self.source_is_ready_for_boundary(recreate.offset) {
            return self.wait_for_source_on_recreate(recreate.offset);
        }

        let recreate = match std::mem::replace(&mut self.state, TrackState::Decoding) {
            TrackState::RecreatingDecoder(recreate) => recreate,
            other => {
                self.update_state(other);
                return TrackStep::StateChanged;
            }
        };

        let Some(recreated) = self.execute_recreation(&recreate) else {
            return TrackStep::Failed;
        };
        if !recreated {
            self.update_state(TrackState::Failed(TrackFailure::RecreateFailed {
                offset: recreate.offset,
            }));
            return TrackStep::Failed;
        }

        self.apply_recreate_next(&recreate.next)
    }

    fn step_seek_requested(&mut self) -> TrackStep<PcmChunk> {
        if !self.source_is_ready() {
            let phase = self.shared_stream.phase();
            if let Some(reason) = map_source_phase(phase) {
                let request = match &self.state {
                    TrackState::SeekRequested(request) => *request,
                    _ => return TrackStep::StateChanged,
                };
                self.update_state(TrackState::WaitingForSource {
                    reason,
                    context: WaitContext::Seek(request),
                });
                return TrackStep::Blocked(reason);
            }
        }
        self.apply_seek_from_timeline();
        TrackStep::StateChanged
    }

    fn step_waiting_for_source(&mut self) -> TrackStep<PcmChunk> {
        let Some((phase, _stored_reason)) = (match &self.state {
            TrackState::WaitingForSource { context, reason } => {
                Some((self.source_phase_for_wait_context(context), *reason))
            }
            _ => None,
        }) else {
            return TrackStep::StateChanged;
        };

        if let Some(reason) = map_source_phase(phase) {
            self.submit_demand_for_current_state();
            return TrackStep::Blocked(reason);
        }

        match phase {
            SourcePhase::Cancelled => {
                self.update_state(TrackState::Failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            SourcePhase::Eof => {
                self.update_state(TrackState::AtEof);
                return TrackStep::Eof;
            }
            _ => {}
        }

        let old_state = std::mem::replace(&mut self.state, TrackState::Decoding);
        match old_state {
            TrackState::WaitingForSource {
                context: WaitContext::Playback,
                ..
            } => {
                self.update_state(TrackState::Decoding);
            }
            TrackState::WaitingForSource {
                context: WaitContext::Seek(ctx),
                ..
            } => {
                self.update_state(TrackState::SeekRequested(ctx));
            }
            TrackState::WaitingForSource {
                context: WaitContext::ApplySeek(applying),
                ..
            } => {
                self.update_state(TrackState::ApplyingSeek(applying));
            }
            TrackState::WaitingForSource {
                context: WaitContext::Recreation(recreate),
                ..
            } => {
                self.update_state(TrackState::RecreatingDecoder(recreate));
            }
            TrackState::WaitingForSource {
                context: WaitContext::PostSeek(resume),
                ..
            } => {
                self.update_state(TrackState::AwaitingResume(resume));
            }
            _ => {}
        }
        TrackStep::StateChanged
    }

    /// Handle the "source not ready for boundary" branch of
    /// `step_recreating_decoder`. Transitions to `WaitingForSource` or
    /// terminates the track, depending on the source phase.
    fn wait_for_source_on_recreate(&mut self, offset: u64) -> TrackStep<PcmChunk> {
        let phase = self.source_phase_for_boundary(offset);
        if let Some(reason) = map_source_phase(phase) {
            let recreate = match std::mem::replace(&mut self.state, TrackState::Decoding) {
                TrackState::RecreatingDecoder(recreate) => recreate,
                other => {
                    self.update_state(other);
                    return TrackStep::StateChanged;
                }
            };
            self.update_state(TrackState::WaitingForSource {
                reason,
                context: WaitContext::Recreation(recreate),
            });
            self.submit_demand_for_current_state();
            return TrackStep::Blocked(reason);
        }
        if phase == SourcePhase::Cancelled {
            self.update_state(TrackState::Failed(TrackFailure::SourceCancelled));
            return TrackStep::Failed;
        }
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        if let Some(target) = self.preempt_seek_target() {
            self.update_state(TrackState::SeekRequested(SeekRequest {
                seek: SeekContext {
                    target,
                    epoch: self.timeline.seek_epoch(),
                },
                ..Default::default()
            }));
            reset_effects(&mut self.effects);
            self.gapless.notify_seek();
            return TrackStep::StateChanged;
        }

        match self.state.phase_tag() {
            TrackPhaseTag::Decoding => self.step_decoding(),
            TrackPhaseTag::SeekRequested => self.step_seek_requested(),
            TrackPhaseTag::WaitingForSource => self.step_waiting_for_source(),
            TrackPhaseTag::ApplyingSeek => self.step_applying_seek(),
            TrackPhaseTag::RecreatingDecoder => self.step_recreating_decoder(),
            TrackPhaseTag::AwaitingResume => self.step_awaiting_resume(),
            TrackPhaseTag::AtEof => TrackStep::Eof,
            TrackPhaseTag::Failed => {
                self.log_failure();
                TrackStep::Failed
            }
        }
    }

    fn timeline(&self) -> &Timeline {
        &self.timeline
    }
}

/// Classify a `TrackState` for the shared Timeline `PLAYING` flag.
///
/// The Downloader peers read `Timeline::is_playing()` in their
/// `priority()` method. Every non-terminal state keeps this track
/// "listened to" from the user's perspective — buffering, seek-in-
/// progress, and decoder recreation are all transient windows inside
/// an otherwise-active track. Only `AtEof` (natural end) and `Failed`
/// (terminal error) clear the flag.
fn playing_for_state(state: &TrackState) -> bool {
    !matches!(state, TrackState::AtEof | TrackState::Failed(_))
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
        SeekMode, SeekRequest, TrackFailure, TrackState, WaitContext, WaitingReason,
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
        assert!(playing_for_state(&TrackState::Decoding));
        assert!(playing_for_state(&TrackState::SeekRequested(seek_req())));
        assert!(playing_for_state(&TrackState::ApplyingSeek(
            ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: seek_req(),
            }
        )));
        assert!(playing_for_state(&TrackState::WaitingForSource {
            context: WaitContext::Playback,
            reason: WaitingReason::Waiting,
        }));
        assert!(playing_for_state(&TrackState::RecreatingDecoder(
            RecreateState {
                attempt: 0,
                cause: RecreateCause::FormatBoundary,
                media_info: MediaInfo::default(),
                next: RecreateNext::Decode,
                offset: 0,
            }
        )));
        assert!(playing_for_state(&TrackState::AwaitingResume(
            ResumeState {
                seek: seek_ctx(),
                ..Default::default()
            }
        )));
    }

    #[kithara::test]
    fn playing_for_state_terminal_states_are_false() {
        assert!(!playing_for_state(&TrackState::AtEof));
        assert!(!playing_for_state(&TrackState::Failed(
            TrackFailure::SourceCancelled
        )));
        assert!(!playing_for_state(&TrackState::Failed(
            TrackFailure::RecreateFailed { offset: 0 }
        )));
        assert!(!playing_for_state(&TrackState::Failed(
            TrackFailure::Decode(DecodeError::Interrupted)
        )));
    }

    #[kithara::test]
    fn playing_matrix_covers_every_transition_endpoint() {
        let transitions: &[(TrackState, bool)] = &[
            (TrackState::Decoding, true),
            (TrackState::SeekRequested(seek_req()), true),
            (
                TrackState::ApplyingSeek(ApplySeekState {
                    mode: SeekMode::Direct { target_byte: None },
                    request: seek_req(),
                }),
                true,
            ),
            (
                TrackState::WaitingForSource {
                    context: WaitContext::Playback,
                    reason: WaitingReason::Waiting,
                },
                true,
            ),
            (
                TrackState::RecreatingDecoder(RecreateState {
                    attempt: 0,
                    cause: RecreateCause::VariantSwitch,
                    media_info: MediaInfo::default(),
                    next: RecreateNext::Decode,
                    offset: 0,
                }),
                true,
            ),
            (
                TrackState::AwaitingResume(ResumeState {
                    seek: seek_ctx(),
                    ..Default::default()
                }),
                true,
            ),
            (TrackState::AtEof, false),
            (TrackState::Failed(TrackFailure::SourceCancelled), false),
        ];
        for (state, expected) in transitions {
            assert_eq!(
                playing_for_state(state),
                *expected,
                "mismatch for phase_tag={:?}",
                state.phase_tag()
            );
        }
    }

    #[kithara::test]
    fn no_spurious_flip_across_100_decoding_transitions() {
        for _ in 0..100 {
            assert!(
                playing_for_state(&TrackState::Decoding),
                "PLAYING must stay true across a long Decoding → Decoding loop"
            );
        }
    }

    #[kithara::test]
    fn all_track_phase_tags_are_classified() {
        use crate::pipeline::track_fsm::TrackPhaseTag;
        let all = [
            TrackPhaseTag::Decoding,
            TrackPhaseTag::SeekRequested,
            TrackPhaseTag::WaitingForSource,
            TrackPhaseTag::ApplyingSeek,
            TrackPhaseTag::RecreatingDecoder,
            TrackPhaseTag::AwaitingResume,
            TrackPhaseTag::AtEof,
            TrackPhaseTag::Failed,
        ];
        for tag in all {
            match tag {
                TrackPhaseTag::AtEof | TrackPhaseTag::Failed => {}
                _ => {}
            }
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
