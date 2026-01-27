//! Generic decoder that runs in a separate thread using SyncWorker.

use std::io::{Read, Seek};

use kanal::Receiver;
use kithara_stream::{MediaInfo, Stream, StreamType};
use kithara_worker::{Fetch, SyncWorker, SyncWorkerSource};
use tracing::{debug, trace, warn};

use crate::{
    symphonia_decoder::SymphoniaDecoder,
    types::{DecodeError, DecodeResult, PcmChunk},
};

#[cfg(feature = "rodio")]
use crate::types::PcmSpec;

/// Command for decode worker.
#[derive(Debug)]
pub enum DecodeCommand {
    // Future: Seek(Duration), Stop, etc.
}

/// Decode source implementing SyncWorkerSource.
struct DecodeSource {
    decoder: SymphoniaDecoder,
    chunks_decoded: u64,
    total_samples: u64,
}

impl DecodeSource {
    fn new(decoder: SymphoniaDecoder) -> Self {
        Self {
            decoder,
            chunks_decoded: 0,
            total_samples: 0,
        }
    }
}

impl SyncWorkerSource for DecodeSource {
    type Chunk = PcmChunk<f32>;
    type Command = DecodeCommand;

    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        loop {
            match self.decoder.next_chunk() {
                Ok(Some(chunk)) => {
                    if chunk.pcm.is_empty() {
                        continue;
                    }

                    self.chunks_decoded += 1;
                    self.total_samples += chunk.pcm.len() as u64;

                    if self.chunks_decoded.is_multiple_of(100) {
                        trace!(
                            chunks = self.chunks_decoded,
                            samples = self.total_samples,
                            spec = ?chunk.spec,
                            "decode progress"
                        );
                    }

                    return Fetch::new(chunk, false, 0);
                }
                Ok(None) => {
                    debug!(
                        chunks = self.chunks_decoded,
                        samples = self.total_samples,
                        "decode complete (EOF)"
                    );
                    return Fetch::new(PcmChunk::default(), true, 0);
                }
                Err(e) => {
                    warn!(?e, "decode error, attempting to continue");
                    continue;
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: Self::Command) {
        match cmd {
            // Future: handle seek, etc.
        }
    }
}

/// Generic audio decoder running in a separate thread.
///
/// Uses SyncWorker from kithara-worker for consistent patterns.
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
    /// Command sender (for future seek support)
    _cmd_tx: kanal::Sender<DecodeCommand>,

    /// PCM chunk receiver
    pcm_rx: Receiver<Fetch<PcmChunk<f32>>>,

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
    /// Command channel capacity.
    pub command_channel_capacity: usize,
    /// Optional format hint (file extension like "mp3", "wav")
    pub hint: Option<String>,
    /// Media info hint for format detection
    pub media_info: Option<MediaInfo>,
}

impl DecodeOptions {
    /// Create default options.
    pub fn new() -> Self {
        Self {
            pcm_buffer_chunks: 10,
            command_channel_capacity: 4,
            hint: None,
            media_info: None,
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
}

impl<S> Decoder<S>
where
    S: Read + Seek + Send + Sync + 'static,
{
    /// Create a new decoder from any Read + Seek source.
    ///
    /// Spawns SyncWorker in a blocking thread for decoding.
    pub fn from_source(source: S, options: DecodeOptions) -> DecodeResult<Self> {
        let cmd_capacity = options.command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = kanal::bounded(cmd_capacity);
        let (data_tx, data_rx) = kanal::bounded(options.pcm_buffer_chunks.max(1));

        // Create Symphonia decoder
        let symphonia = Self::create_decoder(source, &options)?;
        let decode_source = DecodeSource::new(symphonia);

        // Create and spawn SyncWorker
        let worker = SyncWorker::new(decode_source, cmd_rx, data_tx);

        std::thread::Builder::new()
            .name("kithara-decode".to_string())
            .spawn(move || {
                worker.run_blocking();
            })
            .map_err(|e| {
                DecodeError::Io(std::io::Error::other(format!(
                    "failed to spawn decode thread: {}",
                    e
                )))
            })?;

        Ok(Self {
            _cmd_tx: cmd_tx,
            pcm_rx: data_rx,
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
    pub fn pcm_rx(&self) -> &Receiver<Fetch<PcmChunk<f32>>> {
        &self.pcm_rx
    }

    /// Create Symphonia decoder with appropriate method based on options.
    fn create_decoder(source: S, options: &DecodeOptions) -> DecodeResult<SymphoniaDecoder> {
        if let Some(ref media_info) = options.media_info {
            SymphoniaDecoder::new_from_media_info(source, media_info)
        } else {
            SymphoniaDecoder::new_with_probe(source, options.hint.as_deref())
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
    /// let config = DecoderConfig::<Hls>::new(hls_config);
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
            Ok(fetch) => {
                if fetch.is_eof() {
                    debug!("Decoder: received EOF");
                    self.eof = true;
                    return false;
                }

                let chunk = fetch.into_inner();
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
        let _decoder = Decoder::from_source(cursor, options).unwrap();
    }

    #[test]
    fn test_decoder_receive_chunks() {
        let wav_data = create_test_wav(1000);
        let cursor = Cursor::new(wav_data);

        let options = DecodeOptions::new().with_hint("wav");
        let decoder = Decoder::from_source(cursor, options).unwrap();

        let mut chunk_count = 0;
        while let Ok(fetch) = decoder.pcm_rx().recv() {
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
