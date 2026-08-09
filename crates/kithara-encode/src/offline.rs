use kithara_stream::AudioCodec;

#[cfg(feature = "fdk-aac")]
use crate::fdk::aac_he::{AacHeEncoder, AacHeProfile};
#[cfg(feature = "ffmpeg")]
use crate::ffmpeg::{aac::AacFFmpegEncoder, flac::FlacFFmpegEncoder};
use crate::{
    BytesEncodeRequest, EncodeError, EncodeResult, EncodedBytes, EncodedTrack, InnerEncoder,
    PackagedEncodeRequest,
};

/// Offline encoding over a finite PCM source: complete encoded bytes and
/// packaged access units. Each codec is routed to the backend that owns it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OfflineEncoder;

impl InnerEncoder for OfflineEncoder {
    fn encode_bytes(&self, request: BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes> {
        #[cfg(feature = "ffmpeg")]
        {
            crate::ffmpeg::bytes::encode_bytes_audio(&request)
        }

        #[cfg(not(feature = "ffmpeg"))]
        {
            let _ = request;
            Err(EncodeError::InvalidInput(
                "byte encoding needs the `ffmpeg` feature".to_owned(),
            ))
        }
    }

    fn encode_packaged(&self, request: PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack> {
        let codec = request
            .media_info
            .codec
            .ok_or(EncodeError::InvalidMediaInfo("codec"))?;
        match codec {
            #[cfg(feature = "ffmpeg")]
            AudioCodec::AacLc => AacFFmpegEncoder::encode(&request),
            #[cfg(feature = "fdk-aac")]
            AudioCodec::AacHe => AacHeEncoder::encode(&request, AacHeProfile::V1),
            #[cfg(feature = "fdk-aac")]
            AudioCodec::AacHeV2 => AacHeEncoder::encode(&request, AacHeProfile::V2),
            #[cfg(feature = "ffmpeg")]
            AudioCodec::Flac => FlacFFmpegEncoder::encode(&request),
            codec => Err(EncodeError::UnsupportedCodec(codec)),
        }
    }

    fn packaged_frame_samples(&self, codec: AudioCodec) -> EncodeResult<usize> {
        match codec {
            #[cfg(feature = "ffmpeg")]
            AudioCodec::AacLc => Ok(AacFFmpegEncoder::frame_samples()),
            #[cfg(feature = "fdk-aac")]
            AudioCodec::AacHe | AudioCodec::AacHeV2 => Ok(AacHeEncoder::frame_samples()),
            #[cfg(feature = "ffmpeg")]
            AudioCodec::Flac => Ok(FlacFFmpegEncoder::frame_samples()),
            codec => Err(EncodeError::UnsupportedCodec(codec)),
        }
    }
}
