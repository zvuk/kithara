//! Stream-based audio source with format change detection.

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
use kithara_decode::{DecodeError, DecodeResult, InnerDecoder, PcmChunk, PcmSpec};
use kithara_events::{AudioEvent, SeekLifecycleStage};
use kithara_platform::{Mutex, thread::yield_now};
use kithara_stream::{
    Fetch, MediaInfo, SourcePhase, SourceSeekAnchor, Stream, StreamType, Timeline,
};
use tracing::{debug, trace, warn};

/// Nanoseconds per second for frame/duration conversion.
const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Decode progress logging interval in chunks.
const DECODE_PROGRESS_LOG_INTERVAL: u64 = 100;

/// Default read-ahead size in bytes when segment range is unknown.
const DEFAULT_READ_AHEAD_BYTES: u64 = 32 * 1024;

use crate::{
    pipeline::track_fsm::{
        ApplySeekState, DecoderSession, RecreateCause, RecreateNext, RecreateState, ResumeState,
        SeekContext, SeekMode, SeekRequest, TrackFailure, TrackPhaseTag, TrackState, TrackStep,
        WaitContext, WaitingReason, map_source_phase,
    },
    traits::AudioEffect,
    worker::{AudioCommand, AudioWorkerSource, apply_effects, flush_effects, reset_effects},
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
            fn position(&self) -> u64;
            pub(crate) fn len(&self) -> Option<u64>;
            fn media_info(&self) -> Option<MediaInfo>;
            fn current_segment_range(&self) -> Option<Range<u64>>;
            fn format_change_segment_range(&self) -> Option<Range<u64>>;
            pub(crate) fn clear_variant_fence(&self);
            pub(crate) fn set_seek_epoch(&self, seek_epoch: u64);
            fn seek_time_anchor(&self, position: Duration) -> Result<Option<SourceSeekAnchor>, io::Error>;
            fn commit_seek_landing(&self, anchor: Option<SourceSeekAnchor>);
            /// Get the shared timeline for flushing checks.
            pub(crate) fn timeline(&self) -> Timeline;
            /// Overall source readiness at current position.
            fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// Signal that the given byte range will be needed soon.
            fn demand_range(&self, range: Range<u64>);
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

    /// Build a `StreamContext` from the inner stream's source.
    pub(crate) fn build_stream_context(&self) -> Arc<dyn kithara_stream::StreamContext> {
        let stream = self.inner.lock_sync();
        T::build_stream_context(stream.source(), stream.timeline())
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
        // Ensure the stream is positioned at base_offset so reads start from
        // the correct location. This is critical when multiple fallback attempts
        // share the same underlying stream — each one may leave the position
        // in an arbitrary state.
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
    Box<dyn Fn(SharedStream<T>, &MediaInfo, u64) -> Option<Box<dyn InnerDecoder>> + Send>;

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(crate) struct StreamAudioSource<T: StreamType> {
    shared_stream: SharedStream<T>,
    /// Decoder + `base_offset` + `media_info` as an atomic unit.
    pub(crate) session: DecoderSession,
    /// Explicit FSM state — single source of truth for track phase.
    pub(crate) state: TrackState,
    decoder_factory: DecoderFactory<T>,
    epoch: Arc<AtomicU64>,
    chunks_decoded: u64,
    total_samples: u64,
    last_spec: Option<PcmSpec>,
    emit: Option<Box<dyn Fn(AudioEvent) + Send>>,
    effects: Vec<Box<dyn AudioEffect>>,
    /// Cached timeline for lock-free flushing checks.
    pub(crate) timeline: Timeline,
}

impl<T: StreamType> StreamAudioSource<T> {
    pub(crate) fn new(
        shared_stream: SharedStream<T>,
        decoder: Box<dyn InnerDecoder>,
        decoder_factory: DecoderFactory<T>,
        initial_media_info: Option<MediaInfo>,
        epoch: Arc<AtomicU64>,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> Self {
        let timeline = shared_stream.timeline();
        let session = DecoderSession {
            base_offset: 0,
            decoder,
            media_info: initial_media_info,
        };
        Self {
            shared_stream,
            session,
            state: TrackState::Decoding,
            decoder_factory,
            epoch,
            chunks_decoded: 0,
            total_samples: 0,
            last_spec: None,
            emit: None,
            effects,
            timeline,
        }
    }

    pub(crate) fn with_emit(mut self, emit: Box<dyn Fn(AudioEvent) + Send>) -> Self {
        self.emit = Some(emit);
        self
    }

    /// Detect `media_info` change and return the init-bearing boundary.
    ///
    /// The variant fence in `Source::read_at()` prevents the old decoder
    /// from reading data from a new variant. This causes Symphonia to hit
    /// EOF naturally, after which `fetch_next` recreates the decoder.
    fn detect_format_change(&self) -> Option<(MediaInfo, u64)> {
        let current_info = self.shared_stream.media_info()?;
        let codec_changed = self
            .session
            .media_info
            .as_ref()
            .is_some_and(|cached| cached.codec != current_info.codec);
        let variant_changed = self
            .session
            .media_info
            .as_ref()
            .is_some_and(|cached| cached.variant_index != current_info.variant_index);
        if !codec_changed && !variant_changed {
            return None;
        }

        // Prefer format_change_segment_range() which returns the FIRST segment
        // of the new format (where init data lives). Fall back to current_segment_range()
        // if the source doesn't support format_change_segment_range().
        let seg_range = self
            .shared_stream
            .format_change_segment_range()
            .or_else(|| self.shared_stream.current_segment_range());

        seg_range.map(|range| (current_info, range.start))
    }

    /// Apply pending format change: clear fence, seek to segment start, recreate decoder.
    /// Returns true if decoder was recreated successfully.
    fn apply_format_change(&mut self, new_info: &MediaInfo, target_offset: u64) -> bool {
        let current_pos = self.shared_stream.position();
        debug!(
            current_pos,
            target_offset,
            chunks_decoded = self.chunks_decoded,
            total_samples = self.total_samples,
            "Applying format change: old decoder finished, seeking to new segment start"
        );

        // Clear variant fence so the new decoder can read the new variant.
        self.shared_stream.clear_variant_fence();

        if let Err(e) = self.shared_stream.seek(SeekFrom::Start(target_offset)) {
            warn!(?e, target_offset, "Failed to seek to segment boundary");
            return false;
        }

        self.recreate_decoder(new_info, target_offset)
    }

    /// Track chunk statistics and emit format events.
    fn track_chunk(&mut self, chunk: &PcmChunk) {
        self.chunks_decoded += 1;
        self.total_samples += chunk.pcm.len() as u64;

        // Emit FormatDetected on first chunk
        if self.chunks_decoded == 1
            && let Some(ref emit) = self.emit
        {
            emit(AudioEvent::FormatDetected { spec: chunk.spec() });
            self.last_spec = Some(chunk.spec());
        }

        // Detect spec change (e.g. after ABR switch)
        if let Some(old_spec) = self.last_spec
            && old_spec != chunk.spec()
        {
            self.emit_event(AudioEvent::FormatChanged {
                old: old_spec,
                new: chunk.spec(),
            });
            self.last_spec = Some(chunk.spec());
        }
    }

    /// Emit an audio event if the callback is set.
    fn emit_event(&self, event: AudioEvent) {
        if let Some(ref emit) = self.emit {
            emit(event);
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors AudioEvent::SeekLifecycle fields"
    )]
    fn emit_seek_lifecycle(
        &self,
        stage: SeekLifecycleStage,
        seek_epoch: u64,
        task_id: u64,
        variant: Option<usize>,
        segment_index: Option<u32>,
        byte_range_start: Option<u64>,
        byte_range_end: Option<u64>,
    ) {
        self.emit_event(AudioEvent::SeekLifecycle {
            stage,
            seek_epoch,
            task_id,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
        });
    }

    fn seek_context(&self) -> (Option<usize>, Option<u32>, Option<u64>, Option<u64>) {
        let stream_ctx = self.shared_stream.build_stream_context();
        let segment_range = self.shared_stream.current_segment_range();
        (
            stream_ctx.variant_index(),
            stream_ctx.segment_index(),
            segment_range.as_ref().map(|range| range.start),
            segment_range.as_ref().map(|range| range.end),
        )
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

    fn decoder_seek_safe(&mut self, position: Duration) -> DecodeResult<()> {
        match catch_unwind(AssertUnwindSafe(|| self.session.decoder.seek(position))) {
            Ok(result) => result,
            Err(payload) => Err(DecodeError::InvalidData(format!(
                "decoder panic during seek: {}",
                Self::decode_panic_message(payload)
            ))),
        }
    }

    fn decoder_next_chunk_safe(&mut self) -> DecodeResult<Option<PcmChunk>> {
        match catch_unwind(AssertUnwindSafe(|| self.session.decoder.next_chunk())) {
            Ok(result) => result,
            Err(payload) => Err(DecodeError::InvalidData(format!(
                "decoder panic during next_chunk: {}",
                Self::decode_panic_message(payload)
            ))),
        }
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

        if let Some(new_decoder) =
            (self.decoder_factory)(self.shared_stream.clone(), new_info, base_offset)
        {
            let new_duration = new_decoder.duration();
            // Atomic session update — only on success
            self.session = DecoderSession {
                base_offset,
                decoder: new_decoder,
                media_info: Some(new_info.clone()),
            };
            debug!(?new_duration, base_offset, "Decoder recreated successfully");
            true
        } else {
            warn!(base_offset, "Failed to recreate decoder");
            false
        }
    }

    fn seek_request(&self) -> Option<SeekRequest> {
        match &self.state {
            TrackState::SeekRequested(request) => Some(*request),
            TrackState::ApplyingSeek(state) => Some(state.request),
            _ => None,
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
            },
            TrackState::AwaitingResume(state) => Some(state.seek.epoch),
            TrackState::RecreatingDecoder(state) => match &state.next {
                RecreateNext::Decode => None,
                RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => {
                    Some(request.seek.epoch)
                }
            },
            _ => None,
        }
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

    fn start_recreating_decoder(
        &mut self,
        cause: RecreateCause,
        media_info: MediaInfo,
        next: RecreateNext,
        offset: u64,
        attempt: u8,
    ) {
        self.state = TrackState::RecreatingDecoder(RecreateState {
            attempt,
            cause,
            media_info,
            next,
            offset,
        });
    }

    fn log_failure(&self) {
        let TrackState::Failed(failure) = &self.state else {
            return;
        };
        match failure {
            TrackFailure::Decode(err) => warn!(?err, "track failed: decode error"),
            TrackFailure::RecreateFailed { offset } => {
                warn!(offset, "track failed: decoder recreation failed");
            }
            TrackFailure::SourceCancelled => warn!("track failed: source cancelled"),
            TrackFailure::SourceStopped => warn!("track failed: source stopped"),
        }
    }

    #[expect(clippy::too_many_arguments, reason = "seek lifecycle context")]
    fn apply_seek_applied(
        &mut self,
        epoch: u64,
        position: Duration,
        variant: Option<usize>,
        segment_index: Option<u32>,
        byte_range_start: Option<u64>,
        byte_range_end: Option<u64>,
        anchor_offset: Option<u64>,
    ) {
        reset_effects(&mut self.effects);
        self.emit_seek_lifecycle(
            SeekLifecycleStage::SeekApplied,
            epoch,
            epoch,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
        );
        self.state = TrackState::AwaitingResume(ResumeState {
            recover_attempts: 0,
            seek: SeekContext {
                epoch,
                target: position,
            },
            skip: None,
            anchor_offset,
        });
    }

    fn fail_seek(&mut self, request: SeekRequest, err: DecodeError, context: &'static str) {
        warn!(
            ?err,
            epoch = request.seek.epoch,
            ?request.seek.target,
            "{context}"
        );
        self.timeline.clear_seek_pending(request.seek.epoch);
        self.state = TrackState::Failed(TrackFailure::Decode(err));
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
            self.fail_seek(request, err, "seek: decoder.seek failed");
            return false;
        }
        self.shared_stream.commit_seek_landing(None);

        let (variant, segment_index, byte_range_start, byte_range_end) = self.seek_context();
        self.apply_seek_applied(
            epoch,
            position,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
            None, // Direct seek — no anchor offset
        );
        true
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
            self.fail_seek(request, err, "seek anchor path: exact decoder seek failed");
            return false;
        }
        trace!(
            ?position,
            anchor_start = ?anchor.segment_start,
            target_offset = anchor.byte_offset,
            "seek anchor path: exact decoder seek succeeded"
        );
        self.shared_stream.commit_seek_landing(Some(anchor));

        let (variant, segment_index, byte_range_start, byte_range_end) = self.seek_context();
        self.apply_seek_applied(
            epoch,
            position,
            variant,
            segment_index,
            byte_range_start,
            byte_range_end,
            Some(anchor.byte_offset),
        );
        true
    }

    fn apply_anchor_seek_with_fallback(
        &mut self,
        request: SeekRequest,
        anchor: SourceSeekAnchor,
    ) -> bool {
        if !self.align_decoder_with_seek_anchor(request, anchor) {
            return false;
        }
        self.apply_time_anchor_seek(request, anchor)
    }

    fn align_decoder_with_seek_anchor(
        &mut self,
        request: SeekRequest,
        anchor: SourceSeekAnchor,
    ) -> bool {
        let current_codec = self.session.media_info.as_ref().and_then(|info| info.codec);
        let target_info = self.shared_stream.media_info();
        let target_codec = target_info.as_ref().and_then(|info| info.codec);
        let current_variant = self
            .session
            .media_info
            .as_ref()
            .and_then(|info| info.variant_index);
        let target_variant = target_info.as_ref().and_then(|info| info.variant_index);

        // Decoder must be recreated only when codec changed.
        // With per-variant byte maps, same-codec variant changes don't need
        // recreation — each variant has its own independent byte space, so
        // decoder seek tables remain valid within the current layout.
        let codec_changed =
            matches!((current_codec, target_codec), (Some(from), Some(to)) if from != to);
        let variant_changed =
            matches!((current_variant, target_variant), (Some(from), Some(to)) if from != to);
        let needs_recreation = codec_changed;
        let recreate_offset = if variant_changed && !codec_changed {
            anchor.byte_offset
        } else {
            self.shared_stream
                .format_change_segment_range()
                .map_or(anchor.byte_offset, |range| range.start)
        };
        trace!(
            ?current_codec,
            ?target_codec,
            ?current_variant,
            ?target_variant,
            codec_changed,
            variant_changed,
            needs_recreation,
            recreate_offset,
            base_offset = self.session.base_offset,
            "seek anchor alignment: compare format"
        );
        if !needs_recreation {
            return true;
        }

        let target_info = target_info.or_else(|| {
            self.session.media_info.clone().map(|mut info| {
                info.variant_index = target_variant;
                info
            })
        });
        let Some(target_info) = target_info else {
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

        self.start_recreating_decoder(
            RecreateCause::VariantSwitch,
            target_info,
            RecreateNext::Seek(request),
            recreate_offset,
            request.attempt,
        );
        false
    }

    fn update_decoder_len_for_seek(&self) {
        // Refresh the decoder's byte_len from the current total_bytes.
        // For base_offset > 0 (after ABR switch), subtract base_offset
        // so Symphonia sees the relative length from the switch point.
        // Without this, DRM reconciliation can shrink total_bytes after
        // decoder creation, leaving Symphonia with a stale (too large)
        // byte_len that causes SeekFailed at high seek targets.
        if let Some(len) = self.shared_stream.len()
            && len > 0
        {
            let relative = len.saturating_sub(self.session.base_offset);
            self.session.decoder.update_byte_len(relative);
        }
    }

    fn duration_for_frames(spec: PcmSpec, frames: usize) -> Duration {
        if spec.sample_rate == 0 {
            return Duration::ZERO;
        }
        let nanos = (frames as u128)
            .saturating_mul(NANOS_PER_SEC)
            .saturating_div(u128::from(spec.sample_rate));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to u64::MAX before cast"
        )]
        {
            Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
        }
    }

    fn frames_for_duration(spec: PcmSpec, duration: Duration) -> usize {
        if spec.sample_rate == 0 {
            return 0;
        }
        let frames = duration
            .as_nanos()
            .saturating_mul(u128::from(spec.sample_rate))
            .saturating_div(NANOS_PER_SEC);
        frames.min(usize::MAX as u128) as usize
    }

    fn apply_seek_skip(&mut self, epoch: u64, mut chunk: PcmChunk) -> Option<PcmChunk> {
        let Some(resume) = self.resume_state().copied() else {
            return Some(chunk);
        };
        let Some(remaining) = resume.skip else {
            return Some(chunk);
        };
        if resume.seek.epoch != epoch {
            if let Some(state) = self.resume_state_mut() {
                state.skip = None;
            }
            return Some(chunk);
        }
        if remaining.is_zero() {
            if let Some(state) = self.resume_state_mut() {
                state.skip = None;
            }
            return Some(chunk);
        }

        let spec = chunk.spec();
        let channels = usize::from(spec.channels.max(1));
        let chunk_frames = chunk.frames();
        if chunk_frames == 0 {
            return None;
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
            return None;
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
        if let Some(state) = self.resume_state_mut() {
            state.skip = None;
        }
        Some(chunk)
    }
}

/// Whether the decode loop should continue or return.
enum DecodeAction {
    Yield,
    Return(DecodeResult<Option<PcmChunk>>),
}

enum DecodeStep {
    Produced(Fetch<PcmChunk>),
    Interrupted,
    Eof,
    Failed,
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Handle decoder EOF: try format change recovery, then true EOF.
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
            return DecodeAction::Return(Ok(Some(flushed)));
        }

        self.emit_event(AudioEvent::EndOfStream);
        DecodeAction::Return(Ok(None))
    }

    /// Handle an explicit source-level variant boundary signal.
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

    /// Handle decode error without boundary fallback.
    fn handle_decode_error(e: DecodeError) -> DecodeAction {
        warn!(?e, "decode error");
        DecodeAction::Return(Err(e))
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    /// Core decode loop — produces one PCM chunk or signals EOF/error.
    ///
    /// Replaces the old `FallibleIterator::next` implementation.
    /// Called from `decode_one_fetch` to drive the decoder.
    #[kithara_hang_detector::hang_watchdog]
    fn decode_next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        loop {
            hang_tick!();
            yield_now();

            // Exit immediately when a seek is pending so the worker
            // loop can call apply_pending_seek().  This guard is
            // necessary because Symphonia silently retries
            // io::ErrorKind::Interrupted (standard Rust convention),
            // so a flushing signal sent from wait_range may never
            // escape the decoder's internal read loop.
            if self.timeline.is_flushing() || self.timeline.is_seek_pending() {
                trace!(
                    flushing = self.timeline.is_flushing(),
                    seek_pending = self.timeline.is_seek_pending(),
                    current_epoch = self.epoch.load(Ordering::Acquire),
                    timeline_epoch = self.timeline.seek_epoch(),
                    phase = ?self.state.phase_tag(),
                    "decode_next_chunk: exiting early because seek gate is active"
                );
                return Err(DecodeError::Interrupted);
            }

            match self.decoder_next_chunk_safe() {
                Ok(Some(chunk)) => {
                    let current_epoch = self.epoch.load(Ordering::Acquire);
                    let Some(chunk) = self.apply_seek_skip(current_epoch, chunk) else {
                        continue;
                    };
                    if chunk.pcm.is_empty() {
                        continue;
                    }
                    hang_reset!();

                    self.track_chunk(&chunk);

                    if self
                        .chunks_decoded
                        .is_multiple_of(DECODE_PROGRESS_LOG_INTERVAL)
                    {
                        trace!(
                            chunks = self.chunks_decoded,
                            samples = self.total_samples,
                            spec = ?chunk.spec(),
                            "decode progress"
                        );
                    }

                    match apply_effects(&mut self.effects, chunk) {
                        Some(processed) => return Ok(Some(processed)),
                        None => continue,
                    }
                }
                Ok(None) => match self.handle_decode_eof() {
                    DecodeAction::Yield => return Err(DecodeError::Interrupted),
                    DecodeAction::Return(result) => return result,
                },
                Err(e) if e.is_variant_change() => match self.handle_variant_change(e) {
                    DecodeAction::Yield => return Err(DecodeError::Interrupted),
                    DecodeAction::Return(result) => return result,
                },
                Err(e) if e.is_interrupted() => {
                    trace!("decode interrupted by seek, retrying");
                    continue;
                }
                Err(e) => match Self::handle_decode_error(e) {
                    DecodeAction::Yield => return Err(DecodeError::Interrupted),
                    DecodeAction::Return(result) => return result,
                },
            }
        }
    }
}

// Private helpers (renamed from old AudioWorkerSource methods)

impl<T: StreamType> StreamAudioSource<T> {
    fn boundary_end(&self, start: u64) -> u64 {
        self.shared_stream.len().map_or_else(
            || start.saturating_add(DEFAULT_READ_AHEAD_BYTES),
            |len| start.saturating_add(DEFAULT_READ_AHEAD_BYTES).min(len),
        )
    }

    fn source_is_ready_for_boundary(&self, start: u64) -> bool {
        let end = self.boundary_end(start);
        self.source_ready_for_range(start..end)
    }

    fn source_phase_for_boundary(&self, start: u64) -> SourcePhase {
        let end = self.boundary_end(start);
        self.shared_stream.phase_at(start..end)
    }

    fn source_ready_for_range(&self, range: Range<u64>) -> bool {
        matches!(
            self.shared_stream.phase_at(range),
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
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
                || pos.saturating_add(DEFAULT_READ_AHEAD_BYTES),
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
            SeekMode::Direct => self.source_is_ready(),
        }
    }

    fn source_phase_for_wait_context(&self, context: &WaitContext) -> SourcePhase {
        match context {
            WaitContext::ApplySeek(applying) => match applying.mode {
                SeekMode::Anchor(anchor) => self.source_phase_for_boundary(anchor.byte_offset),
                SeekMode::Direct => self.shared_stream.phase(),
            },
            WaitContext::Recreation(recreate) => self.source_phase_for_boundary(recreate.offset),
            _ => self.shared_stream.phase(),
        }
    }

    /// Submit a demand signal for the byte range corresponding to the
    /// current `WaitingForSource` state.  This is a non-blocking hint
    /// that tells the source (and transitively the downloader) which
    /// data the worker needs next.
    fn submit_demand_for_current_state(&self) {
        let TrackState::WaitingForSource { context, .. } = &self.state else {
            return;
        };
        let start = match context {
            WaitContext::ApplySeek(applying) => {
                if let SeekMode::Anchor(anchor) = applying.mode {
                    anchor.byte_offset
                } else {
                    self.shared_stream.position()
                }
            }
            WaitContext::Recreation(recreate) => recreate.offset,
            _ => self.shared_stream.position(),
        };
        self.shared_stream
            .demand_range(start..start.saturating_add(1));
    }

    /// Decode one chunk using the decode loop.
    fn decode_one_step(&mut self) -> DecodeStep {
        if self.timeline.total_duration().is_none() {
            let duration = self.session.decoder.duration();
            if duration.is_some() {
                self.timeline.set_total_duration(duration);
            }
        }
        let current_epoch = self.epoch.load(Ordering::Acquire);
        match self.decode_next_chunk() {
            Ok(Some(chunk)) => {
                if self
                    .resume_state()
                    .is_some_and(|resume| resume.seek.epoch == current_epoch)
                {
                    let segment_range = self.shared_stream.current_segment_range();
                    self.emit_seek_lifecycle(
                        SeekLifecycleStage::DecodeStarted,
                        current_epoch,
                        current_epoch,
                        chunk.meta.variant_index,
                        chunk.meta.segment_index,
                        segment_range.as_ref().map(|range| range.start),
                        segment_range.as_ref().map(|range| range.end),
                    );
                    // FSM: AwaitingResume → Decoding (first valid chunk)
                    self.state = TrackState::Decoding;
                }
                DecodeStep::Produced(Fetch::new(chunk, false, current_epoch))
            }
            Ok(None) => {
                self.state = TrackState::AtEof;
                DecodeStep::Eof
            }
            Err(e) if e.is_interrupted() => {
                // Seek-pending exit from the decode loop guard.
                DecodeStep::Interrupted
            }
            Err(e) => {
                self.state = TrackState::Failed(TrackFailure::Decode(e));
                DecodeStep::Failed
            }
        }
    }

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
            self.state = TrackState::Decoding;
            return;
        }

        let current_epoch = self.epoch.load(Ordering::Acquire);
        if epoch <= current_epoch {
            trace!(
                current_epoch,
                stale_epoch = epoch,
                "apply_pending_seek: dropping stale seek"
            );
            self.timeline.complete_seek(epoch);
            self.timeline.clear_seek_pending(epoch);
            self.state = TrackState::Decoding;
            return;
        }

        if request.attempt == 0 {
            let (variant, segment_index, byte_range_start, byte_range_end) = self.seek_context();
            self.emit_seek_lifecycle(
                SeekLifecycleStage::SeekRequest,
                epoch,
                epoch,
                variant,
                segment_index,
                byte_range_start,
                byte_range_end,
            );
        }

        self.shared_stream.set_seek_epoch(epoch);
        self.shared_stream.clear_variant_fence();
        let anchor_result = self.shared_stream.seek_time_anchor(position);
        self.timeline.complete_seek(epoch);
        self.shared_stream.notify_waiting();

        let mode = match anchor_result {
            Ok(Some(anchor)) => SeekMode::Anchor(anchor),
            Ok(None) => SeekMode::Direct,
            Err(err) => {
                self.fail_seek(
                    request,
                    DecodeError::SeekFailed(format!("seek anchor resolution failed: {err}")),
                    "seek anchor resolution failed",
                );
                return;
            }
        };
        self.state = TrackState::ApplyingSeek(ApplySeekState { mode, request });
    }
}

// FSM step methods

impl<T: StreamType> StreamAudioSource<T> {
    fn step_decoding(&mut self) -> TrackStep<PcmChunk> {
        if !self.source_is_ready() {
            let phase = self.shared_stream.phase();
            trace!(
                ?phase,
                epoch = self.epoch.load(Ordering::Acquire),
                "step_decoding: source not ready"
            );
            if let Some(reason) = map_source_phase(phase) {
                self.state = TrackState::WaitingForSource {
                    context: WaitContext::Playback,
                    reason,
                };
                return TrackStep::Blocked(reason);
            }
            match phase {
                SourcePhase::Cancelled => {
                    self.state = TrackState::Failed(TrackFailure::SourceCancelled);
                    return TrackStep::Failed;
                }
                SourcePhase::Stopped => {
                    self.state = TrackState::Failed(TrackFailure::SourceStopped);
                    return TrackStep::Failed;
                }
                _ => return TrackStep::Blocked(WaitingReason::Waiting),
            }
        }

        match self.decode_one_step() {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }

    fn step_seek_requested(&mut self) -> TrackStep<PcmChunk> {
        if !self.source_is_ready() {
            let phase = self.shared_stream.phase();
            if let Some(reason) = map_source_phase(phase) {
                let request = match &self.state {
                    TrackState::SeekRequested(request) => *request,
                    _ => return TrackStep::StateChanged,
                };
                self.state = TrackState::WaitingForSource {
                    context: WaitContext::Seek(request),
                    reason,
                };
                return TrackStep::Blocked(reason);
            }
        }
        // Source is ready — resolve seek mode first.
        self.apply_seek_from_timeline();
        TrackStep::StateChanged
    }

    fn step_applying_seek(&mut self) -> TrackStep<PcmChunk> {
        let applying = match &self.state {
            TrackState::ApplyingSeek(state) => *state,
            _ => return TrackStep::StateChanged,
        };
        if !self.source_is_ready_for_apply_seek(applying) {
            let phase = self.source_phase_for_wait_context(&WaitContext::ApplySeek(applying));
            if let Some(reason) = map_source_phase(phase) {
                self.state = TrackState::WaitingForSource {
                    context: WaitContext::ApplySeek(applying),
                    reason,
                };
                return TrackStep::Blocked(reason);
            }
            match phase {
                SourcePhase::Cancelled => {
                    self.state = TrackState::Failed(TrackFailure::SourceCancelled);
                    return TrackStep::Failed;
                }
                SourcePhase::Stopped => {
                    self.state = TrackState::Failed(TrackFailure::SourceStopped);
                    return TrackStep::Failed;
                }
                _ => return TrackStep::Blocked(WaitingReason::Waiting),
            }
        }
        let request = applying.request;
        let applied = match applying.mode {
            SeekMode::Anchor(anchor) => self.apply_anchor_seek_with_fallback(request, anchor),
            SeekMode::Direct => self.apply_seek_from_decoder(request),
        };
        if applied {
            self.epoch.store(request.seek.epoch, Ordering::Release);
            self.timeline.clear_seek_pending(request.seek.epoch);
            trace!(
                epoch = request.seek.epoch,
                flushing = self.timeline.is_flushing(),
                seek_pending = self.timeline.is_seek_pending(),
                stream_pos = self.shared_stream.position(),
                segment_range = ?self.shared_stream.current_segment_range(),
                "step_applying_seek: seek applied"
            );
        }
        TrackStep::StateChanged
    }

    fn step_waiting_for_source(&mut self) -> TrackStep<PcmChunk> {
        let Some((phase, stored_reason)) = (match &self.state {
            TrackState::WaitingForSource { context, reason } => {
                Some((self.source_phase_for_wait_context(context), *reason))
            }
            _ => None,
        }) else {
            return TrackStep::StateChanged;
        };

        // Still waiting?
        if let Some(reason) = map_source_phase(phase) {
            // Submit demand so the downloader knows which data we need.
            // Without this, the worker can deadlock after seek: it polls
            // phase_at() (pure query) but nobody tells the downloader to
            // fetch the target segment.
            self.submit_demand_for_current_state();
            trace!(
                ?phase,
                ?reason,
                ?stored_reason,
                epoch = self.epoch.load(Ordering::Acquire),
                "step_waiting_for_source: still blocked"
            );
            return TrackStep::Blocked(reason);
        }

        // Terminal phases
        match phase {
            SourcePhase::Cancelled => {
                self.state = TrackState::Failed(TrackFailure::SourceCancelled);
                return TrackStep::Failed;
            }
            SourcePhase::Stopped => {
                self.state = TrackState::Failed(TrackFailure::SourceStopped);
                return TrackStep::Failed;
            }
            SourcePhase::Eof => {
                self.state = TrackState::AtEof;
                return TrackStep::Eof;
            }
            _ => {} // Ready, Seeking — proceed
        }

        // Source is ready — resume based on wait context
        let old_state = std::mem::replace(&mut self.state, TrackState::Decoding);
        match old_state {
            TrackState::WaitingForSource {
                context: WaitContext::Playback,
                ..
            } => {
                self.state = TrackState::Decoding;
            }
            TrackState::WaitingForSource {
                context: WaitContext::Seek(ctx),
                ..
            } => {
                self.state = TrackState::SeekRequested(ctx);
            }
            TrackState::WaitingForSource {
                context: WaitContext::ApplySeek(applying),
                ..
            } => {
                self.state = TrackState::ApplyingSeek(applying);
            }
            TrackState::WaitingForSource {
                context: WaitContext::Recreation(recreate),
                ..
            } => {
                self.state = TrackState::RecreatingDecoder(recreate);
            }
            _ => {
                // Already set to Decoding by mem::replace
            }
        }
        TrackStep::StateChanged
    }

    fn step_recreating_decoder(&mut self) -> TrackStep<PcmChunk> {
        let recreate = match &self.state {
            TrackState::RecreatingDecoder(recreate) => recreate.clone(),
            _ => return TrackStep::StateChanged,
        };
        if !self.source_is_ready_for_boundary(recreate.offset) {
            let phase = self.source_phase_for_boundary(recreate.offset);
            if let Some(reason) = map_source_phase(phase) {
                let recreate = match std::mem::replace(&mut self.state, TrackState::Decoding) {
                    TrackState::RecreatingDecoder(recreate) => recreate,
                    other => {
                        self.state = other;
                        return TrackStep::StateChanged;
                    }
                };
                self.state = TrackState::WaitingForSource {
                    context: WaitContext::Recreation(recreate),
                    reason,
                };
                self.submit_demand_for_current_state();
                return TrackStep::Blocked(reason);
            }
            match phase {
                SourcePhase::Cancelled => {
                    self.state = TrackState::Failed(TrackFailure::SourceCancelled);
                    return TrackStep::Failed;
                }
                SourcePhase::Stopped => {
                    self.state = TrackState::Failed(TrackFailure::SourceStopped);
                    return TrackStep::Failed;
                }
                _ => return TrackStep::Blocked(WaitingReason::Waiting),
            }
        }

        let recreate = match std::mem::replace(&mut self.state, TrackState::Decoding) {
            TrackState::RecreatingDecoder(recreate) => recreate,
            other => {
                self.state = other;
                return TrackStep::StateChanged;
            }
        };
        debug!(
            cause = ?recreate.cause,
            offset = recreate.offset,
            attempt = recreate.attempt,
            "step_recreating_decoder: start"
        );

        let recreated = if recreate.cause == RecreateCause::FormatBoundary
            && matches!(recreate.next, RecreateNext::Decode)
        {
            self.apply_format_change(&recreate.media_info, recreate.offset)
        } else {
            self.shared_stream.clear_variant_fence();
            if let Err(err) = self.shared_stream.seek(SeekFrom::Start(recreate.offset)) {
                warn!(
                    ?err,
                    cause = ?recreate.cause,
                    offset = recreate.offset,
                    "step_recreating_decoder: failed to seek stream"
                );
                self.state = TrackState::Failed(TrackFailure::RecreateFailed {
                    offset: recreate.offset,
                });
                return TrackStep::Failed;
            }
            // Clear variant fence before recreation — the new decoder reads
            // from a different variant's segment. Without this, read_at
            // returns VariantChange (fence mismatch) and Symphonia probe fails.
            self.shared_stream.clear_variant_fence();
            self.recreate_decoder(&recreate.media_info, recreate.offset)
        };
        if !recreated {
            self.state = TrackState::Failed(TrackFailure::RecreateFailed {
                offset: recreate.offset,
            });
            return TrackStep::Failed;
        }

        match recreate.next {
            RecreateNext::Decode => {
                reset_effects(&mut self.effects);
                self.state = TrackState::Decoding;
                TrackStep::StateChanged
            }
            RecreateNext::Seek(request) => {
                self.state = TrackState::SeekRequested(request);
                TrackStep::StateChanged
            }
            RecreateNext::ApplySeek(request) => match self.decoder_seek_safe(request.seek.target) {
                Ok(()) => {
                    self.shared_stream.commit_seek_landing(None);
                    let (variant, segment_index, byte_range_start, byte_range_end) =
                        self.seek_context();
                    self.apply_seek_applied(
                        request.seek.epoch,
                        request.seek.target,
                        variant,
                        segment_index,
                        byte_range_start,
                        byte_range_end,
                        None, // Recreate path — no anchor offset available
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
            },
        }
    }

    fn step_awaiting_resume(&mut self) -> TrackStep<PcmChunk> {
        // Use anchor offset for readiness check when available. The decoder
        // may have landed at a different byte position than the anchor, but
        // StreamIndex layout is built around the anchor offset (from reset_to).
        let anchor_offset = match &self.state {
            TrackState::AwaitingResume(resume) => resume.anchor_offset,
            _ => None,
        };
        if !self.source_is_ready() {
            let phase = self.shared_stream.phase();
            if let Some(reason) = map_source_phase(phase) {
                trace!(
                    ?phase,
                    ?reason,
                    stream_pos = self.shared_stream.position(),
                    ?anchor_offset,
                    epoch = self.epoch.load(Ordering::Acquire),
                    "step_awaiting_resume: source not ready"
                );
                // NOTE: anchor-based demand intentionally NOT sent here.
                // It causes DRM regression where encrypted/decrypted sizes
                // differ. Standard demand via submit_demand_for_current_state
                // (in step_waiting_for_source) uses byte_position.
                self.state = TrackState::WaitingForSource {
                    context: WaitContext::Playback,
                    reason,
                };
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
}

// AudioWorkerSource trait implementation

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;
    type Command = AudioCommand;

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        // 1. Seek preemption: detect new seek epoch from Timeline.
        //    Skip if the FSM is already handling this epoch (SeekRequested).
        let timeline_epoch = self.timeline.seek_epoch();
        let current_epoch = self.epoch.load(Ordering::Acquire);
        let already_handling = self
            .active_seek_epoch()
            .is_some_and(|epoch| epoch >= timeline_epoch);
        if timeline_epoch > current_epoch
            && self.timeline.seek_target().is_some()
            && !self.state.is_terminal()
            && !already_handling
        {
            trace!(
                timeline_epoch,
                current_epoch,
                phase = ?self.state.phase_tag(),
                "step_track: seek preemption fired"
            );
            self.state = TrackState::SeekRequested(SeekRequest {
                attempt: 0,
                seek: SeekContext {
                    epoch: timeline_epoch,
                    target: self.timeline.seek_target().unwrap_or(Duration::ZERO),
                },
            });
            reset_effects(&mut self.effects);
            return TrackStep::StateChanged;
        }

        // 2. State dispatch — one transition per call
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

    fn handle_command(&mut self, _cmd: Self::Command) {
        // No commands left to handle. Seek flows through Timeline.
    }

    fn timeline(&self) -> &Timeline {
        &self.timeline
    }
}
