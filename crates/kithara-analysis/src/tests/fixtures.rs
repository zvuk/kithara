use std::{collections::VecDeque, num::NonZeroU32};

#[cfg(feature = "analysis-waveform")]
use kithara_audio::PendingReason;
use kithara_audio::{
    AudioControl, AudioRead, AudioSession, ChunkOutcome, ReadOutcome, SeekOutcome,
};
use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec};
use num_traits::cast::{AsPrimitive, ToPrimitive};

use crate::test_pools::{Pools, sample_buffer};
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
use crate::{
    analyzer::TrackAnalysis,
    beat::{BeatDetectError, BeatDetector, BeatMark, RawBeats},
    blob::to_bytes,
    waveform::bucket::Waveform,
};

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(super) const MARKER_TOLERANCE: u64 = 64;

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(super) type Artifacts = (Waveform, Vec<u64>);

/// One beat a quarter of the way into every window. The pass hands its
/// detector to a worker thread, and a mock has to go back to the thread that
/// made it.
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
struct OneBeatPerWindow;

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
impl BeatDetector for OneBeatPerWindow {
    fn detect(&self, _mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        Ok(RawBeats {
            beats: vec![BeatMark::at(0.25)],
            downbeats: vec![BeatMark::at(0.25)],
        })
    }
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(super) fn beat_detector() -> Box<dyn BeatDetector> {
    Box::new(OneBeatPerWindow)
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(super) fn artifacts(snapshot: &TrackAnalysis) -> Artifacts {
    (
        snapshot.waveform().cloned().unwrap_or_default(),
        snapshot
            .beat()
            .map(|beat| beat.artifact().beats().to_vec())
            .unwrap_or_default(),
    )
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(super) fn assert_agrees(want: &Artifacts, got: &Artifacts, what: &str) {
    assert_eq!(
        to_bytes(&got.0),
        to_bytes(&want.0),
        "{what}: the waveform must be identical"
    );
    assert_eq!(
        got.1.len(),
        want.1.len(),
        "{what}: the same markers must be found"
    );
    for (a, b) in want.1.iter().zip(got.1.iter()) {
        assert!(
            a.abs_diff(*b) <= MARKER_TOLERANCE,
            "{what}: marker moved from {a} to {b}"
        );
    }
}

pub(super) const SR: u32 = 44_100;
pub(super) const CH: u16 = 2;

pub(super) fn spec() -> AudioSpec {
    AudioSpec {
        channels: CH,
        sample_rate: NonZeroU32::new(SR).unwrap(),
    }
}

pub(super) fn sine(frames: usize) -> Vec<f32> {
    sine_from(0, frames)
}

pub(super) fn sine_from(at: u64, frames: usize) -> Vec<f32> {
    let inc = std::f64::consts::TAU * 440.0 / f64::from(SR);
    let mut out = Vec::with_capacity(frames * usize::from(CH));
    for index in 0..frames {
        let frame = at.saturating_add(index.to_u64().unwrap_or(0));
        let sample_f64 = 0.5 * (inc * frame.to_f64().unwrap_or(0.0)).sin();
        let sample: f32 = sample_f64.as_();
        out.push(sample);
        out.push(sample);
    }
    out
}

pub(super) fn chunk(pools: &Pools, samples: &[f32], frame_offset: u64) -> AudioChunk {
    let frames = samples.len() / usize::from(CH);
    AudioChunk::new(
        AudioChunkInfo {
            spec: spec(),
            frames: u32::try_from(frames).unwrap_or(0),
            frame_offset,
            ..Default::default()
        },
        sample_buffer(pools, samples),
    )
}

pub(super) struct FakeReader {
    bus: EventBus,
    metadata: TrackMetadata,
    outcomes: VecDeque<Result<ChunkOutcome, DecodeError>>,
}

impl FakeReader {
    pub(super) fn new(outcomes: VecDeque<Result<ChunkOutcome, DecodeError>>) -> Self {
        Self {
            outcomes,
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
        }
    }

    pub(super) fn chunked(pools: &Pools, samples: &[f32], parts: usize) -> Self {
        let per = samples.len().div_ceil(parts.max(1)) / usize::from(CH) * usize::from(CH);
        let mut frame_offset = 0;
        let mut outcomes: VecDeque<_> = samples
            .chunks(per.max(usize::from(CH)))
            .map(|part| {
                let at = frame_offset;
                frame_offset += u64::try_from(part.len() / usize::from(CH)).unwrap_or(0);
                Ok(ChunkOutcome::Chunk(chunk(pools, part, at)))
            })
            .collect();
        outcomes.push_back(Ok(eof()));
        Self::new(outcomes)
    }

    #[cfg(feature = "analysis-waveform")]
    pub(super) fn chunked_with_pending(pools: &Pools, samples: &[f32], parts: usize) -> Self {
        let mut with_pending = VecDeque::new();
        for outcome in Self::chunked(pools, samples, parts).outcomes {
            with_pending.push_back(Ok(pending()));
            with_pending.push_back(outcome);
        }
        Self::new(with_pending)
    }

    #[cfg(feature = "analysis-waveform")]
    pub(super) fn empty() -> Self {
        Self::new(VecDeque::from([Ok(eof())]))
    }

    #[cfg(feature = "analysis-waveform")]
    pub(super) fn failing() -> Self {
        Self::new(VecDeque::from([Err(DecodeError::InvalidData {
            detail: "scripted failure",
        })]))
    }

    #[cfg(feature = "analysis-waveform")]
    pub(super) fn stalled(stalls: usize) -> Self {
        let mut outcomes: VecDeque<_> = (0..stalls).map(|_| Ok(pending())).collect();
        outcomes.push_back(Ok(eof()));
        Self::new(outcomes)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn idle_ingest() -> crate::producer::ring::Reader {
    crate::producer::ring::open_for(spec().sample_rate).1
}

pub(super) fn eof() -> ChunkOutcome {
    ChunkOutcome::Eof {
        position: Duration::ZERO,
    }
}

#[cfg(feature = "analysis-waveform")]
pub(super) fn pending() -> ChunkOutcome {
    ChunkOutcome::Pending {
        reason: PendingReason::Buffering,
        position: Duration::ZERO,
    }
}

impl AudioSession for FakeReader {
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

impl AudioRead for FakeReader {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        self.outcomes.pop_front().unwrap_or_else(|| Ok(eof()))
    }

    fn position(&self) -> Duration {
        Duration::ZERO
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

    fn spec(&self) -> AudioSpec {
        spec()
    }
}

impl AudioControl for FakeReader {
    fn seek(&mut self, _position: Duration) -> Result<SeekOutcome, DecodeError> {
        unreachable!("analysis never seeks")
    }
}
