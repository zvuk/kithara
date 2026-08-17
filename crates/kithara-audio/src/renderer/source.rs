use kithara_decode::{DecodeError, PcmSpec};
use kithara_platform::sync::Arc;
use kithara_stream::SeekObserve;

use crate::pipeline::track;

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Trait for audio sources processed in a blocking worker thread.
///
/// The worker calls `step_track()` once per scheduling round. Each call
/// performs at most one FSM transition and returns a [`TrackStep`] that
/// tells the worker what happened.
#[kithara::mock(api = MockAudioWorkerSource, type Chunk = kithara_decode::PcmChunk;)]
pub trait AudioWorkerSource: Send + 'static {
    type Chunk: Send + 'static;

    /// The producer's current decode epoch — the seek epoch the most recent
    /// decode operated under. The worker stamps terminal markers (EOF /
    /// failure) with this rather than the live `seek_observe().epoch()`,
    /// which a concurrent consumer seek may have already advanced: stamping
    /// the live epoch would mislabel a stale terminal as the newer seek's and
    /// let the consumer's epoch validator accept it (the oversubscription
    /// false-EOF race). Defaults to the live seek epoch for sources whose
    /// decode epoch is the seek epoch (e.g. test mocks).
    fn decode_epoch(&self) -> u64 {
        self.seek_observe().epoch()
    }

    /// Drain deferred off-core signals armed on the forbid-blocking decode
    /// core: reader-hook events (published to the event bus) and the reader→peer
    /// wake (a cross-thread `notify_one` the RT core must not make). The worker
    /// shell calls this once per pass, off the checked path. Default no-op.
    fn flush_deferred(&mut self) {}

    /// Hand back a chunk the node discarded on the produce core. A pooled buffer returned to a full
    /// shard deallocates, so the chunk goes to the source's retire queue and the shell drops it in
    /// `flush_deferred`. Default no-op for sources that pool nothing.
    fn retire_chunk(&self, chunk: Self::Chunk) {
        let _ = chunk;
    }

    /// Narrow seek-observe handle — epoch queries and decoder-node seek latch.
    fn seek_observe(&self) -> Arc<dyn SeekObserve>;

    /// Advance the track FSM by one step.
    ///
    /// Handles seek preemption, source readiness, decoding, and all
    /// internal state transitions. Returns:
    /// - `Produced` — decoded chunk ready for the consumer.
    /// - `StateChanged` — internal transition; caller should call again.
    /// - `Blocked` — the source is not ready; the caller should wait for a wake.
    /// - `Eof` — end of stream (may transition out via seek-after-EOF).
    /// - `Failed` — terminal failure.
    fn step_track(&mut self) -> track::TrackStep<Self::Chunk>;

    /// Take one ordered same-epoch decoder replacement boundary.
    ///
    /// Sources without decoder replacement retain the default empty result.
    fn take_presentation_barrier(&mut self) -> Option<(u64, PcmSpec)> {
        None
    }

    /// Transition the source FSM after presentation can no longer continue.
    ///
    /// Sources with a typed failure state override this so activity and UI
    /// observe the same terminal failure as the final PCM marker.
    fn presentation_failed(&mut self, error: DecodeError) {
        let _ = error;
    }

    /// One-time worker-thread warmup, called from the scheduler shell when the
    /// node registers. Pre-touches the produce-core read path so lazy global
    /// thread-locals (the `arc_swap` committed-read debt node) allocate here,
    /// off the forbid-blocking decode core. Default no-op.
    fn warm_up(&mut self) {}
}
