use std::{fmt, time::Duration};

use kithara_assets::AssetsError;
use symphonia::core::errors::Error as SymphoniaError;
use thiserror::Error;

/// Main decode error type
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("not implemented")]
    Unimplemented,

    #[error("assets error: {0}")]
    Assets(#[from] AssetsError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("symphonia error: {0}")]
    Symphonia(#[from] SymphoniaError),

    #[error("no supported audio track found")]
    NoAudioTrack,

    #[error("decode error: {0}")]
    DecodeError(String),

    #[error("seek error: {0}")]
    SeekError(String),

    #[error("codec reset required")]
    CodecResetRequired,

    #[error("end of stream")]
    EndOfStream,
}

pub type DecodeResult<T> = Result<T, DecodeError>;

/// Audio specification wrapper types for backward compatibility
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRate(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelCount(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSpec {
    pub sample_rate: SampleRate,
    pub channels: ChannelCount,
}

/// PCM specification - core audio format information
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

impl From<AudioSpec> for PcmSpec {
    fn from(spec: AudioSpec) -> Self {
        Self {
            sample_rate: spec.sample_rate.0,
            channels: spec.channels.0,
        }
    }
}

impl fmt::Display for PcmSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz, {} channels", self.sample_rate, self.channels)
    }
}

/// PCM chunk containing interleaved audio samples
///
/// # Invariants
/// - `pcm.len() % channels == 0` (frame-aligned)
/// - `spec.channels > 0` and `spec.sample_rate > 0`
/// - All samples are of type T and interleaved (LRLRLR...)
#[derive(Clone, Debug)]
pub struct PcmChunk<T> {
    pub spec: PcmSpec,
    pub pcm: Vec<T>,
}

impl<T> PcmChunk<T> {
    pub fn new(spec: PcmSpec, pcm: Vec<T>) -> Self {
        Self { spec, pcm }
    }

    /// Create a new PcmChunk ensuring frame alignment
    ///
    /// Returns error if PCM data length is not divisible by channel count
    pub fn new_frame_aligned(spec: PcmSpec, pcm: Vec<T>) -> DecodeResult<Self> {
        let chunk = Self::new(spec, pcm);
        if chunk.is_frame_aligned() {
            Ok(chunk)
        } else {
            Err(DecodeError::DecodeError(
                "PCM data is not frame-aligned".to_string(),
            ))
        }
    }

    /// Number of audio frames in this chunk
    ///
    /// A frame contains one sample per channel
    pub fn frames(&self) -> usize {
        let channels = self.spec.channels as usize;
        if channels == 0 {
            0
        } else {
            self.pcm.len() / channels
        }
    }

    /// Check if PCM data is frame-aligned (length divisible by channel count)
    pub fn is_frame_aligned(&self) -> bool {
        self.spec.channels == 0 || self.pcm.len().is_multiple_of(self.spec.channels as usize)
    }

    /// Duration of this chunk in seconds
    pub fn duration_secs(&self) -> f64 {
        if self.spec.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.spec.sample_rate as f64
        }
    }
}

/// Decoder settings for Symphonia integration
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DecoderSettings {
    /// Enable gapless playback when supported by format
    pub enable_gapless: bool,
}

/// Commands that can be sent to the decoder/pipeline
///
/// # Command Semantics
/// - Seek: Best-effort seek to absolute time position
///   - May not be frame-accurate depending on format
///   - Next chunk returned will reflect new position
///   - Should not deadlock or hang
/// - Future commands can be added here (Stop, Pause, etc.)
#[derive(Clone, Debug, PartialEq)]
pub enum DecodeCommand {
    Seek(Duration),
}

/// Trait for combined Read+Seek functionality
pub trait ReadSeek: std::io::Read + std::io::Seek + Send + Sync {}
impl<T> ReadSeek for T where T: std::io::Read + std::io::Seek + Send + Sync {}

/// Media source abstraction for Symphonia integration
///
/// This trait provides:
/// - Reader interface for Symphonia's MediaSourceStream
/// - Optional file extension hint for better format probing
/// - Conversion to Symphonia's MediaSource trait
pub trait MediaSource: Send + 'static {
    fn reader(&self) -> Box<dyn ReadSeek + Send + Sync>;

    /// File extension hint for format probing (e.g., "mp3", "wav")
    fn file_ext(&self) -> Option<&str> {
        None
    }

    /// Convert to Symphonia's MediaSource for probing/decoding
    fn as_media_source(&self) -> Box<dyn symphonia::core::io::MediaSource + Send> {
        Box::new(MediaSourceAdapter::new(self.reader()))
    }
}

/// Adapter to convert our ReadSeek to Symphonia's MediaSource
struct MediaSourceAdapter {
    reader: Box<dyn ReadSeek + Send + Sync>,
}

impl MediaSourceAdapter {
    fn new(reader: Box<dyn ReadSeek + Send + Sync>) -> Self {
        Self { reader }
    }
}

impl symphonia::core::io::MediaSource for MediaSourceAdapter {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl std::io::Read for MediaSourceAdapter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl std::io::Seek for MediaSourceAdapter {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.reader.seek(pos)
    }
}

/// Synchronous audio source trait that can be driven in a worker thread
///
/// This is the core interface for audio decoders, providing:
/// - Output specification (sample rate, channels)
/// - Chunked PCM data output
/// - Command handling (seek, etc.)
///
/// # Generic Parameter T
/// T is the sample type (e.g., f32, i16). Must implement:
/// - Send + 'static (for threading)
pub trait AudioSource<T>
where
    T: Send + 'static,
{
    /// Get current output specification if known
    fn output_spec(&self) -> Option<PcmSpec>;

    /// Get next chunk of PCM data
    /// Returns None when stream has ended naturally
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<T>>>;

    /// Handle a command (e.g., seek)
    fn handle_command(&mut self, cmd: DecodeCommand) -> DecodeResult<()>;
}
