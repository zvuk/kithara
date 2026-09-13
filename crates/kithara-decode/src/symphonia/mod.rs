//! Format-reader adapters shared by software decoding and Android MPEG audio.
//! Android uses the in-tree MPEG demuxer with `MediaCodec`; codec registration
//! and general container probing require the `symphonia` software backend.

#[cfg(feature = "fdk-aac")]
pub(crate) mod aac_fdk;
pub(crate) mod adapter;
#[cfg(feature = "symphonia")]
pub(crate) mod codec;
#[cfg(feature = "symphonia")]
pub(crate) mod config;
pub(crate) mod demuxer;
pub(crate) mod echain;
#[cfg(all(test, feature = "symphonia"))]
mod mp4_tests;
#[cfg(feature = "symphonia")]
pub(crate) mod probe;
#[cfg(feature = "symphonia")]
pub(crate) mod registry;
#[cfg(all(test, feature = "symphonia"))]
mod tests;

#[cfg(feature = "symphonia")]
pub(crate) use codec::SymphoniaCodec;
#[cfg(feature = "symphonia")]
pub(crate) use config::SymphoniaConfig;
#[cfg(feature = "symphonia")]
pub(crate) use demuxer::FileOpen;
pub(crate) use demuxer::SymphoniaDemuxer;

mod packets;
