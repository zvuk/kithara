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
//! use kithara_hls::{Hls, HlsConfig};
//!
//! let config = HlsConfig::new(url);
//! let stream = Stream::<Hls>::new(config).await?;
//! ```

// Public modules
pub mod config;
pub mod error;
pub mod events;

// Internal modules (exposed for testing, use with caution)
#[doc(hidden)]
pub mod fetch;
#[doc(hidden)]
pub mod keys;
#[doc(hidden)]
pub mod playlist;

// Private modules
mod inner;
mod parsing;
mod source;

// ============================================================================
// Public API re-exports
// ============================================================================

pub use config::{HlsConfig, KeyContext, KeyOptions, KeyProcessor};
pub use error::{HlsError, HlsResult};
pub use events::HlsEvent;
pub use fetch::EncryptionInfo;
pub use inner::Hls;
pub use kithara_abr::{
    AbrDecision, AbrMode, AbrOptions, AbrReason, ThroughputSample, Variant, VariantInfo,
};
pub use kithara_stream::ContainerFormat;
