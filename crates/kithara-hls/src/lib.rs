#![forbid(unsafe_code)]

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
//! use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
//! use kithara_hls::{Hls, HlsParams};
//!
//! // Async source with events
//! let source = StreamSource::<Hls>::open(url, HlsParams::default()).await?;
//! let events = source.events();  // Receiver<HlsEvent>
//!
//! // Sync reader for decoders (Read + Seek)
//! let reader = SyncReader::<StreamSource<Hls>>::open(
//!     url,
//!     HlsParams::default(),
//!     SyncReaderParams::default()
//! ).await?;
//! ```

// Public modules
pub mod error;
pub mod events;
pub mod options;
pub mod worker;

// Internal modules (exposed for testing, use with caution)
#[doc(hidden)]
pub mod cache;
#[doc(hidden)]
pub mod fetch;
#[doc(hidden)]
pub mod keys;
#[doc(hidden)]
pub mod playlist;

// Private modules
mod media_source;
mod parsing;
mod source;

// ============================================================================
// Public API re-exports
// ============================================================================

// Re-export ABR types from kithara-abr
pub use cache::EncryptionInfo;
pub use error::{HlsError, HlsResult};
pub use events::HlsEvent;
pub use kithara_abr::{
    AbrDecision, AbrMode, AbrOptions, AbrReason, ThroughputSample, Variant, VariantInfo,
};
pub use media_source::HlsMediaSource;
pub use options::{HlsParams, KeyContext, KeyOptions, KeyProcessor};
pub use parsing::ContainerFormat;
pub use source::Hls;
