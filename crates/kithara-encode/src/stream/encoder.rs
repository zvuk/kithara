use bon::bon;

#[cfg(feature = "fdk-aac")]
use crate::fdk::aac_lc::FdkStream;
#[cfg(feature = "ffmpeg")]
use crate::ffmpeg::stream::FfmpegStream;
use crate::{
    error::{EncodeError, EncodeResult},
    types::EncodedAccessUnit,
};

#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(feature = "ffmpeg", feature = "fdk-aac"))
))]
compile_error!(
    "kithara-encode: enable an encode backend - `ffmpeg` for the offline byte, \
     FLAC and packaged paths, `fdk-aac` for HE-AAC and the streaming AAC-LC \
     the live broadcast runs on. With neither, this crate encodes nothing."
);

/// Caller-selected backend for a [`StreamEncoder`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamBackend {
    /// System-FFmpeg AAC encoder.
    #[cfg(feature = "ffmpeg")]
    Ffmpeg,
    /// Vendored fdk-aac encoder.
    #[cfg(feature = "fdk-aac")]
    Fdk,
}

#[derive(Clone, Copy)]
pub(crate) struct StreamParams {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) bit_rate: u64,
    pub(crate) timescale: u32,
}

pub(crate) trait AacStream: Send {
    fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>>;

    fn finish(self: Box<Self>) -> EncodeResult<Vec<EncodedAccessUnit>>;
}

/// Continuous AAC-LC encoder from interleaved f32 to raw access units.
pub struct StreamEncoder {
    inner: Box<dyn AacStream>,
    channels: u16,
}

#[bon]
impl StreamEncoder {
    /// Samples per channel in one AAC-LC access unit.
    pub const FRAME_SAMPLES: usize = 1024;

    /// Open a backend with explicit audio and timestamp parameters.
    ///
    /// # Errors
    ///
    /// Returns invalid input for a zero sample rate, channel count, or timescale,
    /// and a backend error when the encoder cannot open the requested audio.
    #[builder]
    pub fn new(
        backend: StreamBackend,
        sample_rate: u32,
        channels: u16,
        bit_rate: u64,
        timescale: u32,
    ) -> EncodeResult<Self> {
        let params = StreamParams {
            sample_rate,
            channels,
            bit_rate,
            timescale,
        };
        params.validate()?;

        let inner: Box<dyn AacStream> = match backend {
            #[cfg(feature = "ffmpeg")]
            StreamBackend::Ffmpeg => Box::new(FfmpegStream::new(&params)?),
            #[cfg(feature = "fdk-aac")]
            StreamBackend::Fdk => Box::new(FdkStream::new(&params)?),
        };

        Ok(Self { inner, channels })
    }

    /// Encode whole interleaved frames and return completed access units.
    ///
    /// # Errors
    ///
    /// Returns invalid input for partial frames and backend encode failures.
    pub fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        let channels = usize::from(self.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(EncodeError::InvalidInput(format!(
                "interleaved sample count {} is not a multiple of {channels} channels",
                samples.len()
            )));
        }

        self.inner.push(samples)
    }

    /// Drain and return remaining access units.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot drain.
    pub fn finish(self) -> EncodeResult<Vec<EncodedAccessUnit>> {
        self.inner.finish()
    }
}

impl StreamParams {
    fn validate(&self) -> EncodeResult<()> {
        if self.sample_rate == 0 {
            return Err(EncodeError::InvalidInput(
                "sample_rate must be > 0, got 0".to_owned(),
            ));
        }
        if self.channels == 0 {
            return Err(EncodeError::InvalidInput(
                "channels must be > 0, got 0".to_owned(),
            ));
        }
        if self.timescale == 0 {
            return Err(EncodeError::InvalidInput(
                "timescale must be > 0, got 0".to_owned(),
            ));
        }
        Ok(())
    }
}
