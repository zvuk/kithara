use kithara_platform::{CancelToken, tokio::sync::watch};
use kithara_resampler::ResamplerBackend;
use tracing::{debug, warn};

use crate::{
    ChunkOutcome, PcmReader, Waveform,
    analysis::analyzer::{AnalyzerBuilder, Detector, TrackAnalysis, TrackAnalyzers},
    runtime::TickResult,
};

pub(crate) struct Job {
    pub(crate) cancel: CancelToken,
    pub(crate) reader: Box<dyn PcmReader>,
    pub(crate) tx: watch::Sender<Option<TrackAnalysis>>,
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
    analyzers: Option<TrackAnalyzers<B>>,
    cancel: CancelToken,
    phase: TaskPhase,
    reader: Box<dyn PcmReader>,
    tx: watch::Sender<Option<TrackAnalysis>>,
    waveform: Option<Waveform>,
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
            tx: job.tx,
            waveform: None,
        }
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
            debug!("analysis cancelled");
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

    fn emit_waveform(&mut self) -> TickResult {
        let Some(analyzers) = &mut self.analyzers else {
            self.phase = TaskPhase::Done;
            return TickResult::Progress;
        };
        let source_frames = analyzers.source_frames();
        self.waveform = analyzers.finish_waveform();
        let _ = self.tx.send(Some(TrackAnalysis::new(
            None,
            self.waveform.clone(),
            source_frames,
        )));
        self.phase = if analyzers.has_beat() {
            TaskPhase::DetectBeat
        } else {
            TaskPhase::Done
        };
        TickResult::Progress
    }

    fn detect_beat(&mut self, detector: Option<&mut Detector>) -> TickResult {
        let Some(analyzers) = self.analyzers.take() else {
            self.phase = TaskPhase::Done;
            return TickResult::Progress;
        };
        let source_frames = analyzers.source_frames();
        let beat = analyzers.finish_beat(detector);
        let _ = self.tx.send(Some(TrackAnalysis::new(
            beat,
            self.waveform.take(),
            source_frames,
        )));
        self.phase = TaskPhase::Done;
        TickResult::Progress
    }
}
