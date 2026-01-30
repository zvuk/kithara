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
    pub fn from_reader(reader: impl PcmReader + 'static) -> Self {
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
    pub async fn from_file(config: AudioConfig<kithara_file::File>) -> DecodeResult<Self> {
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
    pub async fn from_hls(config: AudioConfig<kithara_hls::Hls>) -> DecodeResult<Self> {
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
