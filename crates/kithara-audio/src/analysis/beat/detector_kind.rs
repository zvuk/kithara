/// Beat detector selection.
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("{self:?}")]
pub(crate) enum BeatDetectorKind {
    /// `kithara-beat` NN (`beat_this` port). Feature `beat-nn`.
    #[cfg(feature = "beat-nn")]
    NnBeatThis,
}

impl BeatDetectorKind {
    /// Detectors compiled into this target/feature set, in selector order.
    pub(crate) const ALL: &'static [Self] = &[
        #[cfg(feature = "beat-nn")]
        Self::NnBeatThis,
    ];

    /// First compiled-in detector, in selector order.
    pub(crate) fn first() -> Self {
        Self::ALL[0]
    }
}

impl Default for BeatDetectorKind {
    fn default() -> Self {
        Self::first()
    }
}
