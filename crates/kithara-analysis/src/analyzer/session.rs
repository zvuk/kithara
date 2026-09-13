use std::num::{NonZeroU32, NonZeroU64};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_resampler::ResamplerBackend;
use kithara_signal::AudioChunk;
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{AnalysisFingerprint, AnalysisToken, Extent, TrackAnalysis};
use crate::{
    AnalysisProgress, BeatSnapshot, BeatState, BlobError,
    coverage::{Coverage, FrameRange},
    progress::{AnalysisResume, ResumeState},
    slots::{
        Intake, Opens,
        beat::{self, Slot},
        waveform,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ingest {
    Accepted,
    Covered,
    Deferred,
    ForeignRate,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TrackAnalyzers<B, S>
where
    B: ResamplerBackend,
{
    pub(super) fingerprint: AnalysisFingerprint,
    pub(super) token: AnalysisToken,
    #[field(get, vis = "pub(crate)")]
    pub(super) coverage: Coverage,
    pub(super) source_sample_rate: NonZeroU32,
    pub(super) pools: PoolRegion<S>,
    pub(super) beat: Slot<B>,
    pub(super) waveform: waveform::Slot,
    pub(super) settled: bool,
    pub(super) revision: u64,
}

impl<B, S> TrackAnalyzers<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32>,
{
    pub(crate) fn analysed(&self) -> &Coverage {
        self.beat.coverage(&self.coverage)
    }

    fn beat_state(&self) -> BeatState {
        let analysed = self.analysed();
        let taken = self
            .coverage
            .runs()
            .iter()
            .all(|run| analysed.contains(*run));
        if self.settled && taken {
            BeatState::Final
        } else {
            BeatState::Provisional
        }
    }

    pub(crate) fn covered_frames(&self) -> u64 {
        self.coverage.frames()
    }

    fn ingest(
        &mut self,
        pcm: &[f32],
        channels: usize,
        range: FrameRange,
        opens: Opens,
        extent: &mut Extent,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        extent.deliver(range);
        let seen = self.coverage.contains(range);
        if !seen {
            self.coverage.insert(range);
            waveform::push(
                &mut self.waveform,
                &self.pools,
                pcm,
                channels,
                range.start(),
            );
        }
        let analysed = self.beat.coverage(&self.coverage).contains(range);
        let took = self
            .beat
            .push(&self.pools, pcm, channels, range.start(), opens, detector);
        if took || !seen {
            return Ingest::Accepted;
        }
        if analysed {
            return Ingest::Covered;
        }
        Ingest::Deferred
    }

    pub(crate) fn prepare_detection(&mut self, trailing: bool) -> Option<beat::DetectRequest> {
        self.beat.prepare_detection(&self.pools, trailing)
    }

    pub(crate) fn progress(
        &mut self,
        detector: Option<&mut beat::Detector>,
        ending: bool,
        chunk_frames: NonZeroU64,
        extent: Option<u64>,
    ) -> AnalysisProgress {
        let analysis = self.snapshot(detector, ending, extent);
        let resume = if analysis.is_settled() {
            None
        } else {
            let waveform = waveform::write_resume(&self.waveform);
            let beat = self.beat.write_resume();
            Some(AnalysisResume::capture(
                chunk_frames,
                waveform.as_deref(),
                beat.as_deref(),
            ))
        };
        AnalysisProgress::new(analysis, resume)
    }

    pub(crate) fn push(
        &mut self,
        chunk: &AudioChunk,
        extent: &mut Extent,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        let rate = chunk.spec().sample_rate;
        if rate != self.source_sample_rate {
            warn!(
                axis = self.source_sample_rate.get(),
                rate = rate.get(),
                "analysis: chunk rate differs from the pass axis; range dropped"
            );
            return Ingest::ForeignRate;
        }

        let channels = usize::from(chunk.spec().channels.max(1));
        let range = FrameRange::from(&chunk.meta);
        self.ingest(
            &chunk.samples[..],
            channels,
            range,
            Opens::Run,
            extent,
            detector,
        )
    }

    pub(crate) fn push_mono(
        &mut self,
        mono: &[f32],
        at: u64,
        extent: &mut Extent,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        let frames = mono.len().to_u64().unwrap_or(0);
        self.ingest(
            mono,
            1,
            FrameRange::new(at, frames),
            Opens::Extends,
            extent,
            detector,
        )
    }

    pub(crate) fn restore(
        &mut self,
        analysis: &TrackAnalysis,
        resume: ResumeState,
        chunk_frames: NonZeroU64,
    ) -> Result<(), BlobError> {
        if analysis.is_settled()
            || analysis.source_sample_rate() != self.source_sample_rate
            || analysis.token() != &self.token
            || analysis.fingerprint() != &self.fingerprint
            || resume.chunk_frames != chunk_frames
        {
            return Err(BlobError::Corrupt);
        }
        #[cfg(not(feature = "analysis-waveform"))]
        let waveform_resume = resume.waveform.as_ref();
        #[cfg(feature = "analysis-waveform")]
        let waveform_resume = resume.waveform;
        waveform::restore(&mut self.waveform, &self.pools, waveform_resume)?;
        self.beat.restore(&self.pools, resume.beat)?;
        self.coverage = analysis.coverage().clone();
        self.settled = false;
        Ok(())
    }

    pub(crate) const fn settle(&mut self) {
        self.settled = true;
    }

    pub(crate) fn snapshot(
        &mut self,
        detector: Option<&mut beat::Detector>,
        ending: bool,
        extent: Option<u64>,
    ) -> TrackAnalysis {
        self.revision = self.revision.saturating_add(1);

        let waveform = waveform::snapshot(&mut self.waveform, extent);
        let state = self.beat_state();
        let beat = self
            .beat
            .snapshot(&self.pools, detector, ending, extent)
            .map(|(grid, unanalysed)| BeatSnapshot::new(grid, state, unanalysed));

        TrackAnalysis::builder()
            .token(self.token.clone())
            .revision(self.revision)
            .source_sample_rate(self.source_sample_rate)
            .maybe_extent(extent)
            .coverage(self.coverage.clone())
            .fingerprint(self.fingerprint.clone())
            .settled(self.settled)
            .maybe_waveform(waveform)
            .maybe_beat(beat)
            .build()
    }

    delegate::delegate! {
        to self.beat {
            pub(crate) fn apply_detection(&mut self, output: beat::DetectOutput);
            #[call(intake)]
            pub(crate) fn beat_intake(&self) -> Intake;
        }
    }
}
