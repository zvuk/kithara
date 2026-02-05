//! Android MediaCodec decoder backend (placeholder).
//!
//! This module will implement hardware-accelerated audio decoding
//! using Android's MediaCodec API via JNI or NDK.
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
    types::{PcmChunk, PcmSpec},
};

/// Configuration for Android MediaCodec decoder.
#[derive(Debug, Clone, Default)]
pub struct AndroidConfig {
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
}

/// Android MediaCodec decoder inner state (placeholder).
struct AndroidInner {
    spec: PcmSpec,
    byte_len_handle: Arc<AtomicU64>,
}

/// Android MediaCodec decoder (placeholder).
///
/// This decoder will use Android's MediaCodec API to decode
/// AAC, MP3, FLAC, and ALAC using hardware acceleration when available.
///
/// Currently returns `UnsupportedCodec` error — actual implementation pending.
pub struct Android<C: CodecType> {
    inner: AndroidInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> std::fmt::Debug for Android<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Android")
            .field("spec", &self.inner.spec)
            .finish_non_exhaustive()
    }
}

impl<C: CodecType> AudioDecoder for Android<C> {
    type Config = AndroidConfig;

    fn create<R>(_source: R, _config: Self::Config) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
        Self: Sized,
    {
        // TODO: Implement actual MediaCodec initialization
        Err(DecodeError::Backend(Box::new(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Android MediaCodec decoder not yet implemented",
        ))))
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        // This should never be called since create() always fails
        unreachable!("Android decoder not yet implemented")
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, _pos: Duration) -> DecodeResult<()> {
        unreachable!("Android decoder not yet implemented")
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

impl<C: CodecType> InnerDecoder for Android<C> {
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
}

// ────────────────────────────────── Type Aliases ──────────────────────────────────

/// Android AAC decoder.
pub type AndroidAac = Android<Aac>;

/// Android MP3 decoder.
pub type AndroidMp3 = Android<Mp3>;

/// Android FLAC decoder.
pub type AndroidFlac = Android<Flac>;

/// Android ALAC decoder.
pub type AndroidAlac = Android<Alac>;

// ────────────────────────────────── Tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn test_android_config_default() {
        let config = AndroidConfig::default();
        assert!(config.byte_len_handle.is_none());
    }

    #[test]
    fn test_android_config_with_handle() {
        let handle = Arc::new(AtomicU64::new(12345));
        let config = AndroidConfig {
            byte_len_handle: Some(Arc::clone(&handle)),
        };
        assert!(config.byte_len_handle.is_some());
    }

    #[test]
    fn test_android_decoder_not_implemented() {
        let cursor = Cursor::new(vec![0u8; 100]);
        let result = AndroidAac::create(cursor, AndroidConfig::default());
        assert!(result.is_err());

        match result {
            Err(DecodeError::Backend(_)) => {}
            other => panic!("Expected Backend error, got: {:?}", other),
        }
    }

    #[test]
    fn test_type_aliases_exist() {
        fn _check_aac(_: AndroidAac) {}
        fn _check_mp3(_: AndroidMp3) {}
        fn _check_flac(_: AndroidFlac) {}
        fn _check_alac(_: AndroidAlac) {}
    }
}
