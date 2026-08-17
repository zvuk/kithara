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
mod traits;
mod waveform;

pub use audio::{Audio, SeekHandle};
pub use blob::frame::BlobError;
pub use effects::{
    eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands},
    limiter::{LimiterError, PeakLimiter},
    timestretch::StretchControls,
};
pub use exports::*;
pub use kithara_resampler::{
    NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality,
};
pub use musical::{
    BeatMapError, CoordinateError, SessionAnchor, SessionBeat, SessionFrame, SourceFrame,
    TrackBeat, TrackBeatMap,
};
pub use pipeline::{
    config::{AudioConfig, AudioDecoderConfig, ConsumerWakeMode, DecoderResamplerSettings},
    fetch::{EpochValidator, Fetch},
};
pub use region::{ActiveRegion, RegionPlan, RegionPlanError};
pub use renderer::{
    AudioWorkerHandle, AudioWorkerSource, EngineLoad, EngineLoadSnapshot, PreloadGate, ServiceClass,
};
pub use traits::{
    AudioBlockMut, AudioEffect, ChunkOutcome, DecodeError, DecodeResult, PcmControl, PcmRead,
    PcmReader, PcmSession, PendingReason, PresentationAdvance, PresentationCursor,
    PresentationPoint, ReadOutcome, SeekBegin, SeekOutcome,
};
pub use waveform::{AnalysisParams, BeatGrid, Bucket, GridSegment, bucket::Waveform};
