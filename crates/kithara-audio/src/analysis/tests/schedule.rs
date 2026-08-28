use kithara_decode::{
    DecodeError, PcmChunk, PcmSpec, TrackMetadata, duration_for_frames, frames_for_duration,
};
use kithara_events::EventBus;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
    tokio::sync::watch,
};
use kithara_resampler::{NoResamplerBackend, ResamplerBackend};
use kithara_test_utils::kithara;
use num_traits::cast::ToPrimitive;

use super::{
    super::{
        analyzer::{AnalyzerBuilder, BeatAnalysisConfig, TrackAnalysis},
        producer::{AnalysisProducer, Offer, ring},
        worker::{AnalysisNode, Job},
    },
    fixtures::{SR, chunk, sine_from, spec},
};
use crate::{
    coverage::FrameRange,
    runtime::Node,
    traits::{ChunkOutcome, PcmControl, PcmRead, PcmSession, ReadOutcome, SeekOutcome},
};

struct Consts;

impl Consts {
    const CHUNK: u64 = 8820;
    const EXTENT: u64 = 4 * 44_100;
    const TOKEN: &'static str = "scheduled-track";
    const TICKS: usize = 8192;
    const WINDOW_SECONDS: u32 = 1;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Seek { to: u64, landed: u64 },
    PastEof { to: u64 },
    Chunk { at: u64 },
    Eof,
}

type Log = Arc<Mutex<Vec<Call>>>;

struct Source {
    bus: EventBus,
    metadata: TrackMetadata,
    frames: u64,
    reports: Option<u64>,
    chunk: u64,
    snap: u64,
    floor: u64,
    refines: bool,
    stalls: bool,
    echoes: bool,
    fails_after: Option<u64>,
    at: u64,
    chunks: u64,
    log: Log,
}

impl Source {
    fn new(frames: u64) -> Self {
        Self {
            frames,
            reports: Some(frames),
            chunk: Consts::CHUNK,
            snap: 1,
            floor: 0,
            refines: false,
            stalls: false,
            echoes: false,
            fails_after: None,
            at: 0,
            chunks: 0,
            bus: EventBus::default(),
            log: Log::default(),
            metadata: TrackMetadata::default(),
        }
    }

    fn snapping(self, snap: u64) -> Self {
        Self { snap, ..self }
    }

    fn flooring(self, floor: u64) -> Self {
        Self { floor, ..self }
    }

    fn reporting(self, frames: Option<u64>) -> Self {
        Self {
            reports: frames,
            ..self
        }
    }

    fn refining(self) -> Self {
        Self {
            refines: true,
            ..self
        }
    }

    fn stalling(self) -> Self {
        Self {
            stalls: true,
            ..self
        }
    }

    fn echoing(self) -> Self {
        Self {
            echoes: true,
            ..self
        }
    }

    fn failing_after(self, chunks: u64) -> Self {
        Self {
            fails_after: Some(chunks),
            ..self
        }
    }

    fn log(&self) -> Log {
        Arc::clone(&self.log)
    }

    fn push(&self, call: Call) {
        self.log.lock().push(call);
    }
}

impl PcmSession for Source {
    fn duration(&self) -> Option<Duration> {
        let reports = self.reports?;
        let frames = if self.refines && self.at > 0 {
            self.frames
        } else {
            reports
        };
        Some(duration_for_frames(SR, frames))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl PcmRead for Source {
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        if self.stalls {
            return Ok(ChunkOutcome::Pending {
                reason: crate::traits::PendingReason::Buffering,
                position: self.position(),
            });
        }
        if self.fails_after.is_some_and(|after| self.chunks >= after) {
            return Err(DecodeError::InvalidData {
                detail: "scripted decode failure",
            });
        }
        if self.at >= self.frames {
            self.push(Call::Eof);
            return Ok(ChunkOutcome::Eof {
                position: self.position(),
            });
        }

        let at = self.at;
        let frames = self.chunk.min(self.frames.saturating_sub(at));
        self.at = at.saturating_add(frames);
        self.chunks = self.chunks.saturating_add(1);
        self.push(Call::Chunk { at });
        Ok(ChunkOutcome::Chunk(decoded(at, frames)))
    }

    fn position(&self) -> Duration {
        duration_for_frames(SR, self.at)
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

impl PcmControl for Source {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let target = frames_for_duration(SR, position).to_u64().unwrap_or(0);
        if target >= self.frames {
            self.push(Call::PastEof { to: target });
            return Ok(SeekOutcome::PastEof {
                target: position,
                duration: duration_for_frames(SR, self.frames),
            });
        }

        let landed = target
            .saturating_div(self.snap)
            .saturating_mul(self.snap)
            .max(self.floor);
        self.at = landed;
        self.push(Call::Seek { to: target, landed });
        let reported = if self.echoes { target } else { landed };
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: duration_for_frames(SR, reported),
        })
    }
}

fn decoded(at: u64, frames: u64) -> PcmChunk {
    chunk(&sine_from(at, frames.to_usize().unwrap_or(0)), at)
}

fn scheduled(window_seconds: u32) -> AnalyzerBuilder<NoResamplerBackend> {
    AnalyzerBuilder::<NoResamplerBackend>::default().with_beat_config(
        BeatAnalysisConfig::builder()
            .resampler_backend(NoResamplerBackend)
            .detector_window_seconds(window_seconds)
            .detector_overlap_seconds(0)
            .build(),
    )
}

struct Pass<B>
where
    B: ResamplerBackend,
{
    node: AnalysisNode<B>,
    producer: AnalysisProducer,
    results: watch::Receiver<Option<TrackAnalysis>>,
    log: Log,
    _jobs: mpsc::Sender<Job>,
}

impl<B> Pass<B>
where
    B: ResamplerBackend,
{
    fn open(source: Source, builder: AnalyzerBuilder<B>) -> Self {
        let rate = spec().sample_rate;
        let log = source.log();
        let (jobs, receiver) = mpsc::channel();
        let (tx, results) = watch::channel(None);
        let (writer, ingest) = ring::open_for(rate);
        jobs.send(Job {
            token: Consts::TOKEN.into(),
            reader: Box::new(source),
            tx,
            rate,
            ingest,
            cancel: CancelToken::root(),
        })
        .expect("analysis node accepts the test job");

        Self {
            log,
            results,
            _jobs: jobs,
            node: AnalysisNode::new(builder, receiver),
            producer: AnalysisProducer::new(writer, rate, Consts::TOKEN.into()),
        }
    }

    fn offer(&mut self, at: u64, frames: u64) {
        let frames = frames.to_usize().unwrap_or(0);
        assert_eq!(
            self.producer.offer(&sine_from(at, frames), spec(), at),
            Offer::Taken,
            "the transport takes a range on its own axis"
        );
    }

    fn tick(&mut self) {
        let _ = Node::tick(&mut self.node);
    }

    fn drive(&mut self, ticks: usize) -> bool {
        for _ in 0..ticks {
            if self.has_ended() {
                return true;
            }
            let _ = Node::tick(&mut self.node);
        }
        self.has_ended()
    }

    fn has_ended(&self) -> bool {
        self.results.has_changed().is_err()
    }

    fn analysis(&self) -> TrackAnalysis {
        self.results
            .borrow()
            .clone()
            .expect("the pass publishes what it covered")
    }

    fn calls(&self) -> Vec<Call> {
        self.log.lock().clone()
    }
}

fn targets(calls: &[Call]) -> Vec<u64> {
    calls
        .iter()
        .filter_map(|call| match call {
            Call::Seek { to, .. } => Some(*to),
            _ => None,
        })
        .collect()
}

fn decoded_at(calls: &[Call]) -> Vec<u64> {
    calls
        .iter()
        .filter_map(|call| match call {
            Call::Chunk { at } => Some(*at),
            _ => None,
        })
        .collect()
}

/// Runs it takes to cover the fixture track at best: its length over the
/// window one run carries. A schedule that leaves part of a gap behind pays
/// a halving sequence on top of this, which is what the bound catches.
fn least_runs() -> usize {
    let window = u64::from(SR).saturating_mul(u64::from(Consts::WINDOW_SECONDS));
    usize::try_from(Consts::EXTENT.div_ceil(window)).unwrap_or(usize::MAX)
}

fn run_lengths(calls: &[Call]) -> Vec<usize> {
    let mut out = Vec::new();
    for call in calls {
        match call {
            Call::Seek { .. } => out.push(0),
            Call::Chunk { .. } => {
                if let Some(last) = out.last_mut() {
                    *last += 1;
                }
            }
            _ => {}
        }
    }
    out
}

#[kithara::test]
fn a_scheduled_pass_seeks_before_it_decodes_anything() {
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT),
        scheduled(Consts::WINDOW_SECONDS),
    );
    pass.drive(2);

    // A one-second run inside a four-second track spans [1.5s, 2.5s).
    assert_eq!(
        pass.calls().first(),
        Some(&Call::Seek {
            to: 66_150,
            landed: 66_150,
        }),
        "the source reports its length, so where the run goes is known before \
         the first chunk is decoded"
    );
}

#[kithara::test]
fn a_growing_duration_replaces_the_one_the_schedule_had() {
    // The source holds twice what it first reports, the way a decode path
    // refines a duration upward as it learns more.
    let short = Consts::EXTENT / 2;
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT)
            .reporting(Some(short))
            .refining(),
        scheduled(Consts::WINDOW_SECONDS),
    );
    pass.drive(2);
    assert_eq!(
        targets(&pass.calls()),
        vec![22_050],
        "the first run is placed inside the length reported so far"
    );

    assert!(pass.drive(Consts::TICKS), "the pass ends");
    assert!(
        pass.analysis().coverage().frontier() > short,
        "a larger report must be planned against, not the cached one"
    );
}

#[kithara::test]
fn a_run_starts_where_the_seek_landed_and_the_next_choice_follows_it() {
    // The decoder snaps every seek down to a whole 30 000 frames.
    const SNAP: u64 = 30_000;
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).snapping(SNAP),
        scheduled(Consts::WINDOW_SECONDS),
    );
    pass.drive(8);

    let calls = pass.calls();
    assert_eq!(
        calls.first(),
        Some(&Call::Seek {
            to: 66_150,
            landed: 60_000,
        })
    );
    assert_eq!(
        decoded_at(&calls).first(),
        Some(&60_000),
        "decoding starts where the seek landed, not where it was asked to go"
    );

    // Five chunks make the one-second run, so the coverage is
    // [60 000, 104 100); the wider of the two gaps left is the tail, and a
    // run centred in it starts at 118 200. Had the schedule planned on the
    // requested 66 150, the head would have looked like the wider gap.
    pass.drive(Consts::TICKS);
    let next = targets(&pass.calls()).get(1).copied().unwrap_or(0);
    assert!(
        next.abs_diff(118_200) <= 1,
        "the next choice is computed from what was covered, not from what \
         was asked for: {next}"
    );
}

#[kithara::test]
fn covering_a_track_costs_the_runs_its_window_divides_it_into() {
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(pass.drive(Consts::TICKS), "the pass ends");

    let analysis = pass.analysis();
    assert!(
        analysis.is_complete(),
        "a reader that can seek anywhere leaves nothing behind: {:?}",
        analysis.missing()
    );
    let runs = targets(&pass.calls()).len();
    assert!(
        runs <= 2 * least_runs(),
        "{runs} runs for a track {least} of them cover: the schedule is \
         approaching its gaps rather than closing them",
        least = least_runs()
    );
}

#[kithara::test]
fn a_run_carries_the_detector_window_it_is_derived_from() {
    let lengths = |seconds: u32| {
        let mut pass = Pass::open(Source::new(Consts::EXTENT), scheduled(seconds));
        pass.drive(64);
        run_lengths(&pass.calls()).first().copied().unwrap_or(0)
    };

    // One second is five chunks of a fifth of a second each.
    assert_eq!(lengths(1), 5, "a run carries one detector window");
    assert_eq!(
        lengths(2),
        10,
        "the run length is derived from the window, not set beside it"
    );
}

#[kithara::test]
fn a_covered_opening_is_not_decoded_a_second_time() {
    let covered = Consts::EXTENT / 2;
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT),
        scheduled(Consts::WINDOW_SECONDS),
    );
    pass.offer(0, covered);

    assert!(pass.drive(Consts::TICKS), "the pass ends");
    let decoded = decoded_at(&pass.calls());
    assert!(!decoded.is_empty(), "the rest of the track is decoded");
    assert!(
        decoded.iter().all(|at| *at >= covered),
        "a covered range must not be decoded again: {decoded:?}"
    );

    let analysis = pass.analysis();
    assert!(
        analysis.missing().is_empty(),
        "between the producer and the schedule the track is covered: {:?}",
        analysis.missing()
    );
}

#[kithara::test]
fn a_source_with_no_length_is_decoded_in_order() {
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).reporting(None),
        scheduled(Consts::WINDOW_SECONDS),
    );

    assert!(pass.drive(Consts::TICKS), "the pass ends");
    let calls = pass.calls();
    assert!(
        targets(&calls).is_empty(),
        "a source with no length is never repositioned"
    );
    assert_eq!(
        pass.analysis().coverage().runs(),
        &[FrameRange::new(0, Consts::EXTENT)],
        "its coverage grows as one run"
    );
    assert!(calls.contains(&Call::Eof), "end of stream ends such a pass");
}

#[kithara::test]
fn a_pass_ends_when_a_producer_covers_the_last_of_it() {
    // The reader never delivers, so everything covered came from the
    // producer and nothing can have reached end of stream.
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).stalling(),
        scheduled(Consts::WINDOW_SECONDS),
    );
    pass.offer(0, Consts::EXTENT / 2);
    pass.drive(8);
    assert!(!pass.has_ended(), "half a track is not a finished pass");

    pass.offer(Consts::EXTENT / 2, Consts::EXTENT / 2);
    assert!(pass.drive(Consts::TICKS), "the pass ends on its own");

    let analysis = pass.analysis();
    assert!(
        analysis.is_complete(),
        "the final snapshot covers the whole extent"
    );
    assert!(
        !pass.calls().contains(&Call::Eof),
        "the pass did not wait for its reader to reach the end"
    );
}

#[kithara::test]
fn a_source_that_over_reports_its_length_still_ends() {
    // Reports four seconds, holds two.
    let held = Consts::EXTENT / 2;
    let mut pass = Pass::open(
        Source::new(held).reporting(Some(Consts::EXTENT)),
        scheduled(Consts::WINDOW_SECONDS),
    );

    assert!(
        pass.drive(Consts::TICKS),
        "a length that cannot be covered must not hold a pass open"
    );
    let analysis = pass.analysis();
    assert!(
        analysis.coverage().frames() > 0,
        "the pass publishes what it did cover"
    );
    assert!(
        analysis.coverage().frontier() <= held,
        "nothing past what the source holds can be covered"
    );
}

#[kithara::test]
fn a_snapshot_published_early_describes_the_whole_track() {
    // Long enough that the pass publishes while it is still decoding: a
    // publication is due every five covered source seconds.
    const EXTENT: u64 = 20 * 44_100;
    let mut pass = Pass::open(Source::new(EXTENT), scheduled(Consts::WINDOW_SECONDS));
    pass.drive(40);
    assert!(!pass.has_ended(), "the pass must still be decoding");

    let analysis = pass.analysis();
    assert!(
        !analysis.missing().is_empty(),
        "this is a snapshot published while coverage is still growing"
    );

    let runs = analysis.coverage().runs();
    assert!(
        runs.len() > 1,
        "coverage must be spread over the source, not one run: {runs:?}"
    );
    assert!(
        runs.first().is_some_and(|run| run.start() > 0),
        "an early snapshot must describe more than the opening: {runs:?}"
    );
    assert!(
        runs.last().is_some_and(|run| run.end() > EXTENT / 2),
        "coverage must reach the far half of the track: {runs:?}"
    );
}

#[kithara::test]
fn a_snapping_source_has_its_gaps_closed_rather_than_halved() {
    // Seeks land on whole 30 000 frames, so a run aimed at a gap's start
    // begins in covered audio and has to read through it to get there.
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).snapping(30_000),
        scheduled(Consts::WINDOW_SECONDS),
    );

    assert!(pass.drive(Consts::TICKS), "the pass ends");
    let analysis = pass.analysis();
    assert!(
        analysis.is_complete(),
        "a gap one run can span must be closed, not halved: {:?}",
        analysis.missing()
    );
    let runs = targets(&pass.calls()).len();
    assert!(
        runs <= 2 * least_runs(),
        "closing the track must cost runs, not a halving sequence: {runs} runs"
    );
}

#[kithara::test]
fn a_source_that_snaps_out_of_its_own_gaps_still_finishes() {
    // Seeks land on whole 88 200 frames, so the two gaps left between them
    // are further from a landing than a run is long: the run spends its
    // window on covered audio and never reaches them. Each such position
    // costs one run to find out and is then retired, which bounds this.
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).snapping(88_200),
        scheduled(Consts::WINDOW_SECONDS),
    );

    assert!(
        pass.drive(Consts::TICKS),
        "a pass that cannot reach a gap must end rather than keep asking"
    );
    let analysis = pass.analysis();
    assert!(
        analysis.coverage().frames() >= Consts::EXTENT / 2,
        "what the reader can reach is still covered: {}",
        analysis.coverage().frames()
    );
    let runs = targets(&pass.calls()).len();
    assert!(
        runs <= 2 * least_runs(),
        "an unreachable position must be retired, not retried: {runs} runs"
    );
}

#[kithara::test]
fn a_head_the_source_cannot_reach_is_retired_after_one_chunk() {
    // Encoder priming: the decoder's first output frame is 1000, so nothing
    // in front of it is a position any seek can be answered with.
    const FLOOR: u64 = 1000;
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).flooring(FLOOR),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(pass.drive(Consts::TICKS), "the pass ends");

    assert_eq!(
        pass.analysis().missing(),
        vec![FrameRange::new(0, FLOOR)],
        "only what the source cannot deliver is left over"
    );
    let lengths = run_lengths(&pass.calls());
    assert_eq!(
        lengths.last().copied().unwrap_or(0),
        1,
        "one chunk is enough to learn the aim cannot be met: {lengths:?}"
    );
}

#[kithara::test]
fn a_pass_with_nothing_left_to_reach_is_settled() {
    const FLOOR: u64 = 1000;
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).flooring(FLOOR),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(pass.drive(Consts::TICKS), "the pass ends");

    let analysis = pass.analysis();
    assert!(
        !analysis.is_complete(),
        "the head of this source is out of its own reach"
    );
    assert!(
        analysis.is_settled(),
        "a pass with nowhere left to go holds what the content gives"
    );
}

#[kithara::test]
fn a_pass_its_reader_cut_short_is_not_settled() {
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).failing_after(3),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(pass.drive(Consts::TICKS), "the failed reader ends the pass");

    assert!(
        !pass.analysis().is_settled(),
        "a pass that still had ranges to reach must not pass for a finished one"
    );
}

#[kithara::test]
fn a_pass_that_gave_up_still_reports_what_it_never_reached() {
    // Seeks land on whole 88 200 frames, so the schedule retires positions
    // it cannot reach and ends with the track only partly covered.
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).snapping(88_200),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(pass.drive(Consts::TICKS), "the pass ends");

    let analysis = pass.analysis();
    let covered = analysis.coverage().frames();
    assert!(
        covered < Consts::EXTENT,
        "this source cannot reach all of its own track: {covered}"
    );
    assert!(
        !analysis.is_complete(),
        "a pass that gave up on a range is not a complete one"
    );
    let missing: u64 = analysis
        .missing()
        .iter()
        .copied()
        .map(FrameRange::frames)
        .sum();
    assert_eq!(
        missing,
        Consts::EXTENT - covered,
        "every frame the pass never reached is reported missing: {:?}",
        analysis.missing()
    );
}

#[kithara::test]
fn a_producer_does_not_keep_an_unreachable_position_alive() {
    // Seeks snap down to whole 88 200 frames, and the opening is already
    // covered, so the position chosen for the middle gap lands back inside
    // covered audio and its run can decode nothing new.
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).snapping(88_200),
        scheduled(Consts::WINDOW_SECONDS),
    );
    pass.offer(0, 100_000);

    // A producer growing a range at the far end of the track on every tick.
    // That is the producer's coverage, not the run's, and must not keep the
    // position the run cannot reach eligible.
    let mut tail = 160_000;
    for _ in 0..Consts::TICKS {
        if pass.has_ended() {
            break;
        }
        if tail < 164_000 {
            pass.offer(tail, 441);
            tail += 441;
        }
        pass.tick();
    }

    assert!(pass.has_ended(), "the pass ends while a producer feeds it");
    let asked = targets(&pass.calls());
    let mut repeats = std::collections::BTreeMap::new();
    for at in &asked {
        *repeats.entry(*at).or_insert(0_usize) += 1;
    }
    let worst = repeats.values().copied().max().unwrap_or(0);
    assert_eq!(
        worst, 1,
        "a position whose run decoded nothing new is retired once, not re-asked \
         for every range a producer happens to fold: {repeats:?}"
    );
}

#[kithara::test]
fn a_decode_error_still_publishes_what_the_pass_covered() {
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).failing_after(3),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(
        pass.drive(Consts::TICKS),
        "a reader that failed ends the pass"
    );

    let analysis = pass.analysis();
    assert!(
        analysis.coverage().frames() > 0,
        "what was decoded before the failure is still an analysis"
    );
    assert!(
        !analysis.missing().is_empty(),
        "the rest of the track is reported missing, not silently dropped"
    );
}

#[kithara::test]
fn a_run_is_measured_from_where_it_decoded_not_where_it_asked() {
    // The readers this runs against answer a seek with the position they
    // were asked for while the decoder resumes at a boundary of its own, so
    // a run sized against the seek's answer outlasts its window.
    let mut pass = Pass::open(
        Source::new(Consts::EXTENT).snapping(30_000).echoing(),
        scheduled(Consts::WINDOW_SECONDS),
    );
    assert!(pass.drive(Consts::TICKS), "the pass ends");

    let lengths = run_lengths(&pass.calls());
    // A one-second window is five chunks at a fifth of a second each.
    assert!(
        lengths.iter().any(|len| *len == 5),
        "a run must carry the window it was sized for: {lengths:?}"
    );
    assert!(
        lengths.iter().all(|len| *len <= 5),
        "no run may outlast the window it was sized for: {lengths:?}"
    );
}

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
mod artifacts {
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_utils::kithara;

    use super::{
        super::{
            super::{analyzer::AnalyzerBuilder, beat::GridParams},
            fixtures::{Artifacts, SR, artifacts, assert_agrees, beat_detector},
        },
        Consts, Pass, Source, targets,
    };
    use crate::analysis::BeatAnalysisConfig;

    const BUCKETS: usize = 64;
    const WINDOW_SECONDS: u32 = 2;
    const EXTENT: u64 = 12 * 44_100;

    struct Route {
        artifacts: Artifacts,
        seeks: usize,
        reclaimed: bool,
    }

    /// Runs it takes to cover this track at best: its length over the window
    /// one run carries.
    fn least_runs() -> usize {
        let window = u64::from(SR).saturating_mul(u64::from(WINDOW_SECONDS));
        usize::try_from(EXTENT.div_ceil(window)).unwrap_or(usize::MAX)
    }

    fn beat_pass() -> AnalyzerBuilder<RubatoBackend> {
        AnalyzerBuilder::<RubatoBackend>::default()
            .with_waveform(BUCKETS)
            .with_beat_config(
                BeatAnalysisConfig::builder()
                    .resampler_backend(RubatoBackend::default())
                    .detector_window_seconds(WINDOW_SECONDS)
                    .detector_overlap_seconds(0)
                    .build(),
            )
            .with_beat_detector(beat_detector(), GridParams::default())
    }

    fn covered(source: Source, offer: &[(u64, u64)]) -> Route {
        let mut pass = Pass::open(source, beat_pass());
        for (at, frames) in offer {
            pass.offer(*at, *frames);
            pass.drive(2);
        }
        assert!(pass.drive(Consts::TICKS), "the pass covers the track");

        let analysis = pass.analysis();
        assert!(
            analysis.missing().is_empty(),
            "this comparison is only about the route, not about coverage: {:?}",
            analysis.missing()
        );
        Route {
            artifacts: artifacts(&analysis),
            seeks: targets(&pass.calls()).len(),
            reclaimed: analysis
                .beat()
                .is_some_and(|beat| !beat.unanalysed().is_empty()),
        }
    }

    #[kithara::test]
    fn a_scheduled_route_and_a_linear_one_produce_the_same_artifacts() {
        // The same source, once decoded in order because it reports no
        // length, and once scheduled because it does.
        let linear = covered(Source::new(EXTENT).reporting(None), &[]);
        assert!(
            !linear.artifacts.1.is_empty(),
            "the harness must find markers at all"
        );
        assert_eq!(linear.seeks, 0, "a source with no length is read in order");

        let scheduled = covered(Source::new(EXTENT), &[]);
        assert!(
            scheduled.seeks > 1,
            "the other route must really have been scheduled, not read in order"
        );
        assert!(
            scheduled.seeks <= 2 * least_runs(),
            "the same artifacts must not cost a halving sequence to reach: {} runs",
            scheduled.seeks
        );
        assert!(
            linear.reclaimed && scheduled.reclaimed,
            "both routes must reach the regime where the beat pass reclaims"
        );
        assert_agrees(
            &linear.artifacts,
            &scheduled.artifacts,
            "scheduled against linear",
        );
    }

    #[kithara::test]
    fn a_track_half_covered_by_a_producer_agrees_with_one_covered_alone() {
        let linear = covered(Source::new(EXTENT).reporting(None), &[]);

        // Playback covered the first half in its own decode blocks; the
        // schedule has to take up the rest.
        let block = EXTENT / 16;
        let offered: Vec<(u64, u64)> = (0..8).map(|index| (index * block, block)).collect();
        let mixed = covered(Source::new(EXTENT), &offered);
        assert!(mixed.seeks > 0, "the rest of the track had to be scheduled");
        assert_agrees(
            &linear.artifacts,
            &mixed.artifacts,
            "half offered, half scheduled",
        );
    }
}
