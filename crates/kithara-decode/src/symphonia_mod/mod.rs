//! Symphonia integration for audio decoding.
//!
//! This module provides the core decoding functionality:
//! - `SymphoniaDecoder`: Main decoder struct with probe and direct creation
//! - `CachedCodecInfo`: Cached codec parameters for ABR switch optimization

mod decoder;

pub use decoder::{CachedCodecInfo, SymphoniaDecoder};
