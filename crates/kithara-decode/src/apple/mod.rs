//! Apple `AudioToolbox` decoder backend.
//!
//! Opens an `AudioFile` through `AudioFileOpenWithCallbacks`, reads
//! compressed packets on demand, and decodes them to PCM through
//! `AudioConverter`. Unlike the previous `AudioFileStream` path this
//! supports atom-structured containers (fMP4, MP4, CAF, WAV) — the
//! capability required for HLS seek.
//!
//! `AppleDecoder` (in `decoder.rs`) implements both [`crate::traits::Decoder`]
//! (runtime) and [`crate::backend::Backend`] (capability + factory). The
//! `Backend` impl is in `backend.rs` for file-level cohesion.

mod audiofile;
mod backend;
mod config;
mod consts;
mod converter;
mod decoder;
mod ffi;
mod fmp4;
mod reader;

pub(crate) use self::{config::AppleConfig, decoder::AppleDecoder};
