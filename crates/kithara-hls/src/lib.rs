// `deny` (not `forbid`) so the `probes` module can locally
// `#[allow(unsafe_code)]` for the inline asm emitted by `usdt::provider!`
// when the `usdt-probes` feature is enabled. Production code outside
// that module remains unsafe-free.
#![deny(unsafe_code)]

//! HLS (HTTP Live Streaming) VOD implementation.
//!
//! # Overview
//!
//! This crate provides transport and caching for HLS VOD streams.
//! It handles playlist parsing, segment fetching, ABR (Adaptive Bitrate),
//! and encryption key management.
//!
//! # Example
//!
//! ```ignore
//! use kithara_stream::Stream;
//! use kithara_hls::{Hls, HlsConfig};
//!
//! let config = HlsConfig::new(url);
//! let stream = Stream::<Hls>::new(config).await?;
//! ```

// Public modules
pub mod config;
pub mod error;

mod context;
mod coord;
mod ids;
mod loading;
mod parsing;
mod peer;
mod playlist;
mod scheduler;
mod source;
mod stream;
mod stream_index;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

// Public API re-exports

pub use config::{HlsConfig, KeyOptions};
pub use context::HlsStreamContext;
pub use coord::{HlsCoord, SegmentRequest};
pub use error::{HlsError, HlsResult};
pub use ids::VariantIndex;
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};
pub use loading::{KeyManager, PlaylistCache, SegmentLoader};
pub use parsing::{
    MasterPlaylist, MediaPlaylist, VariantId, VariantStream, parse_master_playlist,
    parse_media_playlist, variant_info_from_master,
};
pub use playlist::{PlaylistState, SegmentState, VariantSizeMap, VariantState};
pub use scheduler::HlsScheduler;
pub use source::HlsSource;
pub use stream::Hls;
pub use stream_index::{SegmentData, StreamIndex};
