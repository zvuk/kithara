use std::{
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    ops::Range,
};

use kithara_audio::{AudioReadError, AudioReader, ChunkOutcome, SeekOutcome};
use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_platform::{
    CancelToken,
    tokio::sync::{oneshot, watch},
};
use kithara_resampler::ResamplerBackend;
use kithara_signal::{AudioSpec, FrameCoverage};
use kithara_worker::TickResult;
use rangemap::RangeSet;
use tracing::{debug, warn};

use super::{AnalysisError, schedule::Schedule};
use crate::{
    AnalysisProgress, BlobError,
    analyzer::{
        AnalysisDemand, AnalysisToken, AnalyzerBuilder, Detector, Extent, Ingest, PassError,
        TrackAnalyzers,
    },
    producer::ring,
    slots::{
        Intake,
        beat::{DetectOutput, DetectRequest},
    },
};

pub(crate) struct Job {
    pub(crate) demand: AnalysisDemand,
    pub(crate) token: AnalysisToken,
    pub(crate) reader: Box<dyn AudioReader>,
    pub(crate) cancel: CancelToken,
    pub(crate) rate: NonZeroU32,
    pub(crate) resume: Option<AnalysisProgress>,
    pub(crate) ingest: ring::Reader,
    pub(crate) tx: watch::Sender<Option<AnalysisProgress>>,
    pub(crate) revision: u64,
    pub(crate) report: Option<oneshot::Sender<Result<AnalysisProgress, AnalysisError>>>,
}

#[derive(Debug)]
enum End {
    Settled,
    Cancelled,
    Read(AudioReadError),
}

#[derive(Debug)]
enum TaskPhase {
    Decode,
    Ending(End),
    Done,
}

struct Run {
    deferred: bool,
    grew: bool,
    started: bool,
    at: u64,
    chosen: u64,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct AnalysisTask<B, S>
where
    B: ResamplerBackend,
{
    demand: AnalysisDemand,
    token: AnalysisToken,
    reader: Box<dyn AudioReader>,
    #[field(get = cancel_token, vis = "pub(crate)")]
    cancel: CancelToken,
    extent: Extent,
    rate: NonZeroU32,
    chunk_frames: NonZeroU64,
    analyzers: Option<TrackAnalyzers<B, S>>,
    run: Option<Run>,
    scratch: Option<SampleBuffer>,
    ingest: ring::Reader,
    schedule: Schedule,
    tx: watch::Sender<Option<AnalysisProgress>>,
    phase: TaskPhase,
    pub(super) report: Option<oneshot::Sender<Result<AnalysisProgress, AnalysisError>>>,
    beat_dirty: bool,
    frontier: u64,
    publish_frames: u64,
    published_at: u64,
    revision: u64,
    producer_drain_limit: usize,
}

impl<B, S> AnalysisTask<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) fn new(
        job: Job,
        builder: &AnalyzerBuilder<B, S>,
        chunk_seconds: NonZeroU32,
        producer_drain_limit: NonZeroUsize,
        publish_seconds: NonZeroU32,
    ) -> Result<Self, BlobError> {
        let rate = u64::from(job.rate.get());
        let chunk_frames = NonZeroU64::new(rate.saturating_mul(u64::from(chunk_seconds.get())))
            .ok_or(BlobError::Corrupt)?;
        let analyzers = job
            .resume
            .as_ref()
            .map(|progress| builder.restore(progress, chunk_frames))
            .transpose()?;
        let published_at = analyzers.as_ref().map_or(0, TrackAnalyzers::covered_frames);
        let extent = match &job.resume {
            Some(progress) => {
                Extent::restore(progress.analysis().extent().ok_or(BlobError::Corrupt)?)
            }
            None => Extent::default(),
        };
        Ok(Self {
            analyzers,
            chunk_frames,
            extent,
            published_at,
            beat_dirty: false,
            cancel: job.cancel,
            frontier: 0,
            ingest: job.ingest,
            phase: TaskPhase::Decode,
            producer_drain_limit: producer_drain_limit.get(),
            publish_frames: rate.saturating_mul(u64::from(publish_seconds.get())),
            rate: job.rate,
            reader: job.reader,
            run: None,
            schedule: Schedule::default(),
            scratch: None,
            token: job.token,
            revision: job.revision,
            demand: job.demand,
            tx: job.tx,
            report: job.report,
        })
    }

    pub(crate) fn apply_detection(&mut self, output: DetectOutput) {
        if let Some(analyzers) = &mut self.analyzers {
            analyzers.apply_detection(output);
            self.beat_dirty = true;
        }
    }

    fn choose(&self, window: Option<u64>) -> Option<u64> {
        let empty = RangeSet::new();
        let coverage = self.target().unwrap_or(&empty);
        let extent = self.extent.frames();
        match self.intake() {
            Intake::Continuing => self.schedule.extend(coverage, extent),
            Intake::Full | Intake::Anywhere => self.schedule.next(coverage, extent, window),
        }
    }

    fn decode(
        &mut self,
        builder: &AnalyzerBuilder<B, S>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        match self.reader.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                debug!(
                    metadata = ?chunk.meta,
                    samples = chunk.samples.len(),
                    pass_rate = self.rate.get(),
                    "analysis: decoded PCM offered"
                );
                let range = chunk.meta.frame_range();
                let pushed = open(
                    &mut self.analyzers,
                    builder,
                    self.rate,
                    &self.token,
                    self.revision,
                    self.demand,
                )
                .and_then(|analyzers| {
                    analyzers
                        .push(&chunk, &mut self.extent, detector)
                        .map_err(PassError::from)
                });
                let outcome = match pushed {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.fail(error);
                        return TickResult::Progress;
                    }
                };
                if outcome != Ingest::Accepted {
                    debug!(?outcome, "analysis: range not folded in");
                }
                self.frontier = range.end;
                if let Some(run) = &mut self.run {
                    if !run.started {
                        run.started = true;
                        run.at = range.start;
                    }
                    run.grew |= outcome == Ingest::Accepted;
                    run.deferred |= outcome == Ingest::Deferred;
                }
                TickResult::Progress
            }
            Ok(ChunkOutcome::Pending { .. }) => TickResult::UpstreamPending,
            Ok(ChunkOutcome::Eof { .. }) => {
                let scheduled = self.extent.frames().is_some();
                self.extent.prove_end(self.frontier);
                debug!(
                    frontier = self.frontier,
                    extent = ?self.extent.frames(),
                    "analysis: eof bounds the extent"
                );
                if scheduled {
                    self.retire();
                } else {
                    self.finish(End::Settled);
                }
                TickResult::Progress
            }
            Err(error) => {
                warn!(?error, "analysis: decode error; pass ended");
                self.finish(End::Read(error));
                TickResult::Progress
            }
        }
    }

    fn drain(
        &mut self,
        builder: &AnalyzerBuilder<B, S>,
        detector: Option<&mut Detector>,
    ) -> Result<bool, PassError> {
        let scratch = self
            .scratch
            .get_or_insert_with(|| builder.pools().get::<f32>());
        let analyzers = open(
            &mut self.analyzers,
            builder,
            self.rate,
            &self.token,
            self.revision,
            self.demand,
        )?;
        let mut detector = detector;
        let mut folded = false;

        for _ in 0..self.producer_drain_limit {
            let Some(span) = self.ingest.pop(scratch) else {
                break;
            };
            let outcome =
                analyzers.push_ingest(scratch, span, &mut self.extent, detector.as_deref_mut())?;
            if outcome != Ingest::Accepted {
                debug!(
                    ?outcome,
                    at = span.start,
                    "analysis: offered range not folded in"
                );
            }
            folded = true;
        }
        Ok(folded)
    }

    fn due(&self) -> bool {
        let Some(analyzers) = &self.analyzers else {
            return false;
        };
        analyzers.covered_frames().saturating_sub(self.published_at) >= self.publish_frames
    }

    fn fail(&mut self, error: PassError) {
        warn!(?error, "analysis: pass failed; ended unsettled");
        self.report(Err(AnalysisError::Analyzer(error)));
        self.phase = TaskPhase::Done;
    }

    pub(crate) fn fail_compute_unavailable(&mut self) {
        warn!("analysis: compute pool unavailable; pass ended");
        self.report(Err(AnalysisError::ComputeUnavailable));
        self.phase = TaskPhase::Done;
    }

    fn finish(&mut self, end: End) {
        debug!(
            extent = ?self.extent.frames(),
            covered = ?self.analyzers.as_ref().map(TrackAnalyzers::covered_frames),
            ?end,
            "analysis: reading ended"
        );
        if self.analyzers.is_some() {
            self.phase = TaskPhase::Ending(end);
        } else {
            self.phase = TaskPhase::Done;
            self.report_end(end, None);
        }
    }

    fn report(&mut self, result: Result<AnalysisProgress, AnalysisError>) {
        if let Some(report) = self.report.take() {
            report.send(result).ok();
        }
    }

    fn report_end(&mut self, end: End, progress: Option<AnalysisProgress>) {
        match end {
            End::Settled => self.report(progress.ok_or(AnalysisError::Empty)),
            End::Read(error) => self.report(Err(AnalysisError::Read(error))),
            End::Cancelled => {
                drop(self.report.take());
            }
        }
    }

    fn intake(&self) -> Intake {
        self.analyzers
            .as_ref()
            .map_or(Intake::Anywhere, TrackAnalyzers::beat_intake)
    }

    fn is_complete(&self) -> bool {
        let Some(extent) = self.extent.frames() else {
            return false;
        };
        self.analyzers
            .as_ref()
            .is_some_and(|analyzers| analyzers.analysed().covers(&(0..extent)))
    }

    fn is_covered(&self, range: Range<u64>) -> bool {
        self.target()
            .is_some_and(|coverage| coverage.covers(&range))
    }

    pub(crate) fn is_done(&self) -> bool {
        matches!(self.phase, TaskPhase::Done)
    }

    pub(crate) fn is_ending(&self) -> bool {
        matches!(self.phase, TaskPhase::Ending(_))
    }

    pub(crate) fn prepare_detection(&mut self) -> Option<DetectRequest> {
        let trailing = self.is_ending();
        self.analyzers.as_mut()?.prepare_detection(trailing)
    }

    fn publish(
        &mut self,
        detector: Option<&mut Detector>,
        ending: bool,
    ) -> Option<AnalysisProgress> {
        let analyzers = self.analyzers.as_mut()?;
        if analyzers.covered_frames() == 0 {
            return None;
        }
        match analyzers.progress(detector, ending, self.chunk_frames, self.extent.frames()) {
            Ok(progress) => {
                self.published_at = analyzers.covered_frames();
                self.tx.send(Some(progress.clone())).ok();
                Some(progress)
            }
            Err(error) => {
                self.fail(error.into());
                None
            }
        }
    }

    fn run_window(&self) -> Option<u64> {
        (self.intake() != Intake::Continuing).then_some(self.chunk_frames.get())
    }

    fn target(&self) -> Option<&RangeSet<u64>> {
        let analyzers = self.analyzers.as_ref()?;
        match analyzers.beat_intake() {
            Intake::Full => Some(analyzers.coverage()),
            Intake::Continuing | Intake::Anywhere => Some(analyzers.analysed()),
        }
    }
}

impl<B, S> AnalysisTask<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// The source cannot deliver the position the schedule planned against, so it bounds where the
    /// run ends however long it claims to be.
    fn reschedule(&mut self, window: Option<u64>) -> TickResult {
        self.retire();
        let Some(at) = self.choose(window) else {
            if self.intake() == Intake::Full {
                return TickResult::Backpressured;
            }
            self.finish(End::Settled);
            return TickResult::Progress;
        };

        let Ok(position) = AudioSpec::new(1, self.rate).duration_for(at) else {
            warn!(
                at,
                "analysis: scheduled frame cannot be represented as a duration"
            );
            self.schedule.barren(at);
            return TickResult::Progress;
        };
        match self.reader.seek(position) {
            Ok(SeekOutcome::Landed { .. } | SeekOutcome::Pending { .. }) => {
                debug!(at, "analysis: run scheduled");
                self.frontier = at;
                self.run = Some(Run {
                    chosen: at,
                    at,
                    started: false,
                    grew: false,
                    deferred: false,
                });
            }
            Ok(SeekOutcome::PastEof { duration, .. }) => {
                debug!(at, ?duration, "analysis: scheduled position past the end");
                self.extent.prove_end(at);
            }
            Err(error) => {
                warn!(?error, at, "analysis: seek failed; position retired");
                self.schedule.barren(at);
            }
        }
        TickResult::Progress
    }

    /// Retirement follows what the run itself decoded, not what the pass covered while it ran: a
    /// producer folds ranges from anywhere and would otherwise keep the run alive forever.
    fn retire(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        if !run.grew && !run.deferred {
            debug!(at = run.chosen, "analysis: position added nothing; retired");
            self.schedule.barren(run.chosen);
        }
    }

    /// Covered audio ends a run only once it reaches its own gap, since ending earlier would retire
    /// the gap unread. Overshooting the target with nothing gained means this source cannot reach
    /// the schedule's gap.
    fn run_over(&self, run_frames: Option<u64>) -> bool {
        let Some(run) = &self.run else {
            return true;
        };
        if self
            .extent
            .frames()
            .is_some_and(|extent| self.frontier >= extent)
        {
            return true;
        }
        if run.grew && self.is_covered(self.frontier..self.frontier + 1) {
            return true;
        }
        if !run.grew && self.frontier > run.chosen {
            return true;
        }
        run_frames.is_some_and(|window| self.frontier.saturating_sub(run.at) >= window)
    }

    fn step(
        &mut self,
        builder: &AnalyzerBuilder<B, S>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        if self.extent.frames().is_none() {
            if self.intake() == Intake::Full {
                return TickResult::Backpressured;
            }
            return self.decode(builder, detector);
        }
        if self.is_complete() {
            self.finish(End::Settled);
            return TickResult::Progress;
        }
        let window = self.run_window();
        if self.run_over(window) {
            return self.reschedule(window);
        }
        self.decode(builder, detector)
    }

    /// The decode path refines the duration upward as it goes, so re-reading it on each tick lets a
    /// later, more accurate value replace the earlier estimate.
    pub(crate) fn tick(
        &mut self,
        builder: &AnalyzerBuilder<B, S>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        if self.cancel.is_cancelled()
            && matches!(
                self.phase,
                TaskPhase::Decode | TaskPhase::Ending(End::Settled)
            )
        {
            debug!("analysis cancelled");
            self.finish(End::Cancelled);
            return TickResult::Progress;
        }

        match std::mem::replace(&mut self.phase, TaskPhase::Done) {
            TaskPhase::Decode => {
                self.phase = TaskPhase::Decode;
                let visible_origin = match self.analyzers.as_ref() {
                    Some(analyzers) => analyzers.visible_origin(),
                    None => self.reader.visible_origin(),
                };
                let mut detector = detector;
                let drained = match self.drain(builder, detector.as_deref_mut()) {
                    Ok(drained) => drained,
                    Err(error) => {
                        self.fail(error);
                        return TickResult::Progress;
                    }
                };
                let raw_end = self
                    .reader
                    .duration()
                    .and_then(|length| visible_origin?.checked_add(length));
                self.extent.claim(raw_end, self.rate);
                let result = self.step(builder, detector.as_deref_mut());
                if self.is_done() {
                    return TickResult::Progress;
                }
                if self.is_ending() || self.due() || self.beat_dirty {
                    self.publish(detector, false);
                    self.beat_dirty = false;
                }
                if drained {
                    TickResult::Progress
                } else {
                    result
                }
            }
            TaskPhase::Ending(end) => {
                if matches!(end, End::Settled)
                    && let Some(analyzers) = &mut self.analyzers
                {
                    analyzers.settle();
                }
                let progress = self.publish(detector, true);
                self.report_end(end, progress);
                TickResult::Progress
            }
            TaskPhase::Done => TickResult::Done,
        }
    }
}

fn open<'a, B, S>(
    slot: &'a mut Option<TrackAnalyzers<B, S>>,
    builder: &AnalyzerBuilder<B, S>,
    rate: NonZeroU32,
    token: &AnalysisToken,
    revision: u64,
    demand: AnalysisDemand,
) -> Result<&'a mut TrackAnalyzers<B, S>, PassError>
where
    B: ResamplerBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    if let Some(analyzers) = slot {
        Ok(analyzers)
    } else {
        let analyzers = builder.build(rate, token.clone(), revision, demand)?;
        Ok(slot.insert(analyzers))
    }
}
