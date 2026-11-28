//! Progressive source-signal analysis and reusable analysis artifacts.

#![forbid(unsafe_code)]

#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
mod analyzer;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
mod archive;
mod artifact;
#[cfg(feature = "analysis-beat")]
pub(crate) mod beat;
mod blob;
mod model;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) mod producer;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
mod progress;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
mod slots;
#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    any(feature = "analysis-beat", feature = "analysis-waveform")
))]
mod tests;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
mod worker;

pub use artifact::{
    AnalysisFingerprint, AnalysisToken, BeatArtifact, BeatGridUnavailable, BeatSnapshot, BeatState,
    ORDINAL_TOLERANCE_BEATS, TrackAnalysis,
};
pub use kithara_blob::BlobError;
/// The key result, re-exported from its owner so a consumer can name what
/// [`TrackAnalysis::key`] hands it.
pub use kithara_key::{KeyEstimate, Mode, MusicalKey, PitchClass};
/// Frame-range coverage, re-exported from its owners so a consumer can name
/// what a publication reports without depending on the analyzer: the set it is
/// kept in, and the frame reading of it.
pub use kithara_signal::{FrameCoverage, FrameSpan};
#[cfg(feature = "analysis-waveform")]
pub use kithara_waveform::WaveformAnalyzer;
/// The waveform result and its tunables, re-exported from their owner: a
/// consumer that only reads a waveform needs no DSP.
pub use kithara_waveform::{AnalysisParams, Band, Bucket, MAX_BUCKETS, Waveform};
pub use rangemap::RangeSet;
#[cfg(any(feature = "analysis-beat", feature = "analysis-waveform"))]
pub use {
    analyzer::{
        AnalysisDemand, AnalyzerBuilder, BeatAnalysisConfig, BeatAnalysisConfigPatch,
        BeatAnalysisConfigPatchError, PassError,
    },
    archive::{
        AnalysisFile, AnalysisFileError, AnalysisFilePatch, AnalysisFileSpec, AnalysisFileUpdate,
        AnalysisFileWrite,
    },
    producer::AnalysisProducer,
    progress::AnalysisProgress,
    worker::{
        AnalysisCompletion, AnalysisError, AnalysisOpen, AnalysisOutcome, AnalysisPass,
        AnalysisWorker, AnalysisWorkerConfig,
    },
};
/// The served beat-grid contract, re-exported from its owner so a consumer of
/// a publication can name what [`TrackAnalysis::grid`] hands it. The types are
/// `kithara-beat`'s own: a server reading a stored grid needs no analyzer.
pub use {
    kithara_beat::{
        BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, RawBeatGrid,
        SCHEMA_VERSION as GRID_SCHEMA_VERSION,
    },
    kithara_sync::Meter,
};
mod consts;
