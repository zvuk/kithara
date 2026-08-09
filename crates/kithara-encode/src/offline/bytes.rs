#[cfg(not(feature = "ffmpeg"))]
use crate::EncodeError;
use crate::{BytesEncodeRequest, EncodeResult, EncodedBytes};

/// Complete encoded bytes for a finite PCM source. `FFmpeg` muxes them; without
/// it this crate has no byte encoder at all.
pub(crate) fn encode(request: &BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes> {
    #[cfg(feature = "ffmpeg")]
    {
        crate::ffmpeg::bytes::encode_bytes_audio(request)
    }

    #[cfg(not(feature = "ffmpeg"))]
    {
        let _ = request;
        Err(EncodeError::InvalidInput(
            "byte encoding needs the `ffmpeg` feature".to_owned(),
        ))
    }
}
