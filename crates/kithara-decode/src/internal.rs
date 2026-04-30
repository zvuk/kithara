#![forbid(unsafe_code)]

pub use crate::{
    error::{DecodeError, DecodeResult},
    factory::{DecoderConfig, DecoderFactory},
    traits::InnerDecoder,
    types::{DecoderTrackInfo, PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};
