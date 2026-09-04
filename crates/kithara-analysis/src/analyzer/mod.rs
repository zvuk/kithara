mod config;
mod extent;
mod session;
mod set;
#[cfg(feature = "analysis-waveform")]
mod waveform;

pub use config::BeatAnalysisConfig;
pub(crate) use extent::Extent;
pub(crate) use session::{Ingest, TrackAnalyzers};
pub use set::AnalyzerBuilder;
#[cfg(feature = "analysis-waveform")]
pub(crate) use waveform::WaveformPass;

#[cfg(feature = "analysis-beat")]
pub(crate) use crate::model::detector as default_beat_detector;
pub(crate) use crate::{AnalysisFingerprint, AnalysisToken, TrackAnalysis, slots::beat::Detector};
