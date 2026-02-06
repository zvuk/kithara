//! Symphonia-based audio decoder backend.
//!
//! This module provides the generic [`Symphonia<C>`] decoder that implements
//! [`AudioDecoder`] for any codec type implementing [`CodecType`].
//!
//! # Direct Reader Creation (No Probe)
//!
//! When `ContainerFormat` is provided in config, the decoder creates the
//! appropriate format reader directly without probing. This is critical
//! for HLS streams where format is known from playlist metadata.
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::{Symphonia, SymphoniaConfig, SymphoniaMp3};
//! use kithara_decode::AudioDecoder;
//! use kithara_stream::ContainerFormat;
//!
//! let file = std::fs::File::open("audio.mp3")?;
//! let config = SymphoniaConfig {
//!     container: Some(ContainerFormat::MpegAudio),
//!     ..Default::default()
//! };
//! let decoder = SymphoniaMp3::create(file, config)?;
//! ```

use std::{
    io::{Read, Seek},
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_stream::ContainerFormat;
use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions},
    },
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
    io::MediaSourceStream,
    units::Time,
};
use symphonia::default::formats::{
    AdtsReader, FlacReader, IsoMp4Reader, MpaReader, OggReader, WavReader,
};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, AudioDecoder, CodecType, DecoderInput, Flac, Mp3, Vorbis},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Configuration for Symphonia-based decoders.
#[derive(Debug, Clone, Default)]
pub struct SymphoniaConfig {
    /// Enable data verification (slower but safer).
    pub verify: bool,
    /// Enable gapless playback.
    pub gapless: bool,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Container format for direct reader creation (no probe).
    ///
    /// When set, bypasses Symphonia's probe mechanism and creates
    /// the appropriate format reader directly. This is critical for
    /// HLS streams where the container format is known from playlist.
    pub container: Option<ContainerFormat>,
}

// ────────────────────────────────── Inner ──────────────────────────────────

/// Inner implementation shared across all Symphonia codecs.
///
/// This struct holds the actual Symphonia format reader and decoder,
/// encapsulating all the codec-agnostic decoding logic.
struct SymphoniaInner {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn SymphoniaAudioDecoder>,
    track_id: u32,
    spec: PcmSpec,
    position: Duration,
    duration: Option<Duration>,
    metadata: TrackMetadata,
    /// Handle for dynamic byte length updates.
    byte_len_handle: Arc<AtomicU64>,
}

impl SymphoniaInner {
    /// Create a new inner decoder from a Read + Seek source.
    ///
    /// When `container` is specified in config, creates the format reader
    /// directly without probing. Otherwise falls back to probe.
    fn new<R>(source: R, config: &SymphoniaConfig) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Create format reader directly - container MUST be specified
        let container = config.container.ok_or(DecodeError::InvalidData(
            "Container format must be specified".to_string(),
        ))?;

        // Disable seek during initialization for ALL containers.
        // Some format readers (IsoMp4Reader) try to seek to end looking for atoms,
        // which breaks streaming scenarios. After initialization, seek is enabled.
        let adapter = ReadSeekAdapter::new_seek_disabled(source);

        let byte_len_handle = if let Some(ref handle) = config.byte_len_handle {
            Arc::clone(handle)
        } else {
            adapter.byte_len_handle()
        };

        // Keep handle to re-enable seek after initialization
        let seek_enabled_handle = adapter.seek_enabled_handle();

        let mss = MediaSourceStream::new(Box::new(adapter), Default::default());

        // Format options
        let format_opts = FormatOptions {
            enable_gapless: config.gapless,
            ..Default::default()
        };

        tracing::debug!(?container, "Creating format reader directly (no probe)");
        let format_reader = Self::create_reader_for_container(mss, container, format_opts)?;

        // Re-enable seek after successful initialization
        seek_enabled_handle.store(true, Ordering::Release);
        tracing::debug!("Re-enabled seek after decoder initialization");

        Self::init_from_reader(format_reader, config, byte_len_handle)
    }

    /// Create format reader directly based on container format (no probe).
    fn create_reader_for_container(
        mss: MediaSourceStream<'static>,
        container: ContainerFormat,
        format_opts: FormatOptions,
    ) -> DecodeResult<Box<dyn FormatReader>> {
        match container {
            ContainerFormat::Fmp4 => {
                let reader = IsoMp4Reader::try_new(mss, format_opts)
                    .map_err(|e| DecodeError::Backend(Box::new(e)))?;
                Ok(Box::new(reader))
            }
            ContainerFormat::MpegAudio => {
                let reader = MpaReader::try_new(mss, format_opts)
                    .map_err(|e| DecodeError::Backend(Box::new(e)))?;
                Ok(Box::new(reader))
            }
            ContainerFormat::Adts => {
                let reader = AdtsReader::try_new(mss, format_opts)
                    .map_err(|e| DecodeError::Backend(Box::new(e)))?;
                Ok(Box::new(reader))
            }
            ContainerFormat::Flac => {
                let reader = FlacReader::try_new(mss, format_opts)
                    .map_err(|e| DecodeError::Backend(Box::new(e)))?;
                Ok(Box::new(reader))
            }
            ContainerFormat::Wav => {
                let reader = WavReader::try_new(mss, format_opts)
                    .map_err(|e| DecodeError::Backend(Box::new(e)))?;
                Ok(Box::new(reader))
            }
            ContainerFormat::Ogg => {
                let reader = OggReader::try_new(mss, format_opts)
                    .map_err(|e| DecodeError::Backend(Box::new(e)))?;
                Ok(Box::new(reader))
            }
            ContainerFormat::MpegTs => {
                // MPEG-TS not directly supported by Symphonia
                Err(DecodeError::UnsupportedContainer(container))
            }
            ContainerFormat::Caf => {
                // CAF requires separate feature, fall back to probe
                tracing::warn!("CAF container - falling back to probe");
                Err(DecodeError::UnsupportedContainer(container))
            }
            ContainerFormat::Mkv => {
                // MKV requires separate feature, fall back to probe
                tracing::warn!("MKV container - falling back to probe");
                Err(DecodeError::UnsupportedContainer(container))
            }
        }
    }

    /// Initialize decoder from an already-created format reader.
    fn init_from_reader(
        format_reader: Box<dyn FormatReader>,
        config: &SymphoniaConfig,
        byte_len_handle: Arc<AtomicU64>,
    ) -> DecodeResult<Self> {
        // Find audio track
        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::ProbeFailed)?
            .clone();

        let track_id = track.id;

        // Extract codec parameters
        let codec_params = match &track.codec_params {
            Some(CodecParameters::Audio(params)) => params.clone(),
            _ => return Err(DecodeError::ProbeFailed),
        };

        // Extract PCM spec
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| DecodeError::InvalidData("No sample rate".to_string()))?;
        let channels = codec_params
            .channels
            .as_ref()
            .map(|c| c.count() as u16)
            .unwrap_or(2);
        let spec = PcmSpec {
            sample_rate,
            channels,
        };

        // Create decoder
        let decoder_opts = AudioDecoderOptions {
            verify: config.verify,
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&codec_params, &decoder_opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        // Calculate duration if available
        let duration = Self::calculate_duration(&track);

        // Extract metadata (will be populated from format_reader later if available)
        let metadata = TrackMetadata::default();

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
            position: Duration::ZERO,
            duration,
            metadata,
            byte_len_handle,
        })
    }

    /// Calculate duration from track metadata.
    fn calculate_duration(track: &symphonia::core::formats::Track) -> Option<Duration> {
        let num_frames = track.num_frames?;
        let time_base = track.time_base?;
        let time = time_base.calc_time(num_frames);
        Some(Duration::new(
            time.seconds,
            (time.frac * 1_000_000_000.0) as u32,
        ))
    }

    /// Decode the next chunk of PCM data.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        loop {
            // Read next packet
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None), // EOF
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    tracing::debug!("Treating UnexpectedEof as EOF");
                    return Ok(None);
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };

            // Skip packets from other tracks
            if packet.track_id() != self.track_id {
                continue;
            }

            // Decode packet
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(_)) => {
                    // Skip bad packets
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };

            // Extract audio info
            let spec = decoded.spec();
            let channels = spec.channels().count();
            let num_samples = decoded.samples_interleaved();

            if num_samples == 0 {
                continue;
            }

            // Convert to f32 interleaved
            let mut pcm = vec![0.0f32; num_samples];
            decoded.copy_to_slice_interleaved(&mut pcm);

            let pcm_spec = PcmSpec {
                sample_rate: spec.rate(),
                channels: channels as u16,
            };

            let chunk = PcmChunk::new(pcm_spec, pcm);

            // Update position
            if self.spec.sample_rate > 0 {
                let frames = chunk.frames();
                let frame_duration =
                    Duration::from_secs_f64(frames as f64 / self.spec.sample_rate as f64);
                self.position = self.position.saturating_add(frame_duration);
            }

            return Ok(Some(chunk));
        }
    }

    /// Seek to a time position.
    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        let seek_to = SeekTo::Time {
            time: Time::new(pos.as_secs(), pos.subsec_nanos() as f64 / 1_000_000_000.0),
            track_id: Some(self.track_id),
        };

        tracing::debug!(
            position_secs = pos.as_secs_f64(),
            track_id = self.track_id,
            "sending seek to symphonia"
        );

        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| DecodeError::SeekFailed(e.to_string()))?;

        self.decoder.reset();
        self.position = pos;
        Ok(())
    }

    /// Update the byte length reported to symphonia's MediaSource.
    fn update_byte_len(&self, len: u64) {
        self.byte_len_handle.store(len, Ordering::Release);
    }
}

// ────────────────────────────────── Symphonia<C> ──────────────────────────────────

/// Generic Symphonia-based decoder parameterized by codec type.
///
/// This decoder wraps Symphonia's format reader and codec decoder,
/// providing a unified interface for all supported audio codecs.
///
/// # Type Parameters
///
/// * `C` - A codec type implementing [`CodecType`], such as [`Mp3`], [`Aac`], or [`Flac`].
///
/// # Example
///
/// ```ignore
/// use kithara_decode::symphonia::SymphoniaMp3;
/// use kithara_decode::traits::AudioDecoder;
///
/// let decoder = SymphoniaMp3::create(file, SymphoniaConfig::default())?;
/// while let Some(chunk) = decoder.next_chunk()? {
///     // Process PCM samples
/// }
/// ```
pub struct Symphonia<C: CodecType> {
    inner: SymphoniaInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> AudioDecoder for Symphonia<C> {
    type Config = SymphoniaConfig;
    type Source = Box<dyn DecoderInput>;

    fn create(source: Self::Source, config: Self::Config) -> DecodeResult<Self>
    where
        Self: Sized,
    {
        let inner = SymphoniaInner::new(source, &config)?;

        Ok(Self {
            inner,
            _codec: PhantomData,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn position(&self) -> Duration {
        self.inner.position
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }
}

// ────────────────────────────────── InnerDecoder ──────────────────────────────────

use crate::traits::InnerDecoder;

impl<C: CodecType> InnerDecoder for Symphonia<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        AudioDecoder::next_chunk(self)
    }

    fn spec(&self) -> PcmSpec {
        AudioDecoder::spec(self)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        AudioDecoder::seek(self, pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.update_byte_len(len);
    }

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }

    fn reset(&mut self) {
        self.inner.decoder.reset();
    }
}

// ────────────────────────────────── Type Aliases ──────────────────────────────────

/// Symphonia-based AAC decoder.
pub type SymphoniaAac = Symphonia<Aac>;

/// Symphonia-based MP3 decoder.
pub type SymphoniaMp3 = Symphonia<Mp3>;

/// Symphonia-based FLAC decoder.
pub type SymphoniaFlac = Symphonia<Flac>;

/// Symphonia-based Vorbis decoder.
pub type SymphoniaVorbis = Symphonia<Vorbis>;

// ────────────────────────────────── ReadSeekAdapter ──────────────────────────────────

/// Adapter that wraps a Read + Seek source as a Symphonia MediaSource.
///
/// This adapter tracks the byte length dynamically, allowing it to be
/// updated after construction (useful for HLS streams where the total
/// length becomes known later).
struct ReadSeekAdapter<R> {
    inner: R,
    /// Dynamic byte length. Updated externally via `Arc<AtomicU64>`.
    /// 0 means unknown (returns None from byte_len()).
    byte_len: Arc<AtomicU64>,
    /// Controls whether seek operations are allowed.
    /// Used to temporarily disable seeking during fMP4 reader initialization
    /// (prevents IsoMp4Reader from seeking to end looking for moov atom).
    seek_enabled: Arc<AtomicBool>,
}

impl<R: Seek> ReadSeekAdapter<R> {
    /// Create adapter with seek initially disabled.
    ///
    /// Seek is disabled during decoder initialization to prevent format readers
    /// from seeking to end of stream looking for metadata atoms.
    /// Call `seek_enabled_handle().store(true, ...)` after initialization.
    fn new_seek_disabled(mut inner: R) -> Self {
        let byte_len = Arc::new(AtomicU64::new(0));
        let seek_enabled = Arc::new(AtomicBool::new(false));
        // Try to probe the byte length statically (works for Cursor, File, etc.)
        if let Some(len) = Self::probe_byte_len(&mut inner) {
            byte_len.store(len, Ordering::Release);
        }
        Self {
            inner,
            byte_len,
            seek_enabled,
        }
    }

    fn byte_len_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.byte_len)
    }

    fn seek_enabled_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.seek_enabled)
    }

    /// Probe stream length via `Seek::seek(End(0))` + restore position.
    fn probe_byte_len(reader: &mut R) -> Option<u64> {
        let current = reader.stream_position().ok()?;
        let end = reader.seek(std::io::SeekFrom::End(0)).ok()?;
        reader.seek(std::io::SeekFrom::Start(current)).ok()?;
        Some(end)
    }
}

impl<R: Read> Read for ReadSeekAdapter<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for ReadSeekAdapter<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<R: Read + Seek + Send + Sync> symphonia::core::io::MediaSource for ReadSeekAdapter<R> {
    fn is_seekable(&self) -> bool {
        self.seek_enabled.load(Ordering::Acquire)
    }

    fn byte_len(&self) -> Option<u64> {
        let len = self.byte_len.load(Ordering::Acquire);
        if len > 0 { Some(len) } else { None }
    }
}

// ────────────────────────────────── Tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::traits::AudioDecoder;

    #[test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_none());
        assert!(config.container.is_none());
    }

    #[test]
    fn test_symphonia_config_with_container() {
        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Fmp4),
            ..Default::default()
        };
        assert_eq!(config.container, Some(ContainerFormat::Fmp4));
    }

    #[test]
    fn test_symphonia_types_exist() {
        fn _check_aac(_: SymphoniaAac) {}
        fn _check_mp3(_: SymphoniaMp3) {}
        fn _check_flac(_: SymphoniaFlac) {}
        fn _check_vorbis(_: SymphoniaVorbis) {}
    }

    /// Create minimal valid WAV file (PCM 16-bit stereo, 44100Hz)
    fn create_test_wav(sample_count: usize, sample_rate: u32, channels: u16) -> Vec<u8> {
        let bytes_per_sample = 2; // 16-bit = 2 bytes
        let data_size = (sample_count * channels as usize * bytes_per_sample) as u32;
        let file_size = 36 + data_size;

        let mut wav = Vec::new();

        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        // fmt chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        let block_align = channels * bytes_per_sample as u16;
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        // data chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());

        // Generate simple sine wave samples
        for i in 0..sample_count {
            let sample = ((i as f32 * 0.1).sin() * 32767.0) as i16;
            for _ in 0..channels {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
        }

        wav
    }

    // Use PCM codec type for WAV files in tests
    use kithara_stream::AudioCodec;

    use crate::traits::CodecType;

    /// PCM codec marker for testing with WAV files.
    struct Pcm;
    impl CodecType for Pcm {
        const CODEC: AudioCodec = AudioCodec::Pcm;
    }

    type SymphoniaPcm = Symphonia<Pcm>;

    #[test]
    fn test_create_decoder_wav_with_container() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        // Create with explicit container format (no probe)
        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let decoder = SymphoniaPcm::create(Box::new(cursor), config);
        assert!(decoder.is_ok());

        let decoder = decoder.unwrap();
        assert_eq!(AudioDecoder::spec(&decoder).sample_rate, 44100);
        assert_eq!(AudioDecoder::spec(&decoder).channels, 2);
    }

    #[test]
    fn test_create_decoder_without_container_fails() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        // Create without container - should fail (container is required)
        let result = SymphoniaPcm::create(Box::new(cursor), SymphoniaConfig::default());
        assert!(result.is_err());
        assert!(matches!(result, Err(DecodeError::InvalidData(_))));
    }

    #[test]
    fn test_next_chunk_returns_data() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        let chunk = AudioDecoder::next_chunk(&mut decoder).unwrap();
        assert!(chunk.is_some());

        let chunk = chunk.unwrap();
        assert_eq!(chunk.spec.sample_rate, 44100);
        assert_eq!(chunk.spec.channels, 2);
        assert!(!chunk.pcm.is_empty());
    }

    #[test]
    fn test_next_chunk_eof() {
        let wav_data = create_test_wav(10, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        // Read all chunks
        while AudioDecoder::next_chunk(&mut decoder).unwrap().is_some() {}

        // Next call should return None (EOF)
        let result = AudioDecoder::next_chunk(&mut decoder).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_seek_to_beginning() {
        let wav_data = create_test_wav(10000, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        // Read some chunks
        let _ = AudioDecoder::next_chunk(&mut decoder).unwrap();
        let _ = AudioDecoder::next_chunk(&mut decoder).unwrap();

        // Seek to beginning
        AudioDecoder::seek(&mut decoder, Duration::from_secs(0)).unwrap();

        // Should be able to read again
        let chunk = AudioDecoder::next_chunk(&mut decoder).unwrap();
        assert!(chunk.is_some());
    }

    #[test]
    fn test_position_updates() {
        let wav_data = create_test_wav(44100, 44100, 2); // 1 second of audio
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        assert_eq!(AudioDecoder::position(&decoder), Duration::ZERO);

        // Read a chunk
        let _ = AudioDecoder::next_chunk(&mut decoder).unwrap();

        // Position should have advanced
        assert!(AudioDecoder::position(&decoder) > Duration::ZERO);
    }

    #[test]
    fn test_duration_available() {
        let wav_data = create_test_wav(44100, 44100, 2); // 1 second of audio
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        let duration = AudioDecoder::duration(&decoder);
        assert!(duration.is_some());

        // Should be approximately 1 second
        let dur = duration.unwrap();
        assert!(dur.as_secs_f64() > 0.9 && dur.as_secs_f64() < 1.1);
    }

    #[test]
    fn test_read_seek_adapter_byte_len() {
        use symphonia::core::io::MediaSource;

        let data = vec![0u8; 5000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new_seek_disabled(cursor);

        assert_eq!(adapter.byte_len(), Some(5000));
        // Seek is initially disabled
        assert!(!adapter.is_seekable());

        // Enable seek
        adapter.seek_enabled_handle().store(true, Ordering::Release);
        assert!(adapter.is_seekable());
    }

    #[test]
    fn test_read_seek_adapter_dynamic_update() {
        use symphonia::core::io::MediaSource;

        let data = vec![0u8; 1000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new_seek_disabled(cursor);
        let handle = adapter.byte_len_handle();

        // Override to simulate HLS case
        handle.store(0, Ordering::Release);
        assert_eq!(adapter.byte_len(), None);

        // Update with actual length
        handle.store(2000, Ordering::Release);
        assert_eq!(adapter.byte_len(), Some(2000));
    }

    #[test]
    fn test_config_with_custom_handle() {
        let handle = Arc::new(AtomicU64::new(12345));
        let config = SymphoniaConfig {
            verify: true,
            gapless: false,
            byte_len_handle: Some(Arc::clone(&handle)),
            container: Some(ContainerFormat::Fmp4),
        };

        assert!(config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_some());
        assert_eq!(config.container, Some(ContainerFormat::Fmp4));
    }

    #[test]
    fn test_empty_input_fails() {
        let empty = Vec::new();
        let cursor = Cursor::new(empty);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let result = SymphoniaPcm::create(Box::new(cursor), config);
        assert!(result.is_err());
    }

    #[test]
    fn test_corrupted_input_fails() {
        let corrupted = [0xDE, 0xAD, 0xBE, 0xEF].repeat(100);
        let cursor = Cursor::new(corrupted);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let result = SymphoniaPcm::create(Box::new(cursor), config);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_container_returns_error() {
        let data = vec![0u8; 100];
        let cursor = Cursor::new(data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::MpegTs),
            ..Default::default()
        };
        let result = SymphoniaPcm::create(Box::new(cursor), config);
        assert!(matches!(result, Err(DecodeError::UnsupportedContainer(_))));
    }
}
