/// What one [`LivePcmFeed::poll`] saw: the gap that preceded the samples it
/// appended, and whether the producer is gone.
#[derive(Debug, Clone, Copy, Default)]
pub struct FeedChunk {
    /// Samples the producer dropped since the previous poll.
    pub dropped: u64,
    /// The producer will send nothing further.
    pub has_ended: bool,
}

/// The broadcast's PCM intake: interleaved f32 read without blocking.
///
/// One poll is one consistent view of the feed — the gap, the samples that
/// follow it, and end-of-stream — so the worker cannot see the producer leave
/// while samples are still pending.
pub trait LivePcmFeed: Send {
    /// Append whatever interleaved audio is ready onto `out` and report the
    /// gap that preceded it. Returns without waiting when nothing is ready.
    fn poll(&mut self, out: &mut Vec<f32>) -> FeedChunk;
}
