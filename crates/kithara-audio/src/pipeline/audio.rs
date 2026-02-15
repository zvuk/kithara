//! Audio pipeline struct and public API.

use std::{
    io::{Read, Seek, SeekFrom},
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use kanal::Receiver;
use kithara_bufpool::{PcmPool, pcm_pool};
use kithara_decode::{PcmChunk, PcmSpec, TrackMetadata};
use kithara_events::{AudioEvent, EventBus};
use kithara_stream::{EpochValidator, Fetch, Stream, StreamType};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use super::{
    config::{AudioConfig, create_effects, expected_output_spec},
    stream_source::{OffsetReader, SharedStream, StreamAudioSource},
    worker::{AudioCommand, run_audio_loop},
};
use crate::types::{DecodeError, DecodeResult, PcmReader};

/// Default capacity for broadcast event channels.
const DEFAULT_EVENT_CAPACITY: usize = 64;

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
    /// Command sender for seek.
    cmd_tx: kanal::Sender<AudioCommand>,

    /// PCM chunk receiver.
    pcm_rx: Receiver<Fetch<PcmChunk>>,

    /// Shared epoch counter with worker (kept alive for `Arc` shared ownership).
    _epoch: Arc<AtomicU64>,

    /// Epoch validator for filtering stale chunks.
    validator: EpochValidator,

    /// Current audio specification (updated from chunks).
    pub(crate) spec: PcmSpec,

    /// Current chunk being read (auto-recycles to pool on drop).
    pub(crate) current_chunk: Option<PcmChunk>,

    /// Current position in chunk.
    pub(crate) chunk_offset: usize,

    /// End of stream reached.
    pub(crate) eof: bool,

    /// Number of interleaved samples read since last seek.
    pub(crate) samples_read: u64,

    /// Base position after last seek.
    seek_base: Duration,

    /// Total duration from format metadata.
    pub(crate) total_duration: Option<Duration>,

    /// Track metadata (title, artist, album, artwork).
    metadata: TrackMetadata,

    /// Audio events channel (for `decode_events()` backward compat).
    audio_events_tx: broadcast::Sender<AudioEvent>,

    /// Unified event bus.
    bus: EventBus,

    /// Cancellation token for graceful shutdown.
    cancel: Option<CancellationToken>,

    /// Shared pool for temporary interleaved buffers (used in `read_planar`).
    pcm_pool: PcmPool,

    /// Target sample rate of the audio host (shared for dynamic updates).
    /// 0 means "not set".
    host_sample_rate: Arc<AtomicU32>,

    /// Notify for async preload (first chunk available).
    preload_notify: Arc<Notify>,

    /// Whether `preload()` has been called (enables non-blocking mode).
    preloaded: bool,

    /// Marker for source type.
    _marker: std::marker::PhantomData<S>,
}

// Public API for cpal/rodio compatibility

impl<S> Audio<S> {
    /// Get reference to PCM receiver for direct channel access.
    #[must_use]
    pub fn pcm_rx(&self) -> &Receiver<Fetch<PcmChunk>> {
        &self.pcm_rx
    }

    /// Enable non-blocking mode for `read()`.
    ///
    /// After calling this, `read()` returns immediately from buffered data
    /// without blocking. Must be called after construction so that
    /// `fill_buffer()` calls from JS (via `requestAnimationFrame`) don't hang.
    pub fn preload(&mut self) {
        self.preloaded = true;
        if self.current_chunk.is_none() && !self.eof {
            self.fill_buffer();
        }
    }

    /// Subscribe to audio events.
    ///
    /// For `Audio<Stream<T>>`, prefer `events()` which provides unified
    /// stream + audio events.
    #[must_use]
    pub fn decode_events(&self) -> broadcast::Receiver<AudioEvent> {
        self.audio_events_tx.subscribe()
    }

    /// Get current audio specification.
    ///
    /// Returns sample rate and channel count for audio output setup.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Check if end of stream has been reached.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.eof
    }

    /// Get current playback position.
    ///
    /// Calculated from samples read since last seek plus the seek base.
    #[must_use]
    pub fn position(&self) -> Duration {
        let rate = f64::from(self.spec.sample_rate) * f64::from(self.spec.channels.max(1));
        if rate == 0.0 {
            return self.seek_base;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "sample count precision loss is negligible"
        )]
        let samples = self.samples_read as f64;
        self.seek_base + Duration::from_secs_f64(samples / rate)
    }

    /// Get total duration of the audio stream.
    ///
    /// Returns `None` for streaming sources where duration is unknown.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.total_duration
    }

    /// Get track metadata (title, artist, album, artwork).
    #[must_use]
    pub fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    /// Read decoded PCM samples into buffer.
    ///
    /// Returns number of samples written (may be less than buffer size).
    /// Returns 0 when EOF is reached.
    ///
    /// Samples are interleaved f32 (e.g., LRLRLR for stereo).
    #[cfg_attr(feature = "perf", hotpath::measure)]
    pub fn read(&mut self, buf: &mut [f32]) -> usize {
        if self.eof || buf.is_empty() {
            return 0;
        }

        let mut written = 0;

        while written < buf.len() {
            // Try to read from current chunk
            if let Some(ref chunk) = self.current_chunk {
                let remaining_in_chunk = chunk.pcm.len() - self.chunk_offset;
                let to_copy = (buf.len() - written).min(remaining_in_chunk);

                buf[written..written + to_copy]
                    .copy_from_slice(&chunk.pcm[self.chunk_offset..self.chunk_offset + to_copy]);

                written += to_copy;
                self.chunk_offset += to_copy;

                if self.chunk_offset >= chunk.pcm.len() {
                    self.current_chunk = None; // auto-recycles via Drop
                    self.chunk_offset = 0;
                }

                if written >= buf.len() {
                    break;
                }
            }

            // Need more data - fetch next chunk
            if !self.fill_buffer() {
                break;
            }
        }

        self.samples_read += written as u64;
        written
    }

    /// Seek to position in the audio stream.
    ///
    /// Note: Seek clears internal buffers and invalidates pending chunks.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::SeekError`] if the command channel is closed.
    pub fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        // Increment epoch to invalidate pending chunks.
        // Do NOT store to shared atomic here — the worker does it in handle_command().
        // Storing here would race: the worker's fetch_next() could read the new epoch
        // before processing the seek, stamping a pre-seek chunk with the new epoch
        // and making it pass the consumer's epoch filter.
        let new_epoch = self.validator.next_epoch();

        // Send seek command to worker with new epoch
        self.cmd_tx
            .send(AudioCommand::Seek {
                position,
                epoch: new_epoch,
            })
            .map_err(|_| DecodeError::SeekError("channel closed".to_string()))?;

        // Clear local state (current_chunk auto-recycles via Drop)
        self.current_chunk = None;
        self.chunk_offset = 0;
        self.eof = false;
        self.samples_read = 0;
        self.seek_base = position;

        // Drain stale chunks from channel to unblock worker.
        // Stop if we encounter a valid chunk (save it for next read).
        while let Ok(Some(fetch)) = self.pcm_rx.try_recv() {
            if self.validator.is_valid(&fetch) {
                // Found new valid chunk - save it and stop draining
                if !fetch.is_eof() {
                    let chunk = fetch.into_inner();
                    self.spec = chunk.spec();
                    self.current_chunk = Some(chunk);
                    self.chunk_offset = 0;
                    trace!("seek: saved first valid chunk after drain");
                }
                break;
            }
            // Stale chunk (old epoch) - discard and continue draining
            trace!(
                chunk_epoch = fetch.epoch(),
                current_epoch = new_epoch,
                "seek: discarding stale chunk"
            );
        }

        // Reset preload flag - first read after seek will be blocking if needed
        self.preloaded = false;

        debug!(?position, epoch = new_epoch, "seek initiated");
        Ok(())
    }

    /// Receive next valid chunk from channel, filtering stale chunks.
    ///
    /// After `preload()`, non-blocking. Before `preload()`, blocks on first call.
    /// Returns `None` on EOF or channel close.
    #[expect(
        clippy::cognitive_complexity,
        reason = "chunk validation with multiple conditions"
    )]
    fn recv_valid_chunk(&mut self) -> Option<PcmChunk> {
        if self.eof {
            return None;
        }

        loop {
            // On wasm32, blocking recv is forbidden (Atomics.wait cannot run
            // on the browser main thread). Always use non-blocking try_recv.
            #[cfg(target_arch = "wasm32")]
            let use_nonblocking = true;
            #[cfg(not(target_arch = "wasm32"))]
            let use_nonblocking = self.preloaded;

            let result = if use_nonblocking {
                // Non-blocking mode
                match self.pcm_rx.try_recv() {
                    Ok(Some(fetch)) => Ok(fetch),
                    Ok(None) => return None, // No data available
                    Err(_) => {
                        debug!("Audio: channel closed (EOF)");
                        self.eof = true;
                        return None;
                    }
                }
            } else {
                // Blocking mode before preload (native only, for tests/backward compat)
                self.pcm_rx.recv().map_err(|_| ())
            };

            if let Ok(fetch) = result {
                // Skip stale chunks (from before seek)
                if !self.validator.is_valid(&fetch) {
                    trace!(
                        chunk_epoch = fetch.epoch(),
                        current_epoch = self.validator.epoch,
                        "skipping stale chunk"
                    );
                    continue;
                }

                if fetch.is_eof() {
                    debug!(epoch = fetch.epoch(), "Audio: received EOF");
                    self.eof = true;
                    return None;
                }

                let chunk = fetch.into_inner();
                trace!(
                    samples = chunk.pcm.len(),
                    spec = ?chunk.spec(),
                    "Audio: received chunk"
                );
                return Some(chunk);
            }

            debug!("Audio: channel closed (EOF)");
            self.eof = true;
            return None;
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
    #[expect(
        clippy::cognitive_complexity,
        reason = "pipeline setup with debug tracing"
    )]
    pub async fn new(config: AudioConfig<T>) -> Result<Self, DecodeError> {
        let cancel = CancellationToken::new();

        // Destructure config to avoid partial move issues.
        let AudioConfig {
            byte_pool,
            command_channel_capacity,
            hint,
            host_sample_rate: config_host_sr,
            media_info: user_media_info,
            pcm_buffer_chunks,
            pcm_pool: mut pool,
            prefer_hardware,
            preload_chunks,
            resampler_quality,
            stream: stream_config,
            thread_pool,
            bus: config_bus,
        } = config;

        // Resolve thread pool: AudioConfig overrides stream config, which overrides global.
        let thread_pool = thread_pool.unwrap_or_else(|| T::thread_pool(&stream_config));

        // Create event bus: prefer stream config bus (HlsConfig/FileConfig),
        // fall back to AudioConfig bus, or create a new one.
        let bus = T::event_bus(&stream_config)
            .or(config_bus)
            .unwrap_or_else(|| EventBus::new(DEFAULT_EVENT_CAPACITY));

        // Create stream.
        debug!("Audio::new — creating Stream...");
        let stream = Stream::<T>::new(stream_config)
            .await
            .map_err(|e| DecodeError::Io(std::io::Error::other(e.to_string())))?;
        debug!("Audio::new — Stream created");

        // Resolve byte pool: config overrides global.
        let byte_pool = byte_pool.unwrap_or_else(|| kithara_bufpool::byte_pool().clone());

        // Trigger first segment load to get MediaInfo for proper decoder creation.
        debug!("Audio::new — spawning probe task on thread pool...");
        let stream = thread_pool
            .spawn_async(move || {
                let mut stream = stream;
                let mut probe_buf = byte_pool.get_with(|b| b.resize(1024, 0));
                let _ = stream.read(&mut probe_buf);
                stream.seek(SeekFrom::Start(0)).map_err(DecodeError::Io)?;
                Ok::<_, DecodeError>(stream)
            })
            .await
            .map_err(|e| {
                DecodeError::Io(std::io::Error::other(format!("probe task panicked: {e}")))
            })??;
        debug!("Audio::new — probe task done");

        // Get initial MediaInfo.
        // For decoder creation: user-provided overrides stream-detected.
        // For format change tracking: always use stream-detected (so ABR switches
        // are detected correctly even when user overrides the format).
        let stream_media_info = stream.media_info();
        let initial_media_info = user_media_info.or_else(|| stream_media_info.clone());
        debug!(?initial_media_info, "Initial MediaInfo from stream");

        // Create shared stream for format change detection
        let shared_stream = SharedStream::new(stream);

        // Build stream context once — shared across all decoder instances.
        let stream_ctx = shared_stream.build_stream_context();

        let pool = pool.get_or_insert_with(|| pcm_pool().clone());

        // Create initial decoder on the thread pool to avoid blocking tokio runtime.
        // Symphonia probe() does blocking IO which would deadlock the async downloader.
        debug!("Audio::new — spawning decoder creation on thread pool...");
        let decoder_config = kithara_decode::DecoderConfig {
            prefer_hardware,
            hint: hint.clone(),
            pcm_pool: Some(pool.clone()),
            stream_ctx: Some(Arc::clone(&stream_ctx)),
            ..Default::default()
        };
        let shared_stream_for_decoder = shared_stream.clone();
        let hint_for_decoder = hint.clone();
        let initial_media_info_for_decoder = initial_media_info.clone();
        let decoder: Box<dyn kithara_decode::InnerDecoder> = thread_pool
            .spawn_async(move || {
                if let Some(ref info) = initial_media_info_for_decoder {
                    kithara_decode::DecoderFactory::create_from_media_info(
                        shared_stream_for_decoder,
                        info,
                        decoder_config,
                    )
                } else {
                    kithara_decode::DecoderFactory::create_with_probe(
                        shared_stream_for_decoder,
                        hint_for_decoder.as_deref(),
                        decoder_config,
                    )
                }
            })
            .await
            .map_err(|e| {
                DecodeError::Io(std::io::Error::other(format!("decoder task panicked: {e}")))
            })??;
        debug!("Audio::new — decoder created");

        let initial_spec = decoder.spec();
        let total_duration = decoder.duration();
        let metadata = decoder.metadata();

        let cmd_capacity = command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = kanal::bounded(cmd_capacity);
        let (data_tx, data_rx) = kanal::bounded(pcm_buffer_chunks.max(1));

        let epoch = Arc::new(AtomicU64::new(0));
        let (audio_events_tx, _) = broadcast::channel(DEFAULT_EVENT_CAPACITY);
        let host_sample_rate = Arc::new(AtomicU32::new(config_host_sr.map_or(0, NonZeroU32::get)));

        let output_spec = expected_output_spec(initial_spec, &host_sample_rate);
        let effects = create_effects(
            initial_spec,
            &host_sample_rate,
            resampler_quality,
            Some(pool.clone()),
        );

        info!(
            ?initial_spec,
            ?output_spec,
            host_sr = host_sample_rate.load(Ordering::Relaxed),
            "Audio pipeline created"
        );

        // Emit closure sends AudioEvent to both the EventBus and the raw channel.
        let emit_bus = bus.clone();
        let emit_raw_tx = audio_events_tx.clone();
        let emit = Box::new(move |event: AudioEvent| {
            emit_bus.publish(event.clone());
            let _ = emit_raw_tx.send(event);
        });

        // Factory for creating decoders after ABR switch.
        //
        // Three-level fallback:
        // 1. create_from_media_info — uses codec/container from HLS metadata
        // 2. create_with_probe — uses extension hint for codec detection
        // 3. create_with_symphonia_probe — lets Symphonia detect format from data
        let factory_stream_ctx = Arc::clone(&stream_ctx);
        let factory_epoch = Arc::clone(&epoch);
        let factory_pool = pool.clone();
        let decoder_factory: super::stream_source::DecoderFactory<T> =
            Box::new(move |stream, info, base_offset| {
                let current_epoch = factory_epoch.load(Ordering::Acquire);
                let reader = OffsetReader::new(stream.clone(), base_offset);
                let config = kithara_decode::DecoderConfig {
                    prefer_hardware,
                    pcm_pool: Some(factory_pool.clone()),
                    stream_ctx: Some(Arc::clone(&factory_stream_ctx)),
                    epoch: current_epoch,
                    ..Default::default()
                };
                match kithara_decode::DecoderFactory::create_from_media_info(reader, info, config) {
                    Ok(d) => {
                        d.update_byte_len(0);
                        Some(d)
                    }
                    Err(e) => {
                        warn!(?e, "Failed to recreate decoder, trying probe fallback");
                        let reader = OffsetReader::new(stream.clone(), base_offset);
                        let config = kithara_decode::DecoderConfig {
                            prefer_hardware,
                            pcm_pool: Some(factory_pool.clone()),
                            stream_ctx: Some(Arc::clone(&factory_stream_ctx)),
                            epoch: current_epoch,
                            ..Default::default()
                        };
                        match kithara_decode::DecoderFactory::create_with_probe(
                            reader, None, config,
                        ) {
                            Ok(d) => {
                                d.update_byte_len(0);
                                Some(d)
                            }
                            Err(e) => {
                                warn!(?e, "Probe fallback failed, trying Symphonia native probe");
                                let reader = OffsetReader::new(stream, base_offset);
                                let config = kithara_decode::DecoderConfig {
                                    prefer_hardware,
                                    pcm_pool: Some(factory_pool.clone()),
                                    stream_ctx: Some(Arc::clone(&factory_stream_ctx)),
                                    epoch: current_epoch,
                                    ..Default::default()
                                };
                                match kithara_decode::DecoderFactory::create_with_symphonia_probe(
                                    reader, config,
                                ) {
                                    Ok(d) => {
                                        d.update_byte_len(0);
                                        Some(d)
                                    }
                                    Err(e) => {
                                        warn!(?e, "Symphonia native probe also failed");
                                        None
                                    }
                                }
                            }
                        }
                    }
                }
            });

        // Use StreamAudioSource for format change detection.
        // Pass stream_media_info (not initial_media_info) so format change detection
        // compares against what the stream actually reports, avoiding false positives
        // when the user overrides media_info (e.g., WAV over HLS).
        let audio_source = StreamAudioSource::new(
            shared_stream,
            decoder,
            decoder_factory,
            stream_media_info,
            Arc::clone(&epoch),
            effects,
        )
        .with_emit(emit);

        let preload_notify = Arc::new(Notify::new());
        let worker_notify = preload_notify.clone();

        let worker_preload_chunks = preload_chunks.max(1);
        let worker_cancel = cancel.clone();

        thread_pool.spawn(move || {
            run_audio_loop(
                audio_source,
                &cmd_rx,
                &data_tx,
                &worker_notify,
                worker_preload_chunks,
                &worker_cancel,
            );
        });

        Ok(Self {
            cmd_tx,
            pcm_rx: data_rx,
            _epoch: epoch,
            validator: EpochValidator::new(),
            spec: output_spec,
            current_chunk: None,
            chunk_offset: 0,
            eof: false,
            samples_read: 0,
            seek_base: Duration::ZERO,
            total_duration,
            metadata,
            audio_events_tx,
            bus,
            cancel: Some(cancel),
            pcm_pool: pool.clone(),
            host_sample_rate,
            preload_notify,
            preloaded: false,
            _marker: std::marker::PhantomData,
        })
    }

    /// Subscribe to unified events via the `EventBus`.
    ///
    /// Returns a receiver for all events published to the bus.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<kithara_events::Event> {
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
        // Channels will be dropped automatically, signaling worker to exit.
        // The worker task on the thread pool will terminate when it detects
        // closed channels or the cancellation token fires.
    }
}

// PcmReader implementation for Audio
impl<S: Send> PcmReader for Audio<S> {
    fn read(&mut self, buf: &mut [f32]) -> usize {
        Self::read(self, buf)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_planar(&mut self, output: &mut [&mut [f32]]) -> usize {
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

    fn decode_events(&self) -> broadcast::Receiver<AudioEvent> {
        Self::decode_events(self)
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        self.host_sample_rate
            .store(sample_rate.get(), Ordering::Relaxed);
    }

    fn preload_notify(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.preload_notify.clone())
    }

    fn preload(&mut self) {
        Self::preload(self);
    }

    fn next_chunk(&mut self) -> Option<PcmChunk> {
        // Discard partially-consumed chunk
        self.current_chunk = None;
        self.chunk_offset = 0;

        let chunk = self.recv_valid_chunk()?;
        self.spec = chunk.spec();
        self.samples_read += chunk.pcm.len() as u64;
        Some(chunk)
    }
}

#[cfg(test)]
mod tests {
    use kithara_decode::test_support::create_test_wav;
    use kithara_stream::Stream;

    use super::*;

    /// Write test WAV to a temp file and return config for it.
    fn test_wav_config(
        sample_count: usize,
    ) -> (tempfile::NamedTempFile, AudioConfig<kithara_file::File>) {
        let wav_data = create_test_wav(sample_count, 44100, 2);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut std::fs::File::create(tmp.path()).unwrap(), &wav_data)
            .unwrap();
        let file_config =
            kithara_file::FileConfig::new(kithara_file::FileSrc::Local(tmp.path().to_path_buf()));
        let config = AudioConfig::<kithara_file::File>::new(file_config).with_hint("wav");
        (tmp, config)
    }

    #[tokio::test]
    async fn test_audio_new() {
        let (_tmp, config) = test_wav_config(1000);
        let _audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_audio_receive_chunks() {
        let (_tmp, config) = test_wav_config(1000);
        let audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        let mut chunk_count = 0;
        while let Ok(fetch) = audio.pcm_rx().recv() {
            if fetch.is_eof() {
                break;
            }
            chunk_count += 1;
            assert!(!fetch.data.pcm.is_empty());
            if chunk_count >= 5 {
                break;
            }
        }

        assert!(chunk_count > 0);
    }

    #[test]
    fn test_audio_config_with_media_info() {
        let info = kithara_stream::MediaInfo::default()
            .with_container(kithara_stream::ContainerFormat::Wav)
            .with_sample_rate(44100);

        let config = AudioConfig::<kithara_file::File>::new(kithara_file::FileConfig::default())
            .with_media_info(info.clone());

        assert!(config.media_info.is_some());
        assert_eq!(
            config.media_info.unwrap().container,
            Some(kithara_stream::ContainerFormat::Wav)
        );
    }

    #[tokio::test]
    async fn test_audio_spec() {
        let (_tmp, config) = test_wav_config(1000);
        let audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        let spec = audio.spec();
        assert_eq!(spec.sample_rate, 44100);
        assert_eq!(spec.channels, 2);
    }

    #[tokio::test]
    async fn test_audio_read() {
        let (_tmp, config) = test_wav_config(1000);
        let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        let mut buf = [0.0f32; 256];
        let mut total_read = 0;

        while !audio.is_eof() {
            let n = audio.read(&mut buf);
            if n == 0 {
                break;
            }
            total_read += n;
        }

        assert!(total_read > 0);
        assert!(audio.is_eof());
    }

    #[tokio::test]
    async fn test_audio_read_small_buffer() {
        let (_tmp, config) = test_wav_config(100);
        let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        // Read with very small buffer
        let mut buf = [0.0f32; 4];
        let n = audio.read(&mut buf);

        assert_eq!(n, 4);
    }

    #[tokio::test]
    async fn test_audio_is_eof() {
        let (_tmp, config) = test_wav_config(10);
        let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        assert!(!audio.is_eof());

        // Read all data
        let mut buf = [0.0f32; 1024];
        while audio.read(&mut buf) > 0 {}

        assert!(audio.is_eof());
    }

    #[tokio::test]
    async fn test_audio_seek() {
        let (_tmp, config) = test_wav_config(44100); // 1 second
        let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        // Read some data
        let mut buf = [0.0f32; 256];
        audio.read(&mut buf);

        // Seek to beginning
        let result = audio.seek(Duration::from_secs(0));
        assert!(result.is_ok());

        // Should be able to read again
        assert!(!audio.is_eof());
    }

    #[tokio::test]
    async fn test_audio_preload() {
        let (_tmp, config) = test_wav_config(1000);
        let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        // Before preload, current_chunk should be None
        assert!(audio.current_chunk.is_none());

        // Get notify and await it
        let notify = audio.preload_notify.clone();
        notify.notified().await;
        audio.preload();

        // After preload, current_chunk should have data
        assert!(audio.current_chunk.is_some());

        // First read should return data immediately
        let mut buf = [0.0f32; 64];
        let n = audio.read(&mut buf);
        assert!(n > 0);
    }

    #[tokio::test]
    async fn test_audio_preload_idempotent() {
        let (_tmp, config) = test_wav_config(1000);
        let mut audio = Audio::<Stream<kithara_file::File>>::new(config)
            .await
            .unwrap();

        // Preload first time
        let notify = audio.preload_notify.clone();
        notify.notified().await;
        audio.preload();
        assert!(audio.current_chunk.is_some());

        // Preload again — should be no-op since chunk is already loaded
        audio.preload();
        assert!(audio.current_chunk.is_some());

        // Reading should still work normally
        let mut buf = [0.0f32; 64];
        let n = audio.read(&mut buf);
        assert!(n > 0);
    }
}
