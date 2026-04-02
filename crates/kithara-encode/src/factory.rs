use kithara_stream::AudioCodec;

use crate::{
    error::{EncodeError, EncodeResult},
    traits::InnerEncoder,
    types::{BytesEncodeRequest, EncodedBytes, EncodedTrack, PackagedEncodeRequest},
};

#[cfg(not(target_arch = "wasm32"))]
use crate::ffmpeg::FfmpegEncoder;

/// Factory for creating encoded outputs with runtime codec selection.
pub struct EncoderFactory;

impl EncoderFactory {
    /// Create an encoder backend for complete encoded bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding is unavailable on the current target.
    pub fn create_bytes(target: crate::BytesEncodeTarget) -> EncodeResult<Box<dyn InnerEncoder>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            match target {
                crate::BytesEncodeTarget::Mp3
                | crate::BytesEncodeTarget::Flac
                | crate::BytesEncodeTarget::Aac
                | crate::BytesEncodeTarget::M4a => Ok(Box::new(FfmpegEncoder)),
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = target;
            Err(EncodeError::InvalidInput(
                "encoding is not supported on wasm32".to_owned(),
            ))
        }
    }

    /// Create an encoder backend for packaged access units of `codec`.
    ///
    /// # Errors
    ///
    /// Returns an error when the codec is unsupported or encoding is unavailable
    /// on the current target.
    pub fn create_packaged(codec: AudioCodec) -> EncodeResult<Box<dyn InnerEncoder>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            match codec {
                AudioCodec::AacLc | AudioCodec::Flac => Ok(Box::new(FfmpegEncoder)),
                codec => Err(EncodeError::UnsupportedCodec(codec)),
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = codec;
            Err(EncodeError::InvalidInput(
                "encoding is not supported on wasm32".to_owned(),
            ))
        }
    }

    /// Return the natural frame size for packaged encoding of `codec`.
    ///
    /// # Errors
    ///
    /// Returns an error when the codec does not support packaged encoding.
    pub fn frame_samples(codec: AudioCodec) -> EncodeResult<usize> {
        Self::create_packaged(codec)?.packaged_frame_samples(codec)
    }

    /// Encode a finite PCM source into complete encoded bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the target codec/backend rejects the request.
    pub fn encode_bytes(request: BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes> {
        Self::create_bytes(request.target)?.encode_bytes(request)
    }

    /// Encode a finite PCM source into packaged access units for downstream muxing.
    ///
    /// # Errors
    ///
    /// Returns an error when `request.media_info.codec` is missing or the codec/backend
    /// rejects the request.
    pub fn encode_packaged(request: PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack> {
        let codec = request
            .media_info
            .codec
            .ok_or(EncodeError::InvalidMediaInfo("codec"))?;
        Self::create_packaged(codec)?.encode_packaged(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BytesEncodeTarget, InnerEncoder};

    #[test]
    fn frame_samples_match_aac_runtime_contract() {
        let frame_samples = EncoderFactory::frame_samples(AudioCodec::AacLc).expect("aac");
        assert_eq!(frame_samples, 1024);
    }

    #[test]
    fn frame_samples_match_flac_runtime_contract() {
        let frame_samples = EncoderFactory::frame_samples(AudioCodec::Flac).expect("flac");
        assert_eq!(frame_samples, 4608);
    }

    #[test]
    fn frame_samples_reject_unknown_packaged_codec() {
        let error = EncoderFactory::frame_samples(AudioCodec::Mp3).expect_err("unsupported");
        assert!(matches!(
            error,
            EncodeError::UnsupportedCodec(AudioCodec::Mp3)
        ));
    }

    #[test]
    fn create_bytes_returns_public_encoder_abstraction() {
        let encoder: Box<dyn InnerEncoder> =
            EncoderFactory::create_bytes(BytesEncodeTarget::Mp3).expect("mp3 encoder");
        let frame_samples = encoder
            .packaged_frame_samples(AudioCodec::AacLc)
            .expect("aac");
        assert_eq!(frame_samples, 1024);
    }

    #[test]
    fn create_packaged_returns_public_encoder_abstraction() {
        let encoder: Box<dyn InnerEncoder> =
            EncoderFactory::create_packaged(AudioCodec::AacLc).expect("aac encoder");
        let frame_samples = encoder
            .packaged_frame_samples(AudioCodec::AacLc)
            .expect("aac");
        assert_eq!(frame_samples, 1024);
    }

    #[test]
    fn create_packaged_supports_flac() {
        let encoder: Box<dyn InnerEncoder> =
            EncoderFactory::create_packaged(AudioCodec::Flac).expect("flac encoder");
        let frame_samples = encoder
            .packaged_frame_samples(AudioCodec::Flac)
            .expect("flac");
        assert_eq!(frame_samples, 4608);
    }
}
