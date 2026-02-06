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
pub trait AudioDecoder: Send + 'static {
    /// Configuration type specific to this decoder implementation.
    type Config: Default + Send;

    /// Create a new decoder from a Read + Seek source.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if:
    /// - The source cannot be read
    /// - The codec is not supported
    /// - The container format is invalid
    fn create<R>(source: R, config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
        Self: Sized;

    /// Decode the next chunk of PCM data.
    ///
    /// Returns `Ok(Some(chunk))` with PCM data, or `Ok(None)` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if decoding fails.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>>;

    /// Get the PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::SeekFailed`] if seeking is not supported
    /// or the position is invalid.
    fn seek(&mut self, pos: Duration) -> DecodeResult<()>;

    /// Get current playback position.
    fn position(&self) -> Duration;

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
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>>;

    /// Get the PCM output specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to a time position.
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

    /// Reset decoder state (called after seek).
    fn reset(&mut self) {}
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
        fn _accepts_boxed(_: Box<dyn AudioDecoder<Config = ()>>) {}
    }
}
