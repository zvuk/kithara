//! Progressive source-signal analysis and reusable analysis artifacts.

#![forbid(unsafe_code)]

mod analyzer;
mod archive;
mod artifact;
#[cfg(feature = "analysis-beat")]
pub(crate) mod beat;
mod blob;
mod model;
pub(crate) mod producer;
mod progress;
mod slots;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
mod worker;

pub use analyzer::{
    AnalysisDemand, AnalyzerBuilder, BeatAnalysisConfig, BeatAnalysisConfigPatch,
    BeatAnalysisConfigPatchError,
};
pub use archive::{
    AnalysisFile, AnalysisFileError, AnalysisFilePatch, AnalysisFileSpec, AnalysisFileUpdate,
    AnalysisFileWrite,
};
pub use artifact::{
    AnalysisFingerprint, AnalysisToken, BeatArtifact, BeatGridUnavailable, BeatSnapshot, BeatState,
    ORDINAL_TOLERANCE_BEATS, TrackAnalysis,
};
/// The served beat-grid contract, re-exported from its owner so a consumer of
/// a publication can name what [`TrackAnalysis::grid`] hands it. The types are
/// `kithara-beat`'s own: a server reading a stored grid needs no analyzer.
pub use kithara_beat::{
    BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid,
    SCHEMA_VERSION as GRID_SCHEMA_VERSION,
};
pub use kithara_blob::BlobError;
/// Frame-range coverage, re-exported from its owners so a consumer can name
/// what a publication reports without depending on the analyzer: the set it is
/// kept in, and the frame reading of it.
pub use kithara_signal::{FrameCoverage, FrameSpan};
#[cfg(feature = "analysis-waveform")]
pub use kithara_waveform::WaveformAnalyzer;
/// The waveform result and its tunables, re-exported from their owner: a
/// consumer that only reads a waveform needs no DSP.
pub use kithara_waveform::{AnalysisParams, Bucket, Waveform};
pub use producer::AnalysisProducer;
pub use progress::AnalysisProgress;
pub use rangemap::RangeSet;
pub use worker::{AnalysisOpen, AnalysisPass, AnalysisWorker, AnalysisWorkerConfig};
mod consts;
