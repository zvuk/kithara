use kithara::decode::DecoderChunkOutcome;

/// Test-only `Chunk`-variant predicate for [`DecoderChunkOutcome`]. The
/// decoder integration tests assert on chunk-vs-eof outcomes; production
/// matches the variant directly, so this convenience lives in the harness.
pub trait DecoderChunkOutcomeTestExt {
    fn is_chunk(&self) -> bool;
}

impl DecoderChunkOutcomeTestExt for DecoderChunkOutcome {
    fn is_chunk(&self) -> bool {
        matches!(self, Self::Chunk(_))
    }
}
