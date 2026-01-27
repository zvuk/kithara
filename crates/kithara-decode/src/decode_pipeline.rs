//! Generic decoder that runs in a separate thread.
//!
//! Takes any `Read + Seek + Send` source and decodes to PCM via kanal channel.

use std::io::{Read, Seek};
use std::thread::JoinHandle;

use kanal::{Receiver, Sender};
use kithara_stream::{MediaInfo, Stream, StreamType};
use tracing::{debug, trace, warn};

use crate::{
    symphonia_mod::SymphoniaDecoder,
    types::{DecodeError, DecodeResult, PcmChunk},
};

#[cfg(feature = "rodio")]
use crate::types::PcmSpec;

/// Generic audio decoder running in a separate thread.
///
/// Takes any `Read + Seek + Send` source (HlsInner, FileInner, etc.)
/// and outputs PCM chunks via kanal bounded channel.
///
/// # Example
///
/// ```ignore
/// use kithara_decode::Decoder;
/// use kithara_hls::{Hls, HlsConfig};
/// use kithara_stream::Stream;
///
/// // Create decoder from config
/// let config = DecoderConfig::<Hls>::new(hls_config);
/// let decoder = Decoder::new(config).await?;
///
/// // Or read PCM from channel directly
/// while let Ok(chunk) = decoder.pcm_rx().recv() {
///     play_audio(chunk);
/// }
/// ```
pub struct Decoder<S> {
    /// Decode thread handle
    decode_thread: Option<JoinHandle<()>>,

    /// PCM chunk receiver
    pcm_rx: Receiver<PcmChunk<f32>>,

    /// Current audio specification (updated from chunks).
    #[cfg(feature = "rodio")]
    spec: PcmSpec,

    /// Current chunk being read.
    #[cfg(feature = "rodio")]
    current_chunk: Option<Vec<f32>>,

    /// Current position in chunk.
    #[cfg(feature = "rodio")]
    chunk_offset: usize,

    /// End of stream reached.
    #[cfg(feature = "rodio")]
    eof: bool,

    /// Marker for source type
    _marker: std::marker::PhantomData<S>,
}

/// Decode-specific options.
#[derive(Debug, Clone, Default)]
pub struct DecodeOptions {
    /// PCM buffer size in chunks (~100ms per chunk = 10 chunks ≈ 1s)
    pub pcm_buffer_chunks: usize,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Media info hint for format detection
    pub media_info: Option<MediaInfo>,
    /// Whether source is streaming (affects seek behavior)
    pub is_streaming: bool,
}

impl DecodeOptions {
    /// Create default options.
    pub fn new() -> Self {
        Self {
            pcm_buffer_chunks: 10,
            hint: None,
            media_info: None,
            is_streaming: false,
        }
    }

    /// Create options for streaming source (HLS).
    pub fn streaming() -> Self {
        Self {
            is_streaming: true,
            ..Self::new()
        }
    }

    /// Set format hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Set media info.
    pub fn with_media_info(mut self, info: MediaInfo) -> Self {
        self.media_info = Some(info);
        self
    }
}

/// Configuration for decoder with stream config.
///
/// Generic over StreamType to include stream-specific configuration.
#[derive(Debug, Clone)]
pub struct DecoderConfig<T: StreamType> {
    /// Stream configuration (HlsConfig, FileConfig, etc.)
    pub stream: T::Config,
    /// Decode-specific options
    pub decode: DecodeOptions,
}

impl<T: StreamType> DecoderConfig<T> {
    /// Create config with stream config.
    pub fn new(stream: T::Config) -> Self {
        Self {
            stream,
            decode: DecodeOptions::new(),
        }
    }

    /// Set decode options.
    pub fn with_decode(mut self, decode: DecodeOptions) -> Self {
        self.decode = decode;
        self
    }

    /// Set format hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.decode.hint = Some(hint.into());
        self
    }

    /// Set streaming mode.
    pub fn streaming(mut self) -> Self {
        self.decode.is_streaming = true;
        self
    }
}

impl<S> Decoder<S>
where
    S: Read + Seek + Send + Sync + 'static,
{
    /// Create a new decoder from any Read + Seek source.
    ///
    /// Spawns a blocking thread for decoding.
    /// If called from within Tokio runtime, captures handle for async operations.
    pub fn from_source(source: S, options: DecodeOptions) -> DecodeResult<Self> {
        let buffer_chunks = if options.pcm_buffer_chunks == 0 {
            10
        } else {
            options.pcm_buffer_chunks
        };
        let (pcm_tx, pcm_rx) = kanal::bounded(buffer_chunks);

        // Try to capture tokio runtime handle (optional)
        let rt_handle = tokio::runtime::Handle::try_current().ok();

        let decode_thread = std::thread::Builder::new()
            .name("kithara-decode".to_string())
            .spawn(move || {
                // Enter tokio runtime context if available
                let _guard = rt_handle.as_ref().map(|h| h.enter());
                Self::decode_loop(source, pcm_tx, options);
            })
            .map_err(|e| {
    DecodeError::Io(std::io::Error::other(format!(
                    "failed to spawn decode thread: {}",
                    e
                )))
            })?;

        Ok(Self {
            decode_thread: Some(decode_thread),
            pcm_rx,
            #[cfg(feature = "rodio")]
            spec: PcmSpec {
                sample_rate: 44100,
                channels: 2,
            },
            #[cfg(feature = "rodio")]
            current_chunk: None,
            #[cfg(feature = "rodio")]
            chunk_offset: 0,
            #[cfg(feature = "rodio")]
            eof: false,
            _marker: std::marker::PhantomData,
        })
    }

    /// Get reference to PCM receiver.
    pub fn pcm_rx(&self) -> &Receiver<PcmChunk<f32>> {
        &self.pcm_rx
    }

    /// Clone the PCM receiver (for multiple consumers).
    pub fn pcm_rx_clone(&self) -> Receiver<PcmChunk<f32>> {
        self.pcm_rx.clone()
    }

    /// Check if decode thread is still running.
    pub fn is_running(&self) -> bool {
        self.decode_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Decode loop running in blocking thread.
    fn decode_loop(source: S, pcm_tx: Sender<PcmChunk<f32>>, options: DecodeOptions) {
        // Create Symphonia decoder
        let mut decoder = match Self::create_decoder(source, &options) {
            Ok(d) => d,
            Err(e) => {
                warn!(?e, "failed to create decoder");
                return;
            }
        };

        let mut chunks_decoded = 0u64;
        let mut total_samples = 0u64;

        loop {
            match decoder.next_chunk() {
                Ok(Some(chunk)) => {
                    if chunk.pcm.is_empty() {
                        continue;
                    }

                    chunks_decoded += 1;
                    total_samples += chunk.pcm.len() as u64;

                    if chunks_decoded.is_multiple_of(100) {
                        trace!(
                            chunks = chunks_decoded,
                            samples = total_samples,
                            spec = ?chunk.spec,
                            "decode progress"
                        );
                    }

                    // Send chunk to channel (blocks if buffer full - backpressure)
                    if pcm_tx.send(chunk).is_err() {
                        debug!("pcm channel closed, stopping decode");
                        break;
                    }
                }
                Ok(None) => {
                    debug!(
                        chunks = chunks_decoded,
                        samples = total_samples,
                        "decode complete (EOF)"
                    );
                    break;
                }
                Err(e) => {
                    warn!(?e, "decode error, attempting to continue");
                    continue;
                }
            }
        }
    }

    /// Create Symphonia decoder with appropriate method based on options.
    fn create_decoder(source: S, options: &DecodeOptions) -> DecodeResult<SymphoniaDecoder> {
        if let Some(ref media_info) = options.media_info {
            SymphoniaDecoder::new_from_media_info(source, media_info, options.is_streaming)
        } else {
            SymphoniaDecoder::new_with_probe(source, options.hint.as_deref())
        }
    }
}

impl<S> Drop for Decoder<S> {
    fn drop(&mut self) {
        // Close channel to signal decode thread to stop
        drop(self.pcm_rx.clone());

        // Wait for decode thread to finish
        if let Some(handle) = self.decode_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Specialized impl for Stream-based decoders.
///
/// Provides async constructor that creates Stream internally.
impl<T> Decoder<Stream<T>>
where
    T: StreamType,
    T::Inner: Read + Seek + Send + Sync + 'static,
{
    /// Create decoder from DecoderConfig.
    ///
    /// This is the target API for Stream sources.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = DecoderConfig::<Hls>::new(hls_config).streaming();
    /// let decoder = Decoder::new(config).await?;
    /// sink.append(decoder);
    /// ```
    pub async fn new(config: DecoderConfig<T>) -> Result<Self, DecodeError> {
        let stream = Stream::<T>::new(config.stream)
            .await
            .map_err(|e| DecodeError::Io(std::io::Error::other(e.to_string())))?;

        Self::from_source(stream, config.decode)
    }
}

// rodio::Source implementation for Decoder
#[cfg(feature = "rodio")]
impl<S> Decoder<S> {
    /// Receive next chunk from channel.
    fn fill_buffer(&mut self) -> bool {
        if self.eof {
            return false;
        }

        match self.pcm_rx.recv() {
            Ok(chunk) => {
                trace!(
                    samples = chunk.pcm.len(),
                    spec = ?chunk.spec,
                    "Decoder: received chunk"
                );
                self.spec = chunk.spec;
                self.current_chunk = Some(chunk.pcm);
                self.chunk_offset = 0;
                true
            }
            Err(_) => {
                debug!("Decoder: channel closed (EOF)");
                self.eof = true;
                false
            }
        }
    }
}

#[cfg(feature = "rodio")]
impl<S> Iterator for Decoder<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }

        // Try to get sample from current chunk
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            let sample = chunk[self.chunk_offset];
            self.chunk_offset += 1;
            return Some(sample);
        }

        // Chunk exhausted or no chunk - need more data
        if self.fill_buffer()
            && let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            let sample = chunk[self.chunk_offset];
            self.chunk_offset += 1;
            return Some(sample);
        }

        None
    }
}

#[cfg(feature = "rodio")]
impl<S> rodio::Source for Decoder<S> {
    fn current_span_len(&self) -> Option<usize> {
        if let Some(ref chunk) = self.current_chunk
            && self.chunk_offset < chunk.len()
        {
            return Some(chunk.len() - self.chunk_offset);
        }
        None
    }

    fn channels(&self) -> u16 {
        self.spec.channels
    }

    fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Create minimal valid WAV file
    fn create_test_wav(sample_count: usize) -> Vec<u8> {
        let channels = 2u16;
        let sample_rate = 44100u32;
        let bytes_per_sample = 2;
        let data_size = (sample_count * channels as usize * bytes_per_sample) as u32;
        let file_size = 36 + data_size;

        let mut wav = Vec::new();

        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels * bytes_per_sample as u16;
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());

        // Generate samples
        for i in 0..sample_count {
            let sample = ((i as f32 * 0.1).sin() * 32767.0) as i16;
            for _ in 0..channels {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
        }

        wav
    }

    #[test]
    fn test_decoder_from_source() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let decoder = Decoder::from_source(cursor, options).unwrap();

        assert!(decoder.is_running());
    }

    #[test]
    fn test_decoder_receive_chunks() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let decoder = Decoder::from_source(cursor, options).unwrap();

        let mut chunk_count = 0;
        while let Ok(chunk) = decoder.pcm_rx().recv() {
            chunk_count += 1;
            assert!(!chunk.pcm.is_empty());
            if chunk_count >= 5 {
                break;
            }
        }

        assert!(chunk_count > 0);
    }

    #[test]
    fn test_decode_options_streaming() {
        let options = DecodeOptions::streaming();
        assert!(options.is_streaming);
    }

    #[test]
    fn test_decode_options_with_media_info() {
        let info = MediaInfo::default()
            .with_container(kithara_stream::ContainerFormat::Wav)
            .with_sample_rate(44100);

        let options = DecodeOptions::new().with_media_info(info.clone());

        assert!(options.media_info.is_some());
        assert_eq!(
            options.media_info.unwrap().container,
            Some(kithara_stream::ContainerFormat::Wav)
        );
    }
}
