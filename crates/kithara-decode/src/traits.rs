//! Codec type markers and traits for type-safe decoder construction.
//!
//! Each codec has a marker type implementing [`CodecType`], which maps
//! to the runtime [`AudioCodec`] enum from kithara-stream.

use std::{
    io::{Read, Seek},
    time::Duration,
};

use kithara_stream::AudioCodec;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::{
    error::DecodeResult,
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Combined trait for decoder input sources.
///
/// This supertrait combines `Read + Seek + Send + Sync` into a single trait
/// that can be used as a trait object (`Box<dyn DecoderInput>`).
///
/// A blanket implementation is provided for all types satisfying the bounds.
pub trait DecoderInput: Read + Seek + Send + Sync {}

impl<T: Read + Seek + Send + Sync> DecoderInput for T {}

/// Marker trait for codec types.
///
/// Implementations provide compile-time codec identification that maps
/// to runtime [`AudioCodec`] values.
pub trait CodecType: Send + 'static {
    /// The codec this type represents.
    const CODEC: AudioCodec;
}

/// AAC codec marker (maps to AAC-LC, the most common variant).
pub struct Aac;
impl CodecType for Aac {
    const CODEC: AudioCodec = AudioCodec::AacLc;
}

/// MP3 codec marker.
pub struct Mp3;
impl CodecType for Mp3 {
    const CODEC: AudioCodec = AudioCodec::Mp3;
}

/// FLAC codec marker.
pub struct Flac;
impl CodecType for Flac {
    const CODEC: AudioCodec = AudioCodec::Flac;
}

/// ALAC codec marker.
pub struct Alac;
impl CodecType for Alac {
    const CODEC: AudioCodec = AudioCodec::Alac;
}

/// Vorbis codec marker.
pub struct Vorbis;
impl CodecType for Vorbis {
    const CODEC: AudioCodec = AudioCodec::Vorbis;
}

/// Trait for all audio decoders (Symphonia, Apple, Android).
///
/// This trait abstracts over different decoder backends, allowing unified
/// access to audio decoding functionality regardless of the underlying
/// implementation.
///
/// The `Source` associated type replaces a generic parameter on `create`,
/// enabling the trait to be used with unimock for testing.
#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock(api = AudioDecoderMock, type Config = (); type Source = Box<dyn DecoderInput>;)
)]
pub trait AudioDecoder: Send + 'static {
    /// Configuration type specific to this decoder implementation.
    type Config: Default + Send;

    /// Input source type for decoder construction.
    ///
    /// Typically `Box<dyn DecoderInput>` for concrete implementations.
    type Source: Read + Seek + Send + Sync + 'static;

    /// Create a new decoder from a source.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError`] if:
    /// - The source cannot be read
    /// - The codec is not supported
    /// - The container format is invalid
    ///
    /// The default implementation returns [`crate::error::DecodeError::ProbeFailed`].
    /// All concrete backends override this.
    fn create(source: Self::Source, config: Self::Config) -> DecodeResult<Self>
    where
        Self: Sized,
    {
        let _ = (source, config);
        Err(crate::error::DecodeError::ProbeFailed)
    }

    /// Decode the next chunk of PCM data.
    ///
    /// Returns `Ok(Some(chunk))` with PCM data, or `Ok(None)` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError`] if decoding fails.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>>;

    /// Get the PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError::SeekFailed`] if seeking is not supported
    /// or the position is invalid.
    fn seek(&mut self, pos: Duration) -> DecodeResult<()>;

    /// Get total duration if known.
    ///
    /// Returns `None` if the duration cannot be determined (e.g., for
    /// streams without a known length).
    fn duration(&self) -> Option<Duration> {
        None
    }
}

/// Trait for runtime-polymorphic audio decoders.
///
/// This trait is used by kithara-audio for dynamic dispatch when the
/// decoder type is determined at runtime (e.g., based on media info).
///
/// Unlike [`AudioDecoder`], this trait does not have an associated
/// `Config` type, making it object-safe for `Box<dyn InnerDecoder>`.
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = InnerDecoderMock))]
pub trait InnerDecoder: Send + 'static {
    /// Decode the next chunk of PCM data.
    ///
    /// Returns `Ok(Some(chunk))` with PCM data, or `Ok(None)` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError`] if decoding fails.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>>;

    /// Get the PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::DecodeError::SeekFailed`] if seeking is not supported
    /// or the position is invalid.
    fn seek(&mut self, pos: Duration) -> DecodeResult<()>;

    /// Update the byte length reported to the underlying media source.
    ///
    /// For HLS streams, the total length becomes known after metadata
    /// calculation. Call this before seeking so the decoder can compute
    /// correct seek deltas.
    fn update_byte_len(&self, len: u64);

    /// Get total duration from track metadata.
    ///
    /// Returns `None` if duration cannot be determined.
    fn duration(&self) -> Option<Duration>;

    /// Get track metadata (title, artist, album, artwork).
    ///
    /// Returns default metadata if not available.
    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aac_codec_type() {
        assert_eq!(Aac::CODEC, AudioCodec::AacLc);
    }

    #[test]
    fn test_mp3_codec_type() {
        assert_eq!(Mp3::CODEC, AudioCodec::Mp3);
    }

    #[test]
    fn test_flac_codec_type() {
        assert_eq!(Flac::CODEC, AudioCodec::Flac);
    }

    #[test]
    fn test_alac_codec_type() {
        assert_eq!(Alac::CODEC, AudioCodec::Alac);
    }

    #[test]
    fn test_vorbis_codec_type() {
        assert_eq!(Vorbis::CODEC, AudioCodec::Vorbis);
    }

    #[test]
    fn test_audio_decoder_trait_is_object_safe() {
        // This test verifies the trait can be used as dyn AudioDecoder
        fn _accepts_boxed(_: Box<dyn AudioDecoder<Config = (), Source = Box<dyn DecoderInput>>>) {}
    }
}
