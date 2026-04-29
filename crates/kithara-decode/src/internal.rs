#![forbid(unsafe_code)]

pub use crate::{
    error::{DecodeError, DecodeResult},
    factory::{DecoderConfig, DecoderFactory},
    traits::Decoder,
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};
