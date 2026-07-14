use std::{
    any::Any,
    io::{self, Read, Seek, SeekFrom},
    mem,
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use arc_swap::ArcSwap;
use crossbeam_queue::ArrayQueue;
use delegate::delegate;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, ErrorClass,
    GaplessMode, InputRequirement, PcmChunk, PcmSpec, duration_for_frames,
};
use kithara_events::{AudioEvent, AudioFormat, DeferredBus, SeekLifecycleStage, SegmentLocation};
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_blocking_on},
};
use kithara_stream::{
    Activity, ByteMap, ContainerFormat, MediaInfo, PendingReason, PlayheadWrite, SeekControl,
    SeekObserve, SourcePhase, SourceSeekAnchor, Stream, StreamType, WorkerWake,
};
use kithara_test_utils::kithara;
use tracing::{debug, info, trace, warn};

use crate::{
    pipeline::{
        fetch::Fetch,
        gapless::GaplessStage,
        track_fsm::{
            ApplySeekState, ApplyingSeek, AwaitingResume, CurrentFsm, DecoderRebuildComplete,
            DecoderSession, Decoding, RebuildState, RebuildingDecoder, RecreateCause, RecreateNext,
            RecreateOutcome, RecreateState, RecreatingDecoder, ResumeState, SeekContext, SeekMode,
            SeekRequest, SeekRequested, Track, TrackFailure, TrackStep, WaitContext, WaitState,
            WaitingForSource, WaitingReason, map_source_phase,
        },
    },
    traits::AudioEffect,
    worker::{AudioWorkerSource, apply_effects, reset_effects},
};

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
pub(crate) struct SharedStream<T: StreamType> {
    /// Construction-phase read mode, shared across clones. When set, `Read`
    /// routes through the blocking off-RT [`Stream::read`] adapter (waits for
    /// the seeked range to download, cancel-bounded by its own timeout)
    /// instead of the non-blocking RT [`Stream::probe_read`]. `Audio::new`
    /// arms it for the single up-front decoder build (with the init body
    /// prefetched, the build read normally hits committed bytes; the blocking
    /// adapter is the bounded safety net for residual lateness) and disarms it
    /// before the worker is registered, so the RT decode loop the worker then
    /// drives always uses `probe_read`. See the crate `CONTEXT.md`
    /// "Construction reads".
    blocking: Arc<AtomicBool>,
    inner: Arc<Mutex<Stream<T>>>,
}

impl<T: StreamType> SharedStream<T> {
    pub(crate) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
            blocking: Arc::new(AtomicBool::new(false)),
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
        to self.inner.lock() {
            pub(crate) fn position(&self) -> u64;
            /// Absolute byte cursor set — forwards to the inner source's
            /// atomic, used post-seek when the audio FSM lands at a
            /// known byte position.
            pub(crate) fn set_position(&self, pos: u64);
            pub(crate) fn len(&self) -> Option<u64>;
            fn media_info(&self) -> Option<MediaInfo>;
            pub(crate) fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            fn format_change_segment_range(&self) -> kithara_stream::StreamResult<Range<u64>>;
            pub(crate) fn clear_variant_fence(&self);
            pub(crate) fn has_variant_change_pending(&self) -> bool;
            fn variant_change_target(&self) -> Option<usize>;
            fn seek_time_anchor(&self, position: Duration) -> Result<Option<SourceSeekAnchor>, io::Error>;
            /// Build a fresh reader-side event-sink instance from the inner source.
            pub(crate) fn take_reader_event_sink(&self) -> Option<kithara_stream::BoxedEventSink>;
            /// Pull a clone of the optional byte-map handle from the
            /// inner source. Used by the decoder factory to activate the
            /// segment-by-segment fMP4 path on HLS.
            pub(crate) fn byte_map(&self) -> Option<Arc<dyn ByteMap>>;
            /// Narrow mutating playhead handle.
            pub(crate) fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;
            /// Narrow seek-control handle.
            pub(crate) fn seek_control(&self) -> Arc<dyn SeekControl>;
            /// Narrow seek-observe handle.
            pub(crate) fn seek_observe(&self) -> Arc<dyn SeekObserve>;
            /// Narrow activity handle.
            pub(crate) fn activity(&self) -> Arc<dyn Activity>;
            /// Overall source readiness at current position.
            pub(crate) fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// The reader→peer wake handle — `Some` for segmented sources
            /// (HLS) that push a downloader peer. The FSM arms it on the
            /// produce core (seek-apply / finalize); the scheduler shell
            /// flushes it off the forbid-blocking path.
            pub(crate) fn peer_wake(&self) -> Option<Arc<kithara_stream::DeferredWake>>;
            /// Install the audio worker's data-arrival wake on the inner
            /// source. Segmented sources (HLS) fire it from their off-RT
            /// write/settle sites; no-op for non-segmented sources. Set once,
            /// after the worker exists.
            pub(crate) fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>);
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
        let mut stream = self.inner.lock();
        if self.blocking.load(Ordering::Acquire) {
            stream.read(buf)
        } else {
            stream.probe_read(buf)
        }
    }
}

impl<T: StreamType> Seek for SharedStream<T> {
    delegate! {
        to self.inner.lock() {
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
pub(crate) type DecoderFactory<T> = Arc<
    dyn Fn(SharedStream<T>, MediaInfo, u64) -> Result<Box<dyn Decoder>, DecodeError> + Send + Sync,
>;

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(crate) struct StreamAudioSource<T: StreamType> {
    /// Explicit FSM state — single source of truth for track phase.
    pub(crate) state: CurrentFsm,
    /// Decoder + `base_offset` + `media_info` as an atomic unit.
    pub(crate) session: DecoderSession,
    /// Narrow activity handle — set/query the `PLAYING` flag.
    activity: Arc<dyn Activity>,
    epoch: Arc<AtomicU64>,
    /// Narrow mutating playhead handle — committed position and total duration.
    playhead: Arc<dyn PlayheadWrite>,
    /// Narrow seek-control handle — begin / complete / clear-pending.
    seek: Arc<dyn SeekControl>,
    /// Narrow seek-observe handle — read seek state without mutation.
    seek_obs: Arc<dyn SeekObserve>,
    decoder_factory: DecoderFactory<T>,
    runtime_handle: RuntimeHandle,
    worker_wake: Arc<dyn WorkerWake>,
    rebuild_completion: Arc<ArrayQueue<DecoderRebuildComplete>>,
    pending_rebuild_submit: Option<DecoderRebuildJob<T>>,
    next_rebuild_ticket: u64,
    /// Gapless trim mode applied per-track. Built once at construction;
    /// ABR variant switches inside one track keep the same trimmer
    /// (production semantics) so we never retrim audible content
    /// around a recreate boundary.
    gapless_mode: GaplessMode,
    /// Per-track gapless trimmer adapter.
    gapless: GaplessStage,
    /// Absolute content frame offset just past the most recently emitted chunk
    /// (the producer's decode head), tagged with its epoch. A mid-playback
    /// variant-switch recreate continues the new decoder from here — NOT from
    /// the consumer's lagging `committed_position`: the chunks in
    /// `[committed..decode_head]` are already queued in the outlet ring (a
    /// `FormatBoundary` recreate neither flushes it nor bumps the seek epoch),
    /// so resuming at `committed` would re-emit them and rewind content. Stored
    /// as an exact frame plus the sample rate of that produced chunk, then
    /// converted back with `duration_for_frames`; the demuxer quantizes the
    /// seek landing to a sample and `frame_offset_for` rounds to the nearest
    /// frame, so the rebuilt decoder relabels its first chunk at this point. See
    /// `execute_recreation`.
    decode_head: Option<(u64, u64, u32)>,
    /// Deferred sink for FSM lifecycle events ([`AudioEvent`]). The FSM runs on
    /// the produce core, so `emit_event` enqueues lock-free; the scheduler shell
    /// flushes via [`flush_deferred`](AudioWorkerSource::flush_deferred) and on
    /// `Drop`, keeping the cross-thread `broadcast::send` (a `kevent`) off the
    /// forbid path. `None` for sources built without an event bus.
    #[field(with, option_set_some, vis = "pub(crate)")]
    emit: Option<DeferredBus<AudioEvent>>,
    /// Incremental end-of-stream effect drain. Allocated at source construction;
    /// true EOF only flips booleans and pulls one tail chunk per pass.
    eof_drain_inputs_exhausted: Vec<bool>,
    eof_drain_active: bool,
    /// Host/device sample rate last observed by the decoder recreate owner.
    /// A change requests a normal decoder recreate when decoder output is
    /// planned in the host-rate domain.
    host_sample_rate: Arc<AtomicU32>,
    recreate_on_host_rate_change: bool,
    decoder_host_sample_rate: u32,
    last_spec: Option<PcmSpec>,
    /// Reader→peer wake handle, resolved from [`Source::peer_wake`] at
    /// construction. The FSM runs on the produce core, so it `arm`s this
    /// (lock-free) at seek-apply / `finalize_seek_pending`; the scheduler shell
    /// flushes it off the forbid path. Holding the handle directly (not via the
    /// `SharedStream` mutex) keeps the arm out of the lock the FSM already holds
    /// at every `clear_seek_pending` callsite. `None` for file streams.
    peer_wake: Option<Arc<kithara_stream::DeferredWake>>,
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
    /// Decoders displaced on the produce core. They are dropped from
    /// `flush_deferred`, outside the forbid-blocking region.
    retired_decoders: ArrayQueue<Box<dyn Decoder>>,
    decoder_retire_overflowed: AtomicBool,
    shared_stream: SharedStream<T>,
    effects: Vec<Box<dyn AudioEffect>>,
    chunks_decoded: u64,
    total_samples: u64,
}

/// Initial decode state handed to [`StreamAudioSource::new`]: the freshly
/// built `decoder`, the `decoder_factory` for later ABR/seek rebuilds, the
/// optional `media_info`, and the `gapless_mode`.
pub(crate) struct DecodeInit<T: StreamType> {
    pub(crate) decoder: Box<dyn Decoder>,
    pub(crate) decoder_factory: DecoderFactory<T>,
    pub(crate) gapless_mode: GaplessMode,
    pub(crate) host_sample_rate: Arc<AtomicU32>,
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) recreate_on_host_rate_change: bool,
}

struct DecoderRebuildJob<T: StreamType> {
    ticket: u64,
    shared_stream: SharedStream<T>,
    media_info: MediaInfo,
    base_offset: u64,
    decoder_factory: DecoderFactory<T>,
    completion: Arc<ArrayQueue<DecoderRebuildComplete>>,
    worker_wake: Arc<dyn WorkerWake>,
}

impl<T: StreamType> DecoderRebuildJob<T> {
    fn run(self) {
        let Self {
            ticket,
            shared_stream,
            media_info,
            base_offset,
            decoder_factory,
            completion,
            worker_wake,
        } = self;
        // Decoder construction is panic-prone, exactly like
        // `next_chunk`/`seek` on the produce core (both wrapped in
        // `catch_unwind`). A factory panic on the blocking pool would
        // unwind out of `run`, drop the `JoinHandle` with no completion
        // pushed, and strand the FSM in `RebuildingDecoder` forever — a
        // permanent hang under `block_on_underrun`. Catching it here and
        // pushing `SoftFailed` preserves the old synchronous path's
        // "factory panic -> track failed" liveness, now off the RT core.
        let result = match catch_unwind(AssertUnwindSafe(|| {
            decoder_factory(shared_stream, media_info, base_offset)
        })) {
            Ok(built) => built.map_err(|e| classify_recreate_err(&e)),
            Err(payload) => {
                warn!(
                    ticket,
                    base_offset,
                    panic = %StreamAudioSource::<T>::decode_panic_message(payload),
                    "decoder factory panicked during rebuild; failing track"
                );
                Err(RecreateOutcome::SoftFailed)
            }
        };
        let complete = DecoderRebuildComplete { result, ticket };
        // `prepare_rebuild` is the only spawn site and the FSM guarantees
        // at most one outstanding rebuild job, so this push never contends;
        // the pop/retry/warn branch is defensive only.
        if let Err(complete) = completion.push(complete) {
            let _ = completion.pop();
            if completion.push(complete).is_err() {
                warn!(ticket, "decoder rebuild completion queue overflowed");
            }
        }
        worker_wake.wake();
    }
}

/// The seek that failed inside [`StreamAudioSource::recover_from_decoder_seek_error`]:
/// the `request`, the target `position`, the `recreate_offset` to rebuild the
/// decoder at, and the `seek_mode` (direct vs anchor). The triggering
/// `DecodeError` is passed separately.
#[derive(Clone, Copy)]
struct SeekAttempt {
    position: Duration,
    seek_mode: SeekMode,
    request: SeekRequest,
    recreate_offset: u64,
}

// Construction, lifecycle, and state access
impl<T: StreamType> StreamAudioSource<T> {
    /// Default read-ahead size in bytes when segment range is unknown.
    const DEFAULT_READ_AHEAD_BYTES: u64 = 32 * 1024;

    /// Bounded off-RT retire queue for decoders displaced on the produce core.
    const DECODER_RETIRE_CAPACITY: usize = 4;

    /// Nanoseconds per second for frame/duration conversion.
    const NANOS_PER_SEC: u128 = 1_000_000_000;

    const SEEK_ANCHOR_NO_INIT_RANGE: &str =
        "seek anchor alignment: decoder recreate has no init segment range";
    const SEEK_RECOVERY_NO_INIT_RANGE: &str =
        "seek recovery: decoder recreate requires init segment range";
    const SEEK_ANCHOR_RESOLUTION_FAILED: &str = "seek anchor resolution failed";
    const VARIANT_CHANGE_NO_FORMAT_TRANSITION: &str =
        "variant change signal without observable format transition";

    pub(crate) fn new(
        shared_stream: SharedStream<T>,
        init: DecodeInit<T>,
        epoch: Arc<AtomicU64>,
        effects: Vec<Box<dyn AudioEffect>>,
        runtime_handle: RuntimeHandle,
        worker_wake: Arc<dyn WorkerWake>,
    ) -> Self {
        let DecodeInit {
            decoder,
            decoder_factory,
            media_info: initial_media_info,
            gapless_mode,
            host_sample_rate,
            recreate_on_host_rate_change,
        } = init;
        let decoder_host_sample_rate = host_sample_rate.load(Ordering::Acquire);
        let playhead = shared_stream.playhead_write();
        let seek = shared_stream.seek_control();
        let seek_obs = shared_stream.seek_observe();
        let activity = shared_stream.activity();
        let peer_wake = shared_stream.peer_wake();
        let gapless =
            GaplessStage::build(decoder.as_ref(), gapless_mode, initial_media_info.as_ref());
        let session = DecoderSession {
            decoder,
            base_offset: 0,
            media_info: initial_media_info,
            installed_at_seek_epoch: seek_obs.epoch(),
        };
        let eof_drain_inputs_exhausted = vec![false; effects.len()];
        activity.set_playing(true);
        Self {
            shared_stream,
            session,
            decoder_factory,
            runtime_handle,
            worker_wake,
            rebuild_completion: Arc::new(ArrayQueue::new(2)),
            pending_rebuild_submit: None,
            next_rebuild_ticket: 1,
            epoch,
            effects,
            playhead,
            seek,
            seek_obs,
            activity,
            gapless_mode,
            gapless,
            peer_wake,
            eof_drain_inputs_exhausted,
            eof_drain_active: false,
            host_sample_rate,
            recreate_on_host_rate_change,
            decoder_host_sample_rate,
            state: CurrentFsm::decoding(),
            chunks_decoded: 0,
            total_samples: 0,
            last_spec: None,
            emit: None,
            resume_target: None,
            decode_head: None,
            retired_decoders: ArrayQueue::new(Self::DECODER_RETIRE_CAPACITY),
            decoder_retire_overflowed: AtomicBool::new(false),
        }
    }

    /// Publish the current FSM phase to the shared activity flag and assign
    /// the new state.
    ///
    /// `PLAYING` mirrors "audio FSM has an active decode target": every
    /// non-terminal state keeps it set (`Decoding`,
    /// `SeekRequested`, `ApplyingSeek`, `AwaitingResume`,
    /// `WaitingForSource`, `RecreatingDecoder`), while terminal states
    /// (`AtEof`, `Failed`) clear it. The Downloader's peer
    /// `priority()` reads this flag to decide between High and Low
    /// priority slots — keeping PLAYING set through buffering and
    /// mid-seek windows is deliberate, because the listener is still
    /// attached to this track.
    fn update_state(&mut self, new: CurrentFsm) {
        self.activity.set_playing(playing_for_state(&new));
        self.state = new;
    }

    /// Reset the effect chain and discard any armed EOF drain.
    /// Used on seek / decoder recreation, so a buffering effect's stale tail never leaks past
    /// the discontinuity and a seek-after-EOF re-arms the drain from scratch.
    fn reset_effect_chain(&mut self) {
        reset_effects(&mut self.effects);
        self.eof_drain_active = false;
    }

    /// Emit an audio event if the bus is set. Runs on the produce core, so it
    /// only *enqueues* (lock-free); the shell publishes via `flush_deferred` /
    /// `Drop` — the `broadcast::send` is a `kevent` the forbid path must not make.
    fn emit_event(&self, event: AudioEvent) {
        if let Some(ref emit) = self.emit {
            emit.enqueue(event);
        }
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
}

// Seek: anchor resolution and alignment
impl<T: StreamType> StreamAudioSource<T> {
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
        let same_codec =
            matches!((current_codec, target_codec), (Some(from), Some(to)) if from == to);
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
        let needs_recreation = codec_changed
            || variant_requires_format_boundary(variant_changed, same_codec, target_container);
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
            same_codec,
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
                DecodeError::InvalidData {
                    detail: Self::SEEK_ANCHOR_NO_INIT_RANGE,
                },
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
                DecodeError::InvalidData { detail: "seek anchor alignment: variant/codec changed but media info unavailable" },
                "seek anchor alignment failed",
            );
            return false;
        };
        if let Some(v) = target_variant_u32 {
            target_info.variant_index = Some(v);
        }

        self.start_recreating_decoder(RecreateState {
            cause: RecreateCause::VariantSwitch,
            media_info: target_info,
            next: RecreateNext::Seek(request),
            offset: recreate_offset,
        });
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

    fn apply_time_anchor_seek(&mut self, request: SeekRequest, anchor: SourceSeekAnchor) -> bool {
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        self.shared_stream.clear_variant_fence();
        self.update_decoder_len_for_seek();
        debug!(
            ?position,
            epoch,
            emit_request = request.emit_request,
            anchor_start = ?anchor.segment_start,
            anchor_byte_offset = anchor.byte_offset,
            anchor_variant = ?anchor.variant_index,
            stream_pos = self.shared_stream.position(),
            committed_position = ?self.playhead.position(),
            variant = ?self
                .shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            "apply_time_anchor_seek: enter (anchor path)"
        );
        if let Err(err) = self.decoder_seek_safe(position) {
            return self.recover_from_decoder_seek_error(
                SeekAttempt {
                    request,
                    position,
                    recreate_offset: anchor.byte_offset,
                    seek_mode: SeekMode::Anchor(anchor),
                },
                err,
            );
        }
        trace!(
            ?position,
            anchor_start = ?anchor.segment_start,
            target_offset = anchor.byte_offset,
            "seek anchor path: exact decoder seek succeeded"
        );

        self.apply_seek_applied(epoch, position, self.seek_context(), Some(anchor));
        true
    }
}

// Seek: apply, commit, and skip
impl<T: StreamType> StreamAudioSource<T> {
    fn apply_seek_applied(
        &mut self,
        epoch: u64,
        position: Duration,
        location: SegmentLocation,
        anchor: Option<SourceSeekAnchor>,
    ) {
        self.reset_effect_chain();
        self.resume_target = Some((epoch, position));
        self.emit_seek_lifecycle(SeekLifecycleStage::SeekApplied, epoch, location);
        self.update_state(CurrentFsm::awaiting_resume(ResumeState {
            anchor_offset: anchor.map(|anchor| anchor.byte_offset),
            anchor_variant_index: anchor.and_then(|anchor| anchor.variant_index),
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
        debug!(
            ?position,
            epoch,
            emit_request = request.emit_request,
            stream_pos,
            committed_position = ?self.playhead.position(),
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
                "seek: codec-changing format boundary pending, recreating decoder before seek"
            );
            self.start_recreating_decoder(RecreateState {
                cause: RecreateCause::VariantSwitch,
                media_info: new_info,
                next: RecreateNext::ApplySeek(request),
                offset: target_offset,
            });
            return false;
        }

        self.update_decoder_len_for_seek();

        let stream_len = self.shared_stream.len();
        debug!(
            ?position,
            epoch,
            stream_pos,
            base_offset = self.session.base_offset,
            ?stream_len,
            "seek: about to call decoder.seek()"
        );
        if let Err(err) = self.decoder_seek_safe(position) {
            return self.recover_from_decoder_seek_error(
                SeekAttempt {
                    request,
                    position,
                    recreate_offset: self.session.base_offset,
                    seek_mode: SeekMode::Direct {
                        target_byte: self.estimate_target_byte(position),
                    },
                },
                err,
            );
        }

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
        let len = chunk.samples.len();
        chunk.samples.copy_within(drop_samples..len, 0);
        chunk.samples.truncate(len - drop_samples);

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

    /// Pin the timeline playhead to the decoder's actual landing point
    /// from a [`DecoderSeekOutcome`]. The decoder is the only source
    /// of ground truth — both for `landed_frame` (frame counter) and
    /// for the wall-clock position it parked at; we never recompute
    /// `frame * 1e9 / sample_rate` here.
    fn commit_decoder_seek_outcome(&self, outcome: &DecoderSeekOutcome) {
        let sample_rate = self.session.decoder.spec().sample_rate.get();
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
            frame_offset,
            end_position_ns,
            frames: 0,
            source_bytes: 0,
            source_byte_offset: applied_landed_byte,
        };
        self.playhead.land(&pos);
        if let Some(byte) = applied_landed_byte {
            self.shared_stream.set_position(byte);
        }
    }
}

// Seek: targets and lifecycle events
impl<T: StreamType> StreamAudioSource<T> {
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
            CurrentFsm::RebuildingDecoder(h) => h
                .data()
                .superseded_seek
                .map(|request| request.seek.epoch)
                .or_else(|| epoch_from_recreate(&h.data().recreate)),
            CurrentFsm::WaitingForSource(h) => match &h.data().context {
                WaitContext::Seek(request) => Some(request.seek.epoch),
                WaitContext::ApplySeek(applying) => Some(applying.request.seek.epoch),
                WaitContext::Recreation(recreate) => epoch_from_recreate(recreate),
                WaitContext::Playback | WaitContext::PostSeek(_) => None,
            },
            CurrentFsm::Decoding(_) | CurrentFsm::AtEof(_) | CurrentFsm::Failed(_) => None,
        }
    }

    /// Approximate the byte Symphonia will target for `position` before we
    /// issue the seek. Used to gate `apply_seek_from_decoder` on the byte
    /// range being downloaded — without this, `decoder.seek()` issues a
    /// read past the source's buffered tail and errors out.
    ///
    /// Returns `None` when we can't form a ratio (duration unknown, stream
    /// length unknown, or zero-length stream). Callers fall back to the
    /// historical readiness check in that case.
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

    /// Resolve a seek-preemption target in one Option-chain so the per-tick
    /// hot path of `step_track` short-circuits with a single branch instead
    /// of four sequential predicates. Returns `Some(target)` only when a
    /// new seek epoch must preempt the current state; otherwise
    /// `None` and the caller falls through to the phase dispatcher.
    ///
    /// Fast-path: `SeekObserve::take_preempt` returns `true` exactly
    /// once per `begin` call. The typical no-seek tick reads a
    /// single Acquire bool and falls through, instead of dereferencing
    /// two `Arc<AtomicU64>`s. A spurious consume (e.g. seek already
    /// processed by an earlier tick) is harmless because the slow path
    /// below re-validates against the seek state.
    #[inline]
    fn preempt_seek_target(&self) -> Option<Duration> {
        if !self.seek_obs.take_preempt() {
            return None;
        }
        let timeline_epoch = self.seek_obs.epoch();
        if timeline_epoch <= self.epoch.load(Ordering::Acquire) {
            return None;
        }
        let target = self.seek_obs.target()?;
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

    fn seek_context(&self) -> SegmentLocation {
        SegmentLocation::new(
            self.shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            None,
            None,
            None,
        )
    }

    fn update_decoder_len_for_seek(&self) {
        if let Some(len) = self.shared_stream.len()
            && len > 0
        {
            let relative = len.saturating_sub(self.session.base_offset);
            self.session.decoder.update_byte_len(relative);
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
}

// Seek: recovery and failure
impl<T: StreamType> StreamAudioSource<T> {
    /// Shared recovery path for a failed `decoder.seek()`.
    ///
    /// Splits by [`DecodeError`] variant: [`DecodeError::SeekOutOfRange`]
    /// fails the seek (no recreate), anything else recreates at the
    /// init/offset range. Always returns `false`. See the crate
    /// `CONTEXT.md` "Seek error recovery".
    fn recover_from_decoder_seek_error(&mut self, failed: SeekAttempt, err: DecodeError) -> bool {
        let SeekAttempt {
            request,
            position,
            recreate_offset,
            seek_mode,
        } = failed;
        let epoch = request.seek.epoch;
        let (warn_msg, fail_ctx) = match seek_mode {
            SeekMode::Direct { .. } => (
                "seek: decoder.seek failed, rebuilding decoder for deterministic recovery",
                "seek: decoder.seek failed",
            ),
            SeekMode::Anchor(_) => (
                "seek anchor path: decoder seek failed, recreating decoder",
                "seek anchor path: exact decoder seek failed",
            ),
        };
        warn!(?err, epoch, ?position, recreate_offset, "{warn_msg}");

        if matches!(err, DecodeError::SeekOutOfRange { .. }) {
            self.reject_seek(request, &err, fail_ctx);
            return false;
        }

        if err.is_interrupted() {
            let applying = ApplySeekState {
                request,
                mode: seek_mode,
            };
            let phase = self.source_phase_for_wait_context(&WaitContext::ApplySeek(applying));
            let reason = self.source_park(phase).unwrap_or(WaitingReason::Waiting);
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
                DecodeError::InvalidData {
                    detail: Self::SEEK_RECOVERY_NO_INIT_RANGE,
                },
                fail_ctx,
            );
            return false;
        };
        self.start_recreating_decoder(RecreateState {
            cause: RecreateCause::VariantSwitch,
            media_info: info,
            next: RecreateNext::ApplySeek(request),
            offset: recreate_offset,
        });
        false
    }

    /// Soft seek rejection: the seek cannot be honoured
    /// (target out-of-range, decoder.seek failed after a fresh
    /// recreate, etc.) but the existing decoder is still alive —
    /// the track keeps playing from its current position. Emits
    /// `SeekRejected`, clears the pending epoch, and parks the FSM
    /// back in `Decoding`. Used for both caller-side errors
    /// (`SeekOutOfRange`) and post-recreate seek failures, where a
    /// another recreate cycle would form a loop. The previous
    /// code marked the track `Failed` for these and broke
    /// auto-advance, seek-after-near-end, and stress reproducers.
    fn reject_seek(&mut self, request: SeekRequest, err: &DecodeError, context: &'static str) {
        warn!(
            ?err,
            epoch = request.seek.epoch,
            ?request.seek.target,
            "{context}"
        );
        self.emit_event(AudioEvent::SeekRejected {
            epoch: request.seek.epoch,
            target: request.seek.target,
        });
        self.epoch.store(request.seek.epoch, Ordering::Release);
        self.finalize_seek_pending(request.seek.epoch);
        self.update_state(CurrentFsm::decoding());
    }

    fn fail_seek(&mut self, request: SeekRequest, err: DecodeError, context: &'static str) {
        warn!(
            ?err,
            epoch = request.seek.epoch,
            ?request.seek.target,
            "{context}"
        );
        self.emit_event(AudioEvent::SeekRejected {
            epoch: request.seek.epoch,
            target: request.seek.target,
        });
        self.finalize_seek_pending(request.seek.epoch);
        self.update_state(CurrentFsm::failed(TrackFailure::Decode(err)));
    }
}

// Decoder: panic-safe wrappers and format detection
impl<T: StreamType> StreamAudioSource<T> {
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
                Err(payload) => {
                    warn!(
                        panic = %Self::decode_panic_message(payload),
                        "decoder panicked during next_chunk"
                    );
                    Err(DecodeError::InvalidData {
                        detail: "decoder panicked during next_chunk",
                    })
                }
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
                warn!(
                    panic = %Self::decode_panic_message(payload),
                    "decoder panicked during seek"
                );
                return Err(DecodeError::InvalidData {
                    detail: "decoder panicked during seek",
                });
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

    /// Detect a decoder format boundary and return the recovery anchor.
    ///
    /// Same-codec variant-index changes are byte-continuity only for containers
    /// that can be decoded from a packet boundary. Init-bearing containers
    /// still need a decoder boundary even when the codec is unchanged.
    /// Codec/container are NOT re-derived from `current_info`: the source's
    /// `media_info()` may return a declarative container (e.g. `Fmp4` inferred
    /// from an `EXT-X-MAP` URL extension) that disagrees with the bytes the
    /// decoder is actually reading. The cached `session.media_info` reflects
    /// what was probed and built successfully — that's the authoritative
    /// decoder type.
    ///
    /// Recovery anchor: cross-codec init-segment offset via
    /// `format_change_segment_range`. `NoChange` when it does not apply.
    #[kithara::probe]
    fn detect_format_change(&self) -> FormatChangeDetection {
        // NOTE: seek-epoch suppression (see CONTEXT.md "Decoder recreate policy").
        if self.seek_obs.is_pending()
            && self.session.installed_at_seek_epoch == self.seek_obs.epoch()
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
        let Ok(range) = self.shared_stream.format_change_segment_range() else {
            return FormatChangeDetection::NoChange;
        };
        FormatChangeDetection::Applicable {
            target,
            target_offset: range.start,
        }
    }

    /// Prepare a pending format change: clear the fence and seek to segment start.
    ///
    /// The decoder factory itself runs later on the blocking pool.
    #[kithara::probe(target_offset)]
    fn prepare_format_change_rebuild(
        &mut self,
        _new_info: &MediaInfo,
        target_offset: u64,
    ) -> Result<(), RecreateOutcome> {
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
                classify_recreate_err(&DecodeError::from(e))
            })?;

        let pos_after_seek = self.shared_stream.position();
        debug!(
            target_offset,
            pos_after_seek, "apply_format_change: stream seeked, rebuild queued"
        );
        Ok(())
    }
}

// Decoder: rebuild and recreate lifecycle
impl<T: StreamType> StreamAudioSource<T> {
    fn install_recreated_session(
        &mut self,
        new_info: &MediaInfo,
        base_offset: u64,
        new_decoder: Box<dyn Decoder>,
    ) {
        let new_duration = new_decoder.duration();
        let variant = new_info.variant_index;
        let old_decoder = mem::replace(&mut self.session.decoder, new_decoder);
        self.retire_decoder(old_decoder);
        self.session.base_offset = base_offset;
        self.session.media_info = Some(new_info.clone());
        self.session.installed_at_seek_epoch = self.seek_obs.epoch();
        debug!(?new_duration, base_offset, "Decoder recreated successfully");
        self.emit_event(AudioEvent::DecoderReady {
            base_offset,
            variant,
        });
    }

    fn retire_decoder(&self, decoder: Box<dyn Decoder>) {
        if let Err(decoder) = self.retired_decoders.push(decoder) {
            self.decoder_retire_overflowed
                .store(true, Ordering::Release);
            mem::forget(decoder);
        }
    }

    fn drain_retired_decoders(&self) {
        while self.retired_decoders.pop().is_some() {}
        if self.decoder_retire_overflowed.swap(false, Ordering::AcqRel) {
            warn!("decoder retire queue overflowed; leaked decoder to keep RT core free");
        }
    }

    fn submit_pending_rebuild(&mut self) {
        let Some(job) = self.pending_rebuild_submit.take() else {
            return;
        };
        drop(spawn_blocking_on(&self.runtime_handle, move || job.run()));
    }

    fn next_rebuild_ticket(&mut self) -> u64 {
        let ticket = self.next_rebuild_ticket;
        self.next_rebuild_ticket = self.next_rebuild_ticket.wrapping_add(1);
        ticket
    }

    fn prepare_rebuild(&mut self, recreate: &RecreateState) -> Result<(), RecreateOutcome> {
        if recreate.cause == RecreateCause::FormatBoundary
            && matches!(recreate.next, RecreateNext::Decode)
        {
            debug!(
                offset = recreate.offset,
                cause = ?recreate.cause,
                next = ?recreate.next,
                committed = ?self.playhead.position(),
                stream_pos = self.shared_stream.position(),
                stream_len = ?self.shared_stream.len(),
                "execute_recreation: FormatBoundary+Decode branch enter"
            );
            self.prepare_format_change_rebuild(&recreate.media_info, recreate.offset)?;
        } else {
            self.shared_stream.clear_variant_fence();
            if self
                .shared_stream
                .probe_seek(SeekFrom::Start(recreate.offset))
                .is_err()
            {
                return Err(RecreateOutcome::SoftFailed);
            }
            self.shared_stream.clear_variant_fence();
        }

        let ticket = self.next_rebuild_ticket();
        let completion = Arc::clone(&self.rebuild_completion);
        let job = DecoderRebuildJob {
            ticket,
            shared_stream: self.shared_stream.clone(),
            media_info: recreate.media_info.clone(),
            base_offset: recreate.offset,
            decoder_factory: Arc::clone(&self.decoder_factory),
            completion: Arc::clone(&completion),
            worker_wake: Arc::clone(&self.worker_wake),
        };
        self.pending_rebuild_submit = Some(job);
        self.update_state(CurrentFsm::rebuilding(RebuildState {
            ticket,
            recreate: recreate.clone(),
            started_seek_epoch: self.seek_obs.epoch(),
            completion,
            superseded_seek: None,
        }));
        Ok(())
    }

    #[kithara::probe]
    fn start_recreating_decoder(&mut self, state: RecreateState) {
        let pending_seek_target = match &state.next {
            RecreateNext::Seek(req) | RecreateNext::ApplySeek(req) => Some(req.seek.target),
            RecreateNext::Decode => None,
        };
        debug!(
            cause = ?state.cause,
            codec = ?state.media_info.codec,
            container = ?state.media_info.container,
            target_offset = state.offset,
            next = ?mem::discriminant(&state.next),
            ?pending_seek_target,
            committed_position = ?self.playhead.position(),
            stream_pos = self.shared_stream.position(),
            "start_recreating_decoder"
        );
        self.update_state(CurrentFsm::recreating(state));
    }

    fn start_route_change_recreate_if_needed(&mut self) -> bool {
        if !self.recreate_on_host_rate_change {
            return false;
        }
        if self.active_seek_epoch().is_some() {
            return false;
        }
        let host_rate = self.host_sample_rate.load(Ordering::Acquire);
        if host_rate == 0 {
            return false;
        }
        if self.decoder_host_sample_rate == 0
            && self.session.decoder.spec().sample_rate.get() == host_rate
        {
            self.decoder_host_sample_rate = host_rate;
            return false;
        }
        if host_rate == self.decoder_host_sample_rate {
            return false;
        }
        let Some(media_info) = self
            .session
            .media_info
            .clone()
            .or_else(|| self.shared_stream.media_info())
        else {
            return false;
        };
        let epoch = self.epoch.load(Ordering::Acquire);
        let target = self.decode_head_resume_position();
        let offset = match self.shared_stream.seek_time_anchor(target) {
            Ok(Some(anchor)) => anchor.byte_offset,
            Ok(None) => self.session.base_offset,
            Err(err) => {
                warn!(
                    ?err,
                    ?target,
                    "route-change recreate: seek anchor resolution failed, using current base offset"
                );
                self.session.base_offset
            }
        };
        self.start_recreating_decoder(RecreateState {
            cause: RecreateCause::RouteChange,
            media_info,
            next: RecreateNext::ApplySeek(SeekRequest {
                seek: SeekContext { target, epoch },
                emit_request: false,
            }),
            offset,
        });
        self.decoder_host_sample_rate = host_rate;
        true
    }
}

// Output: gapless drain, chunk tracking, and frame/time helpers
impl<T: StreamType> StreamAudioSource<T> {
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

    fn next_eof_drain_output(&mut self) -> Option<PcmChunk> {
        if self.effects.is_empty() {
            return None;
        }
        if !self.eof_drain_active {
            self.eof_drain_inputs_exhausted.fill(false);
            self.eof_drain_active = true;
        }
        let last_stage = self.effects.len() - 1;
        Self::pull_eof_drain_stage(
            &mut self.effects,
            &mut self.eof_drain_inputs_exhausted,
            last_stage,
        )
    }

    fn pull_eof_drain_stage(
        effects: &mut [Box<dyn AudioEffect>],
        inputs_exhausted: &mut [bool],
        stage: usize,
    ) -> Option<PcmChunk> {
        if stage == 0 {
            return effects[0].flush();
        }
        loop {
            if !inputs_exhausted[stage] {
                if let Some(chunk) =
                    Self::pull_eof_drain_stage(effects, inputs_exhausted, stage - 1)
                {
                    if let Some(out) = effects[stage].process(chunk) {
                        return Some(out);
                    }
                    continue;
                }
                inputs_exhausted[stage] = true;
            }
            return effects[stage].flush();
        }
    }

    /// Track chunk statistics and emit format events.
    fn track_chunk(&mut self, chunk: &PcmChunk) {
        self.chunks_decoded += 1;
        self.total_samples += chunk.samples.len() as u64;
        self.playhead.set_decoded_frontier(chunk.meta.end_timestamp);

        if self.chunks_decoded == 1
            && let Some(ref emit) = self.emit
        {
            emit.enqueue(AudioEvent::FormatDetected {
                spec: AudioFormat::new(chunk.spec().channels, chunk.spec().sample_rate.get()),
            });
            self.last_spec = Some(chunk.spec());
        }

        if let Some(old_spec) = self.last_spec
            && old_spec != chunk.spec()
        {
            self.emit_event(AudioEvent::FormatChanged {
                old: AudioFormat::new(old_spec.channels, old_spec.sample_rate.get()),
                new: AudioFormat::new(chunk.spec().channels, chunk.spec().sample_rate.get()),
            });
            self.last_spec = Some(chunk.spec());
        }
    }

    fn duration_for_frames(spec: PcmSpec, frames: usize) -> Duration {
        let nanos = (frames as u128)
            .saturating_mul(Self::NANOS_PER_SEC)
            .saturating_div(u128::from(spec.sample_rate.get()));
        let nanos_u64 = num_traits::cast::ToPrimitive::to_u64(&nanos).unwrap_or(u64::MAX);
        Duration::from_nanos(nanos_u64)
    }

    fn frames_for_duration(spec: PcmSpec, duration: Duration) -> usize {
        let frames = duration
            .as_nanos()
            .saturating_mul(u128::from(spec.sample_rate.get()))
            .saturating_div(Self::NANOS_PER_SEC);
        assert!(
            frames <= usize::MAX as u128,
            "source.rs:1036 frames_for_duration: frames={frames} \
             exceeds usize::MAX (duration={duration:?}, sample_rate={})",
            spec.sample_rate
        );
        frames as usize
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

/// Map a recreate-path error to the FSM outcome. Transient
/// `ErrorClass::Interrupted` (probe ran before the source buffered
/// `[0..PROBE)` of a freshly-switched variant) maps to `NeedsSourceWait`
/// so the caller retries after the source phase becomes Ready.
/// Everything else is a hard fail.
fn classify_recreate_err(e: &DecodeError) -> RecreateOutcome {
    if e.classify() == ErrorClass::Interrupted {
        RecreateOutcome::NeedsSourceWait
    } else {
        RecreateOutcome::SoftFailed
    }
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
            self.start_recreating_decoder(RecreateState {
                cause: RecreateCause::FormatBoundary,
                media_info: new_info,
                next: RecreateNext::Decode,
                offset: target_offset,
            });
            return DecodeAction::Yield;
        }

        debug!(
            chunks = self.chunks_decoded,
            samples = self.total_samples,
            pos_at_eof,
            "decode complete (true EOF)"
        );

        if let Some(chunk) = self.next_eof_drain_output() {
            return DecodeAction::Return(Ok(DecoderChunkOutcome::Chunk(chunk)));
        }

        // The source and the whole effect chain are fully drained - nothing left to hear.
        self.emit_event(AudioEvent::EndOfStream);
        DecodeAction::Return(Ok(DecoderChunkOutcome::Eof))
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
    /// `no_change_err` carries the caller's context for the `NoChange`
    /// warn trail — the original decode error on the `Err` path, an
    /// `InvalidData` marker on the `Pending` path. A `NoChange` is NEVER
    /// terminal: the transition only becomes observable EVENTUALLY.
    /// [`HlsCoord::commit_variant_switch`] closes the fence
    /// (`variant_generation`) BEFORE `abr.apply_decision` publishes the
    /// new variant metadata, so the FSM can legally observe the signal in
    /// the synchronous window between the two stores.
    ///
    /// One `NoChange` shape never resolves on its own: the fence targets
    /// the variant the session already labels itself with (a seek
    /// recreate landed on the switch target before the commit raised the
    /// fence — the switch-back-to-current race). No format diff will
    /// ever appear, and the seek/recreate paths that clear fences will
    /// not fire again because the fence itself blocks reads. The session
    /// label can lie about the demuxer's actual bitstream (a stale seek
    /// anchor stamps the pre-commit variant), so the fence is NOT
    /// bare-acked: the `FormatBoundary` recreate re-primes the demuxer on
    /// the active variant's real bytes, clears the fence inside
    /// `apply_format_change`, and resumes from the decode head. The
    /// fence target is published before the generation bump and the arm
    /// additionally requires the published `media_info` to agree, so a
    /// transient pre-publish observation falls through to the retry
    /// path.
    #[cold]
    fn handle_variant_change(&mut self, no_change_err: &DecodeError) -> DecodeAction {
        let FormatChangeDetection::Applicable {
            target: new_info,
            target_offset,
        } = self.detect_format_change()
        else {
            if !self.seek_obs.is_pending()
                && let Some(target) = self.shared_stream.variant_change_target()
                && let Some(session_info) = self.session.media_info.clone()
                && let Some(session_variant) = session_info.variant_index
                && usize::try_from(session_variant) == Ok(target)
                && let Some(current) = self.shared_stream.media_info()
                && current.variant_index == Some(session_variant)
                && let Ok(range) = self.shared_stream.format_change_segment_range()
            {
                info!(
                    target,
                    chunks = self.chunks_decoded,
                    samples = self.total_samples,
                    "variant fence targets the already-aligned session — forcing boundary recreate"
                );
                let mut target_info = session_info;
                if target_info.codec.is_none() {
                    target_info.codec = current.codec;
                }
                if target_info.container.is_none() {
                    target_info.container = current.container;
                }
                self.start_recreating_decoder(RecreateState {
                    cause: RecreateCause::FormatBoundary,
                    media_info: target_info,
                    next: RecreateNext::Decode,
                    offset: range.start,
                });
                return DecodeAction::Yield;
            }
            // Two benign orderings report `NoChange` here: a seek raced in
            // after `decode_next_chunk`'s loop-top `is_seek_pending` check
            // (seek-epoch suppression), or the commit fence closed before
            // `abr.apply_decision` published the new metadata. Both windows
            // are synchronous (no await between the stores), so recheck the
            // loop (`Interrupted`) instead of killing the producer; a
            // genuinely never-observable transition surfaces as a watchdog.
            if !self.seek_obs.is_pending() {
                warn!(
                    ?no_change_err,
                    chunks = self.chunks_decoded,
                    samples = self.total_samples,
                    "variant change without observable format transition yet — rechecking"
                );
            }
            return DecodeAction::Return(Err(DecodeError::Interrupted));
        };
        debug!(
            target_offset,
            chunks = self.chunks_decoded,
            samples = self.total_samples,
            "variant change — recreating decoder"
        );
        self.start_recreating_decoder(RecreateState {
            cause: RecreateCause::FormatBoundary,
            media_info: new_info,
            next: RecreateNext::Decode,
            offset: target_offset,
        });
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
            if self.seek_obs.is_flushing() || self.seek_obs.is_pending() {
                trace!(
                    flushing = self.seek_obs.is_flushing(),
                    pending = self.seek_obs.is_pending(),
                    "decode_next_chunk: gated by seek flags"
                );
                return Err(DecodeError::Interrupted);
            }

            if let Some(ready) = self.next_gapless_output() {
                return Ok(DecoderChunkOutcome::Chunk(ready));
            }

            match self.decoder_next_chunk_safe() {
                Ok(DecoderChunkOutcome::Pending(PendingReason::VariantChange)) => {
                    match self.handle_variant_change(&DecodeError::InvalidData {
                        detail: Self::VARIANT_CHANGE_NO_FORMAT_TRANSITION,
                    }) {
                        DecodeAction::Yield => return Err(DecodeError::Interrupted),
                        DecodeAction::Return(result) => return result,
                    }
                }
                Ok(DecoderChunkOutcome::Pending(reason)) => {
                    if self.shared_stream.has_variant_change_pending() {
                        match self.handle_variant_change(&DecodeError::InvalidData {
                            detail: Self::VARIANT_CHANGE_NO_FORMAT_TRANSITION,
                        }) {
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
                    if chunk.samples.is_empty() {
                        continue;
                    }
                    hang_reset!();
                    self.track_chunk(&chunk);
                    self.gapless.push(chunk);
                    continue;
                }
                Ok(DecoderChunkOutcome::Eof) => {
                    self.gapless
                        .set_tail_compensation(self.session.decoder.track_info().gapless_tail);
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
                    ErrorClass::VariantChange => match self.handle_variant_change(&e) {
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
    /// Apply a pending seek from the shared seek state.
    ///
    /// Reads epoch/target from the seek state and resolves the seek mode.
    fn apply_seek_from_timeline(&mut self, mut request: SeekRequest) {
        let epoch = request.seek.epoch;
        let position = request.seek.target;
        debug!(
            ?position,
            epoch,
            emit_request = request.emit_request,
            current_epoch = self.epoch.load(Ordering::Acquire),
            timeline_seek_target = ?self.seek_obs.target(),
            stream_pos = self.shared_stream.position(),
            variant = ?self
                .shared_stream
                .abr_handle()
                .and_then(|h| h.current_variant_index()),
            "apply_seek_from_timeline: enter (TIMELINE seek picked up)"
        );
        if self.ack_unappliable_seek(epoch) {
            return;
        }

        if let Some(duration) = self.playhead.duration()
            && position >= duration
        {
            debug!(
                ?position,
                ?duration,
                epoch,
                "apply_seek_from_timeline: target at/past duration — landing at EOF"
            );
            self.land_seek_at_eof(epoch, duration);
            return;
        }

        if request.emit_request {
            self.emit_seek_lifecycle(SeekLifecycleStage::SeekRequest, epoch, self.seek_context());
            request.emit_request = false;
        }

        let anchor_result = self.shared_stream.seek_time_anchor(position);
        self.shared_stream.clear_variant_fence();
        self.seek.complete(epoch);
        // Seek applied on the produce core: arm the peer so it re-targets
        // fetches around the new reader position. The shell flushes it.
        self.arm_peer_wake();

        let mode = match anchor_result {
            Ok(Some(anchor)) => SeekMode::Anchor(anchor),
            Ok(None) => SeekMode::Direct {
                target_byte: self.estimate_target_byte(position),
            },
            Err(err) => {
                warn!(?err, "seek anchor resolution failed");
                self.fail_seek(
                    request,
                    DecodeError::SeekFailed {
                        detail: Self::SEEK_ANCHOR_RESOLUTION_FAILED,
                    },
                    "seek anchor resolution failed",
                );
                return;
            }
        };
        self.update_state(CurrentFsm::applying_seek(ApplySeekState { mode, request }));
    }

    /// Ack a timeline seek that cannot be applied — no timeline target
    /// (already consumed) or a stale epoch — clearing the seek flags and
    /// returning the FSM to `Decoding`. Returns `true` when it acked.
    fn ack_unappliable_seek(&mut self, epoch: u64) -> bool {
        if self.seek_obs.target().is_none() {
            debug!(
                epoch,
                "apply_seek_from_timeline: no timeline target — acking epoch"
            );
        } else {
            let current_epoch = self.epoch.load(Ordering::Acquire);
            if epoch > current_epoch {
                return false;
            }
            debug!(
                epoch,
                current_epoch, "apply_seek_from_timeline: stale epoch — acking without apply"
            );
        }
        self.seek.complete(epoch);
        self.finalize_seek_pending(epoch);
        self.update_state(CurrentFsm::decoding());
        true
    }

    /// Land a seek whose target sits at/past the known duration: park the
    /// playhead at the stream end, ack the epoch, and enter `AtEof`.
    fn land_seek_at_eof(&mut self, epoch: u64, duration: Duration) {
        let sample_rate = self.session.decoder.spec().sample_rate.get();
        let end_frame = num_traits::cast::ToPrimitive::to_u64(
            &(duration.as_secs_f64() * f64::from(sample_rate)),
        )
        .unwrap_or(u64::MAX);
        let end_position_ns = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        self.playhead.land(&kithara_stream::ChunkPosition {
            end_position_ns,
            frame_offset: end_frame,
            frames: 0,
            source_bytes: 0,
            source_byte_offset: None,
        });
        self.seek.complete(epoch);
        self.finalize_seek_pending(epoch);
        self.epoch.store(epoch, Ordering::Release);
        self.update_state(CurrentFsm::at_eof());
    }

    /// Arm the reader→peer wake on the produce core (lock-free). The scheduler
    /// shell flushes it off the forbid path, so the cross-thread `notify_one`
    /// (a `kevent`) never fires on the RT core. No-op for file streams.
    fn arm_peer_wake(&self) {
        if let Some(ref wake) = self.peer_wake {
            wake.arm();
        }
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
        let timeline_duration = self.playhead.duration();
        if decoder_duration > timeline_duration {
            self.playhead.set_duration(decoder_duration);
        }
        let current_epoch = self.epoch.load(Ordering::Acquire);
        let result = self.decode_next_chunk();
        match result {
            Ok(DecoderChunkOutcome::Chunk(chunk)) => {
                if self
                    .resume_state()
                    .is_some_and(|resume| resume.seek.epoch == current_epoch)
                {
                    self.emit_seek_lifecycle(
                        SeekLifecycleStage::DecodeStarted,
                        current_epoch,
                        SegmentLocation::new(
                            chunk.meta.variant_index,
                            chunk.meta.segment_index,
                            None,
                            None,
                        ),
                    );
                    self.update_state(CurrentFsm::decoding());
                }
                let fo = chunk.meta.frame_offset;
                let frames = chunk.meta.frames;
                self.decode_head = Some((
                    current_epoch,
                    fo.saturating_add(u64::from(frames)),
                    chunk.meta.spec.sample_rate.get(),
                ));
                DecodeStep::Produced(Fetch::data(chunk, current_epoch))
            }
            Ok(DecoderChunkOutcome::Eof) => {
                self.update_state(CurrentFsm::at_eof());
                DecodeStep::Eof
            }
            Ok(DecoderChunkOutcome::Pending(reason)) => {
                trace!(
                    ?reason,
                    pos = self.shared_stream.position(),
                    "decode_one_step: pending"
                );
                DecodeStep::NotReady(WaitingReason::Waiting)
            }
            Err(e) if e.is_interrupted() => {
                trace!(
                    flushing = self.seek_obs.is_flushing(),
                    pending = self.seek_obs.is_pending(),
                    pos = self.shared_stream.position(),
                    "decode_one_step: interrupted"
                );
                DecodeStep::Interrupted
            }
            Err(e) => {
                self.update_state(CurrentFsm::failed(TrackFailure::Decode(e)));
                DecodeStep::Failed
            }
        }
    }

    /// Clear the seek-pending flag and wake the source's peer in one step.
    ///
    /// `SeekControl::clear_pending` only flips the atomic flag; it does
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
        self.seek.clear_pending(epoch);
        self.arm_peer_wake();
    }

    fn post_seek_anchor_offset(&self, resume: &ResumeState) -> Option<u64> {
        let offset = resume.anchor_offset?;
        let Some(anchor_variant) = resume.anchor_variant_index else {
            return Some(self.post_seek_gate_byte(offset));
        };
        let current_variant = self
            .shared_stream
            .abr_handle()
            .and_then(|handle| handle.current_variant_index());
        match current_variant {
            Some(current) if current == anchor_variant => Some(self.post_seek_gate_byte(offset)),
            _ => None,
        }
    }

    /// Clamp the post-seek resume gate to the decoder's live read head.
    ///
    /// `anchor_offset` is the *segment start* of the seek-target segment. By
    /// `AwaitingResume` the decoder has already been seeked to the target
    /// *within* that segment, so `position()` (the decoder's next read) sits at
    /// or past the anchor. Gating on the raw anchor waits on bytes the decoder
    /// has already moved past; under a small / ephemeral cache those bytes get
    /// evicted right after the seek consumed them and are never re-fetched (the
    /// decoder never re-reads them), so the gate never clears and the worker
    /// re-polls `WaitingForSource(PostSeek)` forever while the consumer parks on
    /// `reader_wake`. Gating on `max(anchor, pos)` tracks the read head so the
    /// resume gate (resolved by [`Self::source_is_ready_for_chunk`] to the
    /// segment-clamped chunk-lookahead window) only waits on bytes the decoder
    /// still needs.
    fn post_seek_gate_byte(&self, anchor_offset: u64) -> u64 {
        anchor_offset.max(self.shared_stream.position())
    }

    /// Decoder-construction input contract for the demuxer `recreate` will
    /// rebuild, declared per-demuxer by the decode factory: an init-bearing
    /// container (segment-aware fMP4) reports [`InputRequirement::InitOnly`] —
    /// its init header (moov/esds/STREAMINFO) must be buffered before the
    /// rebuilt demuxer can construct; a raw / mid-stream source reports
    /// [`InputRequirement::Incremental`] — nothing is gated up front, the
    /// demuxer reads and pends as bytes arrive. The contract names the *shape*;
    /// byte-space *resolution* stays here (see [`Self::recreate_ready_range`])
    /// because only the stream knows the ABR virtual-space byte shift.
    fn recreate_input(&self, recreate: &RecreateState) -> InputRequirement {
        let byte_map = self.shared_stream.byte_map();
        kithara_decode::DecoderFactory::input_requirement(&recreate.media_info, byte_map.as_deref())
    }

    fn recreate_phase(&self, recreate: &RecreateState) -> SourcePhase {
        self.shared_stream
            .phase_at(self.recreate_ready_range(recreate))
    }

    /// Byte range whose readiness gates decoder recreation, shared by the gate
    /// and the wait path so the two never disagree (a mismatch livelocks the
    /// worker). The demuxer contract picks the shape; this layer resolves it in
    /// virtual byte space. An init-bearing recreate gates on the init header via
    /// [`format_change_segment_range`], which is `served_from`-aware: a
    /// byte-shifted same-codec commit (init no longer addressable) and an
    /// oversized init both degrade to the `[offset..offset+READ_AHEAD)` window.
    /// An incremental recreate gates on that window directly. See the crate
    /// `CONTEXT.md` "Recreate readiness gating".
    ///
    /// [`format_change_segment_range`]: kithara_stream::Stream::format_change_segment_range
    fn recreate_ready_range(&self, recreate: &RecreateState) -> Range<u64> {
        if matches!(self.recreate_input(recreate), InputRequirement::Incremental) {
            return recreate.offset..self.boundary_end(recreate.offset);
        }
        if let Ok(init_range) = self.shared_stream.format_change_segment_range()
            && init_range.end.saturating_sub(init_range.start) <= Self::DEFAULT_READ_AHEAD_BYTES
        {
            return init_range;
        }
        recreate.offset..self.boundary_end(recreate.offset)
    }

    fn seek_anchor_stale(&self, mode: SeekMode) -> bool {
        let SeekMode::Anchor(anchor) = mode else {
            return false;
        };
        let Some(anchor_variant) = anchor.variant_index else {
            return false;
        };
        self.shared_stream
            .abr_handle()
            .and_then(|handle| handle.current_variant_index())
            .is_some_and(|current| current != anchor_variant)
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
            .byte_map()
            .and_then(|layout| layout.segment_at_byte(byte))
            .map(|seg| seg.byte_range.end);
        let end = segment_end.unwrap_or_else(|| self.boundary_end(byte));
        self.shared_stream.len().map_or(end, |len| end.min(len))
    }

    /// Byte range whose readiness lets the decoder produce its next chunk from
    /// `byte`: the 32 KB read-ahead window, clamped down to the next segment
    /// boundary (segmented sources) and to `Source::len()`. The clamp to the
    /// next segment boundary matters — a fixed 32 KB window straddles the
    /// boundary into a not-yet-fetched next segment, so the gate would never
    /// clear even though the decoder only needs the rest of the current
    /// segment to emit a chunk. Exactly ON a boundary the window is empty
    /// and therefore vacuously ready: that is deliberate — the demuxer may
    /// still hold buffered input from the previous segment, and gating on
    /// the (possibly withheld) next segment here would park the FSM before
    /// it drains those frames (see `hls_seek_middle_stress_long`). A decode
    /// attempt that finds no bytes parks through `DecodeStep::NotReady`.
    fn chunk_lookahead_range(&self, byte: u64) -> Range<u64> {
        let lookahead_end = byte.saturating_add(Self::DEFAULT_READ_AHEAD_BYTES);
        let check_end = self
            .shared_stream
            .byte_map()
            .and_then(|layout| layout.segment_after_byte(byte))
            .map_or(lookahead_end, |next| {
                next.byte_range.start.min(lookahead_end)
            });
        let check_end = self
            .shared_stream
            .len()
            .map_or(check_end, |len| check_end.min(len));
        byte..check_end
    }

    /// Check whether the underlying source has data ready for a non-blocking
    /// decode. Returns `true` for `Ready`, `Eof`, or `Seeking` phases.
    fn source_is_ready(&self) -> bool {
        self.source_ready_for_range(self.chunk_lookahead_range(self.shared_stream.position()))
    }

    /// Readiness of the chunk-lookahead window starting at `byte` — the
    /// post-seek-resume companion to [`Self::source_is_ready`], which gates on
    /// the same segment-clamped window but at the decoder's resume byte rather
    /// than the live stream position.
    fn source_is_ready_for_chunk(&self, byte: u64) -> bool {
        self.source_ready_for_range(self.chunk_lookahead_range(byte))
    }

    /// Phase of the chunk-lookahead window at `byte`, so the post-seek wait
    /// path blocks on exactly the window it later gates ready on.
    fn source_phase_for_chunk(&self, byte: u64) -> SourcePhase {
        self.shared_stream
            .phase_at(self.chunk_lookahead_range(byte))
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

    /// Readiness check for the byte range the decoder will read first
    /// after a post-seek landing. Gates on the rest of the **segment**
    /// containing the landing byte (a fixed 32 KB window would cross the
    /// segment boundary into a not-yet-fetched next segment and never
    /// clear, or — for a ~700 KB FLAC fmp4 chunk — starve the decoder on
    /// the very next read while `wait_range` budget exceeds and the audio
    /// worker's `PassOutcome::Waiting` ticks the `HangDetector`). For
    /// sources without a segment layout (raw files), falls back to the
    /// boundary window via [`Self::seek_landing_end`].
    fn source_is_ready_for_seek_landing(&self, byte: u64) -> bool {
        let end = self.seek_landing_end(byte);
        self.source_ready_for_range(byte..end)
    }

    fn source_park(&self, phase: SourcePhase) -> Option<WaitingReason> {
        let reason = map_source_phase(phase)?;
        trace!(
            ?phase,
            ?reason,
            pos = self.shared_stream.position(),
            "source_park: parking on source phase"
        );
        self.arm_peer_wake();
        Some(reason)
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
            WaitContext::Recreation(recreate) => self.recreate_phase(recreate),
            WaitContext::PostSeek(resume) => self.post_seek_anchor_offset(resume).map_or_else(
                || self.shared_stream.phase(),
                |byte| self.source_phase_for_chunk(byte),
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

    /// Phase of the read-ahead window the decoder reads *through* during
    /// steady-state playback: `[pos, pos + READ_AHEAD)`, clamped to the
    /// stream length but — unlike [`source_is_ready`] — NOT clamped to the
    /// next segment boundary. The decoder's container parser reads across
    /// segment boundaries, so a boundary-clamped gate reports `Ready` while
    /// the decoder is actually blocked on the (withheld) next segment. The
    /// `WaitContext::Playback` wait path gates on this wider window so the
    /// gate and the decoder's real read never disagree (a mismatch hot-spins
    /// the worker; see crate `CONTEXT.md` "Playback readiness gating").
    fn source_phase_forward(&self) -> SourcePhase {
        let pos = self.shared_stream.position();
        let end = pos.saturating_add(Self::DEFAULT_READ_AHEAD_BYTES);
        let end = self.shared_stream.len().map_or(end, |len| end.min(len));
        self.shared_stream.phase_at(pos..end)
    }

    fn source_ready_for_range(&self, range: Range<u64>) -> bool {
        let phase = self.shared_stream.phase_at(range);
        if self.source_park(phase).is_some() {
            return false;
        }
        matches!(
            phase,
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }

    fn source_ready_for_recreate(&self, recreate: &RecreateState) -> bool {
        let phase = self.recreate_phase(recreate);
        if self.source_park(phase).is_some() {
            return false;
        }
        matches!(
            phase,
            SourcePhase::Ready | SourcePhase::Eof | SourcePhase::Seeking
        )
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    fn decode_head_resume_position(&self) -> Duration {
        let committed = self.playhead.position();
        let epoch_now = self.epoch.load(Ordering::Acquire);
        self.decode_head
            .filter(|&(epoch, _, _)| epoch == epoch_now)
            .map(|(_, frame, rate)| duration_for_frames(rate, frame))
            .filter(|&head| head > committed)
            .unwrap_or(committed)
    }

    fn seek_outcome_position(outcome: DecoderSeekOutcome) -> Duration {
        match outcome {
            DecoderSeekOutcome::Landed { landed_at, .. } => landed_at,
            DecoderSeekOutcome::PastEof { duration } => duration,
        }
    }

    fn finish_route_change_after_recreate(
        &mut self,
        recreate: &RecreateState,
        request: SeekRequest,
    ) -> TrackStep<PcmChunk> {
        let target = request.seek.target;
        match self.decoder_seek_safe(target) {
            Ok(outcome) => {
                self.reset_effect_chain();
                self.resume_target = Some((request.seek.epoch, target));
                let landed_at = Self::seek_outcome_position(outcome);
                let remaining = target.saturating_sub(landed_at);
                let skip = (!remaining.is_zero()).then_some(remaining);
                self.update_state(CurrentFsm::awaiting_resume(ResumeState {
                    anchor_offset: Some(recreate.offset),
                    anchor_variant_index: recreate
                        .media_info
                        .variant_index
                        .and_then(|variant| usize::try_from(variant).ok()),
                    skip,
                    seek: request.seek,
                }));
                TrackStep::StateChanged
            }
            Err(err) => {
                warn!(
                    ?err,
                    ?target,
                    "route-change recreate: recreated decoder seek failed"
                );
                self.update_state(CurrentFsm::failed(TrackFailure::RecreateFailed {
                    offset: self.session.base_offset,
                }));
                TrackStep::StateChanged
            }
        }
    }

    /// Apply the `RecreateNext` action after a successful recreation.
    fn apply_recreate_next(&mut self, recreate: &RecreateState) -> TrackStep<PcmChunk> {
        match &recreate.next {
            RecreateNext::Decode => {
                self.reset_effect_chain();
                self.update_state(CurrentFsm::decoding());
                TrackStep::StateChanged
            }
            RecreateNext::Seek(request) => {
                self.update_state(CurrentFsm::seek_requested(*request));
                TrackStep::StateChanged
            }
            RecreateNext::ApplySeek(request) if recreate.cause == RecreateCause::RouteChange => {
                self.finish_route_change_after_recreate(recreate, *request)
            }
            RecreateNext::ApplySeek(request) => self.finish_apply_seek_after_recreate(*request),
        }
    }

    fn finish_format_boundary_rebuild(&mut self) -> RecreateOutcome {
        // Continue the new decoder from the producer's decode head, not the
        // consumer's lagging `committed`: chunks in [committed..decode_head]
        // are already queued in the outlet ring (a FormatBoundary recreate
        // neither flushes it nor bumps the seek epoch), so resuming at
        // `committed` re-emits them — duplicated content, a backward phase
        // jump. The decode head is an exact frame; the demuxer quantizes the
        // seek landing to a sample, and `frame_offset_for` rounds that back
        // to the nearest frame (consistent with `frames_to_trim`), so the
        // rebuilt decoder relabels its first chunk at exactly `decode_head`.
        let decode_head = self.decode_head_resume_position();
        let epoch_now = self.epoch.load(Ordering::Acquire);
        // `resume_target` wins only while the target has NOT yet
        // materialized in produced chunks (`target > decode_head`);
        // comparing against the consumer's lagging `committed` mislabels
        // the warmed-up case and re-emits `[target..decode_head)`.
        let target_time = match self.resume_target {
            Some((seek_epoch, target)) if seek_epoch == epoch_now && target > decode_head => target,
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
            return classify_recreate_err(&e);
        }
        debug!(
            ?target_time,
            stream_pos_final = self.shared_stream.position(),
            "execute_recreation: FormatBoundary+Decode branch exit"
        );
        RecreateOutcome::Done
    }

    fn finish_recreate_outcome(
        &mut self,
        recreate: RecreateState,
        outcome: RecreateOutcome,
    ) -> TrackStep<PcmChunk> {
        match outcome {
            RecreateOutcome::Done => self.apply_recreate_next(&recreate),
            RecreateOutcome::SoftFailed => {
                self.update_state(CurrentFsm::failed(TrackFailure::RecreateFailed {
                    offset: recreate.offset,
                }));
                TrackStep::Failed
            }
            RecreateOutcome::NeedsSourceWait => self.wait_for_source_on_recreate(recreate),
        }
    }

    fn finish_rebuild(
        &mut self,
        rebuild: RebuildState,
        complete: DecoderRebuildComplete,
    ) -> TrackStep<PcmChunk> {
        if self.rebuild_superseded(&rebuild) {
            if let Ok(decoder) = complete.result {
                self.retire_decoder(decoder);
            }
            return self.transition_after_rebuild_superseded(&rebuild);
        }
        let recreate = rebuild.recreate;
        let decoder = match complete.result {
            Ok(decoder) => decoder,
            Err(outcome) => return self.finish_recreate_outcome(recreate, outcome),
        };
        self.install_recreated_session(&recreate.media_info, recreate.offset, decoder);
        let outcome = if recreate_resumes_decode_head(&recreate) {
            self.finish_format_boundary_rebuild()
        } else {
            RecreateOutcome::Done
        };
        self.finish_recreate_outcome(recreate, outcome)
    }

    fn rebuild_superseded(&self, rebuild: &RebuildState) -> bool {
        rebuild.superseded_seek.is_some()
            || self.seek_obs.epoch() != rebuild.started_seek_epoch
            || self.rebuild_variant_superseded(&rebuild.recreate)
    }

    fn rebuild_variant_superseded(&self, recreate: &RecreateState) -> bool {
        if self.shared_stream.has_variant_change_pending() {
            return true;
        }
        let Some(current) = self.shared_stream.media_info() else {
            return false;
        };
        if let (Some(current), Some(rebuild)) =
            (current.variant_index, recreate.media_info.variant_index)
            && current != rebuild
        {
            return true;
        }
        matches!(
            (current.codec, recreate.media_info.codec),
            (Some(current), Some(rebuild)) if current != rebuild
        )
    }

    fn seek_request_from_observer(&self, min_epoch: u64) -> Option<SeekRequest> {
        let epoch = self.seek_obs.epoch();
        if epoch <= min_epoch {
            return None;
        }
        let target = self.seek_obs.target()?;
        Some(SeekRequest {
            seek: SeekContext { target, epoch },
            emit_request: false,
        })
    }

    fn transition_to_seek_request(&mut self, request: SeekRequest) -> TrackStep<PcmChunk> {
        self.update_state(CurrentFsm::seek_requested(request));
        self.reset_effect_chain();
        self.gapless.notify_seek();
        TrackStep::StateChanged
    }

    fn transition_after_rebuild_superseded(
        &mut self,
        rebuild: &RebuildState,
    ) -> TrackStep<PcmChunk> {
        let carried_seek = match &rebuild.recreate.next {
            RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => Some(*request),
            RecreateNext::Decode => None,
        };
        if let Some(request) = rebuild
            .superseded_seek
            .or_else(|| self.seek_request_from_observer(rebuild.started_seek_epoch))
            .or(carried_seek)
        {
            return self.transition_to_seek_request(request);
        }
        if let FormatChangeDetection::Applicable {
            target,
            target_offset,
        } = self.detect_format_change()
        {
            self.start_recreating_decoder(RecreateState {
                cause: RecreateCause::FormatBoundary,
                media_info: target,
                next: RecreateNext::Decode,
                offset: target_offset,
            });
        } else {
            self.update_state(CurrentFsm::decoding());
        }
        TrackStep::StateChanged
    }

    fn record_rebuild_seek_preempt(&self, rebuild: &mut RebuildState) {
        if !self.seek_obs.take_preempt() {
            return;
        }
        let epoch = self.seek_obs.epoch();
        let min_epoch = rebuild
            .superseded_seek
            .map_or(rebuild.started_seek_epoch, |request| request.seek.epoch);
        if epoch <= min_epoch || epoch <= self.epoch.load(Ordering::Acquire) {
            return;
        }
        let Some(target) = self.seek_obs.target() else {
            return;
        };
        rebuild.superseded_seek = Some(SeekRequest {
            seek: SeekContext { target, epoch },
            emit_request: false,
        });
    }

    fn retire_rebuild_complete(&self, complete: DecoderRebuildComplete) {
        if let Ok(decoder) = complete.result {
            self.retire_decoder(decoder);
        }
    }

    fn finish_apply_seek_after_recreate(&mut self, request: SeekRequest) -> TrackStep<PcmChunk> {
        debug!(
            target = ?request.seek.target,
            epoch = request.seek.epoch,
            emit_request = request.emit_request,
            committed_position = ?self.playhead.position(),
            stream_pos = self.shared_stream.position(),
            "finish_apply_seek_after_recreate: enter"
        );
        match self.decoder_seek_safe(request.seek.target) {
            Ok(_outcome) => {
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
        let phase = self.recreate_phase(&recreate);
        if let Some(reason) = self.source_park(phase) {
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
        if src.seek_anchor_stale(applying.mode) {
            trace!(
                ?applying,
                current_variant = ?src
                    .shared_stream
                    .abr_handle()
                    .and_then(|handle| handle.current_variant_index()),
                "apply seek anchor belongs to inactive variant; re-resolving"
            );
            src.update_state(CurrentFsm::seek_requested(applying.request));
            return TrackStep::StateChanged;
        }
        if !src.source_is_ready_for_apply_seek(applying) {
            let phase = src.source_phase_for_wait_context(&WaitContext::ApplySeek(applying));
            if let Some(reason) = src.source_park(phase) {
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
        let post_seek_offset = src.post_seek_anchor_offset(&resume);
        let ready = post_seek_offset.map_or_else(
            || src.source_is_ready(),
            |byte| src.source_is_ready_for_chunk(byte),
        );
        if !ready {
            let phase = post_seek_offset.map_or_else(
                || src.shared_stream.phase(),
                |byte| src.source_phase_for_chunk(byte),
            );
            if let Some(reason) = src.source_park(phase) {
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
            if !src.seek_obs.is_pending()
                && let FormatChangeDetection::Applicable {
                    target: new_info,
                    target_offset,
                } = src.detect_format_change()
            {
                src.start_recreating_decoder(RecreateState {
                    cause: RecreateCause::FormatBoundary,
                    media_info: new_info,
                    next: RecreateNext::Decode,
                    offset: target_offset,
                });
                return TrackStep::StateChanged;
            }
            let phase = src.shared_stream.phase();
            if let Some(reason) = src.source_park(phase) {
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
        match src.prepare_rebuild(&recreate) {
            Ok(()) => TrackStep::StateChanged,
            Err(outcome) => src.finish_recreate_outcome(recreate, outcome),
        }
    }
}

impl Track<RebuildingDecoder> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let mut rebuild = self.into_inner();
        src.record_rebuild_seek_preempt(&mut rebuild);
        while let Some(complete) = rebuild.completion.pop() {
            if complete.ticket == rebuild.ticket {
                return src.finish_rebuild(rebuild, complete);
            }
            src.retire_rebuild_complete(complete);
        }
        src.update_state(CurrentFsm::rebuilding(rebuild));
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

impl Track<SeekRequested> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let request = self.into_inner();
        if !src.source_is_ready() {
            let phase = src.shared_stream.phase();
            if let Some(reason) = src.source_park(phase) {
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
        if let WaitContext::ApplySeek(applying) = context
            && src.seek_anchor_stale(applying.mode)
        {
            trace!(
                ?applying,
                current_variant = ?src
                    .shared_stream
                    .abr_handle()
                    .and_then(|handle| handle.current_variant_index()),
                "waiting apply seek anchor belongs to inactive variant; re-resolving"
            );
            src.update_state(CurrentFsm::seek_requested(applying.request));
            return TrackStep::StateChanged;
        }
        let phase = src.source_phase_for_wait_context(&context);

        if let Some(reason) = src.source_park(phase) {
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
        self.drain_retired_decoders();
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;

    fn decode_epoch(&self) -> u64 {
        // The epoch the current decode belongs to — stored when a seek is
        // applied (`ApplyingSeek` / `try_apply_seek`), and the same value
        // stamped on produced chunks (`decode_one_step`). It LAGS
        // `timeline().seek_epoch()`, which the consumer bumps the instant it
        // requests a seek, long before the worker applies it. A terminal
        // marker (EOF / failure) must carry this decode epoch so a stale
        // end-of-stream produced for a superseded seek is discarded by the
        // consumer's validator rather than mistaken for the new seek's
        // terminal (the oversubscription false-EOF race).
        self.epoch.load(Ordering::Acquire)
    }

    fn flush_deferred(&mut self) {
        self.session.decoder.flush_reader_signals();
        self.drain_retired_decoders();
        self.submit_pending_rebuild();
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

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        if !matches!(self.state, CurrentFsm::RebuildingDecoder(_))
            && let Some(target) = self.preempt_seek_target()
        {
            self.update_state(CurrentFsm::seek_requested(SeekRequest {
                seek: SeekContext {
                    target,
                    epoch: self.seek_obs.epoch(),
                },
                emit_request: false,
            }));
            self.reset_effect_chain();
            self.gapless.notify_seek();
            return TrackStep::StateChanged;
        }
        if !matches!(
            self.state,
            CurrentFsm::RecreatingDecoder(_)
                | CurrentFsm::RebuildingDecoder(_)
                | CurrentFsm::AtEof(_)
                | CurrentFsm::Failed(_)
        ) && self.start_route_change_recreate_if_needed()
        {
            return TrackStep::StateChanged;
        }

        // Move the typed handle out (sentinel = `Decoding`, the unit
        // phase, so the decode hot path that stays in `Decoding` needs
        // no restore). Each `step` either transitions via
        // `update_state` or restores its own phase before returning.
        match mem::replace(&mut self.state, CurrentFsm::decoding()) {
            CurrentFsm::Decoding(handle) => handle.step(self),
            CurrentFsm::SeekRequested(handle) => handle.step(self),
            CurrentFsm::WaitingForSource(handle) => handle.step(self),
            CurrentFsm::ApplyingSeek(handle) => handle.step(self),
            CurrentFsm::RecreatingDecoder(handle) => handle.step(self),
            CurrentFsm::RebuildingDecoder(handle) => handle.step(self),
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

/// Classify a [`CurrentFsm`] phase for the shared activity `PLAYING` flag.
///
/// The Downloader peers read `Activity::is_playing()` in their
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

fn recreate_resumes_decode_head(recreate: &RecreateState) -> bool {
    recreate.cause == RecreateCause::FormatBoundary && matches!(recreate.next, RecreateNext::Decode)
}

/// Build the recreate target `MediaInfo` for a format boundary.
///
/// Returns `None` when there is no boundary to act on. A boundary triggers on
/// either:
/// - explicit codec change with both sides specified, or
/// - variant-index change where codec continuity is not known.
///
/// A known same-codec variant switch is byte-continuity only when the cached
/// container can be decoded from a packet boundary. Init-bearing containers need
/// a decoder boundary even when the codec is unchanged.
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
    let same_codec = matches!(
        (cached.and_then(|c| c.codec), current.codec),
        (Some(a), Some(b)) if a == b
    );
    let container = current
        .container
        .or_else(|| cached.and_then(|c| c.container));
    let variant_requires_boundary =
        variant_requires_format_boundary(variant_changed, same_codec, container);
    if !variant_requires_boundary && !codec_changed {
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

fn variant_requires_format_boundary(
    variant_changed: bool,
    same_codec: bool,
    container: Option<ContainerFormat>,
) -> bool {
    variant_changed && (!same_codec || same_codec_variant_requires_boundary(container))
}

fn same_codec_variant_requires_boundary(container: Option<ContainerFormat>) -> bool {
    container.is_some_and(|c| c != ContainerFormat::Wav && container_needs_init_range(c))
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
/// - Variant-only change whose codec continuity is unknown and whose
///   container is non-init-bearing: return `anchor_byte_offset` — mid-stream
///   resync is valid for MPEG-ES/ADTS/FLAC/Ogg/MPEG-TS.
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
mod rebuilding_decoder_tests {
    use std::{num::NonZeroU32, ops::Range};

    use kithara_bufpool::PcmPool;
    use kithara_decode::{
        DecodeResult, DecoderChunkOutcome, DecoderSeekOutcome, PcmMeta, PcmSpec,
        frames_for_duration,
    };
    use kithara_platform::{
        sync::{Arc, Mutex},
        tokio::runtime::Handle as RuntimeHandle,
    };
    use kithara_storage::WaitOutcome;
    use kithara_stream::{
        Activity, AudioCodec, ChunkPosition, ContainerFormat, PlayheadRead, PlayheadState,
        PrerollHint, ReadOutcome, SeekState, Source, SourceError, StreamError, StreamResult,
        VariantControl,
    };
    use kithara_test_utils::kithara;

    use super::*;

    fn produced_data(fetch: Fetch<PcmChunk>) -> PcmChunk {
        let Fetch::Data { data, .. } = fetch else {
            panic!("TrackStep::Produced must carry PCM data");
        };
        data
    }

    struct Consts;

    impl Consts {
        const CHANNELS: u16 = 2;
        const ROUTE_CHUNK_FRAMES: usize = 256;
        const ROUTE_SAMPLE_RATE: u32 = 48_000;
        const SAMPLE_RATE: u32 = 44_100;
        const TONE_HZ: f64 = 440.0;
    }

    struct TestDecoder {
        id: u64,
        drops: Arc<Mutex<Vec<u64>>>,
    }

    impl TestDecoder {
        fn new(id: u64, drops: Arc<Mutex<Vec<u64>>>) -> Self {
            Self { id, drops }
        }
    }

    impl Drop for TestDecoder {
        fn drop(&mut self) {
            self.drops.lock().push(self.id);
        }
    }

    impl Decoder for TestDecoder {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            Ok(DecoderChunkOutcome::Eof)
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
            Ok(DecoderSeekOutcome::Landed {
                landed_at: pos,
                landed_frame: 0,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
            })
        }

        fn spec(&self) -> PcmSpec {
            PcmSpec::new(2, NonZeroU32::MIN)
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    struct RouteSignalDecoder {
        drops: Arc<Mutex<Vec<u64>>>,
        id: u64,
        next_frame: u64,
        sample_rate: u32,
    }

    impl RouteSignalDecoder {
        fn new(id: u64, sample_rate: u32, drops: Arc<Mutex<Vec<u64>>>) -> Self {
            Self {
                drops,
                id,
                next_frame: 0,
                sample_rate,
            }
        }

        fn pcm_spec(&self) -> PcmSpec {
            PcmSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(self.sample_rate).unwrap_or(NonZeroU32::MIN),
            )
        }
    }

    impl Drop for RouteSignalDecoder {
        fn drop(&mut self) {
            self.drops.lock().push(self.id);
        }
    }

    impl Decoder for RouteSignalDecoder {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            let spec = self.pcm_spec();
            let channels = usize::from(Consts::CHANNELS);
            let frames = Consts::ROUTE_CHUNK_FRAMES;
            let mut samples = PcmPool::default().get();
            samples
                .ensure_len(frames.saturating_mul(channels))
                .expect("route signal fixture fits PCM pool budget");
            for frame in 0..frames {
                let absolute = self
                    .next_frame
                    .saturating_add(u64::try_from(frame).unwrap_or(u64::MAX));
                let absolute_f64 =
                    num_traits::cast::ToPrimitive::to_f64(&absolute).unwrap_or(f64::MAX);
                let t = absolute_f64 / f64::from(self.sample_rate);
                let sample = (t * Consts::TONE_HZ * std::f64::consts::TAU).sin() * 0.25;
                let sample = num_traits::cast::ToPrimitive::to_f32(&sample).unwrap_or(0.0);
                let base = frame.saturating_mul(channels);
                samples[base] = sample;
                samples[base + 1] = sample;
            }
            let frame_count = u32::try_from(frames).unwrap_or(u32::MAX);
            let start = self.next_frame;
            let end = start.saturating_add(u64::from(frame_count));
            self.next_frame = end;
            Ok(DecoderChunkOutcome::Chunk(PcmChunk::new(
                PcmMeta {
                    spec,
                    timestamp: duration_for_frames(self.sample_rate, start),
                    end_timestamp: duration_for_frames(self.sample_rate, end),
                    frame_offset: start,
                    frames: frame_count,
                    ..Default::default()
                },
                samples,
            )))
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
            let frame =
                u64::try_from(frames_for_duration(self.sample_rate, pos)).unwrap_or(u64::MAX);
            self.next_frame = frame;
            Ok(DecoderSeekOutcome::Landed {
                landed_at: duration_for_frames(self.sample_rate, frame),
                landed_frame: frame,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.pcm_spec()
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    struct TestWake;

    impl WorkerWake for TestWake {
        fn wake(&self) {}
    }

    #[derive(Default)]
    struct CountingWake {
        count: AtomicU64,
    }

    impl CountingWake {
        fn count(&self) -> u64 {
            self.count.load(Ordering::Acquire)
        }
    }

    impl WorkerWake for CountingWake {
        fn wake(&self) {
            self.count.fetch_add(1, Ordering::Release);
        }
    }

    struct TestControl {
        media_info: Mutex<Option<MediaInfo>>,
        variant_pending: AtomicBool,
        variant_target: Mutex<Option<usize>>,
        format_range: Mutex<Option<Range<u64>>>,
    }

    impl TestControl {
        fn new(media_info: MediaInfo) -> Self {
            Self {
                media_info: Mutex::new(Some(media_info)),
                variant_pending: AtomicBool::new(false),
                variant_target: Mutex::new(None),
                format_range: Mutex::new(Some(0..32)),
            }
        }

        fn set_media_info(&self, media_info: MediaInfo) {
            *self.media_info.lock() = Some(media_info);
        }

        fn raise_variant_fence(&self, target: usize, media_info: MediaInfo) {
            self.set_media_info(media_info);
            *self.variant_target.lock() = Some(target);
            self.variant_pending.store(true, Ordering::Release);
        }
    }

    impl VariantControl for TestControl {
        fn clear_variant_fence(&self) {
            self.variant_pending.store(false, Ordering::Release);
            *self.variant_target.lock() = None;
        }

        fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
            self.format_range
                .lock()
                .clone()
                .ok_or(StreamError::Source(SourceError::FormatChangeNotApplicable))
        }

        fn has_variant_change_pending(&self) -> bool {
            self.variant_pending.load(Ordering::Acquire)
        }

        fn variant_change_target(&self) -> Option<usize> {
            *self.variant_target.lock()
        }
    }

    struct TestSource {
        control: Arc<TestControl>,
        playhead: Arc<PlayheadState>,
        position: Arc<AtomicU64>,
        seek: Arc<SeekState>,
    }

    impl TestSource {
        fn new(control: Arc<TestControl>) -> Self {
            Self {
                control,
                playhead: Arc::new(PlayheadState::new()),
                position: Arc::new(AtomicU64::new(0)),
                seek: Arc::new(SeekState::new()),
            }
        }
    }

    impl Source for TestSource {
        fn activity(&self) -> Arc<dyn Activity> {
            Arc::clone(&self.seek) as Arc<dyn Activity>
        }

        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn len(&self) -> Option<u64> {
            Some(4096)
        }

        fn media_info(&self) -> Option<MediaInfo> {
            self.control.media_info.lock().clone()
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
        }

        fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
            Ok(ReadOutcome::Eof)
        }

        fn seek_control(&self) -> Arc<dyn SeekControl> {
            Arc::clone(&self.seek) as Arc<dyn SeekControl>
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
            Some(Arc::clone(&self.control) as Arc<dyn VariantControl>)
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> StreamResult<WaitOutcome> {
            Ok(WaitOutcome::Ready)
        }
    }

    struct TestConfig {
        source: TestSource,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                source: TestSource::new(Arc::new(TestControl::new(media_info(0)))),
            }
        }
    }

    struct TestStream;

    impl StreamType for TestStream {
        type Config = TestConfig;
        type Events = ();
        type Source = TestSource;

        async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
            Ok(config.source)
        }
    }

    fn media_info(variant: u32) -> MediaInfo {
        let mut info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        info.variant_index = Some(variant);
        info
    }

    fn recreate_state(variant: u32) -> RecreateState {
        RecreateState {
            media_info: media_info(variant),
            cause: RecreateCause::FormatBoundary,
            next: RecreateNext::Decode,
            offset: 0,
        }
    }

    async fn test_source(
        control: Arc<TestControl>,
        drops: Arc<Mutex<Vec<u64>>>,
    ) -> StreamAudioSource<TestStream> {
        let stream = match Stream::<TestStream>::new(TestConfig {
            source: TestSource::new(control),
        })
        .await
        {
            Ok(stream) => stream,
            Err(err) => panic!("test stream construction failed: {err}"),
        };
        let shared_stream = SharedStream::new(stream);
        let factory_drops = Arc::clone(&drops);
        let decoder_factory: DecoderFactory<TestStream> =
            Arc::new(move |_stream, _info, _offset| {
                Ok(Box::new(TestDecoder::new(99, Arc::clone(&factory_drops))))
            });
        let runtime_handle = match RuntimeHandle::try_current() {
            Ok(handle) => handle,
            Err(err) => panic!("test requires tokio runtime: {err}"),
        };
        StreamAudioSource::new(
            shared_stream,
            DecodeInit {
                decoder: Box::new(TestDecoder::new(1, Arc::clone(&drops))),
                decoder_factory,
                gapless_mode: GaplessMode::Disabled,
                host_sample_rate: Arc::new(AtomicU32::new(Consts::SAMPLE_RATE)),
                media_info: Some(media_info(0)),
                recreate_on_host_rate_change: true,
            },
            Arc::new(AtomicU64::new(0)),
            Vec::new(),
            runtime_handle,
            Arc::new(TestWake),
        )
    }

    async fn route_signal_source(
        control: Arc<TestControl>,
        drops: Arc<Mutex<Vec<u64>>>,
        host_sample_rate: Arc<AtomicU32>,
    ) -> StreamAudioSource<TestStream> {
        let stream = match Stream::<TestStream>::new(TestConfig {
            source: TestSource::new(control),
        })
        .await
        {
            Ok(stream) => stream,
            Err(err) => panic!("test stream construction failed: {err}"),
        };
        let shared_stream = SharedStream::new(stream);
        let factory_drops = Arc::clone(&drops);
        let factory_host_rate = Arc::clone(&host_sample_rate);
        let decoder_factory: DecoderFactory<TestStream> =
            Arc::new(move |_stream, _info, _offset| {
                let rate = factory_host_rate.load(Ordering::Acquire);
                Ok(Box::new(RouteSignalDecoder::new(
                    99,
                    rate,
                    Arc::clone(&factory_drops),
                )))
            });
        let runtime_handle = match RuntimeHandle::try_current() {
            Ok(handle) => handle,
            Err(err) => panic!("test requires tokio runtime: {err}"),
        };
        StreamAudioSource::new(
            shared_stream,
            DecodeInit {
                decoder: Box::new(RouteSignalDecoder::new(
                    1,
                    Consts::SAMPLE_RATE,
                    Arc::clone(&drops),
                )),
                decoder_factory,
                gapless_mode: GaplessMode::Disabled,
                host_sample_rate,
                media_info: Some(media_info(0)),
                recreate_on_host_rate_change: true,
            },
            Arc::new(AtomicU64::new(0)),
            Vec::new(),
            runtime_handle,
            Arc::new(TestWake),
        )
    }

    fn run_pending_rebuild_inline(source: &mut StreamAudioSource<TestStream>) {
        if let Some(job) = source.pending_rebuild_submit.take() {
            job.run();
        }
    }

    fn append_left_channel(left: &mut Vec<f32>, chunk: &PcmChunk) {
        let channels = usize::from(chunk.meta.spec.channels);
        for frame in 0..chunk.frames() {
            left.push(chunk.samples[frame * channels]);
        }
    }

    fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
        assert!(
            (1..left.len()).contains(&center),
            "first-difference center must be in 1..{}, got {center}",
            left.len(),
        );
        let start = center.saturating_sub(half).max(1);
        let end = center.saturating_add(half).min(left.len() - 1);
        let mut peak = 0.0_f32;
        for i in start..=end {
            peak = peak.max((left[i] - left[i - 1]).abs());
        }
        peak
    }

    fn next_test_chunk(
        source: &mut StreamAudioSource<TestStream>,
        route_recreated: &mut bool,
    ) -> PcmChunk {
        loop {
            run_pending_rebuild_inline(source);
            match source.step_track() {
                TrackStep::Produced(fetch) => return produced_data(fetch),
                TrackStep::StateChanged => {
                    if matches!(
                        &source.state,
                        CurrentFsm::RecreatingDecoder(handle)
                            if handle.data().cause == RecreateCause::RouteChange
                    ) {
                        *route_recreated = true;
                    }
                }
                TrackStep::Blocked(_) => {}
                TrackStep::Eof => panic!("route test source reached EOF"),
                TrackStep::Failed => panic!("route test source failed"),
            }
        }
    }

    fn enter_rebuilding(
        source: &mut StreamAudioSource<TestStream>,
        ticket: u64,
        recreate: RecreateState,
    ) {
        source.state = CurrentFsm::rebuilding(RebuildState {
            ticket,
            recreate,
            started_seek_epoch: source.seek_obs.epoch(),
            completion: Arc::clone(&source.rebuild_completion),
            superseded_seek: None,
        });
    }

    fn push_completion_with_drops(
        source: &StreamAudioSource<TestStream>,
        ticket: u64,
        decoder_id: u64,
        drops: Arc<Mutex<Vec<u64>>>,
    ) {
        let pushed = source.rebuild_completion.push(DecoderRebuildComplete {
            result: Ok(Box::new(TestDecoder::new(decoder_id, drops))),
            ticket,
        });
        assert!(pushed.is_ok());
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_pending_poll_blocks() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, drops).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));

        assert!(matches!(
            source.step_track(),
            TrackStep::Blocked(WaitingReason::Waiting)
        ));
        assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_completion_installs_once() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, Arc::clone(&drops)).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        push_completion_with_drops(&source, 7, 2, Arc::clone(&drops));

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(
            source
                .session
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(1)
        );
        assert!(matches!(source.state, CurrentFsm::Decoding(_)));
        assert_eq!(source.retired_decoders.len(), 1);

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[1]);
    }

    #[kithara::test(tokio)]
    async fn route_change_host_rate_delta_starts_decoder_recreate() {
        let control = Arc::new(TestControl::new(media_info(0)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, drops).await;

        source.host_sample_rate.store(48_000, Ordering::Release);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::RecreatingDecoder(handle) => {
                let recreate = handle.data();
                assert_eq!(recreate.cause, RecreateCause::RouteChange);
                match &recreate.next {
                    RecreateNext::ApplySeek(request) => {
                        assert_eq!(request.seek.epoch, source.epoch.load(Ordering::Acquire));
                        assert_eq!(request.seek.target, source.playhead.position());
                        assert!(!request.emit_request);
                    }
                    _ => panic!("expected route-change recreate to resume via ApplySeek"),
                }
                assert_eq!(recreate.offset, source.session.base_offset);
                assert_eq!(recreate.media_info.variant_index, Some(0));
            }
            _ => panic!("expected route-change recreate"),
        }
    }

    #[kithara::test(tokio)]
    async fn route_change_recreate_preserves_position_and_output_rate_continuity_metric() {
        let control = Arc::new(TestControl::new(media_info(0)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let host_sample_rate = Arc::new(AtomicU32::new(Consts::SAMPLE_RATE));
        let mut source = route_signal_source(control, drops, Arc::clone(&host_sample_rate)).await;
        let mut left = Vec::new();
        let mut route_recreated = false;

        for _ in 0..8 {
            let chunk = next_test_chunk(&mut source, &mut route_recreated);
            assert_eq!(chunk.meta.spec.sample_rate.get(), Consts::SAMPLE_RATE);
            append_left_channel(&mut left, &chunk);
            source.playhead.advance(&ChunkPosition::from(&chunk.meta));
        }

        let route_frame = left.len();
        let route_position = source.playhead.position();
        host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

        let mut first_route_timestamp = None;
        let mut saw_new_rate = false;
        for _ in 0..8 {
            let chunk = next_test_chunk(&mut source, &mut route_recreated);
            if first_route_timestamp.is_none() {
                first_route_timestamp = Some(chunk.meta.timestamp);
            }
            saw_new_rate |= chunk.meta.spec.sample_rate.get() == Consts::ROUTE_SAMPLE_RATE;
            append_left_channel(&mut left, &chunk);
            source.playhead.advance(&ChunkPosition::from(&chunk.meta));
        }

        assert!(
            route_recreated,
            "route change must enter recreate machinery"
        );
        assert!(
            saw_new_rate,
            "route-change output chunks must report the new host rate"
        );
        assert_eq!(
            source.session.decoder.spec().sample_rate.get(),
            Consts::ROUTE_SAMPLE_RATE
        );
        let first_route_timestamp =
            first_route_timestamp.expect("route change should produce post-route PCM");
        let drift_ns = first_route_timestamp.abs_diff(route_position).as_nanos();
        assert!(
            drift_ns <= 1_000_000,
            "route recreate drifted by {drift_ns} ns from {route_position:?} to {first_route_timestamp:?}",
        );

        let route_peak = peak_first_diff(&left, route_frame, 64);
        let control_peak = peak_first_diff(&left, Consts::ROUTE_CHUNK_FRAMES * 4, 64);
        let ratio = route_peak / control_peak.max(f32::EPSILON);
        println!(
            "S_ROUTE_CONTINUITY route_peak={route_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
        );
        assert!(
            ratio < 2.0,
            "route-change discontinuity {route_peak:.6} is {ratio:.1}x the control boundary {control_peak:.6}",
        );
    }

    #[kithara::test(tokio)]
    async fn equal_host_rate_does_not_start_route_recreate() {
        let control = Arc::new(TestControl::new(media_info(0)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, drops).await;

        assert!(!source.start_route_change_recreate_if_needed());
        assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    }

    #[kithara::test(tokio)]
    async fn first_matching_host_rate_latches_without_route_recreate() {
        let control = Arc::new(TestControl::new(media_info(0)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let host_sample_rate = Arc::new(AtomicU32::new(0));
        let mut source = route_signal_source(control, drops, Arc::clone(&host_sample_rate)).await;

        host_sample_rate.store(Consts::SAMPLE_RATE, Ordering::Release);

        assert!(!source.start_route_change_recreate_if_needed());
        assert_eq!(source.decoder_host_sample_rate, Consts::SAMPLE_RATE);
        assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    }

    #[kithara::test(tokio)]
    async fn first_mismatched_host_rate_still_starts_route_recreate() {
        let control = Arc::new(TestControl::new(media_info(0)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let host_sample_rate = Arc::new(AtomicU32::new(0));
        let mut source = route_signal_source(control, drops, Arc::clone(&host_sample_rate)).await;

        host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

        assert!(source.start_route_change_recreate_if_needed());
        assert_eq!(source.decoder_host_sample_rate, Consts::ROUTE_SAMPLE_RATE);
        match &source.state {
            CurrentFsm::RecreatingDecoder(handle) => {
                assert_eq!(handle.data().cause, RecreateCause::RouteChange);
            }
            _ => panic!("expected route-change recreate"),
        }
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_seek_epoch_supersedes_completion() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, Arc::clone(&drops)).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        let epoch = source.seek.begin(Duration::from_secs(3));
        push_completion_with_drops(&source, 7, 2, Arc::clone(&drops));

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::SeekRequested(handle) => {
                assert_eq!(handle.data().seek.epoch, epoch);
                assert_eq!(handle.data().seek.target, Duration::from_secs(3));
            }
            _ => panic!("expected seek request after rebuild supersession"),
        }
        assert_eq!(
            source
                .session
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(0)
        );
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[2]);
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_variant_fence_supersedes_completion() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(Arc::clone(&control), Arc::clone(&drops)).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        control.raise_variant_fence(2, media_info(2));
        push_completion_with_drops(&source, 7, 2, Arc::clone(&drops));

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::RecreatingDecoder(handle) => {
                assert_eq!(handle.data().media_info.variant_index, Some(2));
            }
            _ => panic!("expected fresh recreate after variant supersession"),
        }
        assert_eq!(
            source
                .session
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(0)
        );
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[2]);
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_variant_fence_preserves_inflight_seek() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(Arc::clone(&control), Arc::clone(&drops)).await;
        let target = Duration::from_secs(3);
        let request = SeekRequest {
            seek: SeekContext {
                epoch: source.seek.begin(target),
                target,
            },
            emit_request: false,
        };
        enter_rebuilding(
            &mut source,
            7,
            RecreateState {
                cause: RecreateCause::VariantSwitch,
                next: RecreateNext::Seek(request),
                ..recreate_state(1)
            },
        );
        control.raise_variant_fence(2, media_info(2));
        push_completion_with_drops(&source, 7, 2, Arc::clone(&drops));

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::SeekRequested(handle) => assert_eq!(*handle.data(), request),
            _ => panic!("expected in-flight seek after variant supersession"),
        }
        assert_eq!(
            source
                .session
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(0)
        );
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[2]);
    }

    #[kithara::test(tokio)]
    async fn stale_rebuild_completion_retires_decoder_shell_side() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, Arc::clone(&drops)).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        push_completion_with_drops(&source, 6, 3, Arc::clone(&drops));

        assert!(matches!(
            source.step_track(),
            TrackStep::Blocked(WaitingReason::Waiting)
        ));
        assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[3]);
    }

    // A decoder factory that panics during construction must not strand the
    // FSM in `RebuildingDecoder` forever. `DecoderRebuildJob::run` wraps the
    // factory in `catch_unwind`, so the off-RT blocking task terminates
    // normally (no process abort), pushes a `SoftFailed` completion, and
    // wakes the worker — which then surfaces a recreate failure instead of
    // re-arming `Blocked(Waiting)` (the permanent hang under
    // `block_on_underrun`).
    #[kithara::test(tokio)]
    async fn rebuild_factory_panic_fails_track_without_hang() {
        let control = Arc::new(TestControl::new(media_info(1)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let mut source = test_source(control, Arc::clone(&drops)).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));

        let wake = Arc::new(CountingWake::default());
        let panicking_factory: DecoderFactory<TestStream> =
            Arc::new(|_stream, _info, _offset| panic!("decoder construction blew up"));
        let job = DecoderRebuildJob {
            ticket: 7,
            shared_stream: source.shared_stream.clone(),
            media_info: media_info(1),
            base_offset: 0,
            decoder_factory: panicking_factory,
            completion: Arc::clone(&source.rebuild_completion),
            worker_wake: Arc::clone(&wake) as Arc<dyn WorkerWake>,
        };

        // Run exactly like `submit_pending_rebuild`: off the RT core on the
        // blocking pool. Bound the join so a regression surfaces as a test
        // failure rather than a hung worker.
        let handle = spawn_blocking_on(&source.runtime_handle, move || job.run());
        let joined = kithara_platform::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("rebuild blocking task must finish (catch_unwind prevents abort)");
        assert!(
            joined.is_ok(),
            "factory panic escaped DecoderRebuildJob::run; no completion -> permanent hang"
        );
        assert_eq!(wake.count(), 1, "factory panic must wake the worker");

        // The worker's next step must reach the terminal recreate failure,
        // not loop on `Blocked(Waiting)`.
        assert!(matches!(source.step_track(), TrackStep::Failed));
        match &source.state {
            CurrentFsm::Failed(handle) => {
                assert!(matches!(handle.data(), TrackFailure::RecreateFailed { .. }));
            }
            _ => panic!("expected RecreateFailed terminal state after factory panic"),
        }
    }
}

#[cfg(test)]
mod splice_continuity_tests {
    use std::{
        num::NonZeroUsize,
        ops::Range,
        path::Path,
        sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    };

    use kithara_bufpool::{BytePool, PcmPool};
    use kithara_decode::{DecoderConfig, DecoderFactory as DecodeFactory};
    use kithara_platform::{
        sync::{Arc, Mutex},
        tokio::runtime::Handle as RuntimeHandle,
    };
    use kithara_storage::WaitOutcome;
    use kithara_stream::{
        Activity, AudioCodec, ByteMap, ChunkPosition, ContainerFormat, MediaInfo, PlayheadRead,
        PlayheadState, PlayheadWrite, ReadOutcome, SeekState, SegmentDescriptor, Source,
        SourceError, SourceSeekAnchor, StreamError, StreamResult, VariantControl,
    };
    use kithara_test_utils::kithara;

    use super::*;

    fn produced_data(fetch: Fetch<PcmChunk>) -> PcmChunk {
        let Fetch::Data { data, .. } = fetch else {
            panic!("TrackStep::Produced must carry PCM data");
        };
        data
    }

    struct Consts;

    impl Consts {
        const CHANNELS: usize = 2;
        const SAMPLE_RATE: u32 = 44_100;
        const SEGMENT_DURATION_SECS: u64 = 6;
        const SPLICE_SEGMENT: u32 = 3;
        const TOTAL_SEGMENTS: usize = 7;
        const CAPTURE_END_SEGMENT: u64 = 6;
        const SLQ_VARIANT: usize = 0;
        const SMQ_VARIANT: usize = 1;
    }

    struct VariantLayout {
        blob: Vec<u8>,
        init_range: Range<u64>,
        segments: Vec<SegmentDescriptor>,
    }

    struct SpliceState {
        active: AtomicUsize,
        media_info: Mutex<Option<MediaInfo>>,
        pending_variant_change: AtomicBool,
        target_variant: Mutex<Option<usize>>,
        variants: Vec<VariantLayout>,
        warmup_landing: Mutex<Option<SegmentDescriptor>>,
    }

    impl SpliceState {
        fn new(variants: Vec<VariantLayout>) -> Self {
            Self {
                active: AtomicUsize::new(Consts::SLQ_VARIANT),
                media_info: Mutex::new(Some(media_info(Consts::SLQ_VARIANT))),
                pending_variant_change: AtomicBool::new(false),
                target_variant: Mutex::new(None),
                variants,
                warmup_landing: Mutex::new(None),
            }
        }

        fn active_index(&self) -> usize {
            self.active
                .load(Ordering::Acquire)
                .min(self.variants.len().saturating_sub(1))
        }

        fn active_layout(&self) -> &VariantLayout {
            &self.variants[self.active_index()]
        }

        fn switch_to(&self, variant: usize) {
            self.active.store(variant, Ordering::Release);
            *self.media_info.lock() = Some(media_info(variant));
            *self.target_variant.lock() = Some(variant);
            self.pending_variant_change.store(true, Ordering::Release);
        }

        fn warmup_landing(&self) -> Option<SegmentDescriptor> {
            self.warmup_landing.lock().clone()
        }
    }

    impl VariantControl for SpliceState {
        fn clear_variant_fence(&self) {
            self.pending_variant_change.store(false, Ordering::Release);
            *self.target_variant.lock() = None;
        }

        fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
            let range = self.active_layout().init_range.clone();
            if range.is_empty() {
                Err(StreamError::Source(SourceError::FormatChangeNotApplicable))
            } else {
                Ok(range)
            }
        }

        fn has_variant_change_pending(&self) -> bool {
            self.pending_variant_change.load(Ordering::Acquire)
        }

        fn variant_change_target(&self) -> Option<usize> {
            *self.target_variant.lock()
        }
    }

    impl ByteMap for SpliceState {
        fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
            Ok(self.segment_at_time(position).map(|segment| {
                SourceSeekAnchor::builder()
                    .segment_start(segment.decode_time)
                    .segment_end(segment.decode_time.saturating_add(segment.duration))
                    .segment_index(segment.segment_index)
                    .variant_index(segment.variant_index)
                    .byte_offset(segment.byte_range.start)
                    .build()
            }))
        }

        delegate! {
            to self {
                #[expr($.init_range.clone())]
                #[call(active_layout)]
                fn init_segment_range(&self) -> Range<u64>;
                #[expr(Some(u64::try_from($.blob.len()).expect("blob length fits u64")))]
                #[call(active_layout)]
                fn len(&self) -> Option<u64>;
                #[expr(u32::try_from($.segments.len()).ok())]
                #[call(active_layout)]
                fn segment_count(&self) -> Option<u32>;
            }
        }

        fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
            self.active_layout()
                .segments
                .iter()
                .find(|segment| segment.byte_range.start >= byte_offset)
                .cloned()
        }

        fn segment_at_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
            self.active_layout()
                .segments
                .iter()
                .find(|segment| segment.byte_range.contains(&byte_offset))
                .cloned()
        }

        fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
            self.active_layout()
                .segments
                .get(usize::try_from(segment_index).ok()?)
                .cloned()
        }

        fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
            let found = self
                .active_layout()
                .segments
                .iter()
                .find(|segment| t < segment.decode_time.saturating_add(segment.duration))
                .or_else(|| self.active_layout().segments.last())
                .cloned();
            if self.active_index() == Consts::SMQ_VARIANT
                && t < Duration::from_secs(120)
                && let Some(segment) = found.as_ref()
            {
                *self.warmup_landing.lock() = Some(segment.clone());
            }
            found
        }
    }

    struct SpliceSource {
        playhead: Arc<PlayheadState>,
        position: Arc<AtomicU64>,
        seek: Arc<SeekState>,
        state: Arc<SpliceState>,
    }

    impl SpliceSource {
        fn new(state: Arc<SpliceState>) -> Self {
            Self {
                playhead: Arc::new(PlayheadState::new()),
                position: Arc::new(AtomicU64::new(0)),
                seek: Arc::new(SeekState::new()),
                state,
            }
        }
    }

    impl Source for SpliceSource {
        fn activity(&self) -> Arc<dyn Activity> {
            Arc::clone(&self.seek) as Arc<dyn Activity>
        }

        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
            Some(Arc::clone(&self.state) as Arc<dyn ByteMap>)
        }

        fn len(&self) -> Option<u64> {
            Some(
                u64::try_from(self.state.active_layout().blob.len()).expect("blob length fits u64"),
            )
        }

        fn media_info(&self) -> Option<MediaInfo> {
            self.state.media_info.lock().clone()
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
        }

        fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
            let blob = &self.state.active_layout().blob;
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            if start >= blob.len() {
                return Ok(ReadOutcome::Eof);
            }
            let n = (blob.len() - start).min(buf.len());
            buf[..n].copy_from_slice(&blob[start..start + n]);
            Ok(ReadOutcome::Bytes(
                NonZeroUsize::new(n).expect("non-empty read must produce nonzero bytes"),
            ))
        }

        fn seek_control(&self) -> Arc<dyn SeekControl> {
            Arc::clone(&self.seek) as Arc<dyn SeekControl>
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
            Some(Arc::clone(&self.state) as Arc<dyn VariantControl>)
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> StreamResult<WaitOutcome> {
            Ok(WaitOutcome::Ready)
        }
    }

    struct SpliceConfig {
        state: Arc<SpliceState>,
    }

    impl Default for SpliceConfig {
        fn default() -> Self {
            Self {
                state: Arc::new(SpliceState::new(vec![
                    build_variant_layout("slq", Consts::SLQ_VARIANT),
                    build_variant_layout("smq", Consts::SMQ_VARIANT),
                ])),
            }
        }
    }

    struct SpliceStream;

    impl StreamType for SpliceStream {
        type Config = SpliceConfig;
        type Events = ();
        type Source = SpliceSource;

        async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
            Ok(SpliceSource::new(config.state))
        }
    }

    struct TestWake;

    impl WorkerWake for TestWake {
        fn wake(&self) {}
    }

    fn asset_bytes(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}"))
    }

    fn build_variant_layout(label: &str, variant_index: usize) -> VariantLayout {
        let init = asset_bytes(&format!("init-{label}-a1.mp4"));
        let init_len = u64::try_from(init.len()).expect("init length fits u64");
        let mut blob = init;
        let mut byte_cursor = init_len;
        let mut segments = Vec::new();
        for segment_number in 1..=Consts::TOTAL_SEGMENTS {
            let bytes = asset_bytes(&format!("segment-{segment_number}-{label}-a1.m4s"));
            let start = byte_cursor;
            let end = start + u64::try_from(bytes.len()).expect("segment length fits u64");
            let segment_index = u32::try_from(segment_number - 1).expect("segment index fits u32");
            segments.push(SegmentDescriptor::new(
                start..end,
                Duration::from_secs(u64::from(segment_index) * Consts::SEGMENT_DURATION_SECS),
                Duration::from_secs(Consts::SEGMENT_DURATION_SECS),
                segment_index,
                variant_index,
            ));
            blob.extend_from_slice(&bytes);
            byte_cursor = end;
        }
        VariantLayout {
            blob,
            init_range: 0..init_len,
            segments,
        }
    }

    fn media_info(variant: usize) -> MediaInfo {
        let mut info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        info.variant_index = Some(u32::try_from(variant).expect("variant fits u32"));
        info
    }

    fn decoder_backend() -> kithara_decode::DecoderBackend {
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        {
            kithara_decode::DecoderBackend::Apple
        }
        #[cfg(not(all(feature = "apple", any(target_os = "macos", target_os = "ios"))))]
        {
            kithara_decode::DecoderBackend::Symphonia
        }
    }

    fn decoder_config<T: StreamType>(
        stream: &SharedStream<T>,
        backend: kithara_decode::DecoderBackend,
        byte_len: Arc<AtomicU64>,
    ) -> DecoderConfig<kithara_resampler::NoResamplerBackend> {
        byte_len.store(stream.len().unwrap_or(0), Ordering::Release);
        DecoderConfig::builder()
            .backend(backend)
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .byte_len_handle(byte_len)
            .maybe_byte_map(stream.byte_map())
            .gapless(false)
            .build()
    }

    async fn splice_source(state: Arc<SpliceState>) -> StreamAudioSource<SpliceStream> {
        let stream = Stream::<SpliceStream>::new(SpliceConfig {
            state: Arc::clone(&state),
        })
        .await
        .expect("create in-memory splice stream");
        let shared_stream = SharedStream::new(stream);
        let backend = decoder_backend();
        let initial_byte_len = Arc::new(AtomicU64::new(0));
        let initial_decoder = DecodeFactory::create_from_media_info(
            shared_stream.clone(),
            &media_info(Consts::SLQ_VARIANT),
            decoder_config(&shared_stream, backend, Arc::clone(&initial_byte_len)),
        )
        .expect("create initial slq fMP4 decoder");
        let initial_spec = initial_decoder.spec();
        let host_sample_rate = Arc::new(AtomicU32::new(Consts::SAMPLE_RATE));
        let pcm_pool = PcmPool::default().clone();
        let effects =
            crate::pipeline::config::create_effects(initial_spec, None, &pcm_pool, Vec::new());
        let factory_byte_len = Arc::new(AtomicU64::new(0));
        let decoder_factory: DecoderFactory<SpliceStream> =
            Arc::new(move |stream, info, base_offset| {
                let byte_len = stream
                    .len()
                    .map_or(0, |len| len.saturating_sub(base_offset));
                factory_byte_len.store(byte_len, Ordering::Release);
                let config: DecoderConfig<kithara_resampler::NoResamplerBackend> =
                    DecoderConfig::builder()
                        .backend(backend)
                        .byte_pool(BytePool::default())
                        .pcm_pool(PcmPool::default())
                        .byte_len_handle(Arc::clone(&factory_byte_len))
                        .maybe_byte_map(stream.byte_map())
                        .gapless(false)
                        .build();
                let input = OffsetReader::new(stream, base_offset);
                let decoder = DecodeFactory::create_from_media_info(input, &info, config)?;
                decoder.update_byte_len(byte_len);
                Ok(decoder)
            });
        StreamAudioSource::new(
            shared_stream,
            DecodeInit {
                decoder: initial_decoder,
                decoder_factory,
                gapless_mode: GaplessMode::Disabled,
                host_sample_rate,
                media_info: Some(media_info(Consts::SLQ_VARIANT)),
                recreate_on_host_rate_change: false,
            },
            Arc::new(AtomicU64::new(0)),
            effects,
            RuntimeHandle::try_current().expect("test requires tokio runtime"),
            Arc::new(TestWake),
        )
    }

    fn run_pending_rebuild_inline(source: &mut StreamAudioSource<SpliceStream>) {
        if let Some(job) = source.pending_rebuild_submit.take() {
            job.run();
        }
    }

    fn append_left_channel(left: &mut Vec<f32>, chunk: &PcmChunk) {
        let channels = usize::from(chunk.meta.spec.channels);
        assert_eq!(channels, Consts::CHANNELS, "AAC fixture should be stereo");
        for frame in 0..chunk.frames() {
            left.push(chunk.samples[frame * channels]);
        }
    }

    fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
        assert!(
            (1..left.len()).contains(&center),
            "first-difference center must be in 1..{}, got {center}",
            left.len(),
        );
        let start = center.saturating_sub(half).max(1);
        let end = center.saturating_add(half).min(left.len() - 1);
        let mut peak = 0.0_f32;
        for i in start..=end {
            let diff = (left[i] - left[i - 1]).abs();
            peak = peak.max(diff);
        }
        peak
    }

    fn segment_boundary_frame(segment: usize) -> usize {
        let seconds = u64::try_from(segment)
            .unwrap_or(u64::MAX)
            .saturating_mul(Consts::SEGMENT_DURATION_SECS);
        let frames = seconds.saturating_mul(u64::from(Consts::SAMPLE_RATE));
        usize::try_from(frames).unwrap_or(usize::MAX)
    }

    // splice-continuity contract: RED = audible click on variant switch
    // (see .docs/plans/2026-07-03-resampler-native-src-design.md, S-Click)
    #[kithara::test(tokio)]
    async fn hls_aac_lc_abr_variant_switch_splice_continuity_metric() {
        let state = Arc::new(SpliceState::new(vec![
            build_variant_layout("slq", Consts::SLQ_VARIANT),
            build_variant_layout("smq", Consts::SMQ_VARIANT),
        ]));
        let mut source = splice_source(Arc::clone(&state)).await;
        let splice_time =
            Duration::from_secs(u64::from(Consts::SPLICE_SEGMENT) * Consts::SEGMENT_DURATION_SECS);
        let capture_frames = usize::try_from(
            Consts::CAPTURE_END_SEGMENT
                .saturating_mul(Consts::SEGMENT_DURATION_SECS)
                .saturating_mul(u64::from(Consts::SAMPLE_RATE)),
        )
        .expect("capture frame count fits usize");
        let mut left = Vec::with_capacity(capture_frames);
        let mut switched = false;
        let mut splice_frame = None;

        while left.len() < capture_frames {
            run_pending_rebuild_inline(&mut source);
            match source.step_track() {
                TrackStep::Produced(fetch) => {
                    let chunk = produced_data(fetch);
                    append_left_channel(&mut left, &chunk);
                    source.playhead.advance(&ChunkPosition::from(&chunk.meta));
                    if !switched
                        && chunk.meta.segment_index == Some(Consts::SPLICE_SEGMENT - 1)
                        && chunk.meta.end_timestamp >= splice_time
                    {
                        state.switch_to(Consts::SMQ_VARIANT);
                        switched = true;
                        splice_frame = Some(left.len());
                    }
                }
                TrackStep::StateChanged | TrackStep::Blocked(_) => {}
                TrackStep::Eof => break,
                TrackStep::Failed => panic!("splice source failed before metric collection"),
            }
        }

        assert!(switched, "test must trigger the slq -> smq splice");
        let splice_frame = splice_frame.expect("splice frame should be captured");
        if let Some(landing) = state.warmup_landing() {
            println!(
                "SPLICE_WARMUP variant={} segment={}",
                landing.variant_index, landing.segment_index
            );
        }
        let switch_peak = peak_first_diff(&left, splice_frame, 64);
        let mut control_peak = 0.0_f32;
        let mut control_count = 0usize;
        for k in 1..Consts::TOTAL_SEGMENTS {
            let boundary = segment_boundary_frame(k);
            if boundary >= left.len() || boundary.abs_diff(splice_frame) <= 4096 {
                continue;
            }
            control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
            control_count += 1;
        }
        assert!(
            control_count >= 2,
            "splice continuity metric needs at least two same-variant control boundaries, got {control_count}",
        );
        let ratio = switch_peak / control_peak.max(f32::EPSILON);
        println!(
            "SPLICE_CONTINUITY switch_peak={switch_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
        );
        assert!(
            ratio < 3.0,
            "variant-switch stitch discontinuity {switch_peak:.6} is {ratio:.1}x the worst same-variant segment boundary {control_peak:.6} — audible click at the splice",
        );
    }

    #[kithara::test(tokio)]
    async fn hls_aac_lc_same_variant_recreate_continuity_metric() {
        let state = Arc::new(SpliceState::new(vec![build_variant_layout(
            "slq",
            Consts::SLQ_VARIANT,
        )]));
        let mut source = splice_source(Arc::clone(&state)).await;
        let recreate_after =
            Duration::from_secs(u64::from(Consts::SPLICE_SEGMENT) * Consts::SEGMENT_DURATION_SECS);
        let capture_frames = usize::try_from(
            Consts::CAPTURE_END_SEGMENT
                .saturating_mul(Consts::SEGMENT_DURATION_SECS)
                .saturating_mul(u64::from(Consts::SAMPLE_RATE)),
        )
        .expect("capture frame count fits usize");
        let mut left = Vec::with_capacity(capture_frames);
        let mut recreated = false;
        let mut recreate_frame = None;

        while left.len() < capture_frames {
            run_pending_rebuild_inline(&mut source);
            match source.step_track() {
                TrackStep::Produced(fetch) => {
                    let chunk = produced_data(fetch);
                    append_left_channel(&mut left, &chunk);
                    source.playhead.advance(&ChunkPosition::from(&chunk.meta));
                    if !recreated
                        && chunk.meta.segment_index == Some(Consts::SPLICE_SEGMENT - 1)
                        && chunk.meta.end_timestamp >= recreate_after
                    {
                        let active = state.active_index();
                        assert_eq!(
                            active,
                            Consts::SLQ_VARIANT,
                            "same-variant recreate test must stay on the SLQ variant",
                        );
                        source.start_recreating_decoder(RecreateState {
                            cause: RecreateCause::VariantSwitch,
                            media_info: media_info(active),
                            next: RecreateNext::ApplySeek(SeekRequest {
                                seek: SeekContext {
                                    epoch: source.epoch.load(Ordering::Acquire),
                                    target: chunk.meta.end_timestamp,
                                },
                                emit_request: false,
                            }),
                            offset: state.active_layout().init_range.start,
                        });
                        recreated = true;
                        recreate_frame = Some(left.len());
                    }
                }
                TrackStep::StateChanged | TrackStep::Blocked(_) => {}
                TrackStep::Eof => break,
                TrackStep::Failed => {
                    panic!("same-variant recreate source failed before metric collection");
                }
            }
        }

        assert!(recreated, "test must trigger the same-variant recreate");
        assert_eq!(
            state.active_index(),
            Consts::SLQ_VARIANT,
            "same-variant recreate must not change the active variant",
        );
        let recreate_frame = recreate_frame.expect("recreate frame should be captured");
        let recreate_peak = peak_first_diff(&left, recreate_frame, 64);
        let mut control_peak = 0.0_f32;
        let mut control_count = 0usize;
        for k in 1..Consts::TOTAL_SEGMENTS {
            let boundary = segment_boundary_frame(k);
            if boundary >= left.len() || boundary.abs_diff(recreate_frame) <= 4096 {
                continue;
            }
            control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
            control_count += 1;
        }
        assert!(
            control_count >= 2,
            "recreate continuity metric needs at least two same-content control boundaries, got {control_count}",
        );
        let ratio = recreate_peak / control_peak.max(f32::EPSILON);
        println!(
            "RECREATE_CONTINUITY recreate_peak={recreate_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
        );
        assert!(
            ratio < 3.0,
            "same-variant recreate discontinuity {recreate_peak:.6} is {ratio:.1}x the worst same-content segment boundary {control_peak:.6}",
        );
    }
}

#[cfg(test)]
mod playing_flag_tests {
    use crossbeam_queue::ArrayQueue;
    use kithara_platform::sync::Arc;
    use kithara_stream::MediaInfo;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pipeline::track_fsm::{
        ApplySeekState, RebuildState, RecreateCause, RecreateNext, RecreateState, ResumeState,
        SeekContext, SeekMode, SeekRequest, TrackFailure, WaitContext, WaitingReason,
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

    fn recreate_state() -> RecreateState {
        RecreateState {
            cause: RecreateCause::FormatBoundary,
            media_info: MediaInfo::default(),
            next: RecreateNext::Decode,
            offset: 0,
        }
    }

    fn rebuild_state() -> RebuildState {
        RebuildState {
            ticket: 1,
            recreate: recreate_state(),
            started_seek_epoch: 0,
            completion: Arc::new(ArrayQueue::new(1)),
            superseded_seek: None,
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
            cause: RecreateCause::FormatBoundary,
            media_info: MediaInfo::default(),
            next: RecreateNext::Decode,
            offset: 0,
        })));
        assert!(playing_for_state(&CurrentFsm::rebuilding(rebuild_state())));
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
                    cause: RecreateCause::VariantSwitch,
                    media_info: MediaInfo::default(),
                    next: RecreateNext::Decode,
                    offset: 0,
                }),
                true,
            ),
            (CurrentFsm::rebuilding(rebuild_state()), true),
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
    fn same_codec_fmp4_variant_change_recreates_boundary() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(1),
        );
        let target = resolve_format_change_target(Some(&cached), &current)
            .expect("same-codec fMP4 variant change must re-prime the demuxer");
        assert_eq!(target.variant_index, Some(1));
        assert_eq!(target.codec, Some(AudioCodec::AacLc));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn same_codec_wav_variant_change_is_byte_continuity() {
        let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
        let current = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(1));
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
