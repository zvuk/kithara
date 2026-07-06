use std::{
    io::{Error as IoError, Seek, SeekFrom},
    marker::PhantomData,
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use delegate::delegate;
use fast_interleave::deinterleave_variable;
use kithara_bufpool::{BytePool, PcmBuf, PcmPool};
use kithara_decode::{
    Decoder, DecoderConfig, DecoderFactory, GaplessMode, PcmChunk, PcmMeta, PcmSpec, TrackMetadata,
};
use kithara_events::{AudioEvent, DeferredBus, EventBus, SeekLifecycleStage, SegmentLocation};
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread::{is_worker_thread, sleep as thread_sleep};
use kithara_platform::{
    CancelScope, CancelToken,
    thread::park_timeout,
    time::Duration,
    tokio::{runtime::Handle as RuntimeHandle, task::spawn_blocking},
};
use kithara_stream::{
    ChunkPosition, DeferredWake, MediaInfo, PlayheadWrite, SeekControl, SeekObserve, Stream,
    StreamType, WorkerWake,
};
use kithara_test_utils::kithara;
use portable_atomic::AtomicF32;
use tracing::{debug, info, trace, warn};

use crate::{
    effects::timestretch::StretchControls,
    pipeline::{
        config::{AudioConfig, create_effects, expected_output_spec},
        fetch::{EpochValidator, Fetch, FetchKind},
        fused_src,
        source::{DecodeInit, OffsetReader, SharedStream, StreamAudioSource},
        track_fsm::ConsumerPhase,
    },
    runtime::{AtomicServiceClass, WakeSignal},
    traits::{
        AudioEffect, ChunkOutcome, DecodeError, PcmReader, PendingReason, ReadOutcome, SeekOutcome,
    },
    worker::{
        EngineLoad, PreloadGate,
        handle::{AudioWorkerHandle, TrackRegistration, WorkerWakeBridge},
        thread_wake::ThreadWake,
        types::{ServiceClass, TrackId},
    },
};

/// Saturating-clamp `u128` milliseconds into `u64`. Caller has explicitly
/// chosen "report capped value" semantics for telemetry events that can't
/// surface a wider integer to subscribers; production durations are well
/// under `u64::MAX` ms (~584 million years).
fn clamp_u128_to_u64_millis(ms: u128) -> u64 {
    num_traits::cast::ToPrimitive::to_u64(&ms).unwrap_or(u64::MAX)
}

/// Multiply `frames * channels` and convert to `usize` for buffer indexing.
/// Errors if the product does not fit in `usize` on the host (32-bit targets).
fn frames_to_samples(frames: u64, channels: u64) -> Result<usize, DecodeError> {
    let samples = frames.saturating_mul(channels);
    usize::try_from(samples).map_err(|err| DecodeError::Io {
        source: IoError::other(format!(
            "frames*channels overflow: {samples} does not fit usize: {err}"
        )),
    })
}

fn warm_channels_from_media_info(info: Option<&MediaInfo>) -> usize {
    info.and_then(|info| info.channels).map_or(2, usize::from)
}

enum FetchOutcome {
    Continue,
    Return(Option<PcmChunk>),
}

enum RecvOutcome {
    Closed,
    Empty,
    Item(Fetch<PcmChunk>),
}

const AUDIO_EVENT_CAPACITY: usize = 64;

struct ReaderOutputWake {
    thread: Arc<ThreadWake>,
    emit: DeferredBus<AudioEvent>,
}

impl ReaderOutputWake {
    fn new(thread: Arc<ThreadWake>, emit: DeferredBus<AudioEvent>) -> Self {
        Self { thread, emit }
    }
}

impl WakeSignal for ReaderOutputWake {
    fn wake(&self) {
        WakeSignal::wake(self.thread.as_ref());
    }

    fn on_data_available(&self) {
        self.emit.enqueue(AudioEvent::OutputAvailable);
    }

    fn flush_deferred(&self) {
        self.emit.flush();
    }
}

/// Generic audio pipeline running in a separate thread.
///
/// Provides a simple interface for reading decoded PCM audio, compatible
/// with cpal and rodio backends. See the crate `README.md` "Usage".
pub struct Audio<S> {
    /// Narrow mutating playhead handle — committed position and total duration.
    pub(crate) playhead: Arc<dyn PlayheadWrite>,

    /// Startup gate for async preload (first chunk available).
    pub(crate) preload_gate: Arc<PreloadGate>,

    /// Narrow seek-control handle — initiates and marks seek epochs.
    pub(crate) seek: Arc<dyn SeekControl>,

    /// Narrow seek-observe handle — reads seek state (pending epoch, etc.).
    pub(crate) seek_obs: Arc<dyn SeekObserve>,

    /// Consumer-side phase — replaces the old `eof: bool` flag.
    pub(crate) consumer_phase: ConsumerPhase,

    /// Epoch validator for filtering stale chunks.
    pub(crate) validator: EpochValidator,

    /// Current chunk being read (auto-recycles to pool on drop).
    pub(crate) current_chunk: Option<PcmChunk>,

    /// Current audio specification (updated from chunks).
    pub(crate) spec: PcmSpec,

    /// How many frames of `current_chunk` have been served to the
    /// caller. Local consumer cursor — reset to 0 on every new chunk
    /// (`fill_buffer`) and after seek (the next `fill_buffer` after
    /// `commit_seek_landed` issues a fresh chunk).
    pub(crate) current_chunk_consumed_frames: u64,

    /// Shared epoch counter with worker (kept alive for `Arc` shared ownership).
    _epoch: Arc<AtomicU64>,

    /// Target sample rate of the audio host (shared for dynamic updates).
    /// 0 means "not set".
    host_sample_rate: Arc<AtomicU32>,

    /// Shared playback-rate state for compatibility with direct `Audio`
    /// callers. Runtime speed changes route into `stretch` when present; the
    /// fixed-ratio resampler never reads this value.
    playback_rate: Arc<AtomicF32>,

    /// Wake handle for blocking PCM reads.
    reader_wake: Arc<ThreadWake>,

    /// Shared priority hint for this track's worker node. Written wait-free
    /// from the real-time audio thread (`set_service_class` during fade
    /// transitions) and read by the worker scheduler each pass, so a priority
    /// change needs no allocating command-channel send on the audio thread.
    service_class: Arc<AtomicServiceClass>,

    /// Unified event bus.
    bus: EventBus,

    /// PCM chunk receiver.
    pcm_rx: crate::runtime::Inlet<Fetch<PcmChunk>>,

    /// Runtime ABR handle snapshot taken at construction — cloned from the
    /// underlying stream's source. `None` for non-adaptive sources.
    abr_handle: Option<kithara_abr::AbrHandle>,

    /// Cancellation token for graceful shutdown.
    cancel: Option<CancelToken>,

    /// Interleaved scratch for `read_planar`, drawn from `pcm_pool` once and
    /// pre-sized off the audio thread in `new` so the real-time path never
    /// reallocates. `Option` only so it can be detached during the inner
    /// `read` call, then restored.
    interleaved: Option<PcmBuf>,

    /// `(seek_epoch, position_ms)` of the last emitted `PlaybackProgress`.
    /// Throttles high-frequency progress telemetry within an epoch so it
    /// cannot starve low-frequency control events on the shared bounded bus;
    /// a new seek epoch always emits.
    last_progress_emit: Option<(u64, u64)>,

    /// Off-core reader→peer wake for segmented sources. `Audio::seek`
    /// can notify it after publishing the seek target; non-segmented
    /// sources keep `None`.
    peer_wake: Option<Arc<DeferredWake>>,

    /// Live time-stretch controls when this source owns speed DSP. `Some`
    /// makes it the sink for `set_playback_rate` so the effect chain's
    /// `TimeStretchProcessor` sees rate moves; `None` leaves speed pinned.
    stretch: Option<Arc<StretchControls>>,

    /// Assigned track ID in the shared worker (used for unregister on drop).
    track_id: Option<TrackId>,

    /// Worker handle for unregistration and optional shutdown.
    worker: Option<AudioWorkerHandle>,

    /// Spent-chunk return ring. Every `PcmChunk` this real-time consumer
    /// finishes with is pushed here instead of being dropped, so the pooled
    /// buffer is recycled on the worker thread (`DecoderNode::recycle`)
    /// and never freed on the audio thread. Sized to outlive a full
    /// ring-drain on seek, so the lock-free push never fails on the hot path.
    trash_tx: crate::runtime::Outlet<PcmChunk>,

    /// Shared PCM pool: source for the decode worker's per-packet buffers and
    /// for the held `read_planar` interleaved scratch below.
    pcm_pool: PcmPool,

    /// Marker for source type.
    _marker: PhantomData<S>,

    /// Track metadata (title, artist, album, artwork).
    metadata: TrackMetadata,

    /// Offline-consumer opt-in: a ring underrun blocks (engine-aware park)
    /// instead of returning an empty outcome. Never set on real-time hosts.
    block_on_underrun: bool,

    /// Whether the worker was auto-created for this track (standalone mode).
    /// Standalone workers are shut down when the track is dropped.
    is_standalone_worker: bool,

    /// Whether `preload()` has been called (enables non-blocking mode).
    preloaded: bool,
}

/// Consumer-side snapshot attached to the `recv_outcome_blocking` watchdog
/// (built by [`Audio::consumer_hang_ctx`]). Serialized into the hang dump
/// *only* by the real (test) detector — the release detector drops the
/// context closure uncalled, so none of these reads run in production.
/// Carries the live ABR view so a starved drain reports *which* variant the
/// run landed on and whether an escape/decision is stuck mid-flight.
#[derive(serde::Serialize)]
struct ConsumerHangCtx {
    phase: String,
    variant: Option<usize>,
    abr_escaping: Option<bool>,
    abr_locked: Option<bool>,
    abr_pending: Option<String>,
    epoch: u64,
    preloaded: bool,
    block_on_underrun: bool,
}

impl<S> Audio<S> {
    fn create_channels(
        pcm_buffer_chunks: usize,
        bus: &EventBus,
        reader_wake: &Arc<ThreadWake>,
    ) -> (
        crate::runtime::Outlet<Fetch<PcmChunk>>,
        crate::runtime::Inlet<Fetch<PcmChunk>>,
    ) {
        let wake: Arc<dyn WakeSignal> = Arc::new(ReaderOutputWake::new(
            Arc::clone(reader_wake),
            Self::create_emit(bus),
        ));
        crate::runtime::connect::<Fetch<PcmChunk>>(pcm_buffer_chunks.max(1), Some(wake))
    }

    /// Deferred sink for FSM lifecycle events. The produce core enqueues
    /// lock-free; the scheduler shell flushes (the `broadcast::send` is a
    /// `kevent` the forbid-blocking core must not make). Capacity covers a pass's
    /// worth of lifecycle events with margin — flushed every pass, so under
    /// normal lifecycle it never fills.
    fn create_emit(bus: &EventBus) -> DeferredBus<AudioEvent> {
        DeferredBus::new(bus.clone(), AUDIO_EVENT_CAPACITY)
    }

    /// Minimum playback-position advance (ms) between `PlaybackProgress`
    /// emissions. Caps progress telemetry to ~10/s so it cannot flood the
    /// shared bounded event bus and drop control events.
    const PROGRESS_EMIT_MIN_DELTA_MS: u64 = 100;

    /// Per-buffer frame capacity used to pre-warm the PCM pool for the decode
    /// worker's per-packet buffers. Covers the largest decoder packet across
    /// supported codecs (FLAC's 4608-frame block; AAC/MP3/ALAC are smaller and
    /// reuse these buffers without a realloc). The `read_planar` interleaved
    /// scratch is sized separately and held per-`Audio` (see `interleaved`).
    const WARM_DECODE_FRAMES: usize = 4608;

    /// Runtime ABR handle (cloned from the stream's source at
    /// construction). `Some` for adaptive sources (HLS), `None` for
    /// file/non-adaptive sources.
    #[must_use]
    pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.abr_handle.clone()
    }

    /// Acquire and pre-size the held interleaved scratch for `read_planar`.
    ///
    /// Sized to one second of interleaved output at `spec` — the consumer
    /// reads at most a few hundred ms per call, so this covers the request
    /// with margin for host-rate / playback-rate changes. Called off the audio
    /// thread in `new`, so `read_planar` reuses this buffer without ever
    /// reallocating on the real-time path.
    fn alloc_interleaved_scratch(pool: &PcmPool, spec: PcmSpec) -> PcmBuf {
        let channels = usize::from(spec.channels).max(2);
        let sample_rate = usize::try_from(spec.sample_rate.get()).unwrap_or(usize::MAX);
        let capacity = sample_rate.saturating_mul(channels);
        pool.get_with(|buf| {
            buf.clear();
            let cap = buf.capacity();
            if cap < capacity {
                buf.reserve(capacity - cap);
            }
        })
    }

    fn close_channel_and_mark_eof(&mut self) -> Option<PcmChunk> {
        self.consumer_phase = ConsumerPhase::Failed;
        None
    }

    /// Current variant's metadata. Pulled live from the ABR peer on
    /// every call — no caching — so the UI never sees a stale label
    /// after an ABR switch. `None` for non-adaptive sources or peers
    /// that have not yet registered variants.
    #[must_use]
    pub fn current_variant(&self) -> Option<kithara_events::VariantInfo> {
        self.abr_handle.as_ref()?.current_variant()
    }

    /// Hand a spent chunk to the worker's return ring instead of dropping
    /// it here. The pooled buffer is then recycled on the worker thread,
    /// keeping `free`/`Pool::put` off the real-time audio thread. The ring
    /// is sized so this lock-free push never fails on the hot path; the
    /// `debug_assert` guards the sizing invariant, and the last-resort drop
    /// only runs if that invariant is ever broken.
    fn discard_chunk(&mut self, chunk: PcmChunk) {
        if let Err(_overflow) = self.trash_tx.try_push(chunk) {
            debug_assert!(
                false,
                "PCM trash ring overflow — spent buffer freed on the audio thread"
            );
        }
    }

    /// Get total duration of the audio stream.
    ///
    /// Returns `None` for streaming sources where duration is unknown.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.playhead.duration()
    }

    fn emit_audio_event(&self, event: AudioEvent) {
        self.bus.publish(event);
    }

    fn emit_playback_progress(&mut self) {
        let position_ms = clamp_u128_to_u64_millis(self.position().as_millis());
        let epoch = self.validator.epoch;
        if let Some((last_epoch, last_ms)) = self.last_progress_emit
            && last_epoch == epoch
            && position_ms.abs_diff(last_ms) < Self::PROGRESS_EMIT_MIN_DELTA_MS
        {
            return;
        }
        self.last_progress_emit = Some((epoch, position_ms));

        let total_ms = self
            .playhead
            .duration()
            .map(|duration| clamp_u128_to_u64_millis(duration.as_millis()));
        let buffered_ms = {
            let decoded_ms = clamp_u128_to_u64_millis(self.playhead.decoded_frontier().as_millis());
            Some(total_ms.map_or(decoded_ms, |total| decoded_ms.min(total)))
        };

        self.emit_audio_event(AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            buffered_ms,
            seek_epoch: self.validator.epoch,
        });
    }

    fn emit_post_seek_output_commit(&mut self, meta: Option<PcmMeta>) {
        let Some(seek_epoch) = self.seek_obs.pending_epoch() else {
            return;
        };
        if seek_epoch != self.validator.epoch {
            return;
        }

        let variant = meta.as_ref().and_then(|m| m.variant_index);
        let segment_index = meta.as_ref().and_then(|m| m.segment_index);

        self.emit_audio_event(AudioEvent::SeekLifecycle {
            seek_epoch,
            stage: SeekLifecycleStage::OutputCommitted,
            location: SegmentLocation::new(variant, segment_index, None, None),
        });

        self.emit_audio_event(AudioEvent::SeekComplete {
            seek_epoch,
            position: (*self).position(),
        });
        let _ = self.seek_obs.clear_pending_epoch(seek_epoch);
    }

    /// Receive next chunk and store it as `current_chunk`.
    ///
    /// Returns `true` if a chunk was received, `false` on EOF or no data.
    pub(crate) fn fill_buffer(&mut self) -> bool {
        let Some(chunk) = self.recv_valid_chunk() else {
            return false;
        };
        self.spec = chunk.spec();
        self.current_chunk = Some(chunk);
        self.current_chunk_consumed_frames = 0;

        if matches!(
            self.consumer_phase,
            ConsumerPhase::Buffering | ConsumerPhase::SeekPending { .. }
        ) {
            self.consumer_phase = ConsumerPhase::Playing;
        }
        true
    }

    /// Whether non-blocking recv is active.
    ///
    /// Returns `false` after `seek()` until `preload()` is called again.
    #[must_use]
    pub fn is_preloaded(&self) -> bool {
        self.preloaded
    }

    /// Get track metadata (title, artist, album, artwork).
    #[must_use]
    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    /// Get current playback position.
    ///
    /// Calculated from samples read since last seek plus the seek base.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.playhead.position()
    }

    /// Enable non-blocking mode for `read()` and prime the first chunk.
    ///
    /// After calling this, `read()` returns immediately from buffered
    /// data without blocking. Must be called after construction so
    /// that `fill_buffer()` calls from JS (via `requestAnimationFrame`)
    /// don't hang.
    ///
    /// Returns `Err(DecodeError)` if the producer channel closed
    /// during the initial `fill_buffer` (e.g. upstream decoder
    /// reported `TrackStep::Failed` before any data). Natural EOF
    /// encountered during preload is **not** surfaced here — the
    /// subsequent `read()` / `next_chunk()` call will report
    /// `ReadOutcome::Eof`.
    ///
    /// # Errors
    /// Returns `DecodeError::Io` if the producer channel closed during preload.
    pub fn preload(&mut self) -> Result<(), DecodeError> {
        self.preloaded = true;
        if self.current_chunk.is_none() && self.consumer_phase != ConsumerPhase::AtEof {
            self.fill_buffer();
            if self.consumer_phase == ConsumerPhase::Failed {
                return Err(DecodeError::Io {
                    source: IoError::other("pcm channel closed during preload"),
                });
            }
        }
        Ok(())
    }

    fn process_fetch(&mut self, fetch: Fetch<PcmChunk>) -> FetchOutcome {
        if !self.validator.is_valid(&fetch) {
            self.discard_chunk(fetch.into_inner());
            return FetchOutcome::Continue;
        }

        match fetch.kind {
            FetchKind::NaturalEof => {
                self.consumer_phase = ConsumerPhase::AtEof;
                self.discard_chunk(fetch.into_inner());
                FetchOutcome::Return(None)
            }
            FetchKind::Failure => {
                self.consumer_phase = ConsumerPhase::Failed;
                self.discard_chunk(fetch.into_inner());
                FetchOutcome::Return(None)
            }
            FetchKind::Data => FetchOutcome::Return(Some(fetch.into_inner())),
        }
    }

    /// Read decoded PCM samples into buffer.
    ///
    /// Samples are interleaved f32 (e.g., LRLRLR for stereo).
    ///
    /// Returns [`ReadOutcome::Frames`] with a non-zero count when the
    /// reader produced data, [`ReadOutcome::Pending`] with a typed
    /// [`PendingReason`] when the reader is alive but produced no
    /// frames this tick (buffering, seek-in-progress), or
    /// [`ReadOutcome::Eof`] on natural end-of-stream. Decoder /
    /// channel failures surface as [`DecodeError`] via the `Err` arm.
    ///
    /// # Errors
    /// Returns `DecodeError::Io` when the producer channel closed /
    /// reported a failure (`ConsumerPhase::Failed`) before any frames
    /// could be flushed.
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara::hang_watchdog]
    pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        if buf.is_empty() {
            return Ok(ReadOutcome::Pending {
                reason: PendingReason::Buffering,
                position: self.position(),
            });
        }
        match self.consumer_phase {
            ConsumerPhase::AtEof if self.current_chunk.is_none() => {
                return Ok(ReadOutcome::Eof {
                    position: self.position(),
                });
            }
            ConsumerPhase::Failed => {
                return Err(DecodeError::Io {
                    source: IoError::other("pcm channel closed / producer failed"),
                });
            }
            _ => {}
        }

        let mut written = 0;
        let mut last_output_meta: Option<PcmMeta> = None;

        while written < buf.len() {
            hang_tick!();

            if let Some(chunk) = self.current_chunk.as_ref() {
                let channels = u64::from(chunk.meta.spec.channels.max(1));
                let chunk_total_frames = u64::from(chunk.meta.frames);
                let consumed_frames_in_chunk = self.current_chunk_consumed_frames;
                if consumed_frames_in_chunk >= chunk_total_frames {
                    self.recycle_current_chunk();
                    if !self.fill_buffer() {
                        break;
                    }
                    continue;
                }
                let remaining_frames = chunk_total_frames - consumed_frames_in_chunk;
                let space_frames = ((buf.len() - written) as u64) / channels.max(1);
                let take_frames = remaining_frames.min(space_frames);
                if take_frames == 0 {
                    break;
                }

                hang_reset!();
                let start_sample = frames_to_samples(consumed_frames_in_chunk, channels)?;
                let take_samples = frames_to_samples(take_frames, channels)?;
                buf[written..written + take_samples]
                    .copy_from_slice(&chunk.samples[start_sample..start_sample + take_samples]);
                last_output_meta = Some(chunk.meta);
                written += take_samples;

                let final_segment = take_frames == remaining_frames;
                let consumed_total = consumed_frames_in_chunk + take_frames;
                self.current_chunk_consumed_frames = consumed_total;

                if final_segment {
                    self.playhead.advance(&ChunkPosition::from(&chunk.meta));
                    self.recycle_current_chunk();
                } else {
                    let total_frames = chunk_total_frames.max(1);
                    let start_ns =
                        u64::try_from(chunk.meta.timestamp.as_nanos()).unwrap_or(u64::MAX);
                    let end_ns =
                        u64::try_from(chunk.meta.end_timestamp.as_nanos()).unwrap_or(u64::MAX);
                    let span_ns = u128::from(end_ns.saturating_sub(start_ns));
                    let consumed_ns_offset =
                        span_ns * u128::from(consumed_total) / u128::from(total_frames);
                    let interpolated = u128::from(start_ns).saturating_add(consumed_ns_offset);
                    let interpolated_ns = u64::try_from(interpolated).unwrap_or(u64::MAX);
                    self.playhead
                        .advance_partial(Duration::from_nanos(interpolated_ns));
                }
            }

            if written >= buf.len() {
                break;
            }

            if !self.fill_buffer() {
                break;
            }
        }

        if let Some(count) = NonZeroUsize::new(written) {
            debug_assert!(
                count.get() <= buf.len(),
                "Audio::read Frames contract violated: count={c} > buf.len()={b}",
                c = count.get(),
                b = buf.len(),
            );
            self.emit_post_seek_output_commit(last_output_meta);
            self.emit_playback_progress();
            let position = self.position();
            debug_assert!(
                self.playhead.duration().is_none_or(|dur| position <= dur),
                "Audio::read Frames contract: position={position:?} > duration={:?}",
                self.playhead.duration(),
            );
            return Ok(ReadOutcome::Frames { count, position });
        }

        let position = self.position();
        match self.consumer_phase {
            ConsumerPhase::AtEof => Ok(ReadOutcome::Eof { position }),
            ConsumerPhase::Failed => Err(DecodeError::Io {
                source: IoError::other("pcm channel closed / producer failed"),
            }),
            ConsumerPhase::SeekPending { .. } => Ok(ReadOutcome::Pending {
                position,
                reason: PendingReason::SeekInProgress,
            }),
            _ => Ok(ReadOutcome::Pending {
                position,
                reason: PendingReason::Buffering,
            }),
        }
    }

    fn recv_outcome(&mut self) -> RecvOutcome {
        if self.use_nonblocking_recv() {
            if let Some(fetch) = self.pcm_rx.try_pop() {
                self.wake_worker();
                return RecvOutcome::Item(fetch);
            }
            return RecvOutcome::Empty;
        }

        self.recv_outcome_blocking()
    }

    #[kithara::flash(true)]
    #[kithara::hang_watchdog(ctx = ConsumerHangCtx)]
    fn recv_outcome_blocking(&mut self) -> RecvOutcome {
        loop {
            if let Some(fetch) = self.pcm_rx.try_pop() {
                hang_reset!();
                self.wake_worker();
                return RecvOutcome::Item(fetch);
            }
            if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            self.wake_worker();
            self.reader_wake.register_current();
            if let Some(fetch) = self.pcm_rx.try_pop() {
                hang_reset!();
                self.wake_worker();
                return RecvOutcome::Item(fetch);
            }
            if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            hang_park!(Self::wait_for_fetch, self.consumer_hang_ctx());
        }
    }

    /// Live consumer-side snapshot for the `recv_outcome_blocking` watchdog.
    /// Built lazily by the real detector only (the closure is dropped uncalled
    /// in release), so the ABR reads here never run in production.
    fn consumer_hang_ctx(&self) -> ConsumerHangCtx {
        let abr = self.abr_handle.as_ref();
        ConsumerHangCtx {
            phase: format!("{:?}", self.consumer_phase),
            variant: abr.and_then(kithara_abr::AbrHandle::current_variant_index),
            abr_escaping: abr.map(kithara_abr::AbrHandle::is_escaping),
            abr_locked: abr.map(kithara_abr::AbrHandle::is_locked),
            abr_pending: abr
                .and_then(kithara_abr::AbrHandle::peek_pending_decision)
                .map(|d| format!("{d:?}")),
            epoch: self.validator.epoch,
            preloaded: self.preloaded,
            block_on_underrun: self.block_on_underrun,
        }
    }

    #[kithara::hang_watchdog]
    fn recv_valid_chunk(&mut self) -> Option<PcmChunk> {
        if self.consumer_phase.is_terminal() {
            return None;
        }

        loop {
            match self.recv_outcome() {
                RecvOutcome::Item(fetch) => match self.process_fetch(fetch) {
                    FetchOutcome::Continue => {
                        hang_tick!();
                        continue;
                    }
                    FetchOutcome::Return(chunk) => {
                        hang_reset!();
                        return chunk;
                    }
                },
                RecvOutcome::Empty => return None,
                RecvOutcome::Closed => {
                    hang_reset!();
                    return self.close_channel_and_mark_eof();
                }
            }
        }
    }

    /// Return the current chunk to the worker for off-thread recycling.
    fn recycle_current_chunk(&mut self) {
        if let Some(chunk) = self.current_chunk.take() {
            self.discard_chunk(chunk);
        }
    }

    fn stage_post_seek_fetch(&mut self, fetch: Fetch<PcmChunk>, epoch: u64) {
        debug_assert_eq!(
            fetch.epoch, epoch,
            "PCM ring preserved a fetch from a future seek epoch"
        );
        // Decoder emits a terminal marker as the last item for its epoch, so
        // staging EOF/failure cannot hide same-epoch PCM behind it.
        match fetch.kind {
            FetchKind::Data => {
                let chunk = fetch.into_inner();
                self.spec = chunk.spec();
                self.current_chunk = Some(chunk);
                self.current_chunk_consumed_frames = 0;
                self.consumer_phase = ConsumerPhase::Playing;
            }
            FetchKind::NaturalEof => {
                self.consumer_phase = ConsumerPhase::AtEof;
                self.discard_chunk(fetch.into_inner());
            }
            FetchKind::Failure => {
                self.consumer_phase = ConsumerPhase::Failed;
                self.discard_chunk(fetch.into_inner());
            }
        }
    }

    /// Seek to position in the audio stream.
    ///
    /// This method never blocks. Seek coordination flows entirely through
    /// the shared seek-state atomics (`FLUSH_START`/`FLUSH_STOP` pattern). The
    /// worker thread reads the seek target and epoch from the seek state and
    /// applies the seek.
    ///
    /// Returns [`SeekOutcome::Landed`] when the reader is now parked
    /// at `position`; [`SeekOutcome::PastEof`] when the target is
    /// beyond a known `duration()` (the subsequent read returns
    /// `ReadOutcome::Eof`).
    ///
    /// # Errors
    /// Propagated from the underlying stream (currently infallible at
    /// this layer — the worker thread surfaces errors lazily via
    /// `FetchKind::Failure`, which becomes `Err` from a subsequent
    /// `read()` / `next_chunk()`).
    #[kithara::hang_watchdog]
    pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let epoch = self.seek.begin(position);
        self.seek.mark_pending(epoch);
        self.emit_audio_event(AudioEvent::SeekLifecycle {
            seek_epoch: epoch,
            stage: SeekLifecycleStage::SeekRequest,
            location: SegmentLocation::default(),
        });
        if let Some(wake) = self.peer_wake.as_ref() {
            wake.notify_now();
        }
        self.preload_gate.rearm();
        self.validator.epoch = epoch;
        self.recycle_current_chunk();
        self.current_chunk_consumed_frames = 0;
        self.consumer_phase = ConsumerPhase::SeekPending { epoch };

        while let Some(fetch) = self.pcm_rx.try_pop() {
            if fetch.epoch < epoch {
                self.discard_chunk(fetch.into_inner());
                hang_tick!();
                continue;
            }

            self.stage_post_seek_fetch(fetch, epoch);
            break;
        }

        if let Some(ref worker) = self.worker {
            worker.wake();
        }

        trace!(?position, epoch, "seek initiated via seek state");
        match self.playhead.duration() {
            Some(duration) if position >= duration => {
                debug_assert!(
                    position >= duration,
                    "Audio::seek PastEof contract: target={position:?} < duration={duration:?}",
                );
                Ok(SeekOutcome::PastEof {
                    duration,
                    target: position,
                })
            }
            _ => {
                debug_assert!(
                    self.playhead.duration().is_none_or(|dur| position <= dur),
                    "Audio::seek Landed contract: landed_at={position:?} > duration={:?}",
                    self.playhead.duration(),
                );
                Ok(SeekOutcome::Landed {
                    target: position,
                    landed_at: position,
                })
            }
        }
    }

    /// Subscribe to audio events.
    ///
    /// Get current audio specification.
    ///
    /// Returns sample rate and channel count for audio output setup.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn use_nonblocking_recv(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            true
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.is_preloaded() && !self.block_on_underrun
        }
    }

    /// Park the reader for at most `timeout`, woken early when the worker
    /// signals `reader_wake`. `timeout` is the watchdog's remaining liveness
    /// budget (see `hang_park!`): a genuine stall releases the park at the
    /// deadline so the watchdog fires; progress unparks it first.
    fn wait_for_fetch(timeout: Duration) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            park_timeout(timeout);
        }

        #[cfg(target_arch = "wasm32")]
        {
            if is_worker_thread() {
                park_timeout(timeout);
            } else {
                thread_sleep(timeout);
            }
        }
    }

    /// Receive next valid chunk from channel, filtering stale chunks.
    ///
    /// After `preload()`, non-blocking. Before `preload()`, blocks on first call.
    /// Returns `None` on EOF or channel close.
    /// Wake the shared worker so it can fill the freed ringbuf slot.
    fn wake_worker(&self) {
        if let Some(ref worker) = self.worker {
            worker.wake();
        }
    }

    /// Pre-warm the shared PCM pool so the decode hot path (`pool.get()`
    /// per packet) and the first `read_planar` calls reuse pre-allocated
    /// buffers instead of paying a cold-start allocation on the audio
    /// thread. Warms only a cold pool (`allocated_bytes == 0`), so the
    /// process-global default singleton is warmed once on the first track
    /// while a freshly-built custom pool still gets warmed when it's first
    /// resolved here.
    fn warm_pcm_pool(pool: &PcmPool, channels: usize, chunks: usize) {
        if pool.allocated_bytes() != 0 {
            return;
        }
        let capacity = Self::WARM_DECODE_FRAMES * channels.max(1);
        let count = chunks.saturating_mul(2).max(1);
        pool.pre_warm(count, |buf| {
            buf.clear();
            buf.resize(capacity, 0.0);
        });
    }
}

/// Specialized impl for Stream-based audio pipelines.
/// Host-threaded decoder construction deps shared by
/// [`Audio::create_initial_decoder`] and [`Audio::create_decoder_factory`]:
/// the decoder `backend` plus the host's `pcm_pool` / `byte_pool`. All
/// required — the host's configured pools must reach the decoder, never a
/// silent process-global fallback.
struct DecoderDeps {
    byte_pool: BytePool,
    backend: kithara_decode::DecoderBackend,
    pcm_pool: PcmPool,
    target_output_rate: Option<Arc<AtomicU32>>,
}

impl DecoderDeps {
    fn new(
        backend: kithara_decode::DecoderBackend,
        pcm_pool: PcmPool,
        byte_pool: BytePool,
        host_sample_rate: &Arc<AtomicU32>,
    ) -> Self {
        Self {
            byte_pool,
            backend,
            pcm_pool,
            target_output_rate: fused_src::decoder_target_output_rate(backend, host_sample_rate),
        }
    }
}

struct StreamSourceRegistration<'a, T: StreamType> {
    bus: &'a EventBus,
    cancel: &'a CancelToken,
    decoder: Box<dyn Decoder>,
    decoder_factory: crate::pipeline::source::DecoderFactory<T>,
    effects: Vec<Box<dyn AudioEffect>>,
    emit: DeferredBus<AudioEvent>,
    engine_load: Option<Arc<EngineLoad>>,
    epoch: Arc<AtomicU64>,
    gapless_mode: GaplessMode,
    host_sample_rate: Arc<AtomicU32>,
    initial_media_info: Option<MediaInfo>,
    pcm_buffer_chunks: usize,
    preload_chunks: NonZeroUsize,
    recreate_on_host_rate_change: bool,
    runtime_handle: RuntimeHandle,
    shared_stream: SharedStream<T>,
    worker: Option<AudioWorkerHandle>,
}

struct RegisteredStreamSource {
    data_rx: crate::runtime::Inlet<Fetch<PcmChunk>>,
    is_standalone_worker: bool,
    preload_gate: Arc<PreloadGate>,
    reader_wake: Arc<ThreadWake>,
    service_class: Arc<AtomicServiceClass>,
    track_id: TrackId,
    trash_tx: crate::runtime::Outlet<PcmChunk>,
    worker: AudioWorkerHandle,
}

fn current_decoder_target_output_rate(rate: Option<&Arc<AtomicU32>>) -> Option<u32> {
    rate.and_then(|rate| NonZeroU32::new(rate.load(Ordering::Acquire)).map(NonZeroU32::get))
}

/// Provides async constructor that creates Stream internally.
/// Uses `StreamAudioSource` for automatic format change detection on ABR switch.
impl<T> Audio<Stream<T>>
where
    T: StreamType<Events = EventBus>,
{
    /// Create audio pipeline from `AudioConfig`.
    ///
    /// This is the target API for Stream sources.
    /// Uses `StreamAudioSource` for automatic decoder recreation on format change.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if the stream cannot be created, the initial probe
    /// fails, or the decoder cannot be initialized.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = AudioConfig::<Hls>::new(hls_config);
    /// let audio = Audio::new(config).await?;
    /// sink.append(audio);
    /// ```
    pub async fn new(config: AudioConfig<T>) -> Result<Self, DecodeError> {
        let AudioConfig {
            byte_pool,
            hint,
            host_sample_rate: config_host_sr,
            media_info: user_media_info,
            pcm_buffer_chunks,
            pcm_pool: mut pool,
            playback_rate: config_playback_rate,
            stretch: config_stretch,
            engine_load: config_engine_load,
            decoder_backend,
            preload_chunks,
            block_on_underrun,
            resampler_quality,
            stream: stream_config,
            bus: config_bus,
            effects: custom_effects,
            worker: config_worker,
            gapless_mode: config_gapless_mode,
            cancel: config_cancel,
        } = config;
        let cancel = CancelScope::new(config_cancel).token();
        let runtime_handle = RuntimeHandle::try_current().map_err(|e| DecodeError::Io {
            source: IoError::other(format!(
                "audio stream construction requires a tokio runtime: {e}"
            )),
        })?;

        let bus = Self::resolve_event_bus(&stream_config, config_bus);
        let byte_pool = byte_pool.unwrap_or_else(|| BytePool::default().clone());
        let stream = Self::create_stream_with_probe(stream_config, byte_pool.clone()).await?;

        let initial_byte_len = stream.len().unwrap_or(0);
        let playhead = stream.playhead_write();
        let seek = stream.seek_control();
        let seek_obs = stream.seek_observe();
        let initial_media_info =
            merge_user_and_stream_media_info(user_media_info, stream.media_info());
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        let shared_stream = SharedStream::new(stream);
        let byte_len_handle = Arc::new(AtomicU64::new(initial_byte_len));
        let host_sample_rate = Arc::new(AtomicU32::new(config_host_sr.map_or(0, NonZeroU32::get)));

        let pool = pool.get_or_insert_with(|| PcmPool::default().clone());
        let warm_channels = warm_channels_from_media_info(initial_media_info.as_ref());
        Self::warm_pcm_pool(pool, warm_channels, pcm_buffer_chunks);
        // The single up-front build reads through the blocking off-RT
        // `Stream::read` adapter (waits for residual init lateness, cancel-
        // bounded), then we disarm before the RT worker is registered so the
        // decode loop the worker drives reads non-blocking via `probe_read`.
        shared_stream.set_blocking(true);
        let deps = DecoderDeps::new(
            decoder_backend,
            pool.clone(),
            byte_pool.clone(),
            &host_sample_rate,
        );
        let decoder = Self::create_initial_decoder(
            shared_stream.clone(),
            initial_media_info.clone(),
            hint.clone(),
            &deps,
        )
        .await;
        shared_stream.set_blocking(false);
        let decoder = decoder?;

        let initial_spec = decoder.spec();
        let total_duration = decoder.duration().or_else(|| playhead.duration());
        playhead.set_duration(total_duration);
        let metadata = decoder.metadata();

        let epoch = Arc::new(AtomicU64::new(0));
        let playback_rate = config_playback_rate.unwrap_or_else(|| Arc::new(AtomicF32::new(1.0)));

        let output_spec = expected_output_spec(initial_spec, &host_sample_rate);
        let effects = create_effects(
            initial_spec,
            &host_sample_rate,
            config_stretch.as_ref(),
            fused_src::resampler_stage(decoder_backend, resampler_quality),
            Some(pool.clone()),
            custom_effects,
        );

        Self::log_pipeline_ready(initial_spec, output_spec, &host_sample_rate);

        let interleaved = Self::alloc_interleaved_scratch(pool, output_spec);

        let abr_handle = shared_stream.abr_handle();
        let peer_wake = shared_stream.peer_wake();
        let RegisteredStreamSource {
            data_rx,
            is_standalone_worker,
            preload_gate,
            reader_wake,
            service_class,
            track_id,
            trash_tx,
            worker,
        } = Self::register_stream_audio_source(StreamSourceRegistration {
            bus: &bus,
            cancel: &cancel,
            decoder,
            decoder_factory: Self::create_decoder_factory(&deps, &epoch, &byte_len_handle),
            effects,
            emit: Self::create_emit(&bus),
            engine_load: config_engine_load,
            epoch: Arc::clone(&epoch),
            gapless_mode: config_gapless_mode,
            host_sample_rate: Arc::clone(&host_sample_rate),
            initial_media_info,
            pcm_buffer_chunks,
            preload_chunks,
            recreate_on_host_rate_change: deps.target_output_rate.is_some(),
            runtime_handle,
            shared_stream,
            worker: config_worker,
        });

        Ok(Self {
            playhead,
            seek,
            seek_obs,
            metadata,
            bus,
            host_sample_rate,
            playback_rate,
            preload_gate,
            reader_wake,
            abr_handle,
            peer_wake,
            trash_tx,
            service_class,
            block_on_underrun,
            stretch: config_stretch,
            pcm_rx: data_rx,
            _epoch: epoch,
            validator: EpochValidator::default(),
            spec: output_spec,
            current_chunk: None,
            current_chunk_consumed_frames: 0,
            consumer_phase: ConsumerPhase::Buffering,
            cancel: Some(cancel),
            interleaved: Some(interleaved),
            pcm_pool: pool.clone(),
            preloaded: false,
            last_progress_emit: None,
            track_id: Some(track_id),
            worker: Some(worker),
            is_standalone_worker,
            _marker: PhantomData,
        })
    }

    fn register_stream_audio_source(
        registration: StreamSourceRegistration<'_, T>,
    ) -> RegisteredStreamSource {
        let StreamSourceRegistration {
            bus,
            cancel,
            decoder,
            decoder_factory,
            effects,
            emit,
            engine_load,
            epoch,
            gapless_mode,
            host_sample_rate,
            initial_media_info,
            pcm_buffer_chunks,
            preload_chunks,
            recreate_on_host_rate_change,
            runtime_handle,
            shared_stream,
            worker,
        } = registration;
        let initial_variant = initial_media_info.as_ref().and_then(|i| i.variant_index);
        // Retain a handle to inject the worker wake after the worker exists —
        // `shared_stream` itself is moved into the source below. `SharedStream`
        // is `Arc`-backed, so the clone shares the same inner stream/coord.
        let wake_stream = shared_stream.clone();
        let preload_gate = Arc::new(PreloadGate::default());
        let reader_wake = Arc::new(ThreadWake::default());
        let (data_tx, data_rx) = Self::create_channels(pcm_buffer_chunks, bus, &reader_wake);
        let (trash_tx, trash_inlet) = Self::create_trash_channel(pcm_buffer_chunks);
        let (worker, is_standalone_worker) = worker.map_or_else(
            || (AudioWorkerHandle::with_cancel(cancel.child()), true),
            |w| (w, false),
        );
        let worker_wake: Arc<dyn WorkerWake> = Arc::new(WorkerWakeBridge(worker.clone()));
        let audio_source = StreamAudioSource::new(
            shared_stream,
            DecodeInit {
                decoder,
                decoder_factory,
                gapless_mode,
                host_sample_rate,
                media_info: initial_media_info,
                recreate_on_host_rate_change,
            },
            epoch,
            effects,
            runtime_handle,
            Arc::clone(&worker_wake),
        )
        .with_emit(emit);

        bus.publish(AudioEvent::DecoderReady {
            base_offset: 0,
            variant: initial_variant,
        });

        let service_class = Arc::new(AtomicServiceClass::new(ServiceClass::default()));
        let track_id = worker.register_track(TrackRegistration {
            trash_inlet,
            source: Box::new(audio_source),
            outlet: data_tx,
            preload_gate: preload_gate.clone(),
            preload_chunks: preload_chunks.get(),
            service_class: Arc::clone(&service_class),
            engine_load,
        });

        // Now that the worker exists, wire its data-arrival wake into the
        // source: a segmented (HLS) source re-ticks the worker the instant
        // segment bytes are written/committed, off the 10 ms scheduler poll.
        // No-op for non-segmented sources (the default `set_worker_wake`).
        wake_stream.set_worker_wake(worker_wake);

        RegisteredStreamSource {
            data_rx,
            is_standalone_worker,
            preload_gate,
            reader_wake,
            service_class,
            track_id,
            trash_tx,
            worker,
        }
    }

    fn create_decoder_factory(
        deps: &DecoderDeps,
        epoch: &Arc<AtomicU64>,
        byte_len_handle: &Arc<AtomicU64>,
    ) -> crate::pipeline::source::DecoderFactory<T> {
        let decoder_backend = deps.backend;
        let factory_epoch = Arc::clone(epoch);
        let factory_byte_len = Arc::clone(byte_len_handle);
        let factory_pool = deps.pcm_pool.clone();
        let factory_byte_pool = deps.byte_pool.clone();
        let factory_target_output_rate = deps.target_output_rate.clone();
        Arc::new(move |stream, info, base_offset| {
            let byte_len = stream
                .len()
                .map_or(0, |len| len.saturating_sub(base_offset));
            factory_byte_len.store(byte_len, Ordering::Release);
            let target_output_rate =
                current_decoder_target_output_rate(factory_target_output_rate.as_ref());
            let config = DecoderConfig::builder()
                .backend(decoder_backend)
                .byte_len_handle(Arc::clone(&factory_byte_len))
                .pcm_pool(factory_pool.clone())
                .byte_pool(factory_byte_pool.clone())
                .epoch(factory_epoch.load(Ordering::Acquire))
                .maybe_byte_map(stream.byte_map())
                .maybe_hooks(stream.take_reader_event_sink())
                .maybe_target_output_rate(target_output_rate)
                .build();
            let source = OffsetReader::new(stream.clone(), base_offset);
            match DecoderFactory::create_from_media_info(source, &info, config) {
                Ok(d) => {
                    d.update_byte_len(byte_len);
                    Ok(d)
                }
                Err(e) => {
                    warn!(?e, "failed to recreate decoder");
                    Err(e)
                }
            }
        })
    }

    /// Build the initial decoder EXACTLY ONCE on a `spawn_blocking` thread
    /// (off the real-time produce core). The construction read is a single
    /// plain read through the source's blocking off-RT `Read` adapter
    /// (`SharedStream` in blocking mode → [`Stream::read`]): the active
    /// variant's init body has already been prefetched-and-committed by
    /// `Hls::create`, so the build reads committed bytes; the blocking adapter
    /// is the cancel-bounded safety net for residual lateness. There is NO
    /// retry loop and NO readiness gate — a genuine terminal comes from the
    /// stream layer (`Stream::read` → source `io::Error` / typed
    /// `StreamPending`), never minted here. A `VariantChange`/`SeekPending` at
    /// construction is a stream-state bug (the variant is settled before this
    /// build; construction never calls `clear_variant_fence`), not a rebuild
    /// trigger; a concurrent user seek is applied by the post-construction
    /// seek path. See the crate `CONTEXT.md` "Construction reads".
    async fn create_initial_decoder(
        shared_stream: SharedStream<T>,
        initial_media_info: Option<MediaInfo>,
        hint: Option<String>,
        deps: &DecoderDeps,
    ) -> Result<Box<dyn Decoder>, DecodeError> {
        debug!("Audio::new — spawning decoder creation...");
        let byte_len_handle = Arc::new(AtomicU64::new(shared_stream.len().unwrap_or(0)));
        let decoder_config = DecoderConfig::builder()
            .backend(deps.backend)
            .byte_len_handle(byte_len_handle)
            .pcm_pool(deps.pcm_pool.clone())
            .byte_pool(deps.byte_pool.clone())
            .maybe_byte_map(shared_stream.byte_map())
            .maybe_hooks(shared_stream.take_reader_event_sink())
            .maybe_hint(hint.clone())
            .maybe_target_output_rate(current_decoder_target_output_rate(
                deps.target_output_rate.as_ref(),
            ))
            .build();
        let hint_for_decoder = hint;
        let initial_media_info_for_decoder = initial_media_info;
        let decoder = spawn_blocking(move || {
            if let Some(ref info) = initial_media_info_for_decoder {
                DecoderFactory::create_from_media_info(shared_stream, info, decoder_config)
            } else {
                DecoderFactory::create_with_probe(
                    shared_stream,
                    hint_for_decoder.as_deref(),
                    decoder_config,
                )
            }
        })
        .await
        .map_err(|e| DecodeError::Io {
            source: IoError::other(format!("decoder task panicked: {e}")),
        })?;
        if decoder.is_ok() {
            debug!("Audio::new — decoder created");
        }
        decoder
    }

    async fn create_stream_with_probe(
        stream_config: T::Config,
        byte_pool: BytePool,
    ) -> Result<Stream<T>, DecodeError> {
        let stream = Self::open_stream(stream_config).await?;
        Self::spawn_probe(stream, byte_pool).await
    }

    /// Build the spent-chunk return ring. Capacity covers every chunk the
    /// consumer can hold at once — the whole forward ring plus the current
    /// chunk — so a seek that drains the forward ring back into here never
    /// overflows and the real-time push stays infallible. No wake handle:
    /// the worker is already woken on every `recv_outcome`, and the drain is
    /// not latency-sensitive.
    fn create_trash_channel(
        pcm_buffer_chunks: usize,
    ) -> (
        crate::runtime::Outlet<PcmChunk>,
        crate::runtime::Inlet<PcmChunk>,
    ) {
        crate::runtime::connect::<PcmChunk>(pcm_buffer_chunks.max(1) + 2, None)
    }

    /// Get a reference to the underlying `EventBus`.
    ///
    /// Useful for passing to downstream components that also publish events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    /// Subscribe to unified events via the `EventBus`.
    ///
    /// Returns a receiver for all events published to the bus.
    #[must_use]
    pub fn events(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }

    fn log_pipeline_ready(
        initial_spec: PcmSpec,
        output_spec: PcmSpec,
        host_sample_rate: &Arc<AtomicU32>,
    ) {
        info!(
            ?initial_spec,
            ?output_spec,
            host_sr = host_sample_rate.load(Ordering::Relaxed),
            "Audio pipeline created"
        );
    }

    async fn open_stream(stream_config: T::Config) -> Result<Stream<T>, DecodeError> {
        debug!("Audio::new — creating Stream...");
        let stream = Stream::<T>::new(stream_config)
            .await
            .map_err(|e| DecodeError::Io {
                source: IoError::other(e.to_string()),
            })?;
        debug!("Audio::new — Stream created");
        Ok(stream)
    }

    fn probe_stream_blocking(
        mut stream: Stream<T>,
        _byte_pool: &BytePool,
    ) -> Result<Stream<T>, DecodeError> {
        // No up-front warm read here: the single decoder build reads through
        // the blocking off-RT `Stream::read` adapter (`SharedStream` in
        // blocking mode), which waits for a slow prefix and surfaces a genuine
        // source error (e.g. 503) directly. Reset the cursor to 0 so the build
        // probes the container header from the start.
        stream
            .seek(SeekFrom::Start(0))
            .map_err(|source| DecodeError::Io { source })?;
        Ok(stream)
    }

    fn resolve_event_bus(stream_config: &T::Config, config_bus: Option<EventBus>) -> EventBus {
        T::event_bus(stream_config)
            .or(config_bus)
            .unwrap_or_default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn spawn_probe(stream: Stream<T>, byte_pool: BytePool) -> Result<Stream<T>, DecodeError> {
        debug!("Audio::new — spawning probe task...");
        let result = spawn_blocking(move || Self::probe_stream_blocking(stream, &byte_pool))
            .await
            .map_err(|e| DecodeError::Io {
                source: IoError::other(format!("probe task panicked: {e}")),
            })??;
        debug!("Audio::new — probe task done");
        Ok(result)
    }

    /// Wasm probe path: the browser tokio runtime is single-threaded
    /// and `spawn_blocking` requires `Send` — but `Stream<T>` is
    /// `!Send` because it holds JS-backed network streams. Probe runs
    /// inline on the calling task.
    #[cfg(target_arch = "wasm32")]
    async fn spawn_probe(stream: Stream<T>, byte_pool: BytePool) -> Result<Stream<T>, DecodeError> {
        debug!("Audio::new — running probe inline (wasm)...");
        let result = Self::probe_stream_blocking(stream, &byte_pool)?;
        debug!("Audio::new — probe done");
        Ok(result)
    }
}

/// Merge user-supplied `MediaInfo` over the stream's declarative info.
///
/// Keeps user's specific fields and fills `None` fields from the stream.
/// The result is the single source of truth for what kind of decoder is
/// being run: the initial-decoder factory probes with it AND the FSM's
/// `session.media_info` is seeded with it. Without the merge, user's
/// container override (e.g. Wav) would be silently dropped at session
/// seeding, and `detect_format_change` would later treat the stream's
/// declarative container (e.g. Fmp4 inferred from EXT-X-MAP URL
/// extension) as authoritative.
fn merge_user_and_stream_media_info(
    user: Option<MediaInfo>,
    stream: Option<MediaInfo>,
) -> Option<MediaInfo> {
    match (user, stream) {
        (Some(user), Some(stream)) => {
            let mut merged = user;
            if merged.codec.is_none() {
                merged.codec = stream.codec;
            }
            if merged.container.is_none() {
                merged.container = stream.container;
            }
            if merged.channels.is_none() {
                merged.channels = stream.channels;
            }
            if merged.sample_rate.is_none() {
                merged.sample_rate = stream.sample_rate;
            }
            if merged.variant_index.is_none() {
                merged.variant_index = stream.variant_index;
            }
            Some(merged)
        }
        (Some(user), None) => Some(user),
        (None, stream) => stream,
    }
}

impl<S> Drop for Audio<S> {
    fn drop(&mut self) {
        if let Some(ref cancel) = self.cancel {
            cancel.cancel();
        }

        if let (Some(worker), Some(track_id)) = (&self.worker, self.track_id.take()) {
            worker.unregister_track(track_id);

            if self.is_standalone_worker {
                worker.shutdown();
            }
        }
    }
}

impl<S: kithara_platform::maybe_send::MaybeSend> PcmReader for Audio<S> {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.abr_handle.clone()
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        Self::metadata(self)
    }

    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.preloaded = true;
        let Some(chunk) = self
            .current_chunk
            .take()
            .or_else(|| self.recv_valid_chunk())
        else {
            let position = self.position();
            return match self.consumer_phase {
                ConsumerPhase::AtEof => Ok(ChunkOutcome::Eof { position }),
                ConsumerPhase::Failed => Err(DecodeError::Io {
                    source: IoError::other("pcm channel closed / producer failed"),
                }),
                ConsumerPhase::SeekPending { .. } => Ok(ChunkOutcome::Pending {
                    position,
                    reason: PendingReason::SeekInProgress,
                }),
                _ => Ok(ChunkOutcome::Pending {
                    position,
                    reason: PendingReason::Buffering,
                }),
            };
        };
        self.spec = chunk.spec();

        if matches!(
            self.consumer_phase,
            ConsumerPhase::Buffering | ConsumerPhase::SeekPending { .. }
        ) {
            self.consumer_phase = ConsumerPhase::Playing;
        }

        self.playhead.advance(&ChunkPosition::from(&chunk.meta));
        Ok(ChunkOutcome::Chunk(chunk))
    }

    fn preload(&mut self) -> Result<(), DecodeError> {
        Self::preload(self)
    }

    fn preload_epoch(&self) -> u64 {
        self.seek_obs.epoch()
    }

    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        Some(self.preload_gate.clone())
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Self::read(self, buf)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let channels = output.len();
        if channels == 0 {
            return Ok(ReadOutcome::Pending {
                reason: PendingReason::Buffering,
                position: self.position(),
            });
        }
        let frames = output[0].len();
        let total_samples = frames * channels;

        // NOTE: detach the pre-sized scratch for `&mut self` `read`, restored before return.
        let mut interleaved = self
            .interleaved
            .take()
            .unwrap_or_else(|| self.pcm_pool.get());
        interleaved.clear();
        interleaved.resize(total_samples, 0.0);
        debug_assert!(
            interleaved.capacity() >= total_samples,
            "Audio::read_planar scratch undersized: capacity={} < total_samples={total_samples}",
            interleaved.capacity(),
        );

        let result = match self.read(&mut interleaved[..]) {
            Ok(ReadOutcome::Eof { position }) => Ok(ReadOutcome::Eof { position }),
            Ok(ReadOutcome::Pending { reason, position }) => {
                Ok(ReadOutcome::Pending { reason, position })
            }
            Ok(ReadOutcome::Frames { count, position }) => {
                let actual_frames = count.get() / channels;
                debug_assert!(
                    actual_frames <= frames,
                    "Audio::read_planar Frames contract: actual_frames={actual_frames} \
                     > per-channel buf frames={frames}",
                );
                let num_channels =
                    NonZeroUsize::new(channels).expect("channels checked non-zero above");
                deinterleave_variable(&interleaved[..], num_channels, output, 0..actual_frames);
                NonZeroUsize::new(actual_frames).map_or(
                    Ok(ReadOutcome::Pending {
                        position,
                        reason: PendingReason::Buffering,
                    }),
                    |actual| {
                        Ok(ReadOutcome::Frames {
                            position,
                            count: actual,
                        })
                    },
                )
            }
            Err(err) => Err(err),
        };

        self.interleaved = Some(interleaved);
        result
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Self::seek(self, position)
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        let previous = self
            .host_sample_rate
            .swap(sample_rate.get(), Ordering::AcqRel);
        if previous != sample_rate.get() {
            self.wake_worker();
        }
    }

    fn set_playback_rate(&self, rate: f32) {
        match &self.stretch {
            Some(controls) => controls.set_speed(rate),
            None => self.playback_rate.store(rate, Ordering::Relaxed),
        }
    }

    fn set_service_class(&self, class: ServiceClass) {
        self.service_class.store(class);
        if let Some(worker) = &self.worker {
            worker.wake();
        }
    }
    fn spec(&self) -> PcmSpec {
        Self::spec(self)
    }

    delegate! {
        to self.playhead {
            fn duration(&self) -> Option<Duration>;
        }
    }

    delegate! {
        to self.playhead {
            fn position(&self) -> Duration;
        }
    }

    delegate! {
        to self.playhead {
            fn decoded_frontier(&self) -> Duration;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        marker::PhantomData,
        sync::{
            Arc,
            atomic::{AtomicU32, AtomicU64},
        },
    };

    use kithara_stream::{PlayheadState, SeekState};
    use kithara_test_utils::kithara;

    use super::*;

    fn empty_audio() -> Audio<()> {
        let (_data_tx, pcm_rx) = crate::runtime::connect::<Fetch<PcmChunk>>(1, None);
        let (trash_tx, _trash_rx) = crate::runtime::connect::<PcmChunk>(8, None);
        let playhead = Arc::new(PlayheadState::new());
        let seek = Arc::new(SeekState::new());

        Audio {
            pcm_rx,
            trash_tx,
            _epoch: Arc::new(AtomicU64::new(0)),
            validator: EpochValidator::default(),
            spec: PcmMeta::default().spec,
            stretch: None,
            current_chunk: None,
            current_chunk_consumed_frames: 0,
            consumer_phase: ConsumerPhase::Buffering,
            playhead: Arc::clone(&playhead) as Arc<dyn PlayheadWrite>,
            seek: Arc::clone(&seek) as Arc<dyn SeekControl>,
            seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
            metadata: TrackMetadata::default(),
            bus: EventBus::default(),
            cancel: None,
            interleaved: None,
            pcm_pool: PcmPool::default().clone(),
            host_sample_rate: Arc::new(AtomicU32::new(0)),
            playback_rate: Arc::new(AtomicF32::new(1.0)),
            preload_gate: Arc::new(PreloadGate::default()),
            preloaded: false,
            block_on_underrun: false,
            last_progress_emit: None,
            track_id: None,
            worker: None,
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            reader_wake: Arc::new(ThreadWake::default()),
            is_standalone_worker: false,
            abr_handle: None,
            peer_wake: None,
            _marker: PhantomData,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test(env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
    #[should_panic(expected = "recv_outcome_blocking")]
    fn blocking_recv_without_preload_panics_when_no_chunk_arrives() {
        let mut audio = empty_audio();
        let _ = audio.recv_valid_chunk();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[kithara::test]
    fn blocking_recv_returns_closed_after_cancel() {
        let mut audio = empty_audio();
        let cancel = CancelToken::never();
        cancel.cancel();
        audio.cancel = Some(cancel);

        assert!(matches!(audio.recv_outcome(), RecvOutcome::Closed));
    }

    #[kithara::test]
    fn preloaded_recv_is_nonblocking() {
        let mut audio = empty_audio();
        audio.preload().expect("preload");

        assert!(matches!(audio.recv_outcome(), RecvOutcome::Empty));
    }

    #[kithara::test]
    fn output_available_event_fires_on_empty_to_nonempty_ring_transition() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let reader_wake = Arc::new(ThreadWake::default());
        let (mut tx, mut rx) = Audio::<()>::create_channels(2, &bus, &reader_wake);

        tx.try_push(Fetch::new(PcmChunk::default(), false, 0))
            .expect("first push reaches ring");
        assert!(
            events.try_recv().is_err(),
            "ring wake event is deferred until the scheduler shell flush"
        );
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv(),
            Ok(kithara_events::Event::Audio(AudioEvent::OutputAvailable))
        ));

        tx.try_push(Fetch::new(PcmChunk::default(), false, 0))
            .expect("second push reaches ring");
        tx.flush_wake_signals();
        assert!(
            events.try_recv().is_err(),
            "no duplicate wake while the ring was already non-empty"
        );

        assert!(rx.try_pop().is_some());
        assert!(rx.try_pop().is_some());

        tx.try_push(Fetch::new(PcmChunk::default(), false, 0))
            .expect("third push reaches empty ring");
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv(),
            Ok(kithara_events::Event::Audio(AudioEvent::OutputAvailable))
        ));
    }

    #[kithara::test]
    fn seek_rearms_preload_gate_before_worker_refill() {
        let mut audio = empty_audio();
        audio.preload_gate.signal_epoch(0);
        assert!(audio.preload_gate.is_ready());

        audio
            .seek(Duration::from_millis(250))
            .expect("seek should arm epoch");

        assert!(!audio.preload_gate.is_ready());
    }

    fn audio_with_channel() -> (Audio<()>, crate::runtime::Outlet<Fetch<PcmChunk>>) {
        let (data_tx, pcm_rx) = crate::runtime::connect::<Fetch<PcmChunk>>(4, None);
        let (trash_tx, _trash_rx) = crate::runtime::connect::<PcmChunk>(8, None);
        let playhead = Arc::new(PlayheadState::new());
        let seek = Arc::new(SeekState::new());

        let audio = Audio {
            pcm_rx,
            trash_tx,
            _epoch: Arc::new(AtomicU64::new(0)),
            validator: EpochValidator::default(),
            spec: PcmMeta::default().spec,
            stretch: None,
            current_chunk: None,
            current_chunk_consumed_frames: 0,
            consumer_phase: ConsumerPhase::Buffering,
            playhead: Arc::clone(&playhead) as Arc<dyn PlayheadWrite>,
            seek: Arc::clone(&seek) as Arc<dyn SeekControl>,
            seek_obs: Arc::clone(&seek) as Arc<dyn SeekObserve>,
            metadata: TrackMetadata::default(),
            bus: EventBus::default(),
            cancel: None,
            interleaved: None,
            pcm_pool: PcmPool::default().clone(),
            host_sample_rate: Arc::new(AtomicU32::new(0)),
            playback_rate: Arc::new(AtomicF32::new(1.0)),
            preload_gate: Arc::new(PreloadGate::default()),
            preloaded: true,
            block_on_underrun: false,
            last_progress_emit: None,
            track_id: None,
            worker: None,
            service_class: Arc::new(AtomicServiceClass::new(ServiceClass::default())),
            reader_wake: Arc::new(ThreadWake::default()),
            is_standalone_worker: false,
            abr_handle: None,
            peer_wake: None,
            _marker: PhantomData,
        };
        (audio, data_tx)
    }

    fn make_chunk(samples: &[f32]) -> PcmChunk {
        let mut chunk = PcmChunk::default();
        chunk.samples.clear();
        chunk.samples.extend_from_slice(samples);
        chunk.meta.spec.channels = 1;
        chunk.meta.frames = u32::try_from(samples.len()).unwrap_or(u32::MAX);
        chunk
    }

    fn make_timed_chunk(spec: PcmSpec, frames: u32, start: Duration, end: Duration) -> PcmChunk {
        let channels = usize::from(spec.channels.max(1));
        let frame_count = usize::try_from(frames).expect("test frame count fits usize");
        let samples = vec![0.5; frame_count * channels];
        PcmChunk::new(
            PcmMeta {
                spec,
                timestamp: start,
                end_timestamp: end,
                frames,
                ..Default::default()
            },
            PcmPool::default().attach(samples),
        )
    }

    #[kithara::test]
    fn consumer_phase_starts_buffering() {
        let audio = empty_audio();
        assert_eq!(audio.consumer_phase, ConsumerPhase::Buffering);
    }

    #[kithara::test]
    fn consumer_phase_transitions_to_playing_on_first_chunk() {
        let (mut audio, mut tx) = audio_with_channel();
        let chunk = make_chunk(&[0.1, 0.2]);
        let fetch = Fetch::new(chunk, false, 0);
        tx.try_push(fetch).ok();

        assert!(audio.fill_buffer());
        assert_eq!(audio.consumer_phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn partial_resampled_chunk_position_caps_at_duration() {
        let (mut audio, mut tx) = audio_with_channel();
        let spec = PcmSpec::new(2, NonZeroU32::new(48_000).expect("test rate"));
        let duration = Duration::from_nanos(36_360_000_000);
        let chunk = make_timed_chunk(
            spec,
            148,
            duration.saturating_sub(Duration::from_millis(2)),
            duration.saturating_add(Duration::from_millis(2)),
        );

        audio.playhead.set_duration(Some(duration));
        tx.try_push(Fetch::new(chunk, false, 0))
            .expect("chunk reaches test ring");

        let mut buf = vec![0.0f32; 200];
        let outcome = audio.read(&mut buf).expect("partial read succeeds");
        let ReadOutcome::Frames { count, position } = outcome else {
            panic!("expected frames from partial resampled chunk");
        };

        assert_eq!(count.get(), 200);
        assert_eq!(position, duration);
        assert_eq!(audio.current_chunk_consumed_frames, 100);
    }

    #[kithara::test]
    fn consumer_phase_transitions_to_seek_pending() {
        let (mut audio, _tx) = audio_with_channel();
        audio.seek(Duration::from_secs(5)).ok();
        assert!(matches!(
            audio.consumer_phase,
            ConsumerPhase::SeekPending { .. }
        ));
    }

    #[kithara::test]
    fn consumer_phase_seek_pending_to_playing_on_chunk() {
        let (mut audio, mut tx) = audio_with_channel();

        audio.seek(Duration::from_secs(5)).ok();
        let epoch = audio.validator.epoch;

        let chunk = make_chunk(&[0.1, 0.2]);
        let fetch = Fetch::new(chunk, false, epoch);
        tx.try_push(fetch).ok();

        assert!(audio.fill_buffer());
        assert_eq!(audio.consumer_phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_chunk_after_stale_chunks() {
        let (mut audio, mut tx) = audio_with_channel();

        let stale = Fetch::new(make_chunk(&[0.1, 0.2]), false, 0);
        let fresh = Fetch::new(make_chunk(&[0.7, 0.8]), false, 1);
        assert!(tx.try_push(stale).is_ok());
        assert!(tx.try_push(fresh).is_ok());

        audio.seek(Duration::from_secs(5)).ok();

        let mut buf = [0.0; 2];
        let count = match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            other => panic!("expected preserved post-seek frames, got {other:?}"),
        };
        assert_eq!(count, 2);
        assert_eq!(buf, [0.7, 0.8]);
    }

    #[kithara::test]
    fn seek_drain_preserves_new_epoch_eof_after_stale_chunks() {
        let (mut audio, mut tx) = audio_with_channel();

        let stale = Fetch::new(make_chunk(&[0.1, 0.2]), false, 0);
        let eof = Fetch::new(PcmChunk::default(), true, 1);
        assert!(tx.try_push(stale).is_ok());
        assert!(tx.try_push(eof).is_ok());

        audio.seek(Duration::from_secs(5)).ok();

        let mut buf = [0.0; 2];
        assert!(matches!(audio.read(&mut buf), Ok(ReadOutcome::Eof { .. })));
        assert_eq!(audio.consumer_phase, ConsumerPhase::AtEof);
    }

    #[kithara::test]
    fn consumer_phase_eof_terminates() {
        let (mut audio, mut tx) = audio_with_channel();

        let fetch = Fetch::new(PcmChunk::default(), true, 0);
        tx.try_push(fetch).ok();

        let result = audio.recv_valid_chunk();
        assert!(result.is_none());
        assert_eq!(audio.consumer_phase, ConsumerPhase::AtEof);
        let mut buf = [0.0f32; 16];
        assert!(matches!(audio.read(&mut buf), Ok(ReadOutcome::Eof { .. })));
    }

    #[kithara::test]
    fn consumer_phase_failed_on_channel_close() {
        let (mut audio, _tx) = audio_with_channel();
        let cancel = CancelToken::never();
        cancel.cancel();
        audio.cancel = Some(cancel);
        audio.preloaded = false;

        let result = audio.recv_valid_chunk();
        assert!(result.is_none());
        assert_eq!(audio.consumer_phase, ConsumerPhase::Failed);
        let mut buf = [0.0f32; 16];
        assert!(matches!(
            audio.read(&mut buf),
            Err(DecodeError::Io { source: _ })
        ));
    }

    #[kithara::test]
    fn consumer_does_not_park_in_terminal_phase() {
        let (mut audio, _tx) = audio_with_channel();
        audio.consumer_phase = ConsumerPhase::AtEof;

        let mut buf = [0.0f32; 16];
        assert!(matches!(audio.read(&mut buf), Ok(ReadOutcome::Eof { .. })));
    }

    #[kithara::test]
    fn process_fetch_must_distinguish_failure_from_natural_eof() {
        let (mut audio_eof, mut tx_eof) = audio_with_channel();
        tx_eof
            .try_push(Fetch::new(PcmChunk::default(), true, 0))
            .expect("push natural-eof marker");
        let _ = audio_eof.recv_valid_chunk();
        assert_eq!(audio_eof.consumer_phase, ConsumerPhase::AtEof);

        let (mut audio_failure, mut tx_failure) = audio_with_channel();
        tx_failure
            .try_push(Fetch::failure(PcmChunk::default(), 0))
            .expect("push failure marker");
        let _ = audio_failure.recv_valid_chunk();

        assert_ne!(
            audio_failure.consumer_phase,
            ConsumerPhase::AtEof,
            "process_fetch must not collapse FetchKind::Failure into \
             ConsumerPhase::AtEof — AtEof means 'clip finished' and is \
             used by PlayerTrack to finalize; a transient failure must \
             land in a distinct non-natural-eof state so the pipeline \
             can recover instead of removing the track from the arena"
        );
        assert_eq!(
            audio_failure.consumer_phase,
            ConsumerPhase::Failed,
            "failure marker must route to ConsumerPhase::Failed"
        );
    }
}
