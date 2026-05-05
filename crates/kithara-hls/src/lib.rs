// `deny` (not `forbid`) so the `probes` module can locally
// `#[allow(unsafe_code)]` for the inline asm emitted by `usdt::provider!`
// when the `usdt-probes` feature is enabled. Production code outside
// that module remains unsafe-free.
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns))]
// Without the `internal` feature many helpers are `pub` solely so the feature
// can re-export them without widening the stable API surface. clippy then sees
// them as `unreachable_pub`. With the feature on the items are genuinely
// reachable and the lint does not fire — no allow is needed in that case.
#![cfg_attr(not(feature = "internal"), allow(unreachable_pub))]

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
pub(crate) mod loading;
mod parsing;
mod peer;
pub(crate) mod playlist;
pub(crate) mod scheduler;
mod source;
mod stream;
pub(crate) mod stream_index;

// Public API re-exports

pub use config::{HlsConfig, KeyOptions};
pub use context::HlsStreamContext;
pub use error::{HlsError, HlsResult};
pub use kithara_abr::AbrMode;
pub use kithara_drm::{KeyProcessor, KeyProcessorRegistry, KeyProcessorRule};
pub use stream::Hls;
