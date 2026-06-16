use std::{collections::VecDeque, num::NonZeroU32, sync::Arc, time::Duration};

use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use num_traits::cast::AsPrimitive;

use crate::{
    traits::{ChunkOutcome, PcmReader, PendingReason, ReadOutcome, SeekOutcome},
    worker::PreloadGate,
};

const SR: u32 = 44_100;
const CH: u16 = 2;

fn spec() -> PcmSpec {
    PcmSpec {
        channels: CH,
        sample_rate: SR,
    }
}

/// Interleaved stereo sine, phase-accumulated.
fn sine(frames: usize) -> Vec<f32> {
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
struct FakeReader {
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
    fn chunked(samples: &[f32], parts: usize) -> Self {
        let per = samples.len().div_ceil(parts.max(1)) / usize::from(CH) * usize::from(CH);
        let mut outcomes: VecDeque<_> = samples
            .chunks(per.max(usize::from(CH)))
            .map(|part| Ok(ChunkOutcome::Chunk(chunk(part))))
            .collect();
        outcomes.push_back(Ok(eof()));
        Self::new(outcomes)
    }

    /// Like [`Self::chunked`] with a `Pending` tick between every chunk.
    fn chunked_with_pending(samples: &[f32], parts: usize) -> Self {
        let mut with_pending = VecDeque::new();
        for outcome in Self::chunked(samples, parts).outcomes {
            with_pending.push_back(Ok(pending()));
            with_pending.push_back(outcome);
        }
        Self::new(with_pending)
    }

    fn failing() -> Self {
        Self::new(VecDeque::from([Err(DecodeError::InvalidData(
            "scripted failure".into(),
        ))]))
    }

    fn empty() -> Self {
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

mod run {
    use std::sync::Arc;

    use kithara_platform::{CancellationToken, sync::Mutex};
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::{
        super::{
            analyzer::AnalyzerBuilder,
            beat::{BeatDetector, BeatDetectorMock, GridParams, RawBeats, SharedBeatDetector},
            run::analyze_reader,
        },
        FakeReader, SR, sine,
    };
    use crate::waveform::{AnalysisParams, WaveformAnalyzer};

    const BUCKETS: usize = 64;

    fn waveform_only() -> AnalyzerBuilder {
        AnalyzerBuilder::default().with_waveform(BUCKETS)
    }

    #[kithara::test]
    fn matches_direct_waveform_analyzer_over_chunked_stream() {
        let samples = sine(usize::try_from(SR).unwrap());
        let mut direct = WaveformAnalyzer::new(SR, AnalysisParams::default());
        direct.push_interleaved(&samples, 2);
        let want = direct.finalize(BUCKETS);

        let mut reader = FakeReader::chunked(&samples, 4);
        let out = analyze_reader(&mut reader, &waveform_only(), &CancellationToken::default())
            .expect("stream with data analyses");
        let got = out.waveform.expect("waveform analyzer fills its slot");
        assert_eq!(
            got.to_bytes(),
            want.to_bytes(),
            "worker path must reproduce the direct analyzer output"
        );
    }

    #[kithara::test]
    fn cancelled_token_yields_none() {
        let cancel = CancellationToken::default();
        cancel.cancel();
        let mut reader = FakeReader::chunked(&sine(4096), 2);
        assert!(analyze_reader(&mut reader, &waveform_only(), &cancel).is_none());
    }

    #[kithara::test]
    fn decode_error_yields_none() {
        let mut reader = FakeReader::failing();
        let out = analyze_reader(&mut reader, &waveform_only(), &CancellationToken::default());
        assert!(out.is_none());
    }

    #[kithara::test]
    fn empty_stream_yields_none() {
        let mut reader = FakeReader::empty();
        let out = analyze_reader(&mut reader, &waveform_only(), &CancellationToken::default());
        assert!(out.is_none(), "EOF with no chunks is not an analysis");
    }

    #[kithara::test]
    fn beat_slot_fills_the_beat_grid() {
        let raw = RawBeats {
            beats: Vec::new(),
            downbeats: (0..9u8).map(|n| f32::from(n) * 2.0).collect(),
        };
        let mock = Unimock::new(
            BeatDetectorMock
                .next_call(matching!(_))
                .answers_arc(Arc::new(move |_, _| Ok(raw.clone()))),
        );
        let detector: SharedBeatDetector =
            Arc::new(Mutex::new(Box::new(mock) as Box<dyn BeatDetector>));
        let builder =
            AnalyzerBuilder::default().with_beat_detector(detector, GridParams::default());

        let mut reader = FakeReader::chunked(&sine(8192), 3);
        let out = analyze_reader(&mut reader, &builder, &CancellationToken::default())
            .expect("stream with data analyses");
        let grid = out.beat.expect("beat slot fills its slot");
        assert!(
            (grid.bpm - 120.0).abs() < 1e-6,
            "2 s bars are 120 bpm, got {}",
            grid.bpm
        );
        assert_eq!(grid.downbeats[1], u64::from(SR) * 2, "source frames");
    }

    #[kithara::test]
    fn pending_is_tolerated_mid_stream() {
        let samples = sine(8192);
        let mut reader = FakeReader::chunked_with_pending(&samples, 2);
        let out = analyze_reader(&mut reader, &waveform_only(), &CancellationToken::default());
        assert!(out.is_some_and(|a| a.waveform.is_some()));
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod worker {
    use kithara_platform::CancellationToken;
    use kithara_test_utils::kithara;

    use super::{
        super::{analyzer::AnalyzerBuilder, worker::AnalysisWorker},
        FakeReader, sine,
    };

    fn waveform_only() -> AnalyzerBuilder {
        AnalyzerBuilder::default().with_waveform(16)
    }

    #[kithara::test(tokio)]
    async fn delivers_result_on_its_own_thread() {
        let master = CancellationToken::default();
        let worker = AnalysisWorker::new(&master, waveform_only());
        let mut rx = worker.analyze(
            Box::new(FakeReader::chunked(&sine(8192), 3)),
            master.child_token(),
        );
        rx.changed().await.expect("worker sends a result");
        assert!(rx.borrow().as_ref().is_some_and(|a| a.waveform.is_some()));
    }

    #[kithara::test(tokio)]
    async fn preempted_job_sends_nothing_and_next_job_runs() {
        let master = CancellationToken::default();
        let worker = AnalysisWorker::new(&master, waveform_only());

        let stale = master.child_token();
        stale.cancel();
        let mut stale_rx = worker.analyze(Box::new(FakeReader::chunked(&sine(8192), 3)), stale);

        let mut live_rx = worker.analyze(
            Box::new(FakeReader::chunked(&sine(8192), 3)),
            master.child_token(),
        );
        live_rx.changed().await.expect("live job completes");
        assert!(live_rx.borrow().is_some());
        assert!(
            stale_rx.changed().await.is_err(),
            "preempted job must drop its sender without a result"
        );
        assert!(stale_rx.borrow().is_none());
    }
}
