//! Android `MediaCodec` decoder backend.
//!
//! The whole `android` module is gated by
//! `#[cfg(all(feature = "android", target_os = "android"))]` in `lib.rs`,
//! so no internal `cfg` attributes are needed here.

mod backend;
mod codec;
mod config;
mod decoder;
mod error;
mod extractor;
mod ffi;
mod format;
mod jni;
mod media_format;
mod source;

pub(crate) use backend::AndroidBackend;
pub(crate) use config::AndroidConfig;
pub(crate) use decoder::try_create_android_decoder;
pub(crate) use format::{can_seek_container, default_container_for_codec, supports_codec};
pub(crate) use jni::ensure_current_thread_attached;
