//! Symphonia-based audio decoder.

use std::{
    io::{Read, Seek},
    sync::LazyLock,
    time::Duration,
};

use symphonia::core::{
    codecs::{
        audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions},
        registry::CodecRegistry,
        CodecParameters,
    },
    errors::Error as SymphoniaError,
    formats::{probe::Hint, FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
    io::MediaSourceStream,
    meta::MetadataOptions,
    units::Time,
};

use kithara_stream::{ContainerFormat, MediaInfo};

use crate::{DecodeError, DecodeResult, PcmChunk, PcmSpec};

static CODEC_REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    // Register all default codecs
    symphonia::default::register_enabled_codecs(&mut registry);
    registry
});

/// Cached codec information for ABR switch optimization.
#[derive(Clone, Debug)]
pub struct CachedCodecInfo {
    pub codec_params: AudioCodecParameters,
}

/// Symphonia-based audio decoder outputting f32 samples.
///
/// Supports two creation modes:
/// - `new_with_probe()` - probes format and creates decoder (initial load)
/// - `new_direct()` - creates decoder from cached params (ABR switch)
pub struct SymphoniaDecoder {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: PcmSpec,
}

impl SymphoniaDecoder {
    /// Create decoder by probing the media source.
    ///
    /// This is used for initial load when codec parameters are unknown.
    pub fn new_with_probe<R>(reader: R, hint: Option<&str>) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mss =
            MediaSourceStream::new(Box::new(ReadSeekAdapter::new(reader)), Default::default());

        let mut probe_hint = Hint::new();
        if let Some(ext) = hint {
            probe_hint.with_extension(ext);
        }

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let meta_opts = MetadataOptions::default();

        let format_reader = symphonia::default::get_probe()
            .probe(&probe_hint, mss, format_opts, meta_opts)
            .map_err(DecodeError::Symphonia)?;

        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::NoAudioTrack)?
            .clone();

        let track_id = track.id;
        let codec_params = extract_audio_params(&track)?;
        let spec = extract_spec(&codec_params)?;
        let decoder = create_decoder(&codec_params)?;

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
        })
    }

    /// Create decoder directly from MediaInfo without full probe.
    ///
    /// This is used for ABR switch when we know the container/codec from HLS playlist.
    /// Creates FormatReader and AudioDecoder directly based on MediaInfo.
    pub fn new_from_media_info<R>(reader: R, media_info: &MediaInfo) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mss =
            MediaSourceStream::new(Box::new(ReadSeekAdapter::new(reader)), Default::default());

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };

        // Create FormatReader based on container type
        let format_reader = create_format_reader_direct(mss, media_info.container, format_opts)?;

        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::NoAudioTrack)?
            .clone();

        let track_id = track.id;
        let codec_params = extract_audio_params(&track)?;
        let spec = extract_spec(&codec_params)?;
        let decoder = create_decoder(&codec_params)?;

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
        })
    }

    /// Create decoder directly from cached codec parameters.
    ///
    /// This is used for ABR switch when we know the codec hasn't changed.
    /// Still requires probe for FormatReader but reuses cached codec params.
    pub fn new_direct<R>(reader: R, cached: &CachedCodecInfo) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mss =
            MediaSourceStream::new(Box::new(ReadSeekAdapter::new(reader)), Default::default());

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let meta_opts = MetadataOptions::default();

        let format_reader = symphonia::default::get_probe()
            .probe(&Hint::new(), mss, format_opts, meta_opts)
            .map_err(DecodeError::Symphonia)?;

        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::NoAudioTrack)?
            .clone();

        let track_id = track.id;
        let spec = extract_spec(&cached.codec_params)?;
        let decoder = create_decoder(&cached.codec_params)?;

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
        })
    }

    /// Get codec parameters for caching.
    pub fn codec_params(&self) -> Option<AudioCodecParameters> {
        self.format_reader
            .default_track(TrackType::Audio)
            .and_then(|t| match &t.codec_params {
                Some(CodecParameters::Audio(params)) => Some(params.clone()),
                _ => None,
            })
    }

    /// Get current PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Reset decoder state.
    ///
    /// Called after seek or ABR switch to clear any buffered data.
    pub fn reset(&mut self) {
        self.decoder.reset();
    }

    /// Seek to absolute time position.
    ///
    /// This is best-effort and may not be frame-accurate.
    pub fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        let seek_to = SeekTo::Time {
            time: Time::new(pos.as_secs(), pos.subsec_nanos() as f64 / 1_000_000_000.0),
            track_id: Some(self.track_id),
        };

        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| DecodeError::SeekError(e.to_string()))?;

        self.decoder.reset();
        Ok(())
    }

    /// Decode next chunk of audio as f32 samples.
    ///
    /// Returns `None` on EOF.
    pub fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None),
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(DecodeError::Symphonia(e)),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(_)) => {
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(DecodeError::Symphonia(e)),
            };

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

            return Ok(Some(PcmChunk::new(pcm_spec, pcm)));
        }
    }
}

fn extract_audio_params(
    track: &symphonia::core::formats::Track,
) -> DecodeResult<AudioCodecParameters> {
    match &track.codec_params {
        Some(CodecParameters::Audio(params)) => Ok(params.clone()),
        _ => Err(DecodeError::NoAudioTrack),
    }
}

fn extract_spec(params: &AudioCodecParameters) -> DecodeResult<PcmSpec> {
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| DecodeError::DecodeError("No sample rate".to_string()))?;

    let channels = params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(2);

    Ok(PcmSpec {
        sample_rate,
        channels,
    })
}

fn create_decoder(params: &AudioCodecParameters) -> DecodeResult<Box<dyn AudioDecoder>> {
    let opts = AudioDecoderOptions { verify: false };
    CODEC_REGISTRY
        .make_audio_decoder(params, &opts)
        .map_err(DecodeError::Symphonia)
}

/// Create FormatReader directly based on container type without probe.
fn create_format_reader_direct<'a>(
    mss: MediaSourceStream<'a>,
    container: Option<ContainerFormat>,
    opts: FormatOptions,
) -> DecodeResult<Box<dyn FormatReader + 'a>> {
    use symphonia::default::formats;

    match container {
        Some(ContainerFormat::Fmp4) => {
            let reader = formats::IsoMp4Reader::try_new(mss, opts)
                .map_err(DecodeError::Symphonia)?;
            Ok(Box::new(reader))
        }
        Some(ContainerFormat::MpegTs) => {
            // MPEG-TS reader might not be available in default features
            // Fall back to probe if not available
            probe_fallback(mss, opts)
        }
        Some(ContainerFormat::MpegAudio) => {
            // For raw MPEG audio (MP3 without container), use MpaReader
            let reader = formats::MpaReader::try_new(mss, opts)
                .map_err(DecodeError::Symphonia)?;
            Ok(Box::new(reader))
        }
        Some(ContainerFormat::Wav) => {
            let reader = formats::WavReader::try_new(mss, opts)
                .map_err(DecodeError::Symphonia)?;
            Ok(Box::new(reader))
        }
        Some(ContainerFormat::Ogg) => {
            let reader = formats::OggReader::try_new(mss, opts)
                .map_err(DecodeError::Symphonia)?;
            Ok(Box::new(reader))
        }
        Some(ContainerFormat::Caf) => {
            let reader = formats::CafReader::try_new(mss, opts)
                .map_err(DecodeError::Symphonia)?;
            Ok(Box::new(reader))
        }
        Some(ContainerFormat::Mkv) => {
            let reader = formats::MkvReader::try_new(mss, opts)
                .map_err(DecodeError::Symphonia)?;
            Ok(Box::new(reader))
        }
        None => {
            // No container hint - fall back to probe
            probe_fallback(mss, opts)
        }
    }
}

/// Fallback to probe when direct creation is not possible.
fn probe_fallback<'a>(
    mss: MediaSourceStream<'a>,
    opts: FormatOptions,
) -> DecodeResult<Box<dyn FormatReader + 'a>> {
    let meta_opts = MetadataOptions::default();
    symphonia::default::get_probe()
        .probe(&Hint::new(), mss, opts, meta_opts)
        .map_err(DecodeError::Symphonia)
}

struct ReadSeekAdapter<R> {
    inner: R,
}

impl<R> ReadSeekAdapter<R> {
    fn new(inner: R) -> Self {
        Self { inner }
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
        None
    }
}
