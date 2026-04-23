//! Apple `AudioToolbox` decoder backend.
//!
//! This module implements hardware-accelerated audio decoding using Apple's
//! `AudioToolbox` framework via `AudioFileStream` + `AudioConverter` APIs.
//!
//! Uses a streaming approach: `AudioFileStream` parses container format,
//! `AudioConverter` decodes compressed audio packets to PCM.
//!
//! Supports AAC, MP3, FLAC, and ALAC codecs with hardware acceleration
//! when available on macOS and iOS.

mod backend;
mod config;
mod consts;
mod converter;
mod ffi;
mod inner;
mod parser;

use std::{fmt, marker::PhantomData, sync::atomic::Ordering, time::Duration};

use kithara_stream::AudioCodec;
use tracing::debug;

use self::inner::AppleInner;
pub(crate) use self::{backend::AppleBackend, config::AppleConfig};
use crate::{
    backend::{BoxedSource, RecoverableHardwareError, recoverable_hardware_error},
    error::{DecodeError, DecodeResult},
    traits::{Aac, Alac, AudioDecoder, CodecType, DecoderInput, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Apple `AudioToolbox` streaming decoder parameterized by codec type.
///
/// Uses `AudioFileStream` for parsing and `AudioConverter` for decoding.
pub(crate) struct Apple<C: CodecType> {
    inner: AppleInner,
    _codec: PhantomData<C>,
}

impl<C: CodecType> fmt::Debug for Apple<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Apple")
            .field("spec", &self.inner.spec)
            .field("position", &self.inner.position)
            .field("duration", &self.inner.duration)
            .finish_non_exhaustive()
    }
}

impl<C: CodecType> AudioDecoder for Apple<C> {
    type Config = AppleConfig;
    type Source = Box<dyn DecoderInput>;

    fn create(source: Self::Source, config: Self::Config) -> DecodeResult<Self>
    where
        Self: Sized,
    {
        debug!(codec = ?C::CODEC, "Apple decoder: create called");
        let inner = AppleInner::try_new(source, &config).map_err(|error| error.error)?;
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

impl<C: CodecType> InnerDecoder for Apple<C> {
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
        self.inner.byte_len_handle.store(len, Ordering::Release);
    }

    fn duration(&self) -> Option<Duration> {
        AudioDecoder::duration(self)
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

pub(crate) fn try_create_apple_decoder(
    codec: AudioCodec,
    source: BoxedSource,
    config: &AppleConfig,
) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            AppleInner::try_new(source, config).map(|inner| {
                Box::new(Apple::<Aac> {
                    inner,
                    _codec: PhantomData,
                }) as Box<dyn InnerDecoder>
            })
        }
        AudioCodec::Mp3 => AppleInner::try_new(source, config).map(|inner| {
            Box::new(Apple::<Mp3> {
                inner,
                _codec: PhantomData,
            }) as Box<dyn InnerDecoder>
        }),
        AudioCodec::Flac => AppleInner::try_new(source, config).map(|inner| {
            Box::new(Apple::<Flac> {
                inner,
                _codec: PhantomData,
            }) as Box<dyn InnerDecoder>
        }),
        AudioCodec::Alac => AppleInner::try_new(source, config).map(|inner| {
            Box::new(Apple::<Alac> {
                inner,
                _codec: PhantomData,
            }) as Box<dyn InnerDecoder>
        }),
        _ => Err(recoverable_hardware_error(
            source,
            DecodeError::UnsupportedCodec(codec),
        )),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn test_generic_apple_compiles_for_all_codecs() {
        fn _check_aac(_: Apple<Aac>) {}
        fn _check_mp3(_: Apple<Mp3>) {}
        fn _check_flac(_: Apple<Flac>) {}
        fn _check_alac(_: Apple<Alac>) {}
    }
}
