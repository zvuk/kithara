//! Type-erased resource: unified wrapper over decoded audio streams.

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
    bus: EventBus,
    src: Arc<str>,
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
            #[cfg(feature = "file")]
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                let audio_config = config.into_file_config();
                Self::from_stream_audio(audio_config, src).await
            }
            #[cfg(feature = "hls")]
            SourceType::HlsStream(_) => {
                let audio_config = config.into_hls_config()?;
                Self::from_stream_audio(audio_config, src).await
            }
        }
    }

    /// Create a resource from any `PcmReader`.
    ///
    /// The resource shares the reader's event bus directly.
    /// Use this for custom sources.
    #[cfg_attr(not(any(test, feature = "test-utils")), expect(dead_code))]
    pub(crate) fn from_reader(reader: impl PcmReader + 'static) -> Self {
        let bus = reader.event_bus().clone();
        let mut inner: Box<dyn PcmReader> = Box::new(reader);
        // Player resources are consumed from the audio render thread,
        // which must never block: flip the reader into non-blocking mode
        // immediately. Callers that want to wait for the first decoded
        // chunk still use `Resource::preload()` explicitly. We ignore
        // preload errors here — they surface again on the first
        // `read()` / `next_chunk()` call.
        let _ = inner.preload();
        Self {
            inner,
            bus,
            src: Arc::from("unknown"),
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
        // Extract existing bus from stream config or fall back to the
        // top-level `AudioConfig.bus`; create a fresh one when neither is set.
        let bus = T::event_bus(&config.stream)
            .or_else(|| config.bus.clone())
            .unwrap_or_default();
        // Ensure downstream `Audio::new()` resolves the same bus even
        // when the stream-level field was empty.
        config.bus = Some(bus.clone());

        let mut audio = Audio::<Stream<T>>::new(config).await?;
        // Always non-blocking for the player render thread; see
        // `from_reader` for rationale. Preload failures surface again
        // on the first `read()` / `next_chunk()` call — no need to
        // propagate here.
        let _ = audio.preload();
        Ok(Self {
            inner: Box::new(audio),
            bus,
            src,
        })
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

    /// Get a reference to the underlying `EventBus`.
    ///
    /// Useful for passing to downstream components that also publish events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
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

    /// Read the next decoded chunk with full metadata.
    ///
    /// # Errors
    /// Propagated from the underlying `PcmReader` on decoder / channel failure.
    pub fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.inner.next_chunk()
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

    /// Get current PCM specification.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    /// Get current playback position.
    #[must_use]
    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// Get total duration (if known).
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    /// Get track metadata.
    #[must_use]
    pub fn metadata(&self) -> &TrackMetadata {
        self.inner.metadata()
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

    /// Runtime ABR handle for adaptive sources (HLS). `None` for files.
    #[must_use]
    pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.inner.abr_handle()
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
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test mock code; values are small and positive by construction"
)]
mod tests {
    use kithara_audio::{ReadOutcome, mock::TestPcmReader};
    use kithara_decode::PcmSpec;
    use kithara_events::{AudioEvent, AudioFormat, Event, EventBus};
    use kithara_platform::{time, time::Duration};
    use kithara_test_utils::kithara;

    use super::Resource;

    fn mock_spec() -> PcmSpec {
        PcmSpec {
            channels: 2,
            sample_rate: 44100,
        }
    }

    fn make_resource() -> Resource {
        Resource::from_reader(TestPcmReader::new(mock_spec(), 1.0))
    }

    fn make_resource_with_bus() -> (Resource, EventBus) {
        let reader = TestPcmReader::new(mock_spec(), 1.0);
        let bus = reader.event_bus().clone();
        let resource = Resource::from_reader(reader);
        (resource, bus)
    }

    #[derive(Clone, Copy)]
    enum ReadMode {
        Interleaved,
        Planar,
    }

    #[kithara::test(tokio)]
    #[case(ReadMode::Interleaved)]
    #[case(ReadMode::Planar)]
    async fn test_resource_from_reader_read_variants(#[case] mode: ReadMode) {
        let mut resource = make_resource();
        match mode {
            ReadMode::Interleaved => {
                let mut buf = [0.0f32; 64];
                let outcome = resource.read(&mut buf).expect("read");
                let ReadOutcome::Frames { count, .. } = outcome else {
                    panic!("expected Frames, got {outcome:?}");
                };
                assert_eq!(count, 64);
                for sample in &buf[..count] {
                    assert!((sample - 0.5).abs() < f32::EPSILON);
                }
            }
            ReadMode::Planar => {
                let mut ch0 = [0.0f32; 32];
                let mut ch1 = [0.0f32; 32];
                let mut output: Vec<&mut [f32]> = vec![&mut ch0, &mut ch1];
                let outcome = resource.read_planar(&mut output).expect("read_planar");
                let ReadOutcome::Frames { count, .. } = outcome else {
                    panic!("expected Frames, got {outcome:?}");
                };
                assert_eq!(count, 32);
                for &s in &ch0[..count] {
                    assert!((s - 0.5).abs() < f32::EPSILON);
                }
                for &s in &ch1[..count] {
                    assert!((s - 0.5).abs() < f32::EPSILON);
                }
            }
        }
    }

    #[kithara::test(tokio)]
    async fn test_resource_from_reader_spec() {
        let resource = make_resource();
        let spec = resource.spec();
        assert_eq!(spec.sample_rate, 44100);
        assert_eq!(spec.channels, 2);
    }

    #[kithara::test(tokio)]
    async fn test_resource_from_reader_position_and_duration() {
        let resource = make_resource();
        assert_eq!(resource.position(), Duration::ZERO);
        let dur = resource.duration().unwrap();
        // 1.0 second at 44100 Hz
        assert!((dur.as_secs_f64() - 1.0).abs() < 0.001);
    }

    #[kithara::test(tokio)]
    async fn test_resource_from_reader_seek() {
        let mut resource = make_resource();
        assert_eq!(resource.position(), Duration::ZERO);

        let outcome = resource.seek(Duration::from_millis(500)).expect("seek");
        assert!(matches!(outcome, kithara_audio::SeekOutcome::Landed { .. }));
        let pos = resource.position();
        assert!((pos.as_secs_f64() - 0.5).abs() < 0.001);
    }

    #[kithara::test(tokio)]
    async fn test_resource_from_reader_reads_until_eof() {
        let mut resource = make_resource();

        // Read all samples: 44100 frames * 2 channels = 88200 samples
        let mut buf = [0.0f32; 4096];
        let saw_eof = loop {
            match resource.read(&mut buf).expect("read") {
                ReadOutcome::Frames { count: 0, .. } => break false,
                ReadOutcome::Frames { .. } => continue,
                ReadOutcome::Eof { .. } => break true,
            }
        };
        assert!(
            saw_eof,
            "reader must reach natural EOF after consuming all samples"
        );
    }

    #[kithara::test(tokio)]
    async fn test_resource_subscribe_receives_events() {
        let (resource, bus) = make_resource_with_bus();
        let mut rx = resource.subscribe();

        // Publish an AudioEvent through the shared bus directly.
        let spec = mock_spec();
        let format = AudioFormat::new(spec.channels, spec.sample_rate);
        bus.publish(AudioEvent::FormatDetected { spec: format });

        let event = time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(
            matches!(event, Event::Audio(AudioEvent::FormatDetected { spec: s }) if s == format)
        );
    }

    #[kithara::test(tokio)]
    async fn test_resource_metadata() {
        let resource = make_resource();
        let meta = resource.metadata();
        assert_eq!(meta.title.as_deref(), Some("Mock"));
        assert!(meta.artwork.is_none());
    }
}
