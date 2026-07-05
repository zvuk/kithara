//! Audio pipeline library with decoding, effects, and resampling.
//!
//! - [`Audio`] — generic audio pipeline running in a separate thread
//! - [`AudioConfig`] — pipeline configuration
//! - [`ResamplerQuality`] — sample rate conversion quality
//! - `Audio` also implements `rodio::Source` directly (requires `rodio` feature)
//!
//! See the crate `README.md` for usage and `CONTEXT.md` for threading model and architecture.

#![forbid(unsafe_code)]
#![cfg_attr(rtsan, feature(sanitize))]

pub mod analysis;
mod audio;
mod blob;
pub mod effects;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod pipeline;
mod region;
mod resampler;
mod runtime;
mod traits;
mod waveform;
pub(crate) mod worker;

pub use audio::Audio;
pub use blob::BlobError;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use effects::timestretch::{StretchBackendKind, TimeStretchProcessor};
pub use effects::{
    eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands},
    timestretch::StretchControls,
};
pub use pipeline::{
    config::AudioConfig,
    fetch::{EpochValidator, Fetch},
};
pub use region::{ActiveRegion, RegionPlan, RegionPlanError};
pub use resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality};
pub use traits::{
    AudioEffect, ChunkOutcome, DecodeError, DecodeResult, PcmReader, PendingReason, ReadOutcome,
    SeekOutcome,
};
pub use waveform::{AnalysisParams, BeatGrid, Bucket, GridSegment, Waveform, WaveformAnalyzer};
pub use worker::{
    AudioWorkerSource, EngineLoad, EngineLoadSnapshot, PreloadGate, handle::AudioWorkerHandle,
    types::ServiceClass,
};
