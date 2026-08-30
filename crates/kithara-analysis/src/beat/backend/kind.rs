#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("{self:?}")]
pub(crate) enum BeatDetectorKind {
    #[cfg(feature = "beat-nn")]
    NnBeatThis,
    #[cfg(feature = "beat-dsp")]
    DspSpectral,
}

/// The detector this build uses. A build carrying both uses the network: the
/// signal-processing backend is there for the builds that cannot run one.
#[cfg(feature = "beat-nn")]
pub(crate) const SELECTED_DETECTOR: BeatDetectorKind = BeatDetectorKind::NnBeatThis;
#[cfg(all(not(feature = "beat-nn"), feature = "beat-dsp"))]
pub(crate) const SELECTED_DETECTOR: BeatDetectorKind = BeatDetectorKind::DspSpectral;

impl BeatDetectorKind {
    /// What the detector reads from, named in the analysis fingerprint so a
    /// grid one backend produced is never served to a build using the other.
    pub(crate) const fn model_tag(self) -> &'static str {
        match self {
            #[cfg(feature = "beat-nn")]
            Self::NnBeatThis => "beat_this_small_v1",
            #[cfg(feature = "beat-dsp")]
            Self::DspSpectral => "spectral_flux_comb_v1",
        }
    }
}
