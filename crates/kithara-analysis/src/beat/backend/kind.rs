#[cfg(all(
    feature = "beat-nn",
    not(any(
        feature = "beat-nn-small",
        feature = "beat-nn-full",
        feature = "beat-nn-full-int8"
    ))
))]
compile_error!(
    "the neural backend needs a model: beat-nn-small, beat-nn-full or beat-nn-full-int8"
);

#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("{self:?}")]
pub(crate) enum BeatDetectorKind {
    #[cfg(feature = "beat-nn")]
    NnBeatThis,
    #[cfg(feature = "beat-dsp")]
    DspSpectral,
}

#[cfg(feature = "beat-nn")]
pub(crate) const SELECTED_DETECTOR: BeatDetectorKind = BeatDetectorKind::NnBeatThis;
#[cfg(all(not(feature = "beat-nn"), feature = "beat-dsp"))]
pub(crate) const SELECTED_DETECTOR: BeatDetectorKind = BeatDetectorKind::DspSpectral;

impl BeatDetectorKind {
    pub(crate) const fn model_tag(self) -> &'static str {
        match self {
            #[cfg(feature = "beat-nn")]
            Self::NnBeatThis => kithara_beat::BEAT_MODEL_TAG,
            #[cfg(feature = "beat-dsp")]
            Self::DspSpectral => "spectral_flux_comb_v1",
        }
    }
}
