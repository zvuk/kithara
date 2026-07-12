use std::{collections::VecDeque, num::NonZeroU32};

use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, PcmChunk, PcmMeta, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use num_traits::cast::AsPrimitive;

#[cfg(feature = "analysis-waveform")]
use crate::traits::PendingReason;
use crate::traits::{ChunkOutcome, PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome};

const SR: u32 = 44_100;
const CH: u16 = 2;

fn spec() -> PcmSpec {
    PcmSpec {
        channels: CH,
        sample_rate: NonZeroU32::new(SR).unwrap(),
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
    bus: EventBus,
    metadata: TrackMetadata,
    outcomes: VecDeque<Result<ChunkOutcome, DecodeError>>,
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
    #[cfg(feature = "analysis-waveform")]
    fn chunked_with_pending(samples: &[f32], parts: usize) -> Self {
        let mut with_pending = VecDeque::new();
        for outcome in Self::chunked(samples, parts).outcomes {
            with_pending.push_back(Ok(pending()));
            with_pending.push_back(outcome);
        }
        Self::new(with_pending)
    }

    #[cfg(feature = "analysis-waveform")]
    fn empty() -> Self {
        Self::new(VecDeque::from([Ok(eof())]))
    }

    #[cfg(feature = "analysis-waveform")]
    fn failing() -> Self {
        Self::new(VecDeque::from([Err(DecodeError::InvalidData {
            detail: "scripted failure",
        })]))
    }
}

fn eof() -> ChunkOutcome {
    ChunkOutcome::Eof {
        position: Duration::ZERO,
    }
}

#[cfg(feature = "analysis-waveform")]
fn pending() -> ChunkOutcome {
    ChunkOutcome::Pending {
        reason: PendingReason::Buffering,
        position: Duration::ZERO,
    }
}

impl PcmSession for FakeReader {
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

impl PcmRead for FakeReader {
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

    fn spec(&self) -> PcmSpec {
        spec()
    }
}

impl PcmControl for FakeReader {
    fn seek(&mut self, _position: Duration) -> Result<SeekOutcome, DecodeError> {
        unreachable!("analysis never seeks")
    }
}

mod run {
    use kithara_platform::CancelToken;
    #[cfg(feature = "analysis-beat")]
    use kithara_platform::sync::Arc;
    #[cfg(feature = "analysis-beat")]
    use kithara_platform::sync::Mutex;
    #[cfg(feature = "analysis-beat")]
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_resampler::{NoResamplerBackend, ResamplerBackend};
    use kithara_test_utils::kithara;
    #[cfg(feature = "analysis-beat")]
    use unimock::{MockFn, Unimock, matching};

    #[cfg(feature = "analysis-beat")]
    use super::super::beat::{
        BeatDetector, BeatDetectorMock, GridParams, RawBeats, SharedBeatDetector,
    };
    use super::{
        super::{
            analyzer::{AnalyzerBuilder, TrackAnalysis},
            run::analyze_reader,
        },
        FakeReader, SR, sine,
    };
    use crate::traits::PcmReader;
    #[cfg(feature = "analysis-waveform")]
    use crate::waveform::{AnalysisParams, WaveformAnalyzer};

    #[cfg(feature = "analysis-waveform")]
    const BUCKETS: usize = 64;

    #[cfg(feature = "analysis-waveform")]
    fn waveform_only() -> AnalyzerBuilder<NoResamplerBackend> {
        AnalyzerBuilder::<NoResamplerBackend>::default().with_waveform(BUCKETS)
    }

    fn stages<B>(
        reader: &mut dyn PcmReader,
        builder: &AnalyzerBuilder<B>,
        cancel: &CancelToken,
    ) -> Vec<TrackAnalysis>
    where
        B: ResamplerBackend,
    {
        let mut out = Vec::new();
        analyze_reader(reader, builder, cancel, |a| out.push(a));
        out
    }

    #[cfg(feature = "analysis-waveform")]
    #[kithara::test]
    fn matches_direct_waveform_analyzer_over_chunked_stream() {
        let samples = sine(usize::try_from(SR).unwrap());
        let mut direct = WaveformAnalyzer::new(SR, AnalysisParams::default());
        direct.push_interleaved(&samples, 2);
        let want = direct.finalize(BUCKETS);

        let mut reader = FakeReader::chunked(&samples, 4);
        let out = stages(&mut reader, &waveform_only(), &CancelToken::root());
        assert_eq!(out.len(), 1, "waveform-only emits once");
        let got = out[0]
            .waveform
            .clone()
            .expect("waveform analyzer fills its slot");
        assert_eq!(
            Vec::<u8>::from(&got),
            Vec::<u8>::from(&want),
            "worker path must reproduce the direct analyzer output"
        );
    }

    #[cfg(feature = "analysis-waveform")]
    #[kithara::test]
    fn cancelled_token_yields_none() {
        let cancel = CancelToken::root();
        cancel.cancel();
        let mut reader = FakeReader::chunked(&sine(4096), 2);
        assert!(stages(&mut reader, &waveform_only(), &cancel).is_empty());
    }

    #[cfg(feature = "analysis-waveform")]
    #[kithara::test]
    fn decode_error_yields_none() {
        let mut reader = FakeReader::failing();
        let out = stages(&mut reader, &waveform_only(), &CancelToken::root());
        assert!(out.is_empty());
    }

    #[cfg(feature = "analysis-waveform")]
    #[kithara::test]
    fn empty_stream_yields_none() {
        let mut reader = FakeReader::empty();
        let out = stages(&mut reader, &waveform_only(), &CancelToken::root());
        assert!(out.is_empty(), "EOF with no chunks is not an analysis");
    }

    #[cfg(feature = "analysis-beat")]
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
        let builder = AnalyzerBuilder::<RubatoBackend>::default()
            .with_beat_detector(detector, GridParams::default());

        let mut reader = FakeReader::chunked(&sine(17 * usize::try_from(SR).unwrap()), 3);
        let out = stages(&mut reader, &builder, &CancelToken::root());
        assert_eq!(
            out.len(),
            2,
            "beat pass emits a fast stage then a complete one"
        );
        let grid = out[1]
            .beat
            .clone()
            .expect("beat slot fills its slot in the complete stage");
        assert!(
            (grid.bpm - 120.0).abs() < 1e-6,
            "2 s bars are 120 bpm, got {}",
            grid.bpm
        );
        assert_eq!(grid.downbeats[1], u64::from(SR) * 2, "source frames");
    }

    #[cfg(feature = "analysis-waveform")]
    #[kithara::test]
    fn pending_is_tolerated_mid_stream() {
        let samples = sine(8192);
        let mut reader = FakeReader::chunked_with_pending(&samples, 2);
        let out = stages(&mut reader, &waveform_only(), &CancelToken::root());
        assert!(out.len() == 1 && out[0].waveform.is_some());
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "analysis-waveform"))]
mod worker {
    use kithara_platform::CancelToken;
    use kithara_resampler::NoResamplerBackend;
    use kithara_test_utils::kithara;

    use super::{
        super::{analyzer::AnalyzerBuilder, worker::AnalysisWorker},
        FakeReader, sine,
    };

    fn waveform_only() -> AnalyzerBuilder<NoResamplerBackend> {
        AnalyzerBuilder::<NoResamplerBackend>::default().with_waveform(16)
    }

    #[kithara::test(tokio)]
    async fn delivers_result_on_its_own_thread() {
        let master = CancelToken::root();
        let worker = AnalysisWorker::new(&master, waveform_only());
        let mut rx = worker.analyze(
            Box::new(FakeReader::chunked(&sine(8192), 3)),
            worker.child_token(),
        );
        rx.changed().await.expect("worker sends a result");
        assert!(rx.borrow().as_ref().is_some_and(|a| a.waveform.is_some()));
    }

    #[kithara::test(tokio)]
    async fn preempted_job_sends_nothing_and_next_job_runs() {
        let master = CancelToken::root();
        let worker = AnalysisWorker::new(&master, waveform_only());

        let stale = worker.child_token();
        stale.cancel();
        let mut stale_rx = worker.analyze(Box::new(FakeReader::chunked(&sine(8192), 3)), stale);

        let mut live_rx = worker.analyze(
            Box::new(FakeReader::chunked(&sine(8192), 3)),
            worker.child_token(),
        );
        live_rx.changed().await.expect("live job completes");
        assert!(live_rx.borrow().is_some());
        assert!(
            stale_rx.changed().await.is_err(),
            "preempted job must drop its sender without a result"
        );
        assert!(stale_rx.borrow().is_none());
    }

    #[kithara::test(tokio)]
    async fn job_token_belongs_to_worker_scope() {
        let master = CancelToken::root();
        let worker = AnalysisWorker::new(&master, waveform_only());
        let job = worker.child_token();

        drop(worker);

        assert!(
            job.is_cancelled(),
            "dropping the worker must cancel tokens it creates for jobs"
        );
    }
}
