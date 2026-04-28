//! Factory for creating decoders with runtime codec selection.
//!
//! Exactly one decoder path is taken per call — no fallback. The factory
//! picks either the compiled hardware backend (when `prefer_hardware` is
//! set and the backend accepts the codec/container) or Symphonia (when
//! the `symphonia` feature is enabled). If neither path is available
//! the call fails with a classified `DecodeError` — callers must treat
//! such failures as terminal.
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::{DecoderFactory, DecoderConfig};
//! use kithara_stream::{AudioCodec, MediaInfo};
//!
//! let file = std::fs::File::open("audio.mp3")?;
//! let media_info = MediaInfo { codec: Some(AudioCodec::Mp3), ..Default::default() };
//! let decoder = DecoderFactory::create_from_media_info(
//!     file,
//!     &media_info,
//!     &DecoderConfig::default(),
//! )?;
//! ```

mod hardware;
mod inner;
mod probe;

#[cfg(test)]
mod tests;

pub use inner::{DecoderConfig, DecoderFactory};
