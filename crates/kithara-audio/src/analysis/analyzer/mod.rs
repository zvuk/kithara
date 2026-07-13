mod config;
mod nn;
mod set;
mod track;
#[cfg(feature = "analysis-waveform")]
mod waveform;

pub use config::BeatAnalysisConfig;
#[cfg(feature = "analysis-beat")]
pub(crate) use nn::detector as default_beat_detector;
pub use set::AnalyzerBuilder;
pub(crate) use set::TrackAnalyzers;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) use track::Analyzer;
pub use track::TrackAnalysis;
#[cfg(feature = "analysis-waveform")]
pub(crate) use waveform::WaveformPass;
