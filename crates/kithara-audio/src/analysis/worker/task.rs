use kithara_platform::{CancelToken, sync::Arc, tokio::sync::watch};
use kithara_resampler::ResamplerBackend;
use tracing::{debug, warn};

use crate::{
    ChunkOutcome, PcmReader, Waveform,
    analysis::analyzer::{AnalyzerBuilder, Detector, TrackAnalysis, TrackAnalyzers},
    runtime::TickResult,
};

pub(crate) struct Job {
    pub(crate) reader: Box<dyn PcmReader>,
    pub(crate) cancel: CancelToken,
    pub(crate) tx: watch::Sender<Option<TrackAnalysis>>,
    /// What this run is analysing. A cancellation or a stall says nothing
    /// without it: several tracks are in flight and they preempt each other.
    pub(crate) track: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskPhase {
    Decode,
    EmitWaveform,
    DetectBeat,
    Done,
}

pub(crate) struct AnalysisTask<B>
where
    B: ResamplerBackend,
{
    reader: Box<dyn PcmReader>,
    cancel: CancelToken,
    track: Arc<str>,
    analyzers: Option<TrackAnalyzers<B>>,
    waveform: Option<Waveform>,
    tx: watch::Sender<Option<TrackAnalysis>>,
    phase: TaskPhase,
}

impl<B> AnalysisTask<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn new(job: Job) -> Self {
        Self {
            analyzers: None,
            cancel: job.cancel,
            phase: TaskPhase::Decode,
            reader: job.reader,
            track: job.track,
            tx: job.tx,
            waveform: None,
        }
    }

    fn decode(
        &mut self,
        builder: &AnalyzerBuilder<B>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        match self.reader.next_chunk() {
            Ok(ChunkOutcome::Chunk(chunk)) => {
                self.analyzers
                    .get_or_insert_with(|| builder.build(chunk.spec()))
                    .push(&chunk, detector);
                TickResult::Progress
            }
            Ok(ChunkOutcome::Pending { .. }) => TickResult::UpstreamPending,
            Ok(ChunkOutcome::Eof { .. }) => {
                self.phase = if self.analyzers.is_some() {
                    TaskPhase::EmitWaveform
                } else {
                    TaskPhase::Done
                };
                TickResult::Progress
            }
            Err(error) => {
                warn!(?error, "analysis: decode error");
                self.phase = TaskPhase::Done;
                TickResult::Progress
            }
        }
    }

    fn detect_beat(&mut self, detector: Option<&mut Detector>) -> TickResult {
        let Some(analyzers) = self.analyzers.take() else {
            self.phase = TaskPhase::Done;
            return TickResult::Progress;
        };
        let source_frames = analyzers.source_frames();
        let source_sample_rate = analyzers.source_sample_rate();
        let beat = analyzers.finish_beat(detector);
        debug!(track = %self.track, found = beat.is_some(), "analysis finished the beat pass");
        self.tx
            .send(Some(TrackAnalysis::with_source_rate(
                beat,
                self.waveform.take(),
                source_frames,
                source_sample_rate,
            )))
            .ok();
        self.phase = TaskPhase::Done;
        TickResult::Progress
    }

    fn emit_waveform(&mut self) -> TickResult {
        let Some(analyzers) = &mut self.analyzers else {
            self.phase = TaskPhase::Done;
            return TickResult::Progress;
        };
        let source_frames = analyzers.source_frames();
        let source_sample_rate = analyzers.source_sample_rate();
        self.waveform = analyzers.finish_waveform();
        self.tx
            .send(Some(TrackAnalysis::with_source_rate(
                None,
                self.waveform.clone(),
                source_frames,
                source_sample_rate,
            )))
            .ok();
        self.phase = if analyzers.has_beat() {
            TaskPhase::DetectBeat
        } else {
            TaskPhase::Done
        };
        TickResult::Progress
    }

    pub(crate) fn is_done(&self) -> bool {
        self.phase == TaskPhase::Done
    }

    pub(crate) fn tick(
        &mut self,
        builder: &AnalyzerBuilder<B>,
        detector: Option<&mut Detector>,
    ) -> TickResult {
        if self.cancel.is_cancelled() {
            debug!(track = %self.track, phase = ?self.phase, "analysis cancelled");
            self.phase = TaskPhase::Done;
            return TickResult::Progress;
        }

        match self.phase {
            TaskPhase::Decode => self.decode(builder, detector),
            TaskPhase::EmitWaveform => self.emit_waveform(),
            TaskPhase::DetectBeat => self.detect_beat(detector),
            TaskPhase::Done => TickResult::Done,
        }
    }
}
