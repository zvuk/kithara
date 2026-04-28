//! Apple `AudioToolbox` decoder backend.
//!
//! Opens an `AudioFile` through `AudioFileOpenWithCallbacks`, reads
//! compressed packets on demand, and decodes them to PCM through
//! `AudioConverter`. Unlike the previous `AudioFileStream` path this
//! supports atom-structured containers (fMP4, MP4, CAF, WAV) — the
//! capability required for HLS seek.
//!
//! The backend dispatches on the runtime `AudioCodec` enum in
//! `try_create_apple_decoder`; no compile-time codec markers are used.

mod audiofile;
mod backend;
mod config;
mod consts;
mod converter;
mod decoder;
mod ffi;
mod fmp4;
mod inner;
mod reader;

pub(crate) use self::{
    backend::AppleBackend, config::AppleConfig, decoder::try_create_apple_decoder,
};
