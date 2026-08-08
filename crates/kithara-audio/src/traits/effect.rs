use kithara_decode::PcmChunk;

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Audio processing effect in the chain (transforms PCM chunks).
#[kithara::mock(api = AudioEffectMock)]
pub trait AudioEffect: Send + 'static {
    /// Flush remaining buffered data (called at end of stream).
    fn flush(&mut self) -> Option<PcmChunk>;

    /// Frames accepted by this effect but not yet represented in its output,
    /// counted on the pipeline's **source** axis.
    ///
    /// The decode frontier is the admitted source endpoint minus this count, so
    /// a wrong answer moves where a recreate resumes and where a variant splice
    /// lands. There is no default: an effect that buffers has to say so.
    /// Sample-synchronous effects return `0`. An effect that buffers **and**
    /// sits behind a duration-changing stage cannot express its hold on the
    /// source axis and must not buffer.
    fn held_source_frames(&self) -> u64;

    /// Process a PCM chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output yet).
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk>;

    /// Reset internal state (called after seek).
    fn reset(&mut self);
}
