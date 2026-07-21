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
mod exports;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod musical;
mod pipeline;
mod region;
pub(crate) mod renderer;
mod runtime;
mod source_range;
mod traits;
mod waveform;

pub use audio::Audio;
pub use blob::frame::BlobError;
pub use effects::{
    eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands},
    timestretch::StretchControls,
};
pub use exports::*;
pub use kithara_resampler::{
    NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality,
};
pub use musical::{BeatMapError, CoordinateError, SourceFrame, TrackBeat, TrackBeatMap};
pub use pipeline::{
    config::{AudioConfig, AudioDecoderConfig, DecoderResamplerSettings},
    fetch::{EpochValidator, Fetch},
};
pub use region::{ActiveRegion, RegionPlan, RegionPlanError};
pub use renderer::{
    AudioWorkerHandle, AudioWorkerSource, EngineLoad, EngineLoadSnapshot, PreloadGate, ServiceClass,
};
pub use source_range::{
    SourceFrameIndex, SourceRange, SourceRangeError, SourceRangeReadOutcome, SourceRangeRequest,
};
pub use traits::{
    AudioEffect, ChunkOutcome, DecodeError, DecodeResult, PcmControl, PcmRead, PcmReader,
    PcmSession, PendingReason, ReadOutcome, SeekOutcome,
};
pub use waveform::{AnalysisParams, BeatGrid, Bucket, GridSegment, bucket::Waveform};
