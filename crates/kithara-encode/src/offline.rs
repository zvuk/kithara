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

/// A build without `FFmpeg` still answers every offline route, and answers the
/// ones it cannot serve with an error rather than a panic.
#[cfg(all(test, not(feature = "ffmpeg")))]
mod tests {
    use kithara_stream::AudioCodec;

    use super::OfflineEncoder;
    use crate::{
        BytesEncodeRequest, BytesEncodeTarget, EncodeError, InnerEncoder, test_pcm::TestPcm,
    };

    #[test]
    fn byte_encoding_reports_the_missing_backend() {
        let pcm = TestPcm::sawtooth(1_024, 48_000, 2);

        let error = OfflineEncoder
            .encode_bytes(BytesEncodeRequest {
                pcm: &pcm,
                target: BytesEncodeTarget::Mp3,
                bit_rate: None,
            })
            .map(|_| ())
            .expect_err("no FFmpeg, no byte encoding");

        assert!(matches!(error, EncodeError::InvalidInput(_)), "{error}");
    }

    #[test]
    fn a_codec_whose_backend_is_absent_is_unsupported() {
        let error = OfflineEncoder
            .packaged_frame_samples(AudioCodec::Flac)
            .expect_err("no FFmpeg, no FLAC");

        assert!(matches!(error, EncodeError::UnsupportedCodec(_)), "{error}");
    }
}
