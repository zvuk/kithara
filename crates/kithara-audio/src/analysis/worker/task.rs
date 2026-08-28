use std::num::NonZeroU32;

use kithara_bufpool::PcmBuf;
use kithara_decode::duration_for_frames;
use kithara_platform::{CancelToken, tokio::sync::watch};
use kithara_resampler::ResamplerBackend;
use tracing::{debug, warn};

use super::schedule::{Extent, Schedule};
use crate::{
    ChunkOutcome, PcmReader, SeekOutcome,
    analysis::{
        analyzer::{
            AnalysisToken, AnalyzerBuilder, Detector, Ingest, TrackAnalysis, TrackAnalyzers,
        },
        producer::ring,
    },
    coverage::{Coverage, FrameRange},
    runtime::TickResult,
};

const PUBLISH_SECONDS: u64 = 5;

pub(crate) struct Job {
    pub(crate) reader: Box<dyn PcmReader>,
    pub(crate) cancel: CancelToken,
    pub(crate) ingest: ring::Reader,
    pub(crate) rate: NonZeroU32,
    pub(crate) token: AnalysisToken,
    pub(crate) tx: watch::Sender<Option<TrackAnalysis>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskPhase {
    Decode,
    Ending,
    Done,
}

struct Run {
    chosen: u64,
    at: u64,
    frontier: u64,
    started: bool,
    grew: bool,
}

pub(crate) struct AnalysisTask<B>
where
    B: ResamplerBackend,
{
    reader: Box<dyn PcmReader>,
    cancel: CancelToken,
    analyzers: Option<TrackAnalyzers<B>>,
    ingest: ring::Reader,
    scratch: Option<PcmBuf>,
    rate: NonZeroU32,
    token: AnalysisToken,
    tx: watch::Sender<Option<TrackAnalysis>>,
    phase: TaskPhase,
    published_at: u64,
    extent: Extent,
    schedule: Schedule,
    run: Option<Run>,
}

impl<B> AnalysisTask<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn new(job: Job) -> Self {
        Self {
            analyzers: None,
            cancel: job.cancel,
            extent: Extent::default(),
            ingest: job.ingest,
            phase: TaskPhase::Decode,
            published_at: 0,
            rate: job.rate,
            reader: job.reader,
            run: None,
            schedule: Schedule::default(),
            scratch: None,
            token: job.token,
            tx: job.tx,
        }
    }

    fn is_covered(&self, range: FrameRange) -> bool {
        self.analyzers
            .as_ref()
            .is_some_and(|analyzers| analyzers.coverage().contains(range))
    }

    fn is_complete(&self) -> bool {
        self.extent
            .frames()
            .is_some_and(|extent| self.is_covered(FrameRange::new(0, extent)))
    }

    fn choose(&self, window: Option<u64>) -> Option<u64> {
        let empty = Coverage::default();
        let coverage = self
            .analyzers
            .as_ref()
            .map_or(&empty, TrackAnalyzers::coverage);
        self.schedule.next(coverage, self.extent.frames(), window)
    }

    fn decode(
        &mut self,
        builder: &AnalyzerBuilder<B>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        match self.reader.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                let range = FrameRange::from(&chunk.meta);
                let analyzers = open(&mut self.analyzers, builder, self.rate, &self.token);
                let before = analyzers.covered_frames();
                let outcome = analyzers.push(&chunk, detector);
                if outcome != Ingest::Accepted {
                    debug!(?outcome, "analysis: range not folded in");
                }
                let grew = analyzers.covered_frames() > before;
                if let Some(run) = &mut self.run {
                    if !run.started {
                        run.started = true;
                        run.at = range.start();
                    }
                    run.frontier = range.end();
                    run.grew |= grew;
                }
                TickResult::Progress
            }
            Ok(ChunkOutcome::Pending { .. }) => TickResult::UpstreamPending,
            Ok(ChunkOutcome::Eof { .. }) => {
                // End of stream bounds the extent; gaps behind it may remain.
                if self.extent.frames().is_none() {
                    self.finish(true);
                } else {
                    if let Some(run) = &self.run {
                        self.extent.unreachable(run.frontier);
                        debug!(
                            frontier = run.frontier,
                            extent = ?self.extent.frames(),
                            "analysis: eof bounds the extent"
                        );
                    }
                    self.retire();
                }
                TickResult::Progress
            }
            Err(error) => {
                // The reader failed; the ranges it delivered did not.
                warn!(?error, "analysis: decode error; pass ended");
                self.finish(false);
                TickResult::Progress
            }
        }
    }

    fn drain(&mut self, builder: &AnalyzerBuilder<B>, detector: Option<&mut Detector>) -> bool {
        let scratch = self
            .scratch
            .get_or_insert_with(|| builder.pcm_pool().get_with(Vec::clear));
        let analyzers = open(&mut self.analyzers, builder, self.rate, &self.token);
        let mut detector = detector;
        let mut folded = false;

        while let Some(at) = self.ingest.pop(scratch) {
            let outcome = analyzers.push_mono(scratch, at, detector.as_deref_mut());
            if outcome != Ingest::Accepted {
                debug!(?outcome, at, "analysis: offered range not folded in");
            }
            folded = true;
        }
        folded
    }

    fn due(&self) -> bool {
        let Some(analyzers) = &self.analyzers else {
            return false;
        };
        let interval =
            u64::from(analyzers.source_sample_rate().get()).saturating_mul(PUBLISH_SECONDS);
        analyzers.covered_frames().saturating_sub(self.published_at) >= interval
    }

    /// `settled` when the pass ran out of reachable ranges rather than being
    /// cut short: that is what tells a consumer this is all the content gives.
    fn finish(&mut self, settled: bool) {
        let planned = self.extent.frames();
        debug!(
            extent = ?planned,
            covered = ?self.analyzers.as_ref().map(TrackAnalyzers::covered_frames),
            settled,
            "analysis: pass ended"
        );
        let Some(analyzers) = &mut self.analyzers else {
            self.phase = TaskPhase::Done;
            return;
        };
        if let Some(frames) = planned {
            analyzers.plan_extent(frames);
        }
        if settled {
            analyzers.settle();
        }
        self.phase = TaskPhase::Ending;
    }

    pub(crate) fn is_done(&self) -> bool {
        self.phase == TaskPhase::Done
    }

    fn publish(&mut self, detector: Option<&mut Detector>, ending: bool) {
        let Some(analyzers) = &mut self.analyzers else {
            return;
        };
        if analyzers.covered_frames() == 0 {
            return;
        }
        let snapshot = analyzers.snapshot(detector, ending);
        self.published_at = analyzers.covered_frames();
        self.tx.send(Some(snapshot)).ok();
    }

    fn reschedule(&mut self, window: Option<u64>) -> TickResult {
        self.retire();
        let Some(at) = self.choose(window) else {
            self.finish(true);
            return TickResult::Progress;
        };

        match self.reader.seek(duration_for_frames(self.rate.get(), at)) {
            // `landed_at` only echoes the target here; the first chunk says where.
            Ok(SeekOutcome::Landed { .. }) => {
                debug!(at, "analysis: run scheduled");
                self.run = Some(Run {
                    chosen: at,
                    at,
                    frontier: at,
                    started: false,
                    grew: false,
                });
            }
            // The source cannot deliver the position the schedule planned
            // against, which bounds where it ends however long it says it is.
            Ok(SeekOutcome::PastEof { duration, .. }) => {
                debug!(at, ?duration, "analysis: scheduled position past the end");
                self.extent.unreachable(at);
            }
            Err(error) => {
                warn!(?error, at, "analysis: seek failed; position retired");
                self.schedule.barren(at);
            }
        }
        TickResult::Progress
    }

    fn retire(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        // What the run itself decoded, not what the pass covered while it ran:
        // a producer folds ranges from anywhere and would keep this alive.
        if !run.grew {
            debug!(at = run.chosen, "analysis: position added nothing; retired");
            self.schedule.barren(run.chosen);
        }
    }

    fn run_over(&self, run_frames: Option<u64>) -> bool {
        let Some(run) = &self.run else {
            return true;
        };
        if self
            .extent
            .frames()
            .is_some_and(|extent| run.frontier >= extent)
        {
            return true;
        }
        // Covered audio ends a run that already reached its gap. Before that
        // it is the lead-in a seek snapping back off the gap's start left in
        // front, and ending there would retire the gap unread.
        if run.grew && self.is_covered(FrameRange::new(run.frontier, 1)) {
            return true;
        }
        // Read past what it was aimed at with nothing gained: the gap the
        // schedule saw there is not where this source can put the reader.
        if !run.grew && run.frontier > run.chosen {
            return true;
        }
        run_frames.is_some_and(|window| run.frontier.saturating_sub(run.at) >= window)
    }

    fn step(
        &mut self,
        builder: &AnalyzerBuilder<B>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        if self.extent.frames().is_none() {
            return self.decode(builder, detector);
        }
        if self.is_complete() {
            self.finish(true);
            return TickResult::Progress;
        }
        let window = builder.run_frames(self.rate);
        if self.run_over(window) {
            return self.reschedule(window);
        }
        self.decode(builder, detector)
    }

    pub(crate) fn tick(
        &mut self,
        builder: &AnalyzerBuilder<B>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        if self.cancel.is_cancelled() {
            debug!("analysis cancelled");
            self.phase = TaskPhase::Done;
            return TickResult::Progress;
        }

        match self.phase {
            TaskPhase::Decode => {
                let mut detector = detector;
                let drained = self.drain(builder, detector.as_deref_mut());
                // Re-read: the decode path refines a duration upward as it goes.
                self.extent.report(self.reader.duration(), self.rate);
                let result = self.step(builder, detector.as_deref_mut());
                if self.phase == TaskPhase::Decode && self.due() {
                    self.publish(detector, false);
                }
                if drained {
                    TickResult::Progress
                } else {
                    result
                }
            }
            TaskPhase::Ending => {
                self.publish(detector, true);
                self.phase = TaskPhase::Done;
                TickResult::Progress
            }
            TaskPhase::Done => TickResult::Done,
        }
    }
}

fn open<'a, B>(
    slot: &'a mut Option<TrackAnalyzers<B>>,
    builder: &AnalyzerBuilder<B>,
    rate: NonZeroU32,
    token: &AnalysisToken,
) -> &'a mut TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
    slot.get_or_insert_with(|| builder.build(rate, token.clone()))
}
