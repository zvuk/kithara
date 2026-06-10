use std::{collections::VecDeque, num::NonZeroU32, sync::Arc, time::Duration};

use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use num_traits::cast::AsPrimitive;

use crate::traits::{ChunkOutcome, PcmReader, PendingReason, ReadOutcome, SeekOutcome};
use crate::worker::PreloadGate;

pub(crate) const SR: u32 = 44_100;
const CH: u16 = 2;

pub(crate) fn spec() -> PcmSpec {
    PcmSpec {
        channels: CH,
        sample_rate: SR,
    }
}

/// Interleaved stereo sine, phase-accumulated.
pub(crate) fn sine(frames: usize) -> Vec<f32> {
    let inc = std::f64::consts::TAU * 440.0 / f64::from(SR);
    let mut phase = 0.0_f64;
    let mut out = Vec::with_capacity(frames * usize::from(CH));
    for _ in 0..frames {
        let sample_f64 = 0.5 * phase.sin();
        let sample: f32 = sample_f64.as_();
        out.push(sample);
        out.push(sample);
        phase += inc;
    }
    out
}

fn chunk(samples: &[f32]) -> PcmChunk {
    let frames = samples.len() / usize::from(CH);
    PcmChunk::new(
        PcmMeta {
            spec: spec(),
            frames: u32::try_from(frames).unwrap_or(0),
            ..Default::default()
        },
        PcmPool::default().attach(samples.to_vec()),
    )
}

/// Scripted `PcmReader` for analysis tests: pops pre-built `next_chunk`
/// outcomes; the playback-oriented methods are unreachable on this path.
pub(crate) struct FakeReader {
    outcomes: VecDeque<Result<ChunkOutcome, DecodeError>>,
    bus: EventBus,
    metadata: TrackMetadata,
}

impl FakeReader {
    fn new(outcomes: VecDeque<Result<ChunkOutcome, DecodeError>>) -> Self {
        Self {
            outcomes,
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
        }
    }

    /// Split `samples` into `parts` chunks followed by EOF.
    pub(crate) fn chunked(samples: &[f32], parts: usize) -> Self {
        let per = samples.len().div_ceil(parts.max(1)) / usize::from(CH) * usize::from(CH);
        let mut outcomes: VecDeque<_> = samples
            .chunks(per.max(usize::from(CH)))
            .map(|part| Ok(ChunkOutcome::Chunk(chunk(part))))
            .collect();
        outcomes.push_back(Ok(eof()));
        Self::new(outcomes)
    }

    /// Like [`Self::chunked`] with a `Pending` tick between every chunk.
    pub(crate) fn chunked_with_pending(samples: &[f32], parts: usize) -> Self {
        let mut with_pending = VecDeque::new();
        for outcome in Self::chunked(samples, parts).outcomes {
            with_pending.push_back(Ok(pending()));
            with_pending.push_back(outcome);
        }
        Self::new(with_pending)
    }

    pub(crate) fn failing() -> Self {
        Self::new(VecDeque::from([Err(DecodeError::InvalidData(
            "scripted failure".into(),
        ))]))
    }

    pub(crate) fn empty() -> Self {
        Self::new(VecDeque::from([Ok(eof())]))
    }
}

fn eof() -> ChunkOutcome {
    ChunkOutcome::Eof {
        position: Duration::ZERO,
    }
}

fn pending() -> ChunkOutcome {
    ChunkOutcome::Pending {
        reason: PendingReason::Buffering,
        position: Duration::ZERO,
    }
}

impl PcmReader for FakeReader {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.outcomes.pop_front().unwrap_or_else(|| Ok(eof()))
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        None
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        unreachable!("analysis uses next_chunk")
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        unreachable!("analysis uses next_chunk")
    }

    fn seek(&mut self, _position: Duration) -> Result<SeekOutcome, DecodeError> {
        unreachable!("analysis never seeks")
    }

    fn set_host_sample_rate(&self, _sample_rate: NonZeroU32) {}

    fn spec(&self) -> PcmSpec {
        spec()
    }
}
