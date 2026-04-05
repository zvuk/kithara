//! FFmpeg-backed audio encoders.

pub(crate) mod aac;
pub(crate) mod bytes;
mod core;
pub(crate) mod flac;
pub(crate) mod pcm;

pub(crate) use self::core::{FfmpegEncoder, build_direct_filter, ensure_ffmpeg_initialized};
