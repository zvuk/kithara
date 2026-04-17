use kithara_stream::AudioCodec;

use crate::{BytesEncodeRequest, EncodeResult, EncodedBytes, EncodedTrack, PackagedEncodeRequest};

/// Runtime-polymorphic audio encoder backend.
///
/// Clients obtain an implementation from [`crate::EncoderFactory`] and use it
/// without depending on a concrete backend such as `FFmpeg`.
pub trait InnerEncoder: Send + Sync + 'static {
    /// Encode a finite PCM source into complete encoded bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot encode the provided PCM input.
    fn encode_bytes(&self, request: BytesEncodeRequest<'_>) -> EncodeResult<EncodedBytes>;

    /// Encode a finite PCM source into packaged access units for downstream muxing.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot produce packaged output for the request.
    fn encode_packaged(&self, request: PackagedEncodeRequest<'_>) -> EncodeResult<EncodedTrack>;

    /// Return the natural frame size for packaged encoding of `codec`.
    ///
    /// # Errors
    ///
    /// Returns an error when the codec does not support packaged encoding.
    fn packaged_frame_samples(&self, codec: AudioCodec) -> EncodeResult<usize>;
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn trait_is_object_safe() {
        fn _accepts_boxed(_: Box<dyn InnerEncoder>) {}
    }
}
