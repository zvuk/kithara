use kithara_stream::AudioCodec;

use super::packaged::OfflineEncoder;
use crate::{BytesEncodeRequest, BytesEncodeTarget, EncodeError, InnerEncoder, test_pcm::TestPcm};

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
