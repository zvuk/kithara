use kithara_bufpool::{HasPool, PoolRegion};

use super::{
    super::{BeatDetectError, BeatDetector, BeatMark, RawBeats},
    BeatDetectorKind,
};

pub(crate) fn build_detector<S>(
    kind: BeatDetectorKind,
    pools: &PoolRegion<S>,
) -> Result<Box<dyn BeatDetector>, BeatDetectError>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    match kind {
        #[cfg(feature = "beat-nn")]
        BeatDetectorKind::NnBeatThis => Ok(Box::new(super::nn::Detector::new(pools)?)),
        #[cfg(feature = "beat-dsp")]
        BeatDetectorKind::DspSpectral => Ok(Box::new(super::dsp::Detector::new(pools)?)),
    }
}

/// The crate's marks read as the analysis pass's own.
pub(super) fn marks(raw: kithara_beat::RawBeats) -> RawBeats {
    let mark = |mark: kithara_beat::BeatMark| BeatMark {
        at: mark.at,
        confidence: mark.confidence,
    };
    RawBeats {
        beats: raw.beats.into_iter().map(mark).collect(),
        downbeats: raw.downbeats.into_iter().map(mark).collect(),
    }
}
