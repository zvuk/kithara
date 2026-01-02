//! HLS downloaders with decorator pattern architecture.
//!
//! This module provides a flexible downloader system using the decorator pattern.
//! Components can be composed to add retry, timeout, and caching functionality.

use bytes::Bytes;
use futures_util::stream::BoxStream;

mod base;
mod builder;
mod cache;
mod retry;
mod timeout;
mod traits;
mod types;

/// A boxed stream of HLS byte chunks produced by the downloader/manager pipeline.
pub type HlsByteStream = BoxStream<'static, Result<Bytes, crate::error::HlsError>>;

// New architecture exports
pub use base::HttpDownloader;
pub use builder::{DownloaderBuilder, create_default_downloader};
pub use cache::{CacheDownloader, CachedBytes};
pub use retry::{RetryDownloader, RetryPolicy};
pub use timeout::TimeoutDownloader;
pub use traits::{Downloader, DownloaderExt};
pub use types::Resource;
