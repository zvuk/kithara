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
mod runtime;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod traits;

pub use audio::{Audio, PreparedAudio, ScheduledSeekActivator, SeekHandle};
pub use event::{
    AudioEvent, DecodeErrorClass, DecodeErrorKind, DecoderBackend, DecoderChangeCause,
    DecoderEvent, FrameDomain, GaplessSpan, PlaybackResamplerKind, ResamplerKind,
    SeekLifecycleStage, SegmentLocation, TrackFailureKind,
};
#[cfg(feature = "resample-glide")]
pub use kithara_resampler::glide::{GlideBackend, GlideConfig, GlideInterpolation};
#[cfg(feature = "resample-rubato")]
pub use kithara_resampler::rubato::{RubatoAlgorithm, RubatoBackend, RubatoConfig};
pub use kithara_resampler::{
    NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality,
};
pub use pipeline::{
    config::{
        AudioConfig, AudioConfigPatch, AudioDecoderConfig, AudioDecoderConfigPatch,
        ConsumerWakeMode, DecoderResamplerSettings,
    },
    fetch::{EpochValidator, Fetch, SourceEnd, SourceSpan},
    track::{TrackStep, WaitingReason},
};
pub use producer::PreloadGate;
#[doc(hidden)]
pub use producer::{AudioLaneEvent, PreparedAudioLane, ProducerPort};
pub use traits::{
    AudioControl, AudioObserveError, AudioObserver, AudioObserverRelay, AudioObserverSlot,
    AudioRead, AudioReader, AudioSession, AudioSource, ChunkOutcome, DecodeError, DecodeResult,
    PendingReason, ReadOutcome, RevisionFloorStatus, ScheduledSeek, ScheduledSeekPreparation,
    SeekBegin, SeekOutcome, SeekPresentation, SourceDiscontinuity,
};
