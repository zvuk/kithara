use std::num::NonZeroU32;

use kithara_platform::time::Duration;
#[cfg(any(test, feature = "mock"))]
use kithara_signal::AudioChunk;
use kithara_signal::{AudioChunkInfo, AudioSpec};

use crate::{AudioReadError, SeekOutcome, SourceEnd, TrackStep};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Decoded source driven exclusively by its chain owner.
#[kithara::mock(api = AudioSourceMock, type Chunk = AudioChunk;)]
pub trait AudioSource: Send + 'static {
    type Chunk: Send + 'static;
    /// Commit the decoded-source boundary represented by admitted output.
    fn commit_source_end(&mut self, _source_end: SourceEnd, _meta: AudioChunkInfo) {}
    /// Seek within the open source on the owning thread.
    ///
    /// # Errors
    ///
    /// Returns [`AudioReadError`] when the source cannot reposition to
    /// `position`.
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;
    /// Rebuild for a host-rate change on the owning thread.
    fn set_host_sample_rate(&mut self, rate: NonZeroU32);
    /// Current host-rate value owned by the open source.
    fn host_sample_rate(&self) -> Option<NonZeroU32>;
    /// Current decoder-format discontinuity.
    fn discontinuity(&self) -> Option<SourceDiscontinuity> {
        None
    }
    /// Publish deferred reader demand and diagnostics.
    fn finish_deferred(&mut self) {}
    /// Prepare input and resolve the active output format.
    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        None
    }
    /// Release a chunk discarded by the chain owner.
    fn retire_chunk(&self, chunk: Self::Chunk) {
        drop(chunk);
    }
    /// Advance one decode step.
    fn step_track(&mut self) -> TrackStep<Self::Chunk>;
    /// Warm reader state on the execution thread.
    fn warm_up(&mut self) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get, copy)]
#[non_exhaustive]
pub struct SourceDiscontinuity {
    spec: AudioSpec,
    revision: u64,
}

impl SourceDiscontinuity {
    /// Construct a decoder-format reset stamp.
    #[must_use]
    pub const fn new(revision: u64, spec: AudioSpec) -> Self {
        Self { spec, revision }
    }
}
