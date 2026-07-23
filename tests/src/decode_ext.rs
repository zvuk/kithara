use kithara::decode::DecoderChunkOutcome;

/// Test-only variant predicates for [`DecoderChunkOutcome`]. The decoder
/// integration tests assert on chunk-vs-eof outcomes; production matches the
/// variant directly, so these conveniences live in the harness.
pub trait DecoderChunkOutcomeTestExt {
    fn is_chunk(&self) -> bool;
    fn is_eof(&self) -> bool;
}

impl DecoderChunkOutcomeTestExt for DecoderChunkOutcome {
    fn is_chunk(&self) -> bool {
        matches!(self, Self::Chunk(_))
    }

    fn is_eof(&self) -> bool {
        matches!(self, Self::Eof)
    }
}
