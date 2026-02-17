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

use kithara_bufpool::PcmPool;
use kithara_stream::{ContainerFormat, StreamContext};
use symphonia::{
    core::{
        codecs::{
            CodecParameters,
            audio::{AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions},
        },
        errors::Error as SymphoniaError,
        formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
        units::{Time, Timestamp},
    },
    default::formats::{AdtsReader, FlacReader, IsoMp4Reader, MpaReader, OggReader, WavReader},
};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, AudioDecoder, CodecType, DecoderInput, Flac, Mp3, Vorbis},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Configuration for Symphonia-based decoders.
#[doc(hidden)]
#[derive(Default)]
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
    ///
    /// When not set, falls back to Symphonia's probe mechanism which
    /// can automatically detect format and skip junk data.
    pub container: Option<ContainerFormat>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    ///
    /// Used only when `container` is not set, as a hint for the probe.
    pub hint: Option<String>,
    /// Disable seek during probe-based initialization.
    ///
    /// When true, seek is disabled during Symphonia probe, preventing format
    /// readers from seeking to end to validate file/chunk sizes. Useful after
    /// ABR variant switches where the byte length seen by the adapter may not
    /// match container header expectations (e.g., WAV `data_size` vs actual
    /// available data). Seek is re-enabled after successful initialization.
    pub probe_no_seek: bool,
    /// Stream context for segment/variant metadata.
    pub stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Epoch counter for decoder recreation tracking.
    pub epoch: u64,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub pcm_pool: Option<PcmPool>,
}

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
    frame_offset: u64,
    duration: Option<Duration>,
    metadata: TrackMetadata,
    /// Handle for dynamic byte length updates.
    byte_len_handle: Arc<AtomicU64>,
    /// Stream context for segment/variant metadata.
    stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Epoch counter for decoder recreation tracking.
    epoch: u64,
    /// PCM buffer pool (resolved from config or global).
    pool: PcmPool,
}

impl SymphoniaInner {
    /// Create a new inner decoder from a Read + Seek source.
    ///
    /// When `container` is specified in config, creates the format reader
    /// directly without probing. Otherwise falls back to probe which can
    /// automatically detect format and skip junk data.
    fn new<R>(source: R, config: &SymphoniaConfig) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Format options
        let format_opts = FormatOptions {
            enable_gapless: config.gapless,
            ..Default::default()
        };

        if let Some(container) = config.container {
            // Direct reader creation (no probe) - needed for HLS fMP4
            // where we know the container and need to avoid seek-to-end behavior
            Self::new_direct(source, config, container, format_opts)
        } else if config.probe_no_seek {
            // Probe with seek disabled — prevents format readers from seeking
            // to end to validate file/chunk sizes (ABR switch scenario)
            Self::new_with_probe_no_seek(source, config, format_opts)
        } else {
            // Fallback to probe - handles files with junk data, auto-detects format
            Self::new_with_probe(source, config, format_opts)
        }
    }

    /// Create decoder using direct format reader (no probe).
    ///
    /// Used when container format is known (e.g., HLS with explicit format).
    fn new_direct<R>(
        source: R,
        config: &SymphoniaConfig,
        container: ContainerFormat,
        format_opts: FormatOptions,
    ) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Disable seek during initialization for ALL containers.
        // Some format readers (IsoMp4Reader) try to seek to end looking for atoms,
        // which breaks streaming scenarios. After initialization, seek is enabled.
        let adapter = ReadSeekAdapter::new_seek_disabled(source);

        let byte_len_handle = config
            .byte_len_handle
            .as_ref()
            .map_or_else(|| adapter.byte_len_handle(), Arc::clone);

        // Keep handle to re-enable seek after initialization
        let seek_enabled_handle = adapter.seek_enabled_handle();

        let mss = MediaSourceStream::new(Box::new(adapter), MediaSourceStreamOptions::default());

        tracing::debug!(?container, "Creating format reader directly (no probe)");
        let format_reader = Self::create_reader_for_container(mss, container, format_opts)?;

        // Re-enable seek after successful initialization
        seek_enabled_handle.store(true, Ordering::Release);
        tracing::debug!("Re-enabled seek after decoder initialization");

        Self::init_from_reader(format_reader, config, byte_len_handle)
    }

    /// Create decoder using Symphonia's probe mechanism.
    ///
    /// Used when container format is not known. Probe can automatically
    /// detect format and skip junk data at the beginning of files.
    fn new_with_probe<R>(
        source: R,
        config: &SymphoniaConfig,
        format_opts: FormatOptions,
    ) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        Self::probe_with_seek(source, config, format_opts, true)
    }

    /// Create decoder using Symphonia's probe with seek disabled.
    ///
    /// Used after ABR variant switches where the reported byte length
    /// may not match the actual container header (e.g., WAV over HLS).
    /// Disabling seek prevents format readers from seeking to end to
    /// validate file size, which would fail with mismatched lengths.
    fn new_with_probe_no_seek<R>(
        source: R,
        config: &SymphoniaConfig,
        format_opts: FormatOptions,
    ) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        Self::probe_with_seek(source, config, format_opts, false)
    }

    /// Shared implementation for probe-based decoder creation.
    fn probe_with_seek<R>(
        source: R,
        config: &SymphoniaConfig,
        format_opts: FormatOptions,
        seek_enabled: bool,
    ) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let adapter = if seek_enabled {
            ReadSeekAdapter::new_seek_enabled(source)
        } else {
            ReadSeekAdapter::new_seek_disabled(source)
        };

        let byte_len_handle = config
            .byte_len_handle
            .as_ref()
            .map_or_else(|| adapter.byte_len_handle(), Arc::clone);

        let seek_enabled_handle = adapter.seek_enabled_handle();

        let mss = MediaSourceStream::new(Box::new(adapter), MediaSourceStreamOptions::default());

        // Build probe hint from config
        let mut probe_hint = Hint::new();
        if let Some(ref ext) = config.hint {
            probe_hint.with_extension(ext);
        }

        let meta_opts = MetadataOptions::default();

        tracing::debug!(hint = ?config.hint, seek_enabled, "Probing format");
        let format_reader = symphonia::default::get_probe()
            .probe(&probe_hint, mss, format_opts, meta_opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        // Re-enable seek after successful probe (if it was disabled)
        if !seek_enabled {
            seek_enabled_handle.store(true, Ordering::Release);
            tracing::debug!("Re-enabled seek after probe");
        }

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
        #[expect(clippy::cast_possible_truncation)] // audio channel count always fits u16
        let channels = codec_params
            .channels
            .as_ref()
            .map_or(2, |c| c.count() as u16);
        let spec = PcmSpec {
            channels,
            sample_rate,
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

        let pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
            position: Duration::ZERO,
            frame_offset: 0,
            duration,
            metadata,
            byte_len_handle,
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
            pool,
        })
    }

    /// Calculate duration from track metadata.
    fn calculate_duration(track: &symphonia::core::formats::Track) -> Option<Duration> {
        let num_frames = track.num_frames?;
        let time_base = track.time_base?;
        let time = time_base.calc_time(Timestamp::new(num_frames as i64))?;
        let (seconds, nanos) = time.parts();
        Some(Duration::new(seconds.cast_unsigned(), nanos))
    }

    /// Decode the next chunk of PCM data.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
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

            // Convert to f32 interleaved (pool-backed to reduce allocations).
            // The PooledOwned buffer flows through PcmChunk and auto-recycles on drop.
            let mut pooled = self.pool.get_with(|v| v.resize(num_samples, 0.0));
            decoded.copy_to_slice_interleaved(&mut *pooled);

            #[expect(clippy::cast_possible_truncation)] // audio channel count always fits u16
            let pcm_spec = PcmSpec {
                channels: channels as u16,
                sample_rate: spec.rate(),
            };

            let meta = PcmMeta {
                spec: pcm_spec,
                frame_offset: self.frame_offset,
                timestamp: self.position,
                segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
                variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
                epoch: self.epoch,
            };

            let chunk = PcmChunk::new(meta, pooled);

            // Update position and frame offset
            if self.spec.sample_rate > 0 {
                let frames = chunk.frames();
                #[expect(clippy::cast_precision_loss)] // frame count precision loss is acceptable
                let frame_duration =
                    Duration::from_secs_f64(frames as f64 / f64::from(self.spec.sample_rate));
                self.position = self.position.saturating_add(frame_duration);
                let frames_u64 = frames as u64;
                self.frame_offset += frames_u64;
            }

            return Ok(Some(chunk));
        }
    }

    /// Seek to a time position.
    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        let seek_to = SeekTo::Time {
            time: Time::try_new(pos.as_secs() as i64, pos.subsec_nanos()).unwrap_or(Time::ZERO),
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
        #[expect(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        // seek position and sample rate are non-negative; precision loss acceptable
        {
            self.frame_offset = (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;
        }
        Ok(())
    }

    /// Update the byte length reported to symphonia's `MediaSource`.
    fn update_byte_len(&self, len: u64) {
        self.byte_len_handle.store(len, Ordering::Release);
    }
}

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

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }
}

use crate::traits::InnerDecoder;

impl<C: CodecType> InnerDecoder for Symphonia<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
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
}

/// Symphonia-based AAC decoder.
pub type SymphoniaAac = Symphonia<Aac>;

/// Symphonia-based MP3 decoder.
pub type SymphoniaMp3 = Symphonia<Mp3>;

/// Symphonia-based FLAC decoder.
pub type SymphoniaFlac = Symphonia<Flac>;

/// Symphonia-based Vorbis decoder.
pub type SymphoniaVorbis = Symphonia<Vorbis>;

/// Adapter that wraps a Read + Seek source as a Symphonia `MediaSource`.
///
/// This adapter tracks the byte length dynamically, allowing it to be
/// updated after construction (useful for HLS streams where the total
/// length becomes known later).
struct ReadSeekAdapter<R> {
    inner: R,
    /// Dynamic byte length. Updated externally via `Arc<AtomicU64>`.
    /// 0 means unknown (returns None from `byte_len()`).
    byte_len: Arc<AtomicU64>,
    /// Controls whether seek operations are allowed.
    /// Used to temporarily disable seeking during fMP4 reader initialization
    /// (prevents `IsoMp4Reader` from seeking to end looking for moov atom).
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

    /// Create adapter with seek enabled from the start.
    ///
    /// Used for probe-based initialization where seek is needed for format detection.
    fn new_seek_enabled(mut inner: R) -> Self {
        let byte_len = Arc::new(AtomicU64::new(0));
        let seek_enabled = Arc::new(AtomicBool::new(true));
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{test_support::create_test_wav, traits::AudioDecoder};

    #[test]
    fn test_symphonia_config_default() {
        let config = SymphoniaConfig::default();
        assert!(!config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_none());
        assert!(config.container.is_none());
        assert!(config.hint.is_none());
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
    fn test_create_decoder_without_container_uses_probe() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        // Create without container - should succeed using probe fallback
        let result = SymphoniaPcm::create(Box::new(cursor), SymphoniaConfig::default());
        assert!(result.is_ok(), "probe fallback should detect WAV format");

        let decoder = result.unwrap();
        assert_eq!(AudioDecoder::spec(&decoder).sample_rate, 44100);
        assert_eq!(AudioDecoder::spec(&decoder).channels, 2);
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
        assert_eq!(chunk.spec().sample_rate, 44100);
        assert_eq!(chunk.spec().channels, 2);
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
            hint: Some("mp4".to_string()),
            probe_no_seek: false,
            stream_ctx: None,
            epoch: 0,
            pcm_pool: None,
        };

        assert!(config.verify);
        assert!(!config.gapless);
        assert!(config.byte_len_handle.is_some());
        assert_eq!(config.container, Some(ContainerFormat::Fmp4));
        assert_eq!(config.hint, Some("mp4".to_string()));
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
