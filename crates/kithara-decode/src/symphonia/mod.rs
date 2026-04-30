//! Symphonia probe + reader bootstrap helpers.
//!
//! The legacy whole-stream `SymphoniaDecoder` god-type was removed in
//! the decoder unification. The remaining surface is the format-reader
//! bootstrap shared with [`crate::demuxer::SymphoniaDemuxer`]:
//! [`adapter::ReadSeekAdapter`] (the `Read+Seek -> MediaSource` shim),
//! [`probe::new_direct`] / [`probe::probe_with_seek`] (reader
//! construction), and [`echain`] (error-chain helpers used by the
//! adapter).

pub(crate) mod adapter;
pub(crate) mod config;
pub(crate) mod echain;
pub(crate) mod probe;

#[cfg(test)]
mod tests;
