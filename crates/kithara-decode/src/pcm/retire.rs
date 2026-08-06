use crate::PcmChunk;

/// Where a chunk goes when the caller must not free it.
///
/// A [`PcmChunk`] holds a pooled buffer and returning one to a full shard
/// deallocates, so the produce core hands displaced chunks to a sink the
/// worker shell drains.
pub trait ChunkSink {
    fn retire(&self, chunk: PcmChunk);
}

/// Sink for callers that are free to deallocate. Drops each chunk in place.
pub struct DropChunks;

impl ChunkSink for DropChunks {
    fn retire(&self, _chunk: PcmChunk) {}
}
