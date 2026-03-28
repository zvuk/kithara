//! Audio pipeline struct and public API.

use std::{
    io::{Error as IoError, Read, Seek, SeekFrom},
    marker::PhantomData,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_decode::{DecoderFactory, PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
use kithara_events::{AudioEvent, EventBus, SeekLifecycleStage};
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread::{is_worker_thread, sleep as thread_sleep};
use kithara_platform::{
    thread::park_timeout,
    tokio::{sync::Notify, task::spawn_blocking},
};
use kithara_stream::{
    EpochValidator, Fetch, MediaInfo, Stream, StreamContext, StreamType, Timeline,
};
use portable_atomic::AtomicF32;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Split},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use crate::{
    pipeline::{
        config::{AudioConfig, create_effects, expected_output_spec},
        source::{OffsetReader, SharedStream, StreamAudioSource},
        track_fsm::ConsumerPhase,
    },
    traits::{DecodeError, DecodeResult, PcmReader},
    worker::{
        AudioCommand,
        handle::{AudioWorkerHandle, TrackRegistration},
        thread_wake::ThreadWake,
        types::{ServiceClass, TrackId},
    },
};

/// Backoff duration between receive attempts.
const RECV_BACKOFF: Duration = Duration::from_micros(100);

/// Minimum playback rate to avoid division by zero.
const MIN_PLAYBACK_RATE: f64 = 0.01;

/// Probe buffer size in bytes for initial stream detection.
const PROBE_BUFFER_SIZE: usize = 1024;

enum ChunkOutcome {
    Continue,
    Return(Option<PcmChunk>),
}

enum RecvOutcome {
    Closed,
    Empty,
    Item(Fetch<PcmChunk>),
}

type AudioChannels = (
    HeapProd<AudioCommand>,
    HeapCons<AudioCommand>,
    HeapProd<Fetch<PcmChunk>>,
    HeapCons<Fetch<PcmChunk>>,
);

/// Generic audio pipeline running in a separate thread.
///
/// Provides a simple interface for reading decoded PCM audio,
/// compatible with cpal and rodio audio backends.
///
/// # Example
///
/// ```ignore
/// use kithara_audio::{Audio, AudioConfig};
/// use kithara_hls::{Hls, HlsConfig};
/// use kithara_stream::Stream;
///
/// let config = AudioConfig::<Hls>::new(hls_config)
///     .with_hint("mp3");
/// let audio = Audio::<Stream<Hls>>::new(config).await?;
///
/// // Get audio format
/// let spec = audio.spec();
/// println!("{}Hz, {} channels", spec.sample_rate, spec.channels);
///
/// // Read PCM samples
/// let mut buf = [0.0f32; 1024];
/// while !audio.is_eof() {
///     let n = audio.read(&mut buf);
///     play_samples(&buf[..n]);
/// }
/// ```
pub struct Audio<S> {
    /// Command sender for non-seek commands.
    ///
    /// Seek flows through Timeline atomics. This channel is kept alive
    /// so the worker loop does not exit due to a closed `cmd_rx`.
    #[expect(dead_code, reason = "kept alive for cmd_rx in worker loop")]
    cmd_tx: HeapProd<AudioCommand>,

    /// PCM chunk receiver.
    pcm_rx: HeapCons<Fetch<PcmChunk>>,

    /// Shared epoch counter with worker (kept alive for `Arc` shared ownership).
    _epoch: Arc<AtomicU64>,

    /// Epoch validator for filtering stale chunks.
    pub(crate) validator: EpochValidator,

    /// Current audio specification (updated from chunks).
    pub(crate) spec: PcmSpec,

    /// Current chunk being read (auto-recycles to pool on drop).
    pub(crate) current_chunk: Option<PcmChunk>,

    /// Current position in the in-memory chunk only.
    ///
    /// This is not a global playback position and must not be used as timeline
    /// source-of-truth. Timeline advances only after samples are committed to output.
    pub(crate) chunk_offset: usize,

    /// Consumer-side phase — replaces the old `eof: bool` flag.
    pub(crate) consumer_phase: ConsumerPhase,

    /// Shared stream timeline for committed playback position.
    pub(crate) timeline: Timeline,

    /// Track metadata (title, artist, album, artwork).
    metadata: TrackMetadata,

    /// Unified event bus.
    bus: EventBus,

    /// Cancellation token for graceful shutdown.
    cancel: Option<CancellationToken>,

    /// Shared pool for temporary interleaved buffers (used in `read_planar`).
    pcm_pool: PcmPool,

    /// Target sample rate of the audio host (shared for dynamic updates).
    /// 0 means "not set".
    host_sample_rate: Arc<AtomicU32>,

    /// Shared playback rate for timeline scaling (1.0 = normal speed).
    playback_rate: Arc<AtomicF32>,

    /// Notify for async preload (first chunk available).
    pub(crate) preload_notify: Arc<Notify>,

    /// Whether `preload()` has been called (enables non-blocking mode).
    preloaded: bool,

    /// Callback to wake blocked `wait_range()` calls on seek.
    ///
    /// Set during construction for sources with blocking condvar waits (HLS).
    /// Called from `seek()` after `initiate_seek()` for instant wakeup.
    notify_waiting: Option<Box<dyn Fn() + Send + Sync>>,

    /// Assigned track ID in the shared worker (used for unregister on drop).
    track_id: Option<TrackId>,

    /// Worker handle for unregistration and optional shutdown.
    worker: Option<AudioWorkerHandle>,

    /// Wake handle for blocking PCM reads.
    reader_wake: Arc<ThreadWake>,

    /// Whether the worker was auto-created for this track (standalone mode).
    /// Standalone workers are shut down when the track is dropped.
    is_standalone_worker: bool,

    /// Marker for source type.
    _marker: PhantomData<S>,
}

// Public API for cpal/rodio compatibility

impl<S> Audio<S> {
    /// Whether non-blocking recv is active.
    ///
    /// Returns `false` after `seek()` until `preload()` is called again.
    #[must_use]
    pub fn is_preloaded(&self) -> bool {
        self.preloaded
    }

    /// Get reference to PCM receiver for direct channel access.
    #[must_use]
    pub fn pcm_rx(&mut self) -> &mut HeapCons<Fetch<PcmChunk>> {
        &mut self.pcm_rx
    }

    /// Enable non-blocking mode for `read()`.
    ///
    /// After calling this, `read()` returns immediately from buffered data
    /// without blocking. Must be called after construction so that
    /// `fill_buffer()` calls from JS (via `requestAnimationFrame`) don't hang.
    pub fn preload(&mut self) {
        self.preloaded = true;
        if self.current_chunk.is_none() && !self.consumer_phase.is_terminal() {
            self.fill_buffer();
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

    fn emit_playback_progress(&self) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to u64::MAX before cast"
        )]
        let position_ms = self.position().as_millis().min(u128::from(u64::MAX)) as u64;
        let total_ms = self.timeline.total_duration().map(|duration| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "millis fits in u64 for practical durations"
            )]
            {
                duration.as_millis() as u64
            }
        });

        self.emit_audio_event(AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            seek_epoch: self.validator.epoch,
        });
    }

    fn emit_audio_event(&self, event: AudioEvent) {
        self.bus.publish(event);
    }

    fn emit_post_seek_output_commit(&mut self, meta: Option<PcmMeta>) {
        let Some(seek_epoch) = self.timeline.pending_seek_epoch() else {
            return;
        };
        if seek_epoch != self.validator.epoch {
            return;
        }

        let variant = meta.as_ref().and_then(|m| m.variant_index);
        let segment_index = meta.as_ref().and_then(|m| m.segment_index);

        self.emit_audio_event(AudioEvent::SeekLifecycle {
            stage: SeekLifecycleStage::OutputCommitted,
            seek_epoch,
            task_id: seek_epoch,
            variant,
            segment_index,
            byte_range_start: None,
            byte_range_end: None,
        });

        self.emit_audio_event(AudioEvent::SeekComplete {
            position: (*self).position(),
            seek_epoch,
        });
        let _ = self.timeline.clear_pending_seek_epoch(seek_epoch);
    }

    /// Check if end of stream has been reached.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.consumer_phase.is_terminal()
    }

    /// Get current playback position.
    ///
    /// Calculated from samples read since last seek plus the seek base.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.timeline.committed_position()
    }

    /// Get total duration of the audio stream.
    ///
    /// Returns `None` for streaming sources where duration is unknown.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.timeline.total_duration()
    }

    /// Get track metadata (title, artist, album, artwork).
    #[must_use]
    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    /// Advance the timeline accounting for playback rate.
    ///
    /// At rate > 1.0, position advances faster (fewer effective samples per second).
    /// At rate < 1.0, position advances slower.
    pub(crate) fn advance_timeline(&self, interleaved_samples: u64) {
        let rate = f64::from(self.playback_rate.load(Ordering::Relaxed)).max(MIN_PLAYBACK_RATE);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "audio rate always produces valid sample rate"
        )]
        let effective_sr = (f64::from(self.spec.sample_rate) / rate) as u32;
        self.timeline.advance_committed_samples(
            interleaved_samples,
            effective_sr.max(1),
            self.spec.channels,
        );
    }

    /// Read decoded PCM samples into buffer.
    ///
    /// Returns number of samples written (may be less than buffer size).
    /// Returns 0 when EOF is reached.
    ///
    /// Samples are interleaved f32 (e.g., LRLRLR for stereo).
    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara_hang_detector::hang_watchdog]
    pub fn read(&mut self, buf: &mut [f32]) -> usize {
        if self.consumer_phase.is_terminal() || buf.is_empty() {
            return 0;
        }

        let mut written = 0;
        let mut last_output_meta: Option<PcmMeta> = None;

        while written < buf.len() {
            hang_tick!();

            // Try to read from current chunk
            if let Some(ref chunk) = self.current_chunk {
                let remaining_in_chunk = chunk.pcm.len() - self.chunk_offset;
                let to_copy = (buf.len() - written).min(remaining_in_chunk);
                if to_copy > 0 {
                    hang_reset!();
                }

                buf[written..written + to_copy]
                    .copy_from_slice(&chunk.pcm[self.chunk_offset..self.chunk_offset + to_copy]);
                last_output_meta = Some(chunk.meta);

                written += to_copy;
                self.chunk_offset += to_copy;

                if self.chunk_offset >= chunk.pcm.len() {
                    self.current_chunk = None; // auto-recycles via Drop
                    self.chunk_offset = 0;
                }
            }

            if written >= buf.len() {
                break;
            }

            // Need more data - fetch next chunk
            if !self.fill_buffer() {
                break;
            }
        }

        if written > 0 {
            self.advance_timeline(written as u64);
            self.emit_post_seek_output_commit(last_output_meta);
            self.emit_playback_progress();
        }
        written
    }

    /// Seek to position in the audio stream.
    ///
    /// This method never blocks. Seek coordination flows entirely through
    /// Timeline atomics (`FLUSH_START`/`FLUSH_STOP` pattern). The worker thread reads
    /// the seek target and epoch from Timeline and applies the seek.
    ///
    /// # Errors
    ///
    /// This method is infallible in practice but returns `DecodeResult` for
    /// API compatibility.
    #[kithara_hang_detector::hang_watchdog]
    pub fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        // 1. Atomic write to Timeline — FLUSH_START
        let epoch = self.timeline.initiate_seek(position);
        self.timeline.mark_pending_seek_epoch(epoch);
        // 2. Update local consumer state
        self.validator.epoch = epoch;
        self.current_chunk = None;
        self.chunk_offset = 0;
        self.consumer_phase = ConsumerPhase::SeekPending { epoch };

        // 3. Drain stale chunks from pcm channel to unblock worker
        while self.pcm_rx.try_pop().is_some() {
            hang_tick!();
        }

        // 4. Wake blocked wait_range() calls for instant seek response
        if let Some(ref notify) = self.notify_waiting {
            notify();
        }

        // 5. Wake the shared worker for instant seek response
        if let Some(ref worker) = self.worker {
            worker.wake();
        }

        // Keep preloaded flag unchanged.  Resetting to false switches
        // Audio::read() to blocking recv, which parks the audio callback
        // thread on every empty ringbuf poll — causing persistent crossfade
        // glitches.  The preloaded flag is a one-way latch: once preload()
        // sets it to true, it stays true for the lifetime of this Audio.

        trace!(?position, epoch, "seek initiated via Timeline");
        Ok(())
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

    #[kithara_hang_detector::hang_watchdog]
    fn recv_valid_chunk(&mut self) -> Option<PcmChunk> {
        if self.consumer_phase.is_terminal() {
            return None;
        }

        loop {
            match self.recv_outcome() {
                RecvOutcome::Item(fetch) => match self.process_fetch(fetch) {
                    ChunkOutcome::Continue => {
                        hang_tick!();
                        continue;
                    }
                    ChunkOutcome::Return(chunk) => {
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

    #[kithara_hang_detector::hang_watchdog]
    fn recv_outcome_blocking(&mut self) -> RecvOutcome {
        loop {
            if let Some(fetch) = self.pcm_rx.try_pop() {
                hang_reset!();
                self.wake_worker();
                return RecvOutcome::Item(fetch);
            }
            if self
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
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
            if self
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                hang_reset!();
                return RecvOutcome::Closed;
            }
            hang_tick!();
            Self::wait_for_fetch();
        }
    }

    fn wait_for_fetch() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            park_timeout(RECV_BACKOFF);
        }

        #[cfg(target_arch = "wasm32")]
        {
            if is_worker_thread() {
                park_timeout(RECV_BACKOFF);
            } else {
                thread_sleep(RECV_BACKOFF);
            }
        }
    }

    fn process_fetch(&mut self, fetch: Fetch<PcmChunk>) -> ChunkOutcome {
        if !self.validator.is_valid(&fetch) {
            return ChunkOutcome::Continue;
        }

        if fetch.is_eof() {
            self.consumer_phase = ConsumerPhase::AtEof;
            return ChunkOutcome::Return(None);
        }

        let chunk = fetch.into_inner();
        trace!(
            samples = chunk.pcm.len(),
            spec = ?chunk.spec(),
            "Audio: received chunk"
        );
        ChunkOutcome::Return(Some(chunk))
    }

    fn close_channel_and_mark_eof(&mut self) -> Option<PcmChunk> {
        self.consumer_phase = ConsumerPhase::Failed;
        None
    }

    fn use_nonblocking_recv(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            true
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.preloaded
        }
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
        self.chunk_offset = 0;

        // Transition to Playing on first chunk received.
        if matches!(
            self.consumer_phase,
            ConsumerPhase::Buffering | ConsumerPhase::SeekPending { .. }
        ) {
            self.consumer_phase = ConsumerPhase::Playing;
        }
        true
    }
}

/// Specialized impl for Stream-based audio pipelines.
///
/// Provides async constructor that creates Stream internally.
/// Uses `StreamAudioSource` for automatic format change detection on ABR switch.
impl<T> Audio<Stream<T>>
where
    T: StreamType<Events = EventBus>,
{
    fn resolve_event_bus(stream_config: &T::Config, config_bus: Option<EventBus>) -> EventBus {
        T::event_bus(stream_config)
            .or(config_bus)
            .unwrap_or_default()
    }

    async fn create_stream_with_probe(
        stream_config: T::Config,
        byte_pool: kithara_bufpool::BytePool,
    ) -> Result<Stream<T>, DecodeError> {
        debug!("Audio::new — creating Stream...");
        let stream = Stream::<T>::new(stream_config)
            .await
            .map_err(|e| DecodeError::Io(IoError::other(e.to_string())))?;
        debug!("Audio::new — Stream created");

        debug!("Audio::new — spawning probe task...");
        let stream = spawn_blocking(move || {
            let mut stream = stream;
            let mut probe_buf = byte_pool.get_with(|b| b.resize(PROBE_BUFFER_SIZE, 0));
            let _ = stream.read(&mut probe_buf);
            stream.seek(SeekFrom::Start(0)).map_err(DecodeError::Io)?;
            Ok::<_, DecodeError>(stream)
        })
        .await
        .map_err(|e| DecodeError::Io(IoError::other(format!("probe task panicked: {e}"))))??;
        debug!("Audio::new — probe task done");
        Ok(stream)
    }

    async fn create_initial_decoder(
        shared_stream: SharedStream<T>,
        initial_media_info: Option<MediaInfo>,
        hint: Option<String>,
        pcm_pool: PcmPool,
        prefer_hardware: bool,
        stream_ctx: Arc<dyn StreamContext>,
    ) -> Result<Box<dyn kithara_decode::InnerDecoder>, DecodeError> {
        debug!("Audio::new — spawning decoder creation...");
        let byte_len_handle = Arc::new(AtomicU64::new(shared_stream.len().unwrap_or(0)));
        let decoder_config = kithara_decode::DecoderConfig {
            prefer_hardware,
            hint: hint.clone(),
            byte_len_handle: Some(Arc::clone(&byte_len_handle)),
            pcm_pool: Some(pcm_pool),
            stream_ctx: Some(stream_ctx),
            ..Default::default()
        };
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
        .map_err(|e| DecodeError::Io(IoError::other(format!("decoder task panicked: {e}"))))??;
        debug!("Audio::new — decoder created");
        Ok(decoder)
    }

    fn create_channels(command_channel_capacity: usize, pcm_buffer_chunks: usize) -> AudioChannels {
        let cmd_capacity = command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = HeapRb::<AudioCommand>::new(cmd_capacity).split();
        let (data_tx, data_rx) = HeapRb::<Fetch<PcmChunk>>::new(pcm_buffer_chunks.max(1)).split();
        (cmd_tx, cmd_rx, data_tx, data_rx)
    }

    fn create_emit(bus: &EventBus) -> Box<dyn Fn(AudioEvent) + Send> {
        let emit_bus = bus.clone();
        Box::new(move |event: AudioEvent| {
            emit_bus.publish(event);
        })
    }

    fn create_decoder_factory(
        prefer_hardware: bool,
        stream_ctx: &Arc<dyn StreamContext>,
        epoch: &Arc<AtomicU64>,
        byte_len_handle: &Arc<AtomicU64>,
        pool: &PcmPool,
    ) -> crate::pipeline::source::DecoderFactory<T> {
        let factory_stream_ctx = Arc::clone(stream_ctx);
        let factory_epoch = Arc::clone(epoch);
        let factory_byte_len = Arc::clone(byte_len_handle);
        let factory_pool = pool.clone();
        Box::new(move |stream, info, base_offset| {
            // Compute byte_len from current stream length minus base_offset.
            // Must be recomputed on every recreation — variant switches change
            // the effective total, so a stale value from the previous variant
            // would cause Symphonia to compute corrupted seek deltas.
            let byte_len = stream
                .len()
                .map_or(0, |len| len.saturating_sub(base_offset));
            factory_byte_len.store(byte_len, Ordering::Release);
            let current_epoch = factory_epoch.load(Ordering::Acquire);
            let config = kithara_decode::DecoderConfig {
                prefer_hardware,
                byte_len_handle: Some(Arc::clone(&factory_byte_len)),
                pcm_pool: Some(factory_pool.clone()),
                stream_ctx: Some(Arc::clone(&factory_stream_ctx)),
                epoch: current_epoch,
                ..Default::default()
            };
            match DecoderFactory::create_for_recreate(
                || OffsetReader::new(stream.clone(), base_offset),
                info,
                config,
            ) {
                Ok(d) => {
                    d.update_byte_len(byte_len);
                    Some(d)
                }
                Err(e) => {
                    warn!(?e, "failed to recreate decoder");
                    None
                }
            }
        })
    }

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
        let cancel = CancellationToken::new();

        let AudioConfig {
            byte_pool,
            command_channel_capacity,
            hint,
            host_sample_rate: config_host_sr,
            media_info: user_media_info,
            pcm_buffer_chunks,
            pcm_pool: mut pool,
            playback_rate: config_playback_rate,
            prefer_hardware,
            preload_chunks,
            resampler_quality,
            stream: stream_config,
            bus: config_bus,
            effects: custom_effects,
            worker: config_worker,
        } = config;

        let bus = Self::resolve_event_bus(&stream_config, config_bus);
        let byte_pool = byte_pool.unwrap_or_else(|| kithara_bufpool::byte_pool().clone());
        let stream = Self::create_stream_with_probe(stream_config, byte_pool).await?;

        let stream_media_info = stream.media_info();
        let initial_byte_len = stream.len().unwrap_or(0);
        let timeline = stream.timeline();
        let initial_media_info = user_media_info.or_else(|| stream_media_info.clone());
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        let shared_stream = SharedStream::new(stream);
        let byte_len_handle = Arc::new(AtomicU64::new(initial_byte_len));
        let stream_ctx = shared_stream.build_stream_context();

        let pool = pool.get_or_insert_with(|| pcm_pool().clone());
        let decoder = Self::create_initial_decoder(
            shared_stream.clone(),
            initial_media_info.clone(),
            hint.clone(),
            pool.clone(),
            prefer_hardware,
            Arc::clone(&stream_ctx),
        )
        .await?;

        let initial_spec = decoder.spec();
        let total_duration = decoder.duration().or_else(|| timeline.total_duration());
        timeline.set_total_duration(total_duration);
        let metadata = decoder.metadata();

        let (cmd_tx, cmd_rx, data_tx, data_rx) =
            Self::create_channels(command_channel_capacity, pcm_buffer_chunks);

        let epoch = Arc::new(AtomicU64::new(0));
        let host_sample_rate = Arc::new(AtomicU32::new(config_host_sr.map_or(0, NonZeroU32::get)));
        let playback_rate = config_playback_rate.unwrap_or_else(|| Arc::new(AtomicF32::new(1.0)));

        let output_spec = expected_output_spec(initial_spec, &host_sample_rate);
        let effects = create_effects(
            initial_spec,
            &host_sample_rate,
            &playback_rate,
            resampler_quality,
            Some(pool.clone()),
            custom_effects,
        );

        info!(
            ?initial_spec,
            ?output_spec,
            host_sr = host_sample_rate.load(Ordering::Relaxed),
            "Audio pipeline created"
        );

        let emit = Self::create_emit(&bus);
        let decoder_factory = Self::create_decoder_factory(
            prefer_hardware,
            &stream_ctx,
            &epoch,
            &byte_len_handle,
            pool,
        );
        let notify_waiting = shared_stream.make_notify_fn();

        let initial_variant = stream_media_info.as_ref().and_then(|i| i.variant_index);
        let audio_source = StreamAudioSource::new(
            shared_stream,
            decoder,
            decoder_factory,
            stream_media_info,
            Arc::clone(&epoch),
            effects,
        )
        .with_emit(emit);

        bus.publish(AudioEvent::DecoderReady {
            base_offset: 0,
            variant: initial_variant,
        });

        let preload_notify = Arc::new(Notify::new());
        let reader_wake = Arc::new(ThreadWake::new());

        // Get or create worker — standalone mode when no worker provided.
        let (worker, is_standalone) =
            config_worker.map_or_else(|| (AudioWorkerHandle::new(), true), |w| (w, false));

        let track_id = worker.register_track(TrackRegistration {
            consumer_wake: Arc::clone(&reader_wake),
            source: Box::new(audio_source),
            data_tx,
            cmd_rx,
            preload_notify: preload_notify.clone(),
            preload_chunks: preload_chunks.get(),
            cancel: cancel.clone(),
            service_class: ServiceClass::default(),
        });

        Ok(Self {
            cmd_tx,
            pcm_rx: data_rx,
            _epoch: epoch,
            validator: EpochValidator::new(),
            spec: output_spec,
            current_chunk: None,
            chunk_offset: 0,
            consumer_phase: ConsumerPhase::Buffering,
            timeline,
            metadata,
            bus,
            cancel: Some(cancel),
            pcm_pool: pool.clone(),
            host_sample_rate,
            playback_rate,
            preload_notify,
            preloaded: false,
            notify_waiting,
            track_id: Some(track_id),
            worker: Some(worker),
            reader_wake,
            is_standalone_worker: is_standalone,
            _marker: PhantomData,
        })
    }

    /// Subscribe to unified events via the `EventBus`.
    ///
    /// Returns a receiver for all events published to the bus.
    #[must_use]
    pub fn events(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }

    /// Get a reference to the underlying `EventBus`.
    ///
    /// Useful for passing to downstream components that also publish events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
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

// PcmReader implementation for Audio
impl<S: Send> PcmReader for Audio<S> {
    fn read(&mut self, buf: &mut [f32]) -> usize {
        Self::read(self, buf)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_planar<'a>(&mut self, output: &'a mut [&'a mut [f32]]) -> usize {
        let channels = output.len();
        if channels == 0 {
            return 0;
        }
        let frames = output[0].len();
        let total_samples = frames * channels;
        let mut interleaved = self.pcm_pool.get_with(|b| {
            b.clear();
            b.resize(total_samples, 0.0);
        });
        let n = self.read(&mut interleaved);
        let actual_frames = n / channels;
        for frame in 0..actual_frames {
            for (ch, out_ch) in output.iter_mut().enumerate() {
                out_ch[frame] = interleaved[frame * channels + ch];
            }
        }
        actual_frames
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        Self::seek(self, position)
    }

    fn spec(&self) -> PcmSpec {
        Self::spec(self)
    }

    fn is_eof(&self) -> bool {
        Self::is_eof(self)
    }

    fn position(&self) -> Duration {
        Self::position(self)
    }

    fn duration(&self) -> Option<Duration> {
        Self::duration(self)
    }

    fn metadata(&self) -> &TrackMetadata {
        Self::metadata(self)
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        self.host_sample_rate
            .store(sample_rate.get(), Ordering::Relaxed);
    }

    fn set_playback_rate(&self, rate: f32) {
        self.playback_rate.store(rate, Ordering::Relaxed);
    }

    fn set_service_class(&self, class: ServiceClass) {
        if let (Some(worker), Some(track_id)) = (&self.worker, self.track_id) {
            worker.set_service_class(track_id, class);
        }
    }
    fn preload_notify(&self) -> Option<Arc<Notify>> {
        Some(self.preload_notify.clone())
    }

    fn preload(&mut self) {
        Self::preload(self);
    }

    fn next_chunk(&mut self) -> Option<PcmChunk> {
        // Reuse preloaded chunk if it hasn't been partially consumed
        let chunk = if self.chunk_offset == 0 {
            self.current_chunk.take()
        } else {
            self.current_chunk = None;
            None
        };
        self.chunk_offset = 0;

        let chunk = chunk.or_else(|| self.recv_valid_chunk())?;
        self.spec = chunk.spec();

        // Transition to Playing on first chunk (same as fill_buffer)
        if matches!(
            self.consumer_phase,
            ConsumerPhase::Buffering | ConsumerPhase::SeekPending { .. }
        ) {
            self.consumer_phase = ConsumerPhase::Playing;
        }

        self.advance_timeline(chunk.pcm.len() as u64);
        Some(chunk)
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

    use kithara_test_utils::kithara;
    use ringbuf::traits::Producer;

    use super::*;

    fn empty_audio() -> Audio<()> {
        let (cmd_tx, _cmd_rx) = HeapRb::<AudioCommand>::new(1).split();
        let (_data_tx, pcm_rx) = HeapRb::<Fetch<PcmChunk>>::new(1).split();

        Audio {
            cmd_tx,
            pcm_rx,
            _epoch: Arc::new(AtomicU64::new(0)),
            validator: EpochValidator::new(),
            spec: PcmSpec::default(),
            current_chunk: None,
            chunk_offset: 0,
            consumer_phase: ConsumerPhase::Buffering,
            timeline: Timeline::new(),
            metadata: TrackMetadata::default(),
            bus: EventBus::default(),
            cancel: None,
            pcm_pool: pcm_pool().clone(),
            host_sample_rate: Arc::new(AtomicU32::new(0)),
            playback_rate: Arc::new(AtomicF32::new(1.0)),
            preload_notify: Arc::new(Notify::new()),
            preloaded: false,
            notify_waiting: None,
            track_id: None,
            worker: None,
            reader_wake: Arc::new(ThreadWake::new()),
            is_standalone_worker: false,
            _marker: PhantomData,
        }
    }

    #[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
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
        let cancel = CancellationToken::new();
        cancel.cancel();
        audio.cancel = Some(cancel);

        assert!(matches!(audio.recv_outcome(), RecvOutcome::Closed));
    }

    #[kithara::test]
    fn preloaded_recv_is_nonblocking() {
        let mut audio = empty_audio();
        audio.preload();

        assert!(matches!(audio.recv_outcome(), RecvOutcome::Empty));
    }

    // Helper that returns (Audio, data_tx) so tests can push fetches.
    fn audio_with_channel() -> (Audio<()>, HeapProd<Fetch<PcmChunk>>) {
        let (cmd_tx, _cmd_rx) = HeapRb::<AudioCommand>::new(1).split();
        let (data_tx, pcm_rx) = HeapRb::<Fetch<PcmChunk>>::new(4).split();

        let audio = Audio {
            cmd_tx,
            pcm_rx,
            _epoch: Arc::new(AtomicU64::new(0)),
            validator: EpochValidator::new(),
            spec: PcmSpec::default(),
            current_chunk: None,
            chunk_offset: 0,
            consumer_phase: ConsumerPhase::Buffering,
            timeline: Timeline::new(),
            metadata: TrackMetadata::default(),
            bus: EventBus::default(),
            cancel: None,
            pcm_pool: pcm_pool().clone(),
            host_sample_rate: Arc::new(AtomicU32::new(0)),
            playback_rate: Arc::new(AtomicF32::new(1.0)),
            preload_notify: Arc::new(Notify::new()),
            preloaded: true, // non-blocking for tests
            notify_waiting: None,
            track_id: None,
            worker: None,
            reader_wake: Arc::new(ThreadWake::new()),
            is_standalone_worker: false,
            _marker: PhantomData,
        };
        (audio, data_tx)
    }

    fn make_chunk(samples: &[f32]) -> PcmChunk {
        let mut chunk = PcmChunk::default();
        chunk.pcm.clear();
        chunk.pcm.extend_from_slice(samples);
        chunk
    }

    fn make_spec_chunk(samples: &[f32], spec: PcmSpec) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            pcm_pool().attach(samples.to_vec()),
        )
    }

    // ConsumerPhase transition tests

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
    fn read_preserves_partial_chunk_tail_across_calls() {
        let (mut audio, mut tx) = audio_with_channel();
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44_100,
        };

        let first: Vec<f32> = (0..4608).map(|i| i as f32).collect();
        let second: Vec<f32> = (4608..9216).map(|i| i as f32).collect();

        tx.try_push(Fetch::new(make_spec_chunk(&first, spec), false, 0))
            .unwrap();
        tx.try_push(Fetch::new(make_spec_chunk(&second, spec), false, 0))
            .unwrap();

        let mut buf_a = vec![0.0; 4410];
        let mut buf_b = vec![0.0; 4410];

        let n_a = audio.read(&mut buf_a);
        let n_b = audio.read(&mut buf_b);

        assert_eq!(n_a, 4410);
        assert_eq!(n_b, 4410);
        assert_eq!(buf_a, first[..4410]);

        let mut expected_b = first[4410..].to_vec();
        expected_b.extend_from_slice(&second[..(4410 - 198)]);
        assert_eq!(
            buf_b, expected_b,
            "read() must consume the tail of the partially-read chunk before pulling a new one"
        );
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

        // Seek sets SeekPending
        audio.seek(Duration::from_secs(5)).ok();
        let epoch = audio.validator.epoch;

        // Push chunk with matching epoch
        let chunk = make_chunk(&[0.1, 0.2]);
        let fetch = Fetch::new(chunk, false, epoch);
        tx.try_push(fetch).ok();

        assert!(audio.fill_buffer());
        assert_eq!(audio.consumer_phase, ConsumerPhase::Playing);
    }

    #[kithara::test]
    fn consumer_phase_eof_terminates() {
        let (mut audio, mut tx) = audio_with_channel();

        let fetch = Fetch::new(PcmChunk::default(), true, 0);
        tx.try_push(fetch).ok();

        // recv_valid_chunk should set AtEof
        let result = audio.recv_valid_chunk();
        assert!(result.is_none());
        assert_eq!(audio.consumer_phase, ConsumerPhase::AtEof);
        assert!(audio.is_eof());
    }

    #[kithara::test]
    fn consumer_phase_failed_on_channel_close() {
        let (mut audio, _tx) = audio_with_channel();
        let cancel = CancellationToken::new();
        cancel.cancel();
        audio.cancel = Some(cancel);
        // preloaded=false to trigger blocking recv which checks cancel
        audio.preloaded = false;

        let result = audio.recv_valid_chunk();
        assert!(result.is_none());
        assert_eq!(audio.consumer_phase, ConsumerPhase::Failed);
        assert!(audio.is_eof());
    }

    #[kithara::test]
    fn consumer_does_not_park_in_terminal_phase() {
        let (mut audio, _tx) = audio_with_channel();
        audio.consumer_phase = ConsumerPhase::AtEof;

        // read() should return 0 immediately
        let mut buf = [0.0f32; 16];
        assert_eq!(audio.read(&mut buf), 0);
    }
}
