use std::{num::NonZeroU32, sync::Arc, time::Duration};

use kithara_audio::{
    Audio, AudioConfig, ChunkOutcome, PcmReader, ReadOutcome, SeekOutcome, ServiceClass,
};
use kithara_decode::{DecodeError, DecodeResult, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_stream::{Stream, StreamType};

use crate::impls::{config::ResourceConfig, source_type::SourceType};

/// Type-erased audio resource wrapping any `PcmReader`.
///
/// Provides a unified interface for reading decoded PCM audio
/// regardless of the underlying source (file, HLS, custom).
///
/// # Example
///
/// ```ignore
/// use kithara_play::{Resource, ResourceConfig};
///
/// // Auto-detect: .m3u8 -> HLS, everything else -> progressive file
/// let config = ResourceConfig::new("https://example.com/song.mp3")?;
/// let mut resource = Resource::new(config).await?;
///
/// let spec = resource.spec();
/// let meta = resource.metadata();
///
/// let mut buf = [0.0f32; 1024];
/// resource.read(&mut buf);
/// ```
pub struct Resource {
    pub(crate) inner: Box<dyn PcmReader>,
    src: Arc<str>,
    bus: EventBus,
}

impl Resource {
    /// Create a resource from a `ResourceConfig`.
    ///
    /// Auto-detects the stream type from the URL:
    /// - URLs ending with `.m3u8` -> HLS stream
    /// - All other URLs -> progressive file download
    ///
    /// # Errors
    ///
    /// Returns an error if source type detection fails, or if the underlying
    /// audio stream cannot be created (network failure, invalid format, etc.).
    pub async fn new(config: ResourceConfig) -> DecodeResult<Self> {
        let src: Arc<str> = Arc::from(config.src.to_string());
        let source_type = SourceType::detect(&config.src)?;
        match source_type {
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                let audio_config = config.into_file_config();
                Self::from_stream_audio(audio_config, src).await
            }
            SourceType::HlsStream(_) => {
                let audio_config = config.into_hls_config()?;
                Self::from_stream_audio(audio_config, src).await
            }
        }
    }

    /// Runtime ABR handle for adaptive sources (HLS). `None` for files.
    #[must_use]
    pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.inner.abr_handle()
    }

    /// Get total duration (if known).
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    /// Get a reference to the underlying `EventBus`.
    ///
    /// Useful for passing to downstream components that also publish events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    /// Create a resource from any `PcmReader`.
    ///
    /// The resource shares the reader's event bus directly. Use for custom
    /// sources or offline render harnesses where the production
    /// [`Resource::new`] / [`Resource::from_stream_audio`] paths (which
    /// build a real `Stream<T>`) are heavier than the caller needs.
    ///
    /// `src` rides along on `PlayerEvent::ItemDidPlayToEnd` and is what
    /// the queue uses to tell which track ended. `None` defaults to
    /// `"unknown"`.
    #[must_use]
    pub fn from_reader<R: PcmReader + 'static>(reader: R, src: Option<Arc<str>>) -> Self {
        let bus = reader.event_bus().clone();
        let mut inner: Box<dyn PcmReader> = Box::new(reader);
        let _ = inner.preload();
        Self {
            inner,
            bus,
            src: src.unwrap_or_else(|| Arc::from("unknown")),
        }
    }

    /// Create a resource from a concrete stream-backed audio config.
    ///
    /// Generic over any [`StreamType`] whose config carries an optional
    /// `kithara_events::EventBus`. Callers wanting fine-grained control
    /// over `FileConfig` / `HlsConfig` (ABR, keys, etc.) use this path.
    pub(crate) async fn from_stream_audio<T>(
        mut config: AudioConfig<T>,
        src: Arc<str>,
    ) -> DecodeResult<Self>
    where
        T: StreamType<Events = EventBus> + 'static,
        Audio<Stream<T>>: PcmReader + 'static,
    {
        let bus = T::event_bus(&config.stream)
            .or_else(|| config.bus.clone())
            .unwrap_or_default();
        config.bus = Some(bus.clone());

        let mut audio = Audio::<Stream<T>>::new(config).await?;
        let _ = audio.preload();
        Ok(Self {
            src,
            bus,
            inner: Box::new(audio),
        })
    }

    /// Get track metadata.
    #[must_use]
    pub fn metadata(&self) -> &TrackMetadata {
        self.inner.metadata()
    }

    /// Read the next decoded chunk with full metadata.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.inner.next_chunk()
    }

    /// Get current playback position.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// Wait for first decoded chunk to be available, then move it to internal buffer.
    ///
    /// After preload completes, the first `read()` returns data without blocking.
    /// Safe to call multiple times (no-op if already preloaded).
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader::preload` if the
    /// producer channel closed or the initial fill hit a decoder
    /// failure.
    pub async fn preload(&mut self) -> Result<(), DecodeError> {
        if let Some(notify) = self.inner.preload_notify() {
            notify.notified().await;
        }
        self.inner.preload()
    }

    /// Read interleaved PCM samples.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        self.inner.read(buf)
    }

    /// Read deinterleaved (planar) PCM samples.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        self.inner.read_planar(output)
    }

    /// Seek to position.
    ///
    /// # Errors
    ///
    /// Returns an error if the seek position is out of range or the underlying
    /// stream does not support seeking.
    pub fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        self.inner.seek(position)
    }

    /// Set the target sample rate of the audio host.
    ///
    /// Updates the audio pipeline's host sample rate for resampling.
    /// Can be called at any time to reflect host sample rate changes.
    pub fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        self.inner.set_host_sample_rate(sample_rate);
    }

    /// Set the playback rate for pitch-shifted speed control.
    ///
    /// Updates the resampler's ratio and timeline scaling.
    /// Rate > 1.0 speeds up playback, rate < 1.0 slows it down.
    pub fn set_playback_rate(&self, rate: f32) {
        self.inner.set_playback_rate(rate);
    }

    /// Update the scheduling priority hint for the shared worker.
    pub fn set_service_class(&self, class: ServiceClass) {
        self.inner.set_service_class(class);
    }

    /// Get current PCM specification.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    /// Source identifier for this resource.
    #[must_use]
    pub fn src(&self) -> &Arc<str> {
        &self.src
    }
    /// Subscribe to unified events.
    ///
    /// Returns a receiver for all events published to the bus,
    /// including audio, file, and HLS events.
    #[must_use]
    pub fn subscribe(&self) -> kithara_events::EventReceiver {
        self.bus.subscribe()
    }
}
