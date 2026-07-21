use std::num::NonZeroU32;

use kithara_decode::{PcmChunk, PcmSpec};
use kithara_resampler::ResamplerBackend;
use num_traits::cast::AsPrimitive;

use crate::{
    analysis::slots::{beat, waveform},
    waveform::{BeatGrid, bucket::Waveform},
};

pub(crate) struct TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
    pub(super) beat: beat::Slot<B>,
    pub(super) source_frames: u64,
    pub(super) source_spec: PcmSpec,
    pub(super) waveform: waveform::Slot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AnalysisInputError {
    #[error("analysis input format changed from {expected} to {actual}")]
    FormatChanged { expected: PcmSpec, actual: PcmSpec },
    #[error("analysis source frame count overflowed")]
    SourceFrameCountOverflow,
}

impl<B> TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
    pub(crate) fn finish_beat(self, detector: Option<&mut beat::Detector>) -> Option<BeatGrid> {
        self.beat.finish(detector)
    }

    pub(crate) fn finish_waveform(&mut self) -> Option<Waveform> {
        waveform::finish(std::mem::take(&mut self.waveform))
    }

    pub(crate) fn has_beat(&self) -> bool {
        !self.beat.is_empty()
    }

    pub(crate) fn push(
        &mut self,
        chunk: &PcmChunk,
        detector: Option<&mut beat::Detector>,
    ) -> Result<(), AnalysisInputError> {
        let actual = chunk.spec();
        if actual != self.source_spec {
            return Err(AnalysisInputError::FormatChanged {
                expected: self.source_spec,
                actual,
            });
        }
        let frames: u64 = chunk.frames().as_();
        self.source_frames = self
            .source_frames
            .checked_add(frames)
            .ok_or(AnalysisInputError::SourceFrameCountOverflow)?;

        waveform::push(&mut self.waveform, chunk);
        self.beat.push(chunk, detector);
        Ok(())
    }

    pub(crate) fn source_frames(&self) -> u64 {
        self.source_frames
    }

    pub(crate) fn source_sample_rate(&self) -> NonZeroU32 {
        self.source_spec.sample_rate
    }
}
