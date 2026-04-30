//! Android `MediaCodec` decoder backend.
//!
//! The whole `android` module is gated by
//! `#[cfg(all(feature = "android", target_os = "android"))]` in `lib.rs`,
//! so no internal `cfg` attributes are needed here.
//!
//! `AndroidDecoder` (in `decoder.rs`) implements both
//! [`crate::traits::Decoder`] (runtime) and [`crate::backend::Backend`]
//! (capability + factory). The `Backend` impl is in `backend.rs` for
//! file-level cohesion.

pub(crate) mod aformat;
mod backend;
pub(crate) mod codec;
mod config;
mod decoder;
pub(crate) mod error;
mod extractor;
pub(crate) mod ffi;
mod format;
mod jni;
mod source;

pub(crate) use config::AndroidConfig;
pub(crate) use decoder::AndroidDecoder;
pub(crate) use format::{can_seek_container, default_container_for_codec, supports_codec};
pub(crate) use jni::ensure_current_thread_attached;
