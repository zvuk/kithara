use kithara_encode::BytesEncodeTarget;

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
