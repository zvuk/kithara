use kithara_platform::sync::Arc;
#[cfg(any(test, feature = "mock"))]
use kithara_signal::AudioChunk;
use kithara_signal::AudioSpec;
use kithara_stream::SeekObserve;

use crate::{SourceEnd, TrackStep};

/// Progress of producing old-map PCM before a scheduled decoder seek.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduledSeekPreparation {
    AwaitingActivation,
    ProducingOldPcm,
    Ready,
}

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Worker-independent source of decoded-audio chunks.
///
/// Each step advances at most one source transition; scheduling belongs to the executor.
#[kithara::mock(api = AudioSourceMock, type Chunk = AudioChunk;)]
pub trait AudioSource: Send + 'static {
    type Chunk: Send + 'static;

    /// Commit the decoded-source boundary after rendered audio is accepted by
    /// the final producer port.
    fn commit_source_end(&mut self, _source_end: SourceEnd, _epoch: u64) {}

    /// Decode epoch assigned to the most recent source work.
    /// May lag the live seek epoch until the source applies the seek.
    fn decode_epoch(&self) -> u64 {
        self.seek_observe().epoch()
    }

    /// Current explicit source discontinuity, when the source has one.
    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        None
    }

    /// Finish deferred source publication after decorators are serviced.
    fn finish_deferred(&mut self) {}

    /// Resolve the active output format before producer decorators are serviced.
    /// Sources without a split shell keep the default no-op phases.
    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        None
    }

    /// Report whether old-map PCM has reached the scheduled Warp activation.
    fn prepare_scheduled_seek(&mut self) -> ScheduledSeekPreparation {
        ScheduledSeekPreparation::Ready
    }

    /// Reclaim a discarded chunk from scheduler `recycle`, outside the checked
    /// producer tick.
    fn retire_chunk(&self, chunk: Self::Chunk) {
        let _ = chunk;
    }

    /// Narrow seek-observe handle for epoch queries and the decoder seek latch.
    fn seek_observe(&self) -> Arc<dyn SeekObserve>;

    /// Advance the source FSM by at most one transition.
    fn step_track(&mut self) -> TrackStep<Self::Chunk>;

    /// One-time execution-thread warmup before the first checked source step.
    fn warm_up(&mut self) {}
}

#[cfg(test)]
pub(crate) trait AudioSourceExt: AudioSource {
    fn flush_deferred(&mut self) {
        let _ = self.prepare_deferred();
        self.finish_deferred();
    }
}

#[cfg(test)]
impl<S> AudioSourceExt for S where S: AudioSource + ?Sized {}
/// Exact worker-side reset stamp for a decoded-audio lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, copy)]
#[non_exhaustive]
pub struct SourceDiscontinuity {
    /// Output format active after the reset.
    spec: AudioSpec,
    /// Monotonic lane-local reset revision.
    revision: u64,
}

impl SourceDiscontinuity {
    /// Construct a reset stamp at the active decoded format.
    #[must_use]
    pub const fn new(revision: u64, spec: AudioSpec) -> Self {
        Self { spec, revision }
    }
}
