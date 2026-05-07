#[cfg(not(target_arch = "wasm32"))]
use crate::ffmpeg::FfmpegEncoder;
use crate::{
    codec::AudioCodec,
    error::{EncodeError, EncodeResult},
    traits::InnerEncoder,
    types::{BytesEncodeRequest, EncodedBytes, EncodedTrack, PackagedEncodeRequest},
};

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
            Self::wasm_unsupported()
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
            Self::wasm_unsupported()
        }
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

    /// Return the natural frame size for packaged encoding of `codec`.
    ///
    /// # Errors
    ///
    /// Returns an error when the codec does not support packaged encoding.
    pub fn frame_samples(codec: AudioCodec) -> EncodeResult<usize> {
        Self::create_packaged(codec)?.packaged_frame_samples(codec)
    }

    #[cfg(target_arch = "wasm32")]
    fn wasm_unsupported() -> EncodeResult<Box<dyn InnerEncoder>> {
        Err(EncodeError::InvalidInput(
            "encoding is not supported on wasm32".to_owned(),
        ))
    }
}
