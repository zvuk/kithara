mod analyzer;
#[cfg(test)]
mod fake;
mod run;
#[cfg(not(target_arch = "wasm32"))]
mod worker;

pub use analyzer::{
    AnalyzerFactory, AnalyzerRegistry, TrackAnalysis, TrackAnalyzer, waveform_analyzer,
};
pub use run::analyze_reader;
#[cfg(not(target_arch = "wasm32"))]
pub use worker::AnalysisWorker;
