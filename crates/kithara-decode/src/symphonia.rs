//! Symphonia-based audio decoder backend.
//!
//! This module provides the generic [`Symphonia<C>`] decoder that implements
//! [`AudioDecoder`] for any codec type implementing [`CodecType`].
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::{Symphonia, SymphoniaConfig, SymphoniaMp3};
//! use kithara_decode::AudioDecoder;
//!
//! let file = std::fs::File::open("audio.mp3")?;
//! let decoder = SymphoniaMp3::create(file, SymphoniaConfig::default())?;
//! ```

use std::{
    io::{Read, Seek},
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions},
    },
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
    units::Time,
};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, AudioDecoder, CodecType, Flac, Mp3, Vorbis},
    types::{PcmChunk, PcmSpec},
};

/// Configuration for Symphonia-based decoders.
#[derive(Debug, Clone)]
pub struct SymphoniaConfig {
    /// Enable data verification (slower but safer).
    pub verify: bool,
    /// Enable gapless playback.
    pub gapless: bool,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

impl Default for SymphoniaConfig {
    fn default() -> Self {
        Self {
            verify: false,
            gapless: true,
            byte_len_handle: None,
        }
    }
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
    /// Handle for dynamic byte length updates.
    byte_len_handle: Arc<AtomicU64>,
}

impl SymphoniaInner {
    /// Create a new inner decoder from a Read + Seek source.
    fn new<R>(source: R, config: &SymphoniaConfig, hint: Option<&str>) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Wrap source in our adapter that tracks byte length
        let adapter = ReadSeekAdapter::new(source);
        let byte_len_handle = if let Some(ref handle) = config.byte_len_handle {
            Arc::clone(handle)
        } else {
            adapter.byte_len_handle()
        };

        let mss = MediaSourceStream::new(Box::new(adapter), Default::default());

        // Build probe hint
        let mut probe_hint = Hint::new();
        if let Some(ext) = hint {
            probe_hint.with_extension(ext);
        }

        // Format and metadata options
        let format_opts = FormatOptions {
            enable_gapless: config.gapless,
            ..Default::default()
        };
        let meta_opts = MetadataOptions::default();

        // Probe the format
        let format_reader = symphonia::default::get_probe()
            .probe(&probe_hint, mss, format_opts, meta_opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

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

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
            position: Duration::ZERO,
            duration,
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

impl<C: CodecType> Symphonia<C> {
    /// Get the file extension hint for this codec.
    fn codec_hint() -> Option<&'static str> {
        use kithara_stream::AudioCodec;

        match C::CODEC {
            AudioCodec::Mp3 => Some("mp3"),
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Some("aac"),
            AudioCodec::Flac => Some("flac"),
            AudioCodec::Vorbis => Some("ogg"),
            AudioCodec::Alac => Some("m4a"),
            AudioCodec::Opus => Some("opus"),
            AudioCodec::Pcm => Some("wav"),
            AudioCodec::Adpcm => Some("wav"),
        }
    }
}

impl<C: CodecType> AudioDecoder for Symphonia<C> {
    type Config = SymphoniaConfig;

    fn create<R>(source: R, config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
        Self: Sized,
    {
        let hint = Self::codec_hint();
        let inner = SymphoniaInner::new(source, &config, hint)?;

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
}

impl<R: Seek> ReadSeekAdapter<R> {
    fn new(mut inner: R) -> Self {
        let byte_len = Arc::new(AtomicU64::new(0));
        // Try to probe the byte length statically (works for Cursor, File, etc.)
        if let Some(len) = Self::probe_byte_len(&mut inner) {
            byte_len.store(len, Ordering::Release);
        }
        Self { inner, byte_len }
    }

    fn byte_len_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.byte_len)
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
        true
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
    use crate::traits::Alac;

    #[test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(config.gapless);
        assert!(config.byte_len_handle.is_none());
    }

    #[test]
    fn test_symphonia_types_exist() {
        fn _check_aac(_: SymphoniaAac) {}
        fn _check_mp3(_: SymphoniaMp3) {}
        fn _check_flac(_: SymphoniaFlac) {}
        fn _check_vorbis(_: SymphoniaVorbis) {}
    }

    #[test]
    fn test_codec_hints() {
        assert_eq!(SymphoniaMp3::codec_hint(), Some("mp3"));
        assert_eq!(SymphoniaAac::codec_hint(), Some("aac"));
        assert_eq!(SymphoniaFlac::codec_hint(), Some("flac"));
        assert_eq!(SymphoniaVorbis::codec_hint(), Some("ogg"));
        assert_eq!(Symphonia::<Alac>::codec_hint(), Some("m4a"));
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
    fn test_create_decoder_wav() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let decoder = SymphoniaPcm::create(cursor, SymphoniaConfig::default());
        assert!(decoder.is_ok());

        let decoder = decoder.unwrap();
        assert_eq!(decoder.spec().sample_rate, 44100);
        assert_eq!(decoder.spec().channels, 2);
    }

    #[test]
    fn test_next_chunk_returns_data() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let mut decoder = SymphoniaPcm::create(cursor, SymphoniaConfig::default()).unwrap();

        let chunk = decoder.next_chunk().unwrap();
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

        let mut decoder = SymphoniaPcm::create(cursor, SymphoniaConfig::default()).unwrap();

        // Read all chunks
        while decoder.next_chunk().unwrap().is_some() {}

        // Next call should return None (EOF)
        let result = decoder.next_chunk().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_seek_to_beginning() {
        let wav_data = create_test_wav(10000, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let mut decoder = SymphoniaPcm::create(cursor, SymphoniaConfig::default()).unwrap();

        // Read some chunks
        let _ = decoder.next_chunk().unwrap();
        let _ = decoder.next_chunk().unwrap();

        // Seek to beginning
        decoder.seek(Duration::from_secs(0)).unwrap();

        // Should be able to read again
        let chunk = decoder.next_chunk().unwrap();
        assert!(chunk.is_some());
    }

    #[test]
    fn test_position_updates() {
        let wav_data = create_test_wav(44100, 44100, 2); // 1 second of audio
        let cursor = Cursor::new(wav_data);

        let mut decoder = SymphoniaPcm::create(cursor, SymphoniaConfig::default()).unwrap();

        assert_eq!(decoder.position(), Duration::ZERO);

        // Read a chunk
        let _ = decoder.next_chunk().unwrap();

        // Position should have advanced
        assert!(decoder.position() > Duration::ZERO);
    }

    #[test]
    fn test_duration_available() {
        let wav_data = create_test_wav(44100, 44100, 2); // 1 second of audio
        let cursor = Cursor::new(wav_data);

        let decoder = SymphoniaPcm::create(cursor, SymphoniaConfig::default()).unwrap();

        let duration = decoder.duration();
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
        let adapter = ReadSeekAdapter::new(cursor);

        assert_eq!(adapter.byte_len(), Some(5000));
        assert!(adapter.is_seekable());
    }

    #[test]
    fn test_read_seek_adapter_dynamic_update() {
        use symphonia::core::io::MediaSource;

        let data = vec![0u8; 1000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new(cursor);
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
        };

        assert!(config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_some());
    }

    #[test]
    fn test_empty_input_fails() {
        let empty = Vec::new();
        let cursor = Cursor::new(empty);

        let result = SymphoniaPcm::create(cursor, SymphoniaConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_corrupted_input_fails() {
        let corrupted = [0xDE, 0xAD, 0xBE, 0xEF].repeat(100);
        let cursor = Cursor::new(corrupted);

        let result = SymphoniaPcm::create(cursor, SymphoniaConfig::default());
        assert!(result.is_err());
    }
}
