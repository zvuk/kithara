#[cfg(not(feature = "analysis-beat"))]
#[path = "nn_disabled.rs"]
mod nn;
#[cfg(feature = "analysis-beat")]
#[path = "nn_enabled.rs"]
mod nn;
#[cfg(not(feature = "analysis-beat"))]
#[path = "set_disabled.rs"]
mod set;
#[cfg(feature = "analysis-beat")]
#[path = "set_enabled.rs"]
mod set;
mod track_analysis;
#[cfg(not(feature = "analysis-waveform"))]
mod waveform_pass_disabled;
#[cfg(feature = "analysis-waveform")]
mod waveform_pass_enabled;

pub(crate) use set::TrackAnalyzers;
pub use set::{AnalyzerBuilder, beat_cache_tag};
#[cfg(feature = "analysis-beat")]
pub(crate) use track_analysis::Analyzer;
pub use track_analysis::TrackAnalysis;
#[cfg(not(feature = "analysis-waveform"))]
pub(crate) use waveform_pass_disabled::WaveformPass;
#[cfg(feature = "analysis-waveform")]
pub(crate) use waveform_pass_enabled::WaveformPass;
