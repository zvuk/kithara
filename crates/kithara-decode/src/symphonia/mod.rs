//! Symphonia-based audio decoder backend.
//!
//! Provides the [`Symphonia`] decoder that adapts `SymphoniaInner` to the
//! [`InnerDecoder`] trait used by the rest of the crate.
//!
//! # Direct Reader Creation (No Probe)
//!
//! When `ContainerFormat` is provided in config, the decoder creates the
//! appropriate format reader directly without probing. This is critical
//! for HLS streams where the container format is known.

use std::time::Duration;

use self::inner::SymphoniaInner;
use crate::{
    error::DecodeResult,
    traits::{DecoderChunkOutcome, DecoderInput, DecoderSeekOutcome, InnerDecoder},
    types::{PcmSpec, TrackMetadata},
};

pub(crate) mod adapter;
pub(crate) mod config;
pub(crate) mod entry;
pub(crate) mod error_chain;
pub(crate) mod inner;
pub(crate) mod probe;

pub(crate) use self::{config::SymphoniaConfig, entry::create_from_boxed};

/// Symphonia-backed decoder. Codec selection happens inside
/// `SymphoniaInner` from the config's `container` (direct reader) or
/// via Symphonia's probe chain — no compile-time codec marker needed.
pub(crate) struct Symphonia {
    inner: SymphoniaInner,
}

impl Symphonia {
    /// Create a Symphonia decoder from a boxed source and config.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::error::DecodeError`] when the source cannot be
    /// read, the codec/container is unsupported, or construction fails.
    pub(crate) fn new(
        source: Box<dyn DecoderInput>,
        config: &SymphoniaConfig,
    ) -> DecodeResult<Self> {
        let inner = SymphoniaInner::new(source, config)?;
        Ok(Self { inner })
    }
}

impl InnerDecoder for Symphonia {
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.inner.seek(pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.update_byte_len(len);
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::{Arc, atomic::AtomicU64},
    };

    use kithara_stream::ContainerFormat;
    use kithara_test_utils::{create_test_wav, kithara};

    use super::*;
    use crate::error::DecodeError;

    #[kithara::test]
    #[case(Some(ContainerFormat::Wav))]
    #[case(None)]
    fn test_create_decoder_wav(#[case] container: Option<ContainerFormat>) {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container,
            ..Default::default()
        };
        let decoder = Symphonia::new(Box::new(cursor), &config);
        assert!(decoder.is_ok(), "decoder creation should succeed");

        let decoder = decoder.unwrap();
        assert_eq!(decoder.spec().sample_rate, 44100);
        assert_eq!(decoder.spec().channels, 2);
    }

    #[kithara::test]
    fn test_next_chunk_returns_data() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = Symphonia::new(Box::new(cursor), &config).unwrap();

        let outcome = decoder.next_chunk().unwrap();
        assert!(outcome.is_chunk());

        let chunk = outcome.into_chunk().unwrap();
        assert_eq!(chunk.spec().sample_rate, 44100);
        assert_eq!(chunk.spec().channels, 2);
        assert!(!chunk.pcm.is_empty());
    }

    #[kithara::test]
    fn test_next_chunk_eof() {
        let wav_data = create_test_wav(10, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = Symphonia::new(Box::new(cursor), &config).unwrap();

        while decoder.next_chunk().unwrap().is_chunk() {}

        let result = decoder.next_chunk().unwrap();
        assert!(result.is_eof());
    }

    #[kithara::test]
    fn test_seek_to_beginning() {
        let wav_data = create_test_wav(10000, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = Symphonia::new(Box::new(cursor), &config).unwrap();

        let _ = decoder.next_chunk().unwrap();
        let _ = decoder.next_chunk().unwrap();

        decoder.seek(Duration::from_secs(0)).unwrap();

        let outcome = decoder.next_chunk().unwrap();
        assert!(outcome.is_chunk());
    }

    #[kithara::test]
    fn test_duration_available() {
        let wav_data = create_test_wav(44100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let decoder = Symphonia::new(Box::new(cursor), &config).unwrap();

        let duration = decoder.duration();
        assert!(duration.is_some());

        let dur = duration.unwrap();
        assert!(dur.as_secs_f64() > 0.9 && dur.as_secs_f64() < 1.1);
    }

    #[kithara::test]
    #[case(Vec::new())]
    #[case([0xDE, 0xAD, 0xBE, 0xEF].repeat(100))]
    fn test_invalid_input_fails(#[case] data: Vec<u8>) {
        let cursor = Cursor::new(data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let result = Symphonia::new(Box::new(cursor), &config);
        assert!(result.is_err());
    }

    #[kithara::test]
    fn test_unsupported_container_returns_error() {
        let data = vec![0u8; 100];
        let cursor = Cursor::new(data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::MpegTs),
            ..Default::default()
        };
        let result = Symphonia::new(Box::new(cursor), &config);
        assert!(matches!(result, Err(DecodeError::UnsupportedContainer(_))));
    }

    #[kithara::test]
    fn test_handle_integration() {
        // Sanity check that Arc<AtomicU64> can be shared via config.
        let handle = Arc::new(AtomicU64::new(12345));
        let config = SymphoniaConfig {
            byte_len_handle: Some(Arc::clone(&handle)),
            ..Default::default()
        };
        assert!(config.byte_len_handle.is_some());
    }
}
