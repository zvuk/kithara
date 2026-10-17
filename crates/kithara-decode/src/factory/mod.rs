//! Factory for creating decoders with runtime backend selection.
//!
//! Exactly one decoder path is taken per call — no fallback. The caller selects the
//! backend via [`DecoderConfig::backend`]; a backend not compiled in returns
//! `DecodeError::BackendUnavailable`, one that rejects the codec/container returns
//! `DecodeError::UnsupportedCodec`, both terminal.

mod backend;
mod build;
mod config;
mod inner;
#[cfg(any(android_backend, apple_backend, feature = "symphonia"))]
mod mpeg;
mod probe;
mod segment;
#[cfg(feature = "symphonia")]
mod software;
pub use config::{DecoderConfig, DecoderResamplerConfig};
pub use inner::{DecoderBackend, DecoderFactory};
#[cfg(apple_backend)]
pub(crate) use probe::wav_data_range;
#[cfg(feature = "symphonia")]
pub(crate) use probe::{skip_id3_tags, sniff_container_from_source, sniff_wav_format};
