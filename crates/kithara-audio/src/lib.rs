//! Audio pipeline library with decoding, effects, and resampling.
//!
//! - [`Audio`] — generic audio pipeline running in a separate thread
//! - [`AudioConfig`] — pipeline configuration
//! - [`ResamplerQuality`] — sample rate conversion quality
//! - `Audio` also implements `rodio::Source` directly (requires `rodio` feature)
//!
//! See the crate `README.md` for usage, threading model, and architecture.

#![forbid(unsafe_code)]

mod audio;
pub mod effects;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod pipeline;
mod resampler;
mod runtime;
mod traits;
pub(crate) mod worker;

pub use audio::Audio;
pub use effects::eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands};
pub use pipeline::{
    config::AudioConfig,
    fetch::{EpochValidator, Fetch},
    track_fsm::TrackPhaseTag,
};
pub use resampler::{ResamplerParams, ResamplerProcessor, ResamplerQuality};
pub use traits::{
    AudioEffect, ChunkOutcome, DecodeError, DecodeResult, PcmReader, PendingReason, ReadOutcome,
    SeekOutcome,
};
pub use worker::{AudioWorkerSource, handle::AudioWorkerHandle, types::ServiceClass};
