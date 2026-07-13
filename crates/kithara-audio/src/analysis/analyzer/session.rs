use kithara_decode::PcmChunk;
use kithara_resampler::ResamplerBackend;
use num_traits::cast::AsPrimitive;

use crate::{
    analysis::slots::{beat, waveform},
    waveform::{BeatGrid, bucket::Waveform},
};

#[derive(Default)]
pub(crate) struct TrackAnalyzers<B>
where
    B: ResamplerBackend,
{
    pub(super) beat: beat::Slot<B>,
    pub(super) source_frames: u64,
    pub(super) waveform: waveform::Slot,
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

    pub(crate) fn push(&mut self, chunk: &PcmChunk, detector: Option<&mut beat::Detector>) {
        let frames: u64 = chunk.frames().as_();
        self.source_frames = self.source_frames.saturating_add(frames);

        waveform::push(&mut self.waveform, chunk);
        self.beat.push(chunk, detector);
    }

    pub(crate) fn source_frames(&self) -> u64 {
        self.source_frames
    }
}
