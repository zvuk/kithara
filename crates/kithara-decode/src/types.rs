use std::fmt;

use symphonia::core::errors::Error as SymphoniaError;
use thiserror::Error;

/// Main decode error type
#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("symphonia error: {0}")]
    Symphonia(#[from] SymphoniaError),

    #[error("no supported audio track found")]
    NoAudioTrack,

    #[error("decode error: {0}")]
    DecodeError(String),

    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),

    #[error("seek error: {0}")]
    SeekError(String),

    #[error("end of stream")]
    EndOfStream,
}

pub type DecodeResult<T> = Result<T, DecodeError>;

/// PCM specification - core audio format information
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmSpec {
    pub sample_rate: u32,
    pub channels: u16,
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

    /// Duration of this chunk in seconds
    pub fn duration_secs(&self) -> f64 {
        if self.spec.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.spec.sample_rate as f64
        }
    }

    /// Get reference to raw samples.
    pub fn samples(&self) -> &[T] {
        &self.pcm
    }

    /// Consume chunk and return samples.
    pub fn into_samples(self) -> Vec<T> {
        self.pcm
    }
}

/// Decoder settings
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DecoderSettings {
    /// Enable gapless playback when supported by format
    pub enable_gapless: bool,
}
