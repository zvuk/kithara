//! Factory for creating decoders with runtime backend selection.
//!
//! Exactly one decoder path is taken per call — no fallback. The
//! caller selects the backend explicitly via
//! [`DecoderConfig::backend`] (a [`DecoderBackend`] variant). Selecting
//! a backend that is not compiled in for the current target/feature
//! combination returns
//! [`DecodeError::BackendUnavailable`](crate::DecodeError::BackendUnavailable);
//! a backend that doesn't accept the codec/container returns
//! [`DecodeError::UnsupportedCodec`](crate::DecodeError::UnsupportedCodec).
//! Callers must treat such failures as terminal.
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::{DecoderBackend, DecoderFactory, DecoderConfig};
//! use kithara_stream::{AudioCodec, MediaInfo};
//!
//! let file = std::fs::File::open("audio.mp3")?;
//! let media_info = MediaInfo { codec: Some(AudioCodec::Mp3), ..Default::default() };
//! let mut config = DecoderConfig::default();
//! config.backend = DecoderBackend::Symphonia;
//! let decoder = DecoderFactory::create_from_media_info(file, &media_info, &config)?;
//! ```

mod inner;
mod probe;

#[cfg(test)]
mod tests;

pub use inner::{DecoderBackend, DecoderConfig, DecoderFactory};
