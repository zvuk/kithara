use std::num::NonZeroU32;

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_platform::sync::Arc;
use kithara_stream::{SeekControl, SeekObserve, SeekState};

use super::AudioWorkerSource;
use crate::pipeline::{
    fetch::Fetch,
    track::{TrackStep, WaitingReason},
};

const MOCK_FRAMES: usize = 512;

fn mock_chunk(frame_offset: u64) -> PcmChunk {
    let spec = PcmSpec::new(
        2,
        NonZeroU32::new(48_000).expect("fixture rate is non-zero"),
    );
    PcmChunk::new(
        PcmMeta {
            spec,
            frames: u32::try_from(MOCK_FRAMES).expect("fixture frame count fits u32"),
            frame_offset,
            ..Default::default()
        },
        PcmPool::default().attach(vec![0.0; MOCK_FRAMES * usize::from(spec.channels)]),
    )
}

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
        let frame_offset =
            u64::try_from(self.cursor.saturating_mul(MOCK_FRAMES)).unwrap_or(u64::MAX);
        self.cursor += 1;
        TrackStep::Produced(Fetch::data(mock_chunk(frame_offset), self.seek_obs.epoch()))
    }
}
