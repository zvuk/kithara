//! Android `MediaCodec` decoding with segment-aware fMP4 and exact MPEG readers.
//! Other standalone containers retain the native extractor's track format.
//! Unsupported formats fail without selecting another decoder backend.

pub(crate) mod aformat;
pub(crate) mod codec;
pub(crate) mod error;
pub(crate) mod ffi;
mod jni;
pub(crate) mod media_codec;
pub(crate) mod media_extractor;
pub(crate) mod media_extractor_demuxer;

pub(crate) use codec::AndroidCodec;
pub(crate) use jni::ensure_current_thread_attached;
pub(crate) use media_extractor_demuxer::AndroidMediaExtractorDemuxer;
