use std::fmt;

/// Beat detector selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl Default for BeatDetectorKind {
    fn default() -> Self {
        Self::ALL[0]
    }
}

impl fmt::Display for BeatDetectorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
