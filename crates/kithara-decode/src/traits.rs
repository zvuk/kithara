//! Codec type markers for type-safe decoder construction.
//!
//! Each codec has a marker type implementing [`CodecType`], which maps
//! to the runtime [`AudioCodec`] enum from kithara-stream.

// Types are not yet exported but will be used in later tasks
#![allow(dead_code)]

use kithara_stream::AudioCodec;

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
}
