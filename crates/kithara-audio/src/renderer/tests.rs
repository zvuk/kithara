use kithara_decode::PcmChunk;
use kithara_platform::sync::Arc;
use kithara_stream::{SeekControl, SeekObserve, SeekState};

use super::AudioWorkerSource;
use crate::pipeline::{
    fetch::{Fetch, FetchKind},
    track::{TrackStep, WaitingReason},
};

pub(crate) struct MockSource {
    pub(crate) seek: Arc<dyn SeekControl>,
    seek_obs: Arc<dyn SeekObserve>,
    ready: bool,
    should_panic: bool,
    chunks_to_produce: usize,
    cursor: usize,
}

impl MockSource {
    pub(crate) fn new(chunks: usize) -> Self {
        let state = Arc::new(SeekState::new());
        let seek = Arc::clone(&state) as Arc<dyn SeekControl>;
        let seek_obs = Arc::clone(&state) as Arc<dyn SeekObserve>;
        Self {
            seek,
            seek_obs,
            chunks_to_produce: chunks,
            cursor: 0,
            ready: true,
            should_panic: false,
        }
    }

    pub(crate) fn not_ready(chunks: usize) -> Self {
        Self {
            ready: false,
            ..Self::new(chunks)
        }
    }

    pub(crate) fn panicking() -> Self {
        Self {
            should_panic: true,
            ..Self::new(100)
        }
    }
}

impl AudioWorkerSource for MockSource {
    type Chunk = PcmChunk;

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        if self.seek_obs.is_pending() || self.seek_obs.is_flushing() {
            let epoch = self.seek_obs.epoch();
            self.seek.complete(epoch);
            self.seek.clear_pending(epoch);
            return TrackStep::StateChanged;
        }
        if !self.ready {
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        if self.should_panic {
            panic!("mock panic for testing");
        }
        if self.cursor >= self.chunks_to_produce {
            return TrackStep::Eof;
        }
        self.cursor += 1;
        TrackStep::Produced(Fetch::new(PcmChunk::default(), FetchKind::Data, 0))
    }
}
