mod config;
mod nn;
mod session;
mod set;
#[cfg(feature = "analysis-waveform")]
mod waveform;

pub use config::{BeatAnalysisConfig, BeatAnalysisConfigPatch};
#[cfg(feature = "analysis-beat")]
pub(crate) use nn::detector as default_beat_detector;
pub(crate) use session::{Ingest, TrackAnalyzers};
pub use set::AnalyzerBuilder;
#[cfg(feature = "analysis-waveform")]
pub(crate) use waveform::WaveformPass;

pub(crate) use crate::{AnalysisFingerprint, AnalysisToken, TrackAnalysis, slots::beat::Detector};
