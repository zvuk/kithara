//! Symphonia decoder shell — split out of mod.rs for the
//! `style.no-items-in-lib-or-mod-rs` rule.

use std::time::Duration;

use super::{config::SymphoniaConfig, inner::SymphoniaInner};
use crate::{
    error::DecodeResult,
    traits::{DecoderChunkOutcome, DecoderInput, DecoderSeekOutcome, InnerDecoder},
    types::{PcmSpec, TrackMetadata},
};

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
        self.inner.update_byte_len(len);
    }
}
