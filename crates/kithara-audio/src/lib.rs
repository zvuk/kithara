#![forbid(unsafe_code)]
#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

//! Audio pipeline library with decoding and resampling.
//!
//! - [`Audio`] - decoded-audio reader prepared for an external playback scheduler
//! - [`AudioConfig`] - pipeline configuration
//! - [`ResamplerQuality`] - sample rate conversion quality
//! - `Audio` implements [`AudioReader`] for pull-based audio consumers

mod audio;
mod event;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod pipeline;
mod producer;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod traits;

pub use audio::{Audio, event::map_decode_error_kind};
pub use event::{
    AudioEvent, DecodeErrorClass, DecodeErrorKind, DecoderBackend, DecoderChangeCause,
    DecoderEvent, FrameDomain, PlaybackResamplerKind, ResamplerKind, SeekLifecycleStage,
    SegmentLocation, TrackFailureKind,
};
#[cfg(feature = "resample-glide")]
pub use kithara_resampler::glide::{GlideBackend, GlideConfig};
#[cfg(feature = "resample-rubato")]
pub use kithara_resampler::rubato::{RubatoAlgorithm, RubatoBackend, RubatoConfig};
pub use kithara_resampler::{
    NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality,
};
pub use kithara_signal::SourceSpan;
pub use pipeline::{
    config::{
        AudioConfig, AudioConfigPatch, AudioDecoderConfig, AudioDecoderConfigPatch,
        DecoderResamplerSettings,
    },
    fetch::{Fetch, SourceEnd},
    track::{TrackStep, WaitingReason},
};
#[doc(hidden)]
pub use producer::AudioLaneEvent;
pub use traits::{
    AudioControl, AudioObserveError, AudioObserver, AudioObserverRelay, AudioObserverSlot,
    AudioRead, AudioReadError, AudioReader, AudioSession, AudioSource, ChunkOutcome, DecodeError,
    DecodeResult, FailureSource, PendingReason, ReadOutcome, SeekOutcome, SourceDiscontinuity,
};
mod consts;
