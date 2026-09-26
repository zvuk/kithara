//! Factory for creating decoders with runtime backend selection.
//!
//! Exactly one decoder path is taken per call — no fallback. The caller selects the
//! backend via [`DecoderConfig::backend`]; a backend not compiled in returns
//! `DecodeError::BackendUnavailable`, one that rejects the codec/container returns
//! `DecodeError::UnsupportedCodec`, both terminal.

#[cfg(all(test, apple_backend, feature = "symphonia"))]
mod apple_mp3_tests;
mod inner;
mod probe;

pub use inner::{DecoderBackend, DecoderConfig, DecoderFactory, DecoderResamplerConfig};
