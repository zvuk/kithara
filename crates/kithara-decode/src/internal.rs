#![forbid(unsafe_code)]

pub use crate::{
    error::{DecodeError, DecodeResult},
    factory::{DecoderConfig, DecoderFactory},
    traits::InnerDecoder,
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};
