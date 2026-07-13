//! Audio pipeline library with decoding, effects, and resampling.
//!
//! - [`Audio`] — generic audio pipeline running in a separate thread
//! - [`AudioConfig`] — pipeline configuration
//! - [`ResamplerQuality`] - sample rate conversion quality
//! - `Audio` implements [`PcmReader`] for pull-based PCM consumers
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
pub(crate) mod renderer;
mod runtime;
mod traits;
mod waveform;

pub use audio::Audio;
pub use blob::frame::BlobError;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use effects::timestretch::{StretchKind, TimeStretchProcessor};
pub use effects::{
    eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands},
    timestretch::StretchControls,
};
#[cfg(feature = "resample-glide")]
pub use kithara_resampler::glide::{GlideBackend, GlideConfig, GlideInterpolation};
#[cfg(feature = "resample-rubato")]
pub use kithara_resampler::rubato::{RubatoAlgorithm, RubatoBackend, RubatoConfig};
pub use kithara_resampler::{
    NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality,
};
pub use pipeline::{
    config::{AudioConfig, AudioDecoderConfig, DecoderResamplerSettings},
    fetch::{EpochValidator, Fetch, FetchKind},
};
pub use region::{ActiveRegion, RegionPlan, RegionPlanError};
pub use renderer::{
    AudioWorkerHandle, AudioWorkerSource, EngineLoad, EngineLoadSnapshot, PreloadGate, ServiceClass,
};
pub use traits::{
    AudioEffect, ChunkOutcome, DecodeError, DecodeResult, PcmControl, PcmRead, PcmReader,
    PcmSession, PendingReason, ReadOutcome, SeekOutcome,
};
#[cfg(feature = "analysis-waveform")]
pub use waveform::WaveformAnalyzer;
pub use waveform::{AnalysisParams, BeatGrid, Bucket, GridSegment, bucket::Waveform};
