#![forbid(unsafe_code)]

//! Type-erased resource: unified wrapper over decoded audio streams.

use std::time::Duration;

use kithara_decode::{
    DecodeEvent, DecodeResult, Decoder, DecoderConfig, DecoderEvent, PcmReader, PcmSpec,
    TrackMetadata,
};
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
    inner: Box<dyn PcmReader>,
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
                let decoder_config = config.into_file_config();
                Self::from_file(decoder_config).await
            }
            #[cfg(feature = "hls")]
            SourceType::HlsStream(_) => {
                let decoder_config = config.into_hls_config()?;
                Self::from_hls(decoder_config).await
            }
        }
    }

    /// Create a resource from any `PcmReader`.
    ///
    /// Only decode events are forwarded. Use this for custom sources.
    pub fn from_reader(reader: impl PcmReader + 'static) -> Self {
        let (events_tx, _) = broadcast::channel(64);

        let forward_tx = events_tx.clone();
        let decode_rx = reader.decode_events();
        Self::spawn_decode_forward(decode_rx, forward_tx);

        Self {
            inner: Box::new(reader),
            events_tx,
        }
    }

    /// Create a resource from a file decoder config.
    ///
    /// Use this when you need to customize `FileConfig` or `DecodeOptions`
    /// beyond what `Resource::new()` provides.
    #[cfg(feature = "file")]
    pub async fn from_file(config: DecoderConfig<kithara_file::File>) -> DecodeResult<Self> {
        use kithara_stream::Stream;

        let decoder = Decoder::<Stream<kithara_file::File>>::new(config).await?;

        let (events_tx, _) = broadcast::channel(64);
        Self::spawn_typed_forward(decoder.events(), events_tx.clone());

        Ok(Self {
            inner: Box::new(decoder),
            events_tx,
        })
    }

    /// Create a resource from an HLS decoder config.
    ///
    /// Use this when you need to customize `HlsConfig`, ABR, keys, etc.
    #[cfg(feature = "hls")]
    pub async fn from_hls(config: DecoderConfig<kithara_hls::Hls>) -> DecodeResult<Self> {
        use kithara_stream::Stream;

        let decoder = Decoder::<Stream<kithara_hls::Hls>>::new(config).await?;

        let (events_tx, _) = broadcast::channel(64);
        Self::spawn_typed_forward(decoder.events(), events_tx.clone());

        Ok(Self {
            inner: Box::new(decoder),
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

    // -- Internal helpers -----------------------------------------------------

    fn spawn_decode_forward(
        mut decode_rx: broadcast::Receiver<DecodeEvent>,
        forward_tx: broadcast::Sender<ResourceEvent>,
    ) {
        tokio::spawn(async move {
            loop {
                match decode_rx.recv().await {
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
        mut typed_rx: broadcast::Receiver<DecoderEvent<E>>,
        forward_tx: broadcast::Sender<ResourceEvent>,
    ) where
        E: Clone + Send + 'static,
        ResourceEvent: From<DecoderEvent<E>>,
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

// -- rodio::Source implementation ----------------------------------------------

#[cfg(feature = "rodio")]
impl Iterator for Resource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.inner.is_eof() {
            return None;
        }
        let mut sample = [0.0f32; 1];
        if self.inner.read(&mut sample) == 1 {
            Some(sample[0])
        } else {
            None
        }
    }
}

#[cfg(feature = "rodio")]
impl rodio::Source for Resource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.inner.spec().channels
    }

    fn sample_rate(&self) -> u32 {
        self.inner.spec().sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.duration()
    }
}
