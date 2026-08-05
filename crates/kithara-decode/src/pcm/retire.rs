use crate::PcmChunk;

/// Where a chunk goes when the caller must not free it.
///
/// A [`PcmChunk`] holds a pooled buffer, and returning one to a full pool
/// shard deallocates. Paths that run on the produce core therefore hand the
/// chunks they displace to a sink the worker shell drains, instead of dropping
/// them where they stand.
pub trait ChunkSink {
    fn retire(&self, chunk: PcmChunk);
}

/// Sink for callers that are free to deallocate. Drops each chunk in place.
pub struct DropChunks;

impl ChunkSink for DropChunks {
    fn retire(&self, _chunk: PcmChunk) {}
}
