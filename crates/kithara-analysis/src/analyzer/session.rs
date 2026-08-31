use std::num::{NonZeroU32, NonZeroU64};

use kithara_bufpool::{HasPool, PoolRegion};
use kithara_resampler::ResamplerBackend;
use kithara_signal::AudioChunk;
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{AnalysisFingerprint, AnalysisToken, TrackAnalysis};
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
    pub(super) extent: Option<u64>,
    pub(super) revision: u64,
    pub(super) settled: bool,
    pub(super) ended: bool,
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
    pub(crate) fn plan_extent(&mut self, frames: u64) {
        self.extent = Some(self.extent.map_or(frames, |held| held.max(frames)));
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

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn beat_intake(&self) -> Intake {
        self.beat.intake()
    }

    pub(crate) fn apply_detection(&mut self, output: beat::DetectionOutput) {
        self.beat.apply_detection(output);
    }

    pub(crate) fn push(
        &mut self,
        chunk: &AudioChunk,
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
        self.ingest(&chunk.samples[..], channels, range, true, detector)
    }

    pub(crate) fn push_mono(
        &mut self,
        mono: &[f32],
        at: u64,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        let frames = mono.len().to_u64().unwrap_or(0);
        // Audio the pass did not read for itself extends what the beat pass
        // holds; a run of its own is backlog the pass did not plan for and
        // would have to be read again anyway.
        self.ingest(mono, 1, FrameRange::new(at, frames), false, detector)
    }

    fn ingest(
        &mut self,
        pcm: &[f32],
        channels: usize,
        range: FrameRange,
        opens: bool,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        if self.ended && self.extent.is_some_and(|extent| range.end() > extent) {
            warn!(
                start = range.start(),
                end = range.end(),
                extent = self.extent,
                "analysis: range lies beyond the source extent; dropped"
            );
            return Ingest::OutOfExtent;
        }
        if self.extent.is_some_and(|extent| range.end() > extent) {
            self.extent = Some(range.end());
        }
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
    ) -> TrackAnalysis {
        if ending {
            // The frontier is the extent only for a pass that grew from the
            // start; one that planned against a longer source keeps that
            // length, so the tail it never reached stays missing.
            let frontier = self.coverage.frontier();
            self.extent = Some(
                self.extent
                    .map_or(frontier, |planned| planned.max(frontier)),
            );
            self.ended = true;
        }
        self.revision = self.revision.saturating_add(1);

        let waveform = waveform::snapshot(&mut self.waveform, self.extent);
        let state = self.beat_state();
        let beat = Slot::snapshot(&mut self.beat, &self.pools, detector, ending, self.extent)
            .map(|(grid, unanalysed)| BeatSnapshot::new(grid, state, unanalysed));

        TrackAnalysis::builder()
            .token(self.token.clone())
            .revision(self.revision)
            .source_sample_rate(self.source_sample_rate)
            .maybe_extent(self.extent)
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
    ) -> AnalysisProgress {
        let analysis = self.snapshot(detector, ending);
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
            || analysis.extent().is_none()
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
        self.extent = analysis.extent();
        self.revision = analysis.revision();
        self.settled = false;
        self.ended = false;
        Ok(())
    }

    fn beat_state(&self) -> BeatState {
        let covered = self
            .extent
            .is_some_and(|extent| self.analysed().contains(FrameRange::new(0, extent)));
        if covered {
            BeatState::Final
        } else {
            BeatState::Provisional
        }
    }
}
