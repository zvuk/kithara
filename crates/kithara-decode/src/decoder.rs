//! Generic decoder wrapper.
//!
//! Provides [`Decoder<D>`] which wraps any [`AudioDecoder`] implementation,
//! providing a unified API for audio decoding.

use std::time::Duration;

use crate::{
    error::DecodeResult,
    traits::AudioDecoder,
    types::{PcmChunk, PcmSpec},
};

/// Generic decoder wrapper providing unified API.
///
/// Wraps any [`AudioDecoder`] implementation, delegating all operations
/// to the inner decoder.
///
/// # Example
///
/// ```ignore
/// use kithara_decode::{Decoder, SymphoniaAac, SymphoniaConfig};
///
/// let decoder = Decoder::<SymphoniaAac>::new(file, wav_config())?;
/// while let Some(chunk) = decoder.next_chunk()? {
///     // Process PCM
/// }
/// ```
pub struct Decoder<D: AudioDecoder> {
    inner: D,
}

impl<D: AudioDecoder> Decoder<D> {
    /// Create a new decoder from a source.
    ///
    /// The source type is determined by the decoder's associated `Source` type,
    /// typically `Box<dyn DecoderInput>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the decoder cannot be created (unsupported format,
    /// invalid data, etc.).
    pub fn new(source: D::Source, config: D::Config) -> DecodeResult<Self> {
        Ok(Self {
            inner: D::create(source, config)?,
        })
    }

    /// Decode the next chunk of PCM data.
    ///
    /// Returns `Ok(Some(chunk))` with decoded PCM data, or `Ok(None)` at
    /// end of stream.
    ///
    /// # Errors
    ///
    /// Returns an error if decoding fails.
    pub fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.inner.next_chunk()
    }

    /// Get PCM output specification.
    pub fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    /// Seek to a time position.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking fails or is not supported.
    pub fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    /// Get current playback position.
    pub fn position(&self) -> Duration {
        self.inner.position()
    }

    /// Get total duration if known.
    ///
    /// Returns `None` if duration cannot be determined (e.g., streaming).
    pub fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    /// Get reference to inner decoder.
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// Get mutable reference to inner decoder.
    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    /// Consume wrapper and return inner decoder.
    pub fn into_inner(self) -> D {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_stream::{AudioCodec, ContainerFormat};

    use super::*;
    use crate::{symphonia::SymphoniaConfig, test_support::create_test_wav, traits::CodecType};

    // PCM codec marker for testing with WAV files
    struct Pcm;
    impl CodecType for Pcm {
        const CODEC: AudioCodec = AudioCodec::Pcm;
    }

    type TestDecoder = Decoder<crate::symphonia::Symphonia<Pcm>>;

    fn wav_config() -> SymphoniaConfig {
        SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        }
    }

    #[test]
    fn test_decoder_wrapper_create() {
        let wav = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav);

        let decoder = TestDecoder::new(Box::new(cursor), wav_config());
        assert!(decoder.is_ok());

        let decoder = decoder.unwrap();
        assert_eq!(decoder.spec().sample_rate, 44100);
        assert_eq!(decoder.spec().channels, 2);
    }

    #[test]
    fn test_decoder_wrapper_next_chunk() {
        let wav = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav);

        let mut decoder = TestDecoder::new(Box::new(cursor), wav_config()).unwrap();

        let chunk = decoder.next_chunk().unwrap();
        assert!(chunk.is_some());

        let chunk = chunk.unwrap();
        assert_eq!(chunk.spec.sample_rate, 44100);
        assert!(!chunk.pcm.is_empty());
    }

    #[test]
    fn test_decoder_wrapper_seek() {
        let wav = create_test_wav(44100, 44100, 2); // 1 second
        let cursor = Cursor::new(wav);

        let mut decoder = TestDecoder::new(Box::new(cursor), wav_config()).unwrap();

        // Read a chunk
        let _ = decoder.next_chunk().unwrap();

        // Seek to beginning
        decoder.seek(Duration::from_secs(0)).unwrap();

        // Should be able to read again
        let chunk = decoder.next_chunk().unwrap();
        assert!(chunk.is_some());
    }

    #[test]
    fn test_decoder_wrapper_position() {
        let wav = create_test_wav(44100, 44100, 2);
        let cursor = Cursor::new(wav);

        let mut decoder = TestDecoder::new(Box::new(cursor), wav_config()).unwrap();

        assert_eq!(decoder.position(), Duration::ZERO);

        // Read a chunk
        let _ = decoder.next_chunk().unwrap();

        // Position should advance
        assert!(decoder.position() > Duration::ZERO);
    }

    #[test]
    fn test_decoder_wrapper_duration() {
        let wav = create_test_wav(44100, 44100, 2); // 1 second
        let cursor = Cursor::new(wav);

        let decoder = TestDecoder::new(Box::new(cursor), wav_config()).unwrap();

        let duration = decoder.duration();
        assert!(duration.is_some());

        let dur = duration.unwrap();
        assert!(dur.as_secs_f64() > 0.9 && dur.as_secs_f64() < 1.1);
    }

    #[test]
    fn test_decoder_wrapper_inner_access() {
        let wav = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav);

        let decoder = TestDecoder::new(Box::new(cursor), wav_config()).unwrap();

        // Test inner() returns reference
        let _inner = decoder.inner();
    }

    #[test]
    fn test_decoder_wrapper_into_inner() {
        let wav = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav);

        let decoder = TestDecoder::new(Box::new(cursor), wav_config()).unwrap();

        // Consume and get inner
        let inner = decoder.into_inner();
        assert_eq!(inner.spec().sample_rate, 44100);
    }
}
