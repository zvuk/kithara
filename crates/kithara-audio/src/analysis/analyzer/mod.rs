mod nn;
mod set;
mod track_analysis;
#[cfg(not(feature = "analysis-waveform"))]
mod waveform_pass_disabled;
#[cfg(feature = "analysis-waveform")]
mod waveform_pass_enabled;

pub(crate) use set::TrackAnalyzers;
pub use set::{AnalyzerBuilder, beat_cache_tag};
pub(crate) use track_analysis::Analyzer;
pub use track_analysis::TrackAnalysis;
#[cfg(not(feature = "analysis-waveform"))]
pub(crate) use waveform_pass_disabled::WaveformPass;
#[cfg(feature = "analysis-waveform")]
pub(crate) use waveform_pass_enabled::WaveformPass;
