//! Factory for creating decoders with runtime backend selection.
//!
//! Exactly one decoder path is taken per call — no fallback. The caller
//! selects the backend via [`DecoderConfig::backend`]; a backend not
//! compiled in returns `DecodeError::BackendUnavailable`, one that rejects
//! the codec/container returns `DecodeError::UnsupportedCodec`, both
//! terminal. See README "Decoder recreate strategy" and "Initialization Paths".

mod inner;
mod probe;

pub use inner::{DecoderBackend, DecoderConfig, DecoderFactory};
