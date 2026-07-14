use kithara_decode::PcmChunk;

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Audio processing effect in the chain (transforms PCM chunks).
#[kithara::mock(api = AudioEffectMock)]
pub trait AudioEffect: Send + 'static {
    /// Flush remaining buffered data (called at end of stream).
    fn flush(&mut self) -> Option<PcmChunk>;

    /// Process a PCM chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output yet).
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk>;

    /// Reset internal state (called after seek).
    fn reset(&mut self);
}
