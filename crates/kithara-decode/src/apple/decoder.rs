//! Apple `AudioToolbox` decoder shell + factory.
//!
//! Lives in a sibling file because lib.rs / mod.rs files must stay
//! declaration-only (`style.no-items-in-lib-or-mod-rs`).

use std::{fmt, sync::atomic::Ordering, time::Duration};

use kithara_stream::AudioCodec;

use super::{config::AppleConfig, inner::AppleInner};
use crate::{
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
    traits::{DecoderChunkOutcome, DecoderSeekOutcome, InnerDecoder},
    types::{PcmSpec, TrackMetadata},
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
    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.inner.next_chunk()
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.inner.seek(pos)
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.byte_len_handle.store(len, Ordering::Release);
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
