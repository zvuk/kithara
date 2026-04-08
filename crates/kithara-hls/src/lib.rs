#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns))]
#![allow(
    unreachable_pub,
    reason = "many helpers are `pub` so the `internal` feature can re-export them without widening the stable API surface"
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
mod coord;
mod ids;
mod inner;
pub(crate) mod loading;
mod parsing;
pub(crate) mod playlist;
pub(crate) mod scheduler;
mod source;
pub(crate) mod stream_index;

// Public API re-exports

pub use config::{HlsConfig, KeyContext, KeyOptions, KeyProcessor};
pub use context::HlsStreamContext;
pub use error::{HlsError, HlsResult};
pub use inner::Hls;
pub use kithara_abr::{AbrMode, AbrOptions};
