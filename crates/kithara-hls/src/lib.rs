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
pub mod abr;
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
mod adapter;
mod parsing;
mod source;

// ============================================================================
// Public API re-exports
// ============================================================================

pub use abr::{AbrDecision, AbrReason, ThroughputSample, Variant};
pub use adapter::HlsSource;
pub use cache::EncryptionInfo;
pub use error::{HlsError, HlsResult};
pub use events::HlsEvent;
pub use options::{
    AbrMode, AbrOptions, HlsParams, KeyContext, KeyOptions, KeyProcessor, VariantSelector,
};
pub use source::Hls;
