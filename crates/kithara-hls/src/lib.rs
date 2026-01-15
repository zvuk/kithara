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
//! use kithara_hls::{Hls, HlsOptions};
//!
//! let session = Hls::open(url, HlsOptions::default()).await?;
//! let source = session.source();
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
mod session;
mod source;
mod source_adapter;

// ============================================================================
// Public API re-exports
// ============================================================================

pub use abr::{AbrDecision, AbrReason, ThroughputSample, Variant};
pub use error::{HlsError, HlsResult};
pub use events::HlsEvent;
pub use options::{
    AbrMode, AbrOptions, CacheOptions, HlsOptions, KeyContext, KeyOptions, KeyProcessor,
    NetworkOptions, VariantSelector,
};
pub use session::HlsSession;
pub use source::Hls;
pub use source_adapter::HlsSource;
