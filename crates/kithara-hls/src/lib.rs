#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns))]
#![allow(
    dead_code,
    reason = "internal HLS orchestration types are consumed via integration tests and `internal` feature"
)]
#![allow(
    unreachable_pub,
    reason = "public visibility is used for `internal` re-exports without widening stable API"
)]
#![allow(
    unused_imports,
    reason = "imports differ between feature combinations and test wiring"
)]
#![allow(
    unfulfilled_lint_expectations,
    reason = "some expect attributes only trigger under specific feature sets"
)]

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

#[cfg(feature = "internal")]
pub mod internal;

mod context;
pub(crate) mod download_state;
mod downloader;
mod fetch;
mod inner;
mod keys;
mod parsing;
pub(crate) mod playlist;
mod source;

// Public API re-exports

pub use config::{HlsConfig, KeyContext, KeyOptions, KeyProcessor};
pub use context::HlsStreamContext;
pub use error::{HlsError, HlsResult};
pub use inner::Hls;
pub use kithara_abr::{AbrMode, AbrOptions};
