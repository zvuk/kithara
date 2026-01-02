//! Builder for composing downloader decorators.

use std::sync::Arc;
use std::time::Duration;

use stream_download::source::StreamMsg;
use stream_download::storage::StorageHandle;
use tokio::sync::mpsc;

use super::cache::CacheKeyCallback;
use super::traits::{Downloader, Headers};
use super::{CacheDownloader, HttpDownloader, RetryDownloader, RetryPolicy, TimeoutDownloader};

/// Builder for creating composed downloaders.
pub struct DownloaderBuilder<D> {
    inner: D,
}

impl DownloaderBuilder<HttpDownloader> {
    /// Start building from a base HttpDownloader.
    pub fn from_http(downloader: HttpDownloader) -> Self {
        Self { inner: downloader }
    }
}

impl<D> DownloaderBuilder<D>
where
    D: Downloader + Send + Sync + Clone + 'static,
{
    /// Add retry functionality.
    pub fn with_retry(self, policy: RetryPolicy) -> DownloaderBuilder<RetryDownloader<D>> {
        DownloaderBuilder {
            inner: RetryDownloader::new(self.inner, policy),
        }
    }

    /// Add retry functionality with default policy.
    pub fn with_default_retry(self) -> DownloaderBuilder<RetryDownloader<D>> {
        DownloaderBuilder {
            inner: RetryDownloader::with_default_policy(self.inner),
        }
    }

    /// Add timeout functionality.
    pub fn with_timeout(
        self,
        request_timeout: Duration,
    ) -> DownloaderBuilder<TimeoutDownloader<D>> {
        DownloaderBuilder {
            inner: TimeoutDownloader::new(self.inner, request_timeout),
        }
    }

    /// Add caching functionality.
    pub fn with_cache(
        self,
        handle: StorageHandle,
        key_callback: Arc<CacheKeyCallback>,
        data_sender: mpsc::Sender<StreamMsg>,
    ) -> DownloaderBuilder<CacheDownloader<D>> {
        DownloaderBuilder {
            inner: CacheDownloader::new(self.inner, handle, key_callback, data_sender),
        }
    }

    /// Build the final downloader.
    pub fn build(self) -> D {
        self.inner
    }
}

/// Convenience function to create a downloader with common defaults.
pub fn create_default_downloader(
    request_timeout: Duration,
    max_retries: u32,
    retry_base_delay: Duration,
    max_retry_delay: Duration,
    cancel: tokio_util::sync::CancellationToken,
    storage_handle: StorageHandle,
    key_callback: Arc<CacheKeyCallback>,
    data_sender: mpsc::Sender<StreamMsg>,
    key_request_headers: Option<Headers>,
) -> Arc<dyn Downloader + Send + Sync> {
    let base_downloader = HttpDownloader::new(request_timeout, cancel, key_request_headers);

    let retry_policy = RetryPolicy {
        max_retries,
        base_delay: retry_base_delay,
        max_delay: max_retry_delay,
    };

    let downloader = DownloaderBuilder::from_http(base_downloader)
        .with_timeout(request_timeout)
        .with_retry(retry_policy)
        .with_cache(storage_handle, key_callback, data_sender)
        .build();

    Arc::new(downloader)
}
