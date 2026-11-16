mod beat;
mod fingerprint;
mod grid;
mod meter;
mod snapshot;
mod track;

pub use beat::BeatArtifact;
#[cfg(any(test, feature = "analysis-beat"))]
pub(crate) use beat::FitRegion;
#[cfg(feature = "analysis-beat")]
pub(crate) use beat::MarkedBeat;
pub use fingerprint::AnalysisFingerprint;
pub use grid::{BeatGridUnavailable, ORDINAL_TOLERANCE_BEATS};
#[cfg(feature = "analysis-beat")]
pub(crate) use meter::voted_bar;
pub use snapshot::{BeatSnapshot, BeatState};
pub use track::{AnalysisToken, TrackAnalysis};
