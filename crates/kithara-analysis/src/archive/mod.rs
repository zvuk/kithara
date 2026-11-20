mod error;
mod file;
mod update;

#[cfg(all(
    test,
    feature = "analysis-beat",
    feature = "analysis-waveform",
    not(target_arch = "wasm32")
))]
mod resume_tests;
#[cfg(test)]
pub(crate) mod tests;

pub use error::AnalysisFileError;
pub use file::{AnalysisFile, AnalysisFileSpec};
pub use update::{AnalysisFilePatch, AnalysisFileUpdate, AnalysisFileWrite};
