//! Audio pipeline traits.

use kithara_decode::PcmChunk;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

/// Audio processing effect in the chain (transforms PCM chunks).
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = AudioEffectMock))]
pub trait AudioEffect: Send + 'static {
    /// Process a PCM chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output yet).
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk>;

    /// Flush remaining buffered data (called at end of stream).
    fn flush(&mut self) -> Option<PcmChunk>;

    /// Reset internal state (called after seek).
    fn reset(&mut self);
}
