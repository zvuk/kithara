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
    "kithara-encode: enable an encode backend — `ffmpeg` for the offline byte, \
     FLAC and packaged paths, `fdk-aac` for HE-AAC and the streaming AAC-LC \
     the live broadcast runs on. With neither, this crate encodes nothing."
);

/// Backend a [`StreamEncoder`] runs on, chosen by the caller.
///
/// Variants are gated on cargo features: a variant exists in the type only when
/// its backend is compiled in, so asking for one this build does not carry is a
/// compile error. Failures of the chosen backend are terminal — there is no
/// fallback to the other one.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamBackend {
    /// `FFmpeg`'s AAC encoder, linked against the system `FFmpeg` (requires the
    /// `ffmpeg` feature).
    #[cfg(feature = "ffmpeg")]
    Ffmpeg,
    /// In-tree `fdk-aac`, built from vendored sources (requires the `fdk-aac`
    /// feature).
    #[cfg(feature = "fdk-aac")]
    Fdk,
}

/// Audio the caller hands one stream, validated once for every backend.
#[derive(Clone, Copy)]
pub(crate) struct StreamParams {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) bit_rate: u64,
    pub(crate) timescale: u32,
}

/// What every AAC-LC backend does with a stream of whole frames. `push` takes
/// interleaved f32 the caller already checked, `finish` drains what the encoder
/// still holds.
pub(crate) trait AacStream: Send {
    fn push(&mut self, samples: &[f32]) -> EncodeResult<Vec<EncodedAccessUnit>>;

    fn finish(self: Box<Self>) -> EncodeResult<Vec<EncodedAccessUnit>>;
}

/// Streaming AAC-LC encoder: interleaved f32 in, encoded access units out.
///
/// One instance encodes one continuous stream; [`push`](Self::push) returns the
/// access units the pushed audio completed, [`finish`](Self::finish) drains the
/// encoder's remaining frames.
pub struct StreamEncoder {
    inner: Box<dyn AacStream>,
    channels: u16,
}

impl StreamEncoder {
    /// Samples per channel in one AAC-LC access unit.
    pub const FRAME_SAMPLES: usize = 1024;

    /// Open `backend` on `sample_rate`/`channels` audio, emitting access-unit
    /// timestamps in `timescale` units.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] for a zero sample rate, channel
    /// count, or timescale, and a backend error when the chosen encoder cannot
    /// carry the requested audio.
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

    /// Encode interleaved `samples` and return the access units they completed.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidInput`] when the slice length is not a
    /// multiple of the channel count, and [`EncodeError::Backend`] when the
    /// encoder rejects the audio.
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

    /// Flush the encoder and return its remaining access units.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] when the encoder fails to drain.
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
