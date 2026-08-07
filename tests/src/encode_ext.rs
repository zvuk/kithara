use kithara_encode::{BytesEncodeTarget, EncodedTrack};

use crate::{fixture_protocol::GaplessEncoding, fmp4::mux_audio_track};

/// Expected `Content-Type` per encode target — the oracle integration tests
/// assert the encoder's `EncodedBytes::content_type` field against. Production
/// populates that field on its own path and never calls this mapping, so the
/// expectation table lives in the test harness rather than the encode crate.
pub trait BytesEncodeTargetExt {
    fn content_type(self) -> &'static str;
}

impl BytesEncodeTargetExt for BytesEncodeTarget {
    fn content_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Flac => "audio/flac",
            Self::Aac => "audio/aac",
            Self::M4a => "audio/mp4",
        }
    }
}

/// Mux a packaged track into one fMP4 byte stream — init segment followed by
/// every media segment — the form an in-memory decoder reads.
pub fn mux_fmp4_bytes(track: &EncodedTrack, gapless: GaplessEncoding) -> Vec<u8> {
    let packaged = mux_audio_track(track, gapless).expect("mux packaged track into fMP4");
    let mut bytes = packaged.init_segment.as_ref().clone();
    for segment in &packaged.media_segments {
        bytes.extend_from_slice(segment);
    }
    bytes
}
