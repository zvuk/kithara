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
//! use kithara_stream::Stream;
//! use kithara_hls::{Hls, HlsParams};
//! use kithara_assets::StoreOptions;
//!
//! let params = HlsParams::new(StoreOptions::new("/tmp/cache"));
//! let stream = Stream::<Hls>::open(url, params).await?;
//! let events = stream.events();  // Receiver<HlsEvent>
//! ```

// Public modules
pub mod abr;
pub mod error;
pub mod events;
pub mod options;

// Internal modules (exposed for testing, use with caution)
#[doc(hidden)]
pub mod fetch;
#[doc(hidden)]
pub mod keys;
#[doc(hidden)]
pub mod playlist;
#[doc(hidden)]
pub mod stream;

// Private modules
mod adapter;
mod index;
mod parsing;
mod source;

// ============================================================================
// Public API re-exports
// ============================================================================

pub use abr::{AbrDecision, AbrReason, ThroughputSample, Variant};
pub use error::{HlsError, HlsResult};
pub use events::HlsEvent;
pub use options::{
    AbrMode, AbrOptions, HlsParams, KeyContext, KeyOptions, KeyProcessor, VariantSelector,
};
pub use adapter::HlsSource;
pub use source::Hls;
