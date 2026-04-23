//! Symphonia-based audio decoder backend.
//!
//! Provides the generic [`Symphonia<C>`] decoder that implements
//! [`AudioDecoder`] for any codec type implementing [`CodecType`].
//!
//! # Direct Reader Creation (No Probe)
//!
//! When `ContainerFormat` is provided in config, the decoder creates the
//! appropriate format reader directly without probing. This is critical
//! for HLS streams where the container format is known.

use std::{marker::PhantomData, time::Duration};

use self::inner::SymphoniaInner;
use crate::{
    error::DecodeResult,
    traits::{Aac, AudioDecoder, CodecType, DecoderInput, Flac, InnerDecoder, Mp3, Vorbis},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

pub(crate) mod adapter;
pub(crate) mod config;
pub(crate) mod inner;
pub(crate) mod probe;

pub(crate) use self::config::SymphoniaConfig;

/// Generic Symphonia-based decoder parameterized by codec type.
pub(crate) struct Symphonia<C: CodecType> {
    inner: SymphoniaInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> AudioDecoder for Symphonia<C> {
    type Config = SymphoniaConfig;
    type Source = Box<dyn DecoderInput>;

    fn create(source: Self::Source, config: Self::Config) -> DecodeResult<Self>
    where
        Self: Sized,
    {
        let inner = SymphoniaInner::new(source, &config)?;
        Ok(Self {
            inner,
            _codec: PhantomData,
        })
    }

    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }
}

impl<C: CodecType> InnerDecoder for Symphonia<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        AudioDecoder::next_chunk(self)
    }

    fn spec(&self) -> PcmSpec {
        AudioDecoder::spec(self)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        AudioDecoder::seek(self, pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.update_byte_len(len);
    }

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

/// Symphonia-based AAC decoder.
pub(crate) type SymphoniaAac = Symphonia<Aac>;

/// Symphonia-based MP3 decoder.
pub(crate) type SymphoniaMp3 = Symphonia<Mp3>;

/// Symphonia-based FLAC decoder.
pub(crate) type SymphoniaFlac = Symphonia<Flac>;

/// Symphonia-based Vorbis decoder.
pub(crate) type SymphoniaVorbis = Symphonia<Vorbis>;

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::{Arc, atomic::AtomicU64},
    };

    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_utils::{create_test_wav, kithara};

    use super::*;
    use crate::{
        error::DecodeError,
        traits::{AudioDecoder, CodecType},
    };

    #[kithara::test]
    fn test_symphonia_types_exist() {
        fn _check_aac(_: SymphoniaAac) {}
        fn _check_mp3(_: SymphoniaMp3) {}
        fn _check_flac(_: SymphoniaFlac) {}
        fn _check_vorbis(_: SymphoniaVorbis) {}
    }

    struct Pcm;
    impl CodecType for Pcm {
        const CODEC: AudioCodec = AudioCodec::Pcm;
    }

    type SymphoniaPcm = Symphonia<Pcm>;

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
        let decoder = SymphoniaPcm::create(Box::new(cursor), config);
        assert!(decoder.is_ok(), "decoder creation should succeed");

        let decoder = decoder.unwrap();
        assert_eq!(AudioDecoder::spec(&decoder).sample_rate, 44100);
        assert_eq!(AudioDecoder::spec(&decoder).channels, 2);
    }

    #[kithara::test]
    fn test_next_chunk_returns_data() {
        let wav_data = create_test_wav(100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        let chunk = AudioDecoder::next_chunk(&mut decoder).unwrap();
        assert!(chunk.is_some());

        let chunk = chunk.unwrap();
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
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        while AudioDecoder::next_chunk(&mut decoder).unwrap().is_some() {}

        let result = AudioDecoder::next_chunk(&mut decoder).unwrap();
        assert!(result.is_none());
    }

    #[kithara::test]
    fn test_seek_to_beginning() {
        let wav_data = create_test_wav(10000, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let mut decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        let _ = AudioDecoder::next_chunk(&mut decoder).unwrap();
        let _ = AudioDecoder::next_chunk(&mut decoder).unwrap();

        AudioDecoder::seek(&mut decoder, Duration::from_secs(0)).unwrap();

        let chunk = AudioDecoder::next_chunk(&mut decoder).unwrap();
        assert!(chunk.is_some());
    }

    #[kithara::test]
    fn test_duration_available() {
        let wav_data = create_test_wav(44100, 44100, 2);
        let cursor = Cursor::new(wav_data);

        let config = SymphoniaConfig {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let decoder = SymphoniaPcm::create(Box::new(cursor), config).unwrap();

        let duration = AudioDecoder::duration(&decoder);
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
        let result = SymphoniaPcm::create(Box::new(cursor), config);
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
        let result = SymphoniaPcm::create(Box::new(cursor), config);
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
