use std::num::{NonZeroU32, NonZeroU64};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_resampler::ResamplerBackend;
use kithara_signal::AudioChunk;
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{AnalysisFingerprint, AnalysisToken, Extent, TrackAnalysis};
use crate::{
    AnalysisProgress, BeatSnapshot, BeatState, BlobError,
    beat::Intake,
    coverage::{Coverage, FrameRange},
    progress::{AnalysisResume, ResumeState},
    slots::{
        beat::{self, Slot},
        waveform,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ingest {
    Accepted,
    Covered,
    /// The pass has this range, and the beat pass turned it down until the
    /// detector frees room.
    Deferred,
    ForeignRate,
    OutOfExtent,
}

pub(crate) struct TrackAnalyzers<B, S>
where
    B: ResamplerBackend,
{
    pub(super) beat: Slot<B>,
    pub(super) waveform: waveform::Slot,
    pub(super) coverage: Coverage,
    pub(super) fingerprint: AnalysisFingerprint,
    pub(super) revision: u64,
    pub(super) settled: bool,
    pub(super) source_sample_rate: NonZeroU32,
    pub(super) token: AnalysisToken,
    pub(super) pools: PoolRegion<S>,
}

impl<B, S> TrackAnalyzers<B, S>
where
    B: ResamplerBackend,
    S: HasPool<f32>,
{
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) const fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// What the beat pass has taken. It takes audio only as fast as the
    /// detector frees room, so it trails what the pass has seen.
    pub(crate) fn analysed(&self) -> &Coverage {
        self.beat.coverage(&self.coverage)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) const fn settle(&mut self) {
        self.settled = true;
    }

    pub(crate) fn covered_frames(&self) -> u64 {
        self.coverage.frames()
    }

    pub(crate) fn prepare_detection(&mut self, trailing: bool) -> Option<beat::DetectionRequest> {
        self.beat.prepare_detection(&self.pools, trailing)
    }

    delegate::delegate! {
        to self.beat {
            pub(crate) fn apply_detection(&mut self, output: beat::DetectionOutput);
            #[cfg(not(target_arch = "wasm32"))]
            #[call(intake)]
            pub(crate) fn beat_intake(&self) -> Intake;
        }
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
        self.ingest(&chunk.samples[..], channels, range, true, extent, detector)
    }

    pub(crate) fn push_mono(
        &mut self,
        mono: &[f32],
        at: u64,
        extent: &mut Extent,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        let frames = mono.len().to_u64().unwrap_or(0);
        // Audio the pass did not read for itself extends what the beat pass
        // holds; a run of its own is backlog the pass did not plan for and
        // would have to be read again anyway.
        self.ingest(
            mono,
            1,
            FrameRange::new(at, frames),
            false,
            extent,
            detector,
        )
    }

    fn ingest(
        &mut self,
        pcm: &[f32],
        channels: usize,
        range: FrameRange,
        opens: bool,
        extent: &mut Extent,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        if extent.refuses(range) {
            warn!(
                start = range.start(),
                end = range.end(),
                extent = ?extent.frames(),
                "analysis: range lies past the end the source proved; dropped"
            );
            return Ingest::OutOfExtent;
        }
        extent.show(range);
        // The beat pass trails the rest, so a range the pass has already seen
        // can still be what the beat pass is waiting to be offered again.
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
        let took = Slot::push(
            &mut self.beat,
            &self.pools,
            pcm,
            channels,
            range.start(),
            opens,
            detector,
        );
        if took || !seen {
            return Ingest::Accepted;
        }
        if analysed {
            return Ingest::Covered;
        }
        Ingest::Deferred
    }

    pub(crate) fn snapshot(
        &mut self,
        detector: Option<&mut beat::Detector>,
        ending: bool,
        extent: Option<u64>,
    ) -> TrackAnalysis {
        self.revision = self.revision.saturating_add(1);

        let waveform = waveform::snapshot(&mut self.waveform, extent);
        let state = self.beat_state(extent);
        let beat = Slot::snapshot(&mut self.beat, &self.pools, detector, ending, extent)
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
        waveform::restore(&mut self.waveform, &self.pools, resume.waveform)?;
        self.beat.restore(&self.pools, resume.beat)?;
        self.coverage = analysis.coverage().clone();
        self.settled = false;
        Ok(())
    }

    /// A grid is final once the pass is over and the beat pass took the
    /// whole extent: what an earlier revision holds can still change.
    fn beat_state(&self, extent: Option<u64>) -> BeatState {
        let covered =
            extent.is_some_and(|extent| self.analysed().contains(FrameRange::new(0, extent)));
        if self.settled && covered {
            BeatState::Final
        } else {
            BeatState::Provisional
        }
    }
}
