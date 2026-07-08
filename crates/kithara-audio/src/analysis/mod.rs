mod analyzer;
#[cfg(feature = "analysis-beat")]
pub(crate) mod beat;
mod run;
#[cfg(test)]
mod tests;
#[cfg(not(target_arch = "wasm32"))]
mod worker;

pub use analyzer::{AnalyzerBuilder, BeatAnalysisConfig, BeatResamplerBackend, TrackAnalysis};
pub use run::analyze_reader;
#[cfg(not(target_arch = "wasm32"))]
pub use worker::AnalysisWorker;
