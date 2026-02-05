//! Apple AudioToolbox decoder backend (placeholder).
//!
//! This module will implement hardware-accelerated audio decoding
//! using Apple's AudioToolbox framework via AudioConverter APIs.
//!
//! Currently a placeholder — actual FFI implementation pending.

use std::{
    io::{Read, Seek},
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Aac, Alac, AudioDecoder, CodecType, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Configuration for Apple AudioToolbox decoder.
#[derive(Debug, Clone, Default)]
pub struct AppleConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

/// Apple AudioToolbox decoder inner state (placeholder).
struct AppleInner {
    spec: PcmSpec,
    metadata: TrackMetadata,
    byte_len_handle: Arc<AtomicU64>,
}

/// Apple AudioToolbox decoder (placeholder).
///
/// This decoder will use Apple's AudioConverter API to decode
/// AAC, MP3, FLAC, and ALAC using hardware acceleration when available.
///
/// Currently returns `UnsupportedCodec` error — actual implementation pending.
pub struct Apple<C: CodecType> {
    inner: AppleInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> std::fmt::Debug for Apple<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Apple")
            .field("spec", &self.inner.spec)
            .finish_non_exhaustive()
    }
}

impl<C: CodecType> AudioDecoder for Apple<C> {
    type Config = AppleConfig;

    fn create<R>(_source: R, _config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
        Self: Sized,
    {
        // TODO: Implement actual AudioConverter initialization
        Err(DecodeError::Backend(Box::new(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Apple AudioToolbox decoder not yet implemented",
        ))))
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        // This should never be called since create() always fails
        unreachable!("Apple decoder not yet implemented")
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        unreachable!("Apple decoder not yet implemented")
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

impl<C: CodecType> InnerDecoder for Apple<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        AudioDecoder::next_chunk(self)
    }

    fn spec(&self) -> PcmSpec {
        AudioDecoder::spec(self)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        AudioDecoder::seek(self, pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.byte_len_handle.store(len, Ordering::Release);
    }

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

// ────────────────────────────────── Type Aliases ──────────────────────────────────

/// Apple AAC decoder.
pub type AppleAac = Apple<Aac>;

/// Apple MP3 decoder.
pub type AppleMp3 = Apple<Mp3>;

/// Apple FLAC decoder.
pub type AppleFlac = Apple<Flac>;

/// Apple ALAC decoder.
pub type AppleAlac = Apple<Alac>;

// ────────────────────────────────── Tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn test_apple_config_default() {
        let config = AppleConfig::default();
        assert!(config.byte_len_handle.is_none());
    }

    #[test]
    fn test_apple_config_with_handle() {
        let handle = Arc::new(AtomicU64::new(12345));
        let config = AppleConfig {
            byte_len_handle: Some(Arc::clone(&handle)),
        };
        assert!(config.byte_len_handle.is_some());
    }

    #[test]
    fn test_apple_decoder_not_implemented() {
        let cursor = Cursor::new(vec![0u8; 100]);
        let result = AppleAac::create(cursor, AppleConfig::default());
        assert!(result.is_err());

        match result {
            Err(DecodeError::Backend(_)) => {}
            other => panic!("Expected Backend error, got: {:?}", other),
        }
    }

    #[test]
    fn test_type_aliases_exist() {
        fn _check_aac(_: AppleAac) {}
        fn _check_mp3(_: AppleMp3) {}
        fn _check_flac(_: AppleFlac) {}
        fn _check_alac(_: AppleAlac) {}
    }
}
