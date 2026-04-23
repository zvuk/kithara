//! Apple `AudioToolbox` decoder backend.
//!
//! Opens an `AudioFile` through `AudioFileOpenWithCallbacks`, reads
//! compressed packets on demand, and decodes them to PCM through
//! `AudioConverter`. Unlike the previous `AudioFileStream` path this
//! supports atom-structured containers (fMP4, MP4, CAF, WAV) — the
//! capability required for HLS seek.
//!
//! The backend dispatches on the runtime `AudioCodec` enum in
//! `try_create_apple_decoder`; no compile-time codec markers are used.

mod audiofile;
mod backend;
mod config;
mod consts;
mod converter;
mod ffi;
mod fmp4;
mod inner;
mod reader;

use std::{fmt, sync::atomic::Ordering, time::Duration};

use kithara_stream::AudioCodec;

use self::inner::AppleInner;
pub(crate) use self::{backend::AppleBackend, config::AppleConfig};
use crate::{
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
    traits::InnerDecoder,
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

/// Apple `AudioToolbox` decoder.
///
/// Uses `AudioFile` + callbacks for container parsing (atom-aware, seekable)
/// and `AudioConverter` for compressed → PCM decoding.
pub(crate) struct Apple {
    inner: AppleInner,
}

impl fmt::Debug for Apple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Apple")
            .field("spec", &self.inner.spec)
            .field("position", &self.inner.position)
            .field("duration", &self.inner.duration)
            .finish_non_exhaustive()
    }
}

impl InnerDecoder for Apple {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.inner.next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.inner.seek(pos)
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.byte_len_handle.store(len, Ordering::Release);
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

pub(crate) fn try_create_apple_decoder(
    codec: AudioCodec,
    source: BoxedSource,
    config: &AppleConfig,
) -> Result<Box<dyn InnerDecoder>, DecodeError> {
    match codec {
        AudioCodec::AacLc
        | AudioCodec::AacHe
        | AudioCodec::AacHeV2
        | AudioCodec::Mp3
        | AudioCodec::Flac
        | AudioCodec::Alac
        | AudioCodec::Pcm => {
            let inner = AppleInner::try_new(source, config)?;
            Ok(Box::new(Apple { inner }))
        }
        _ => Err(DecodeError::UnsupportedCodec(codec)),
    }
}
