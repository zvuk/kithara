#![forbid(unsafe_code)]

//! Type-erased resource: unified wrapper over decoded audio streams.

use std::{num::NonZeroU32, time::Duration};

use kithara_audio::{Audio, AudioConfig, AudioEvent, AudioPipelineEvent, PcmReader};
use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
use tokio::sync::broadcast;

use crate::{config::ResourceConfig, events::ResourceEvent, source_type::SourceType};

// -- Resource -----------------------------------------------------------------

/// Type-erased audio resource wrapping any `PcmReader`.
///
/// Provides a unified interface for reading decoded PCM audio
/// regardless of the underlying source (file, HLS, custom).
///
/// # Example
///
/// ```ignore
/// use kithara::{Resource, ResourceConfig};
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
    events_tx: broadcast::Sender<ResourceEvent>,
}

impl Resource {
    /// Create a resource from a `ResourceConfig`.
    ///
    /// Auto-detects the stream type from the URL:
    /// - URLs ending with `.m3u8` -> HLS stream
    /// - All other URLs -> progressive file download
    pub async fn new(config: ResourceConfig) -> DecodeResult<Self> {
        let source_type = SourceType::detect(&config.src)?;
        match source_type {
            #[cfg(feature = "file")]
            SourceType::RemoteFile(_) | SourceType::LocalFile(_) => {
                let audio_config = config.into_file_config();
                Self::from_file(audio_config).await
            }
            #[cfg(feature = "hls")]
            SourceType::HlsStream(_) => {
                let audio_config = config.into_hls_config()?;
                Self::from_hls(audio_config).await
            }
        }
    }

    /// Create a resource from any `PcmReader`.
    ///
    /// Only audio events are forwarded. Use this for custom sources.
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn from_reader(reader: impl PcmReader + 'static) -> Self {
        let (events_tx, _) = broadcast::channel(64);

        let forward_tx = events_tx.clone();
        let decode_rx = reader.decode_events();
        Self::spawn_audio_forward(decode_rx, forward_tx);

        Self {
            inner: Box::new(reader),
            events_tx,
        }
    }

    /// Create a resource from a file audio config.
    ///
    /// Use this when you need to customize `FileConfig` or `AudioConfig`
    /// beyond what `Resource::new()` provides.
    #[cfg(feature = "file")]
    pub(crate) async fn from_file(config: AudioConfig<kithara_file::File>) -> DecodeResult<Self> {
        use kithara_stream::Stream;

        let audio = Audio::<Stream<kithara_file::File>>::new(config).await?;

        let (events_tx, _) = broadcast::channel(64);
        Self::spawn_typed_forward(audio.events(), events_tx.clone());

        Ok(Self {
            inner: Box::new(audio),
            events_tx,
        })
    }

    /// Create a resource from an HLS audio config.
    ///
    /// Use this when you need to customize `HlsConfig`, ABR, keys, etc.
    #[cfg(feature = "hls")]
    pub(crate) async fn from_hls(config: AudioConfig<kithara_hls::Hls>) -> DecodeResult<Self> {
        use kithara_stream::Stream;

        let audio = Audio::<Stream<kithara_hls::Hls>>::new(config).await?;

        let (events_tx, _) = broadcast::channel(64);
        Self::spawn_typed_forward(audio.events(), events_tx.clone());

        Ok(Self {
            inner: Box::new(audio),
            events_tx,
        })
    }

    /// Subscribe to unified resource events.
    pub fn subscribe(&self) -> broadcast::Receiver<ResourceEvent> {
        self.events_tx.subscribe()
    }

    /// Read interleaved PCM samples.
    pub fn read(&mut self, buf: &mut [f32]) -> usize {
        self.inner.read(buf)
    }

    /// Read deinterleaved (planar) PCM samples.
    pub fn read_planar(&mut self, output: &mut [&mut [f32]]) -> usize {
        self.inner.read_planar(output)
    }

    /// Seek to position.
    pub fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        self.inner.seek(position)
    }

    /// Get current PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    /// Check if end of stream has been reached.
    pub fn is_eof(&self) -> bool {
        self.inner.is_eof()
    }

    /// Get current playback position.
    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// Get total duration (if known).
    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    /// Get track metadata.
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

    /// Wait for first decoded chunk to be available, then move it to internal buffer.
    ///
    /// After preload completes, the first `read()` returns data without blocking.
    /// Safe to call multiple times (no-op if already preloaded).
    pub async fn preload(&mut self) {
        if let Some(notify) = self.inner.preload_notify() {
            notify.notified().await;
        }
        self.inner.preload();
    }

    // -- Internal helpers -----------------------------------------------------

    fn spawn_audio_forward(
        mut audio_rx: broadcast::Receiver<AudioEvent>,
        forward_tx: broadcast::Sender<ResourceEvent>,
    ) {
        tokio::spawn(async move {
            loop {
                match audio_rx.recv().await {
                    Ok(event) => {
                        let _ = forward_tx.send(event.into());
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn spawn_typed_forward<E>(
        mut typed_rx: broadcast::Receiver<AudioPipelineEvent<E>>,
        forward_tx: broadcast::Sender<ResourceEvent>,
    ) where
        E: Clone + Send + 'static,
        ResourceEvent: From<AudioPipelineEvent<E>>,
    {
        tokio::spawn(async move {
            loop {
                match typed_rx.recv().await {
                    Ok(event) => {
                        let _ = forward_tx.send(event.into());
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_audio::{AudioEvent, PcmReader};
    use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
    use tokio::sync::broadcast;

    use super::Resource;

    // -- Mock PcmReader -----------------------------------------------------------

    /// A mock `PcmReader` for testing the Resource facade.
    ///
    /// Produces a constant sample value, tracks seek position,
    /// and exposes an `AudioEvent` sender for event forwarding tests.
    struct MockPcmReader {
        spec: PcmSpec,
        metadata: TrackMetadata,
        total_frames: u64,
        position_frames: u64,
        eof: bool,
        sample_value: f32,
        events_tx: broadcast::Sender<AudioEvent>,
    }

    impl MockPcmReader {
        fn new(spec: PcmSpec, duration_secs: f64) -> Self {
            let total_frames = (spec.sample_rate as f64 * duration_secs) as u64;
            let (events_tx, _) = broadcast::channel(64);
            Self {
                spec,
                metadata: TrackMetadata {
                    title: Some("Mock Track".to_owned()),
                    artist: Some("Mock Artist".to_owned()),
                    album: Some("Mock Album".to_owned()),
                    artwork: None,
                },
                total_frames,
                position_frames: 0,
                eof: false,
                sample_value: 0.5,
                events_tx,
            }
        }

        /// Get a clone of the event sender for sending mock events.
        fn events_sender(&self) -> broadcast::Sender<AudioEvent> {
            self.events_tx.clone()
        }

        fn frames_to_duration(&self, frames: u64) -> Duration {
            if self.spec.sample_rate == 0 {
                return Duration::ZERO;
            }
            Duration::from_secs_f64(frames as f64 / self.spec.sample_rate as f64)
        }
    }

    impl PcmReader for MockPcmReader {
        fn read(&mut self, buf: &mut [f32]) -> usize {
            if self.eof {
                return 0;
            }
            let channels = self.spec.channels as u64;
            if channels == 0 {
                return 0;
            }
            let remaining_samples = (self.total_frames - self.position_frames) * channels;
            let to_write = (buf.len() as u64).min(remaining_samples) as usize;
            for sample in &mut buf[..to_write] {
                *sample = self.sample_value;
            }
            let frames_advanced = to_write as u64 / channels;
            self.position_frames += frames_advanced;
            if self.position_frames >= self.total_frames {
                self.eof = true;
            }
            to_write
        }

        fn read_planar(&mut self, output: &mut [&mut [f32]]) -> usize {
            if self.eof || output.is_empty() {
                return 0;
            }
            let channels = self.spec.channels as usize;
            if channels == 0 || output.len() < channels {
                return 0;
            }
            let frames_per_channel = output[0].len();
            let remaining = (self.total_frames - self.position_frames) as usize;
            let frames_to_write = frames_per_channel.min(remaining);
            for ch in output.iter_mut().take(channels) {
                for sample in ch.iter_mut().take(frames_to_write) {
                    *sample = self.sample_value;
                }
            }
            self.position_frames += frames_to_write as u64;
            if self.position_frames >= self.total_frames {
                self.eof = true;
            }
            frames_to_write
        }

        fn seek(&mut self, position: Duration) -> DecodeResult<()> {
            let frame = (position.as_secs_f64() * self.spec.sample_rate as f64) as u64;
            self.position_frames = frame.min(self.total_frames);
            self.eof = self.position_frames >= self.total_frames;
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn is_eof(&self) -> bool {
            self.eof
        }

        fn position(&self) -> Duration {
            self.frames_to_duration(self.position_frames)
        }

        fn duration(&self) -> Option<Duration> {
            Some(self.frames_to_duration(self.total_frames))
        }

        fn metadata(&self) -> &TrackMetadata {
            &self.metadata
        }

        fn decode_events(&self) -> broadcast::Receiver<AudioEvent> {
            self.events_tx.subscribe()
        }
    }

    // -- Helpers ------------------------------------------------------------------

    fn mock_spec() -> PcmSpec {
        PcmSpec {
            sample_rate: 44100,
            channels: 2,
        }
    }

    fn make_resource() -> Resource {
        Resource::from_reader(MockPcmReader::new(mock_spec(), 1.0))
    }

    fn make_resource_with_sender() -> (Resource, broadcast::Sender<AudioEvent>) {
        let reader = MockPcmReader::new(mock_spec(), 1.0);
        let sender = reader.events_sender();
        let resource = Resource::from_reader(reader);
        (resource, sender)
    }

    // -- Tests --------------------------------------------------------------------

    #[tokio::test]
    async fn test_resource_from_reader_read() {
        let mut resource = make_resource();
        let mut buf = [0.0f32; 64];
        let n = resource.read(&mut buf);
        assert_eq!(n, 64);
        for sample in &buf[..n] {
            assert!((sample - 0.5).abs() < f32::EPSILON);
        }
    }

    #[tokio::test]
    async fn test_resource_from_reader_read_planar() {
        let mut resource = make_resource();
        let mut ch0 = [0.0f32; 32];
        let mut ch1 = [0.0f32; 32];
        let mut output: Vec<&mut [f32]> = vec![&mut ch0, &mut ch1];
        let frames = resource.read_planar(&mut output);
        assert_eq!(frames, 32);
        for &s in &ch0[..frames] {
            assert!((s - 0.5).abs() < f32::EPSILON);
        }
        for &s in &ch1[..frames] {
            assert!((s - 0.5).abs() < f32::EPSILON);
        }
    }

    #[tokio::test]
    async fn test_resource_from_reader_spec() {
        let resource = make_resource();
        let spec = resource.spec();
        assert_eq!(spec.sample_rate, 44100);
        assert_eq!(spec.channels, 2);
    }

    #[tokio::test]
    async fn test_resource_from_reader_position_and_duration() {
        let resource = make_resource();
        assert_eq!(resource.position(), Duration::ZERO);
        let dur = resource.duration().unwrap();
        // 1.0 second at 44100 Hz
        assert!((dur.as_secs_f64() - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_resource_from_reader_seek() {
        let mut resource = make_resource();
        assert_eq!(resource.position(), Duration::ZERO);

        resource.seek(Duration::from_millis(500)).unwrap();
        let pos = resource.position();
        assert!((pos.as_secs_f64() - 0.5).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_resource_from_reader_is_eof() {
        let mut resource = make_resource();
        assert!(!resource.is_eof());

        // Read all samples: 44100 frames * 2 channels = 88200 samples
        let mut buf = [0.0f32; 4096];
        loop {
            let n = resource.read(&mut buf);
            if n == 0 {
                break;
            }
        }
        assert!(resource.is_eof());
    }

    #[tokio::test]
    async fn test_resource_subscribe_receives_events() {
        let (resource, sender) = make_resource_with_sender();
        let mut rx = resource.subscribe();

        // Send an AudioEvent through the mock's broadcast channel.
        // The Resource's spawn_audio_forward task converts it to ResourceEvent.
        let spec = mock_spec();
        sender.send(AudioEvent::FormatDetected { spec }).unwrap();

        // Allow the forwarding task to run.
        tokio::task::yield_now().await;

        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(
            matches!(event, crate::events::ResourceEvent::FormatDetected { spec: s } if s == spec)
        );
    }

    #[tokio::test]
    async fn test_resource_metadata() {
        let resource = make_resource();
        let meta = resource.metadata();
        assert_eq!(meta.title.as_deref(), Some("Mock Track"));
        assert_eq!(meta.artist.as_deref(), Some("Mock Artist"));
        assert_eq!(meta.album.as_deref(), Some("Mock Album"));
        assert!(meta.artwork.is_none());
    }
}
