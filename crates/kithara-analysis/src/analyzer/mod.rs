mod config;
mod demand;
mod extent;
mod session;
mod set;

pub use config::{BeatAnalysisConfig, BeatAnalysisConfigPatch, BeatAnalysisConfigPatchError};
pub use demand::AnalysisDemand;
pub(crate) use extent::Extent;
pub(crate) use session::{Ingest, TrackAnalyzers};
pub use set::AnalyzerBuilder;

#[cfg(feature = "analysis-beat")]
pub(crate) use crate::model::detector as default_beat_detector;
pub(crate) use crate::{AnalysisFingerprint, AnalysisToken, TrackAnalysis, slots::beat::Detector};
