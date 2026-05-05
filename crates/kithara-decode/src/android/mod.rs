//! Android `MediaCodec` codec surface.
//!
//! `AndroidCodec` (`FrameCodec` impl over `AMediaCodec`) consumes already-demuxed
//! frames. The lower-level `media_codec` module wraps the raw NDK `AMediaCodec`
//! handle (queue/dequeue lifecycle, output format negotiation); `codec` is the
//! public FrameCodec face.

pub(crate) mod aformat;
pub(crate) mod codec;
pub(crate) mod error;
pub(crate) mod ffi;
mod jni;
pub(crate) mod media_codec;

pub(crate) use codec::AndroidCodec;
pub(crate) use jni::ensure_current_thread_attached;
