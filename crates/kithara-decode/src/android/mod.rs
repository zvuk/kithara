#[cfg(target_os = "android")]
// Runtime Android pieces stay behind one module boundary so the rest of the
// crate does not need scattered `target_os = "android"` checks.
mod codec;
#[cfg(target_os = "android")]
mod config;
#[cfg(target_os = "android")]
mod decoder;
#[cfg(target_os = "android")]
mod error;
#[cfg(target_os = "android")]
mod extractor;
mod ffi;
mod format;
#[cfg(target_os = "android")]
mod jni;
#[cfg(target_os = "android")]
mod media_format;
#[cfg(target_os = "android")]
mod source;

#[cfg(target_os = "android")]
pub(crate) use config::AndroidConfig;
#[cfg(target_os = "android")]
pub(crate) use decoder::try_create_android_decoder;
#[cfg(target_os = "android")]
pub(crate) use format::{can_seek_container, default_container_for_codec, supports_codec};
#[cfg(target_os = "android")]
pub(crate) use jni::ensure_current_thread_attached;
