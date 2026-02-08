//! Audio pipeline traits: `AudioGenerator` and `AudioEffect`.

use std::time::Duration;

use kithara_decode::{DecodeResult, InnerDecoder, PcmChunk, PcmSpec};
#[cfg(test)]
use unimock::unimock;

/// Source of PCM audio in the processing chain.
#[cfg_attr(test, unimock(api = AudioGeneratorMock))]
pub trait AudioGenerator: Send + 'static {
    /// Decode/generate next chunk of audio.
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>>;

    /// Get current PCM specification.
    fn spec(&self) -> PcmSpec;

    /// Seek to position.
    fn seek(&mut self, position: Duration) -> DecodeResult<()>;

    /// Reset internal state (e.g. after seek).
    fn reset(&mut self);
}

/// Audio processing effect in the chain (transforms PCM chunks).
#[cfg_attr(test, unimock(api = AudioEffectMock))]
pub trait AudioEffect: Send + 'static {
    /// Process a PCM chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output yet).
    fn process(&mut self, chunk: PcmChunk<f32>) -> Option<PcmChunk<f32>>;

    /// Flush remaining buffered data (called at end of stream).
    fn flush(&mut self) -> Option<PcmChunk<f32>>;

    /// Reset internal state (called after seek).
    fn reset(&mut self);
}

impl AudioGenerator for Box<dyn InnerDecoder> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk<f32>>> {
        InnerDecoder::next_chunk(self.as_mut())
    }

    fn spec(&self) -> PcmSpec {
        InnerDecoder::spec(self.as_ref())
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        InnerDecoder::seek(self.as_mut(), position)
    }

    fn reset(&mut self) {
        InnerDecoder::reset(self.as_mut());
    }
}
