use std::num::NonZeroU32;

use kithara_decode::PcmChunk;
use kithara_resampler::ResamplerBackend;
use num_traits::cast::ToPrimitive;
use tracing::warn;

use super::{
    snapshot::{BeatSnapshot, GridState},
    track::{AnalysisFingerprint, AnalysisToken, TrackAnalysis},
};
use crate::{
    analysis::slots::{
        beat::{self, Slot},
        waveform,
    },
    coverage::{Coverage, FrameRange},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ingest {
    Accepted,
    Covered,
    ForeignRate,
    OutOfExtent,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct TrackAnalyzers<B>
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
    #[field(get, copy, vis = "pub(crate)")]
    pub(super) source_sample_rate: NonZeroU32,
    pub(super) token: AnalysisToken,
}

impl<B> TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) const fn coverage(&self) -> &Coverage {
        &self.coverage
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

    pub(crate) fn push(
        &mut self,
        chunk: &PcmChunk,
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
        self.ingest(&chunk.samples[..], channels, range, detector)
    }

    pub(crate) fn push_mono(
        &mut self,
        mono: &[f32],
        at: u64,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        let frames = mono.len().to_u64().unwrap_or(0);
        self.ingest(mono, 1, FrameRange::new(at, frames), detector)
    }

    fn ingest(
        &mut self,
        pcm: &[f32],
        channels: usize,
        range: FrameRange,
        detector: Option<&mut beat::Detector>,
    ) -> Ingest {
        if self.extent.is_some_and(|extent| range.end() > extent) {
            warn!(
                start = range.start(),
                end = range.end(),
                extent = self.extent,
                "analysis: range lies beyond the source extent; dropped"
            );
            return Ingest::OutOfExtent;
        }
        if self.coverage.contains(range) {
            return Ingest::Covered;
        }
        self.coverage.insert(range);

        waveform::push(&mut self.waveform, pcm, channels, range.start());
        Slot::push(&mut self.beat, pcm, channels, range.start(), detector);
        Ingest::Accepted
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
        }
        self.revision = self.revision.saturating_add(1);

        let waveform = waveform::snapshot(&mut self.waveform, self.extent);
        let state = self.grid_state();
        let beat = Slot::snapshot(&mut self.beat, detector, ending, self.extent)
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

    fn grid_state(&self) -> GridState {
        let covered = self
            .extent
            .is_some_and(|extent| self.coverage.contains(FrameRange::new(0, extent)));
        if covered {
            GridState::Final
        } else {
            GridState::Provisional
        }
    }
}
