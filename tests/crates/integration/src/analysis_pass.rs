//! Helpers for driving an analysis pass from a test.

use kithara::{
    audio::{
        AudioControl, AudioRead, AudioReader, AudioSession, ChunkOutcome, DecodeError,
        PendingReason, ReadOutcome, SeekOutcome,
    },
    decode::TrackMetadata,
    events::EventBus,
    platform::time::Duration,
    signal::AudioSpec,
};

/// A reader that never yields a chunk, so a pass fed by it covers nothing on
/// its own. Whatever such a pass ends up covering came from a producer.
pub struct Stalled {
    bus: EventBus,
    metadata: TrackMetadata,
    spec: AudioSpec,
}

/// Boxed [`Stalled`] reader on `spec`, ready for `AnalysisWorker::analyze`.
#[must_use]
pub fn stalled_reader(spec: AudioSpec) -> Box<dyn AudioReader> {
    Box::new(Stalled {
        spec,
        bus: EventBus::default(),
        metadata: TrackMetadata::default(),
    })
}

impl AudioSession for Stalled {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl AudioRead for Stalled {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        Ok(ChunkOutcome::Pending {
            reason: PendingReason::Buffering,
            position: Duration::ZERO,
        })
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        unreachable!("analysis pulls chunks")
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        unreachable!("analysis pulls chunks")
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for Stalled {
    fn seek(&mut self, _position: Duration) -> Result<SeekOutcome, DecodeError> {
        unreachable!("this reader never moves")
    }
}
