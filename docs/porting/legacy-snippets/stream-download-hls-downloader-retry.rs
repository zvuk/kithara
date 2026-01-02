//! Retry decorator for downloaders.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::error::{HlsError, HlsResult};

use super::traits::{ByteStream, Downloader, Headers};
use super::types::Resource;

/// Retry policy configuration.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retry).
    pub max_retries: u32,
    /// Base delay between retries.
    pub base_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Create a new retry policy.
    pub fn new(max_retries: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            max_delay,
        }
    }

    /// Default retry policy (3 retries, exponential backoff).
    pub fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::default()
    }
}

/// Downloader decorator that adds retry logic.
#[derive(Debug, Clone)]
pub struct RetryDownloader<D> {
    inner: D,
    policy: RetryPolicy,
}

impl<D> RetryDownloader<D> {
    /// Create a new retry decorator.
    pub fn new(inner: D, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Create with default retry policy.
    pub fn with_default_policy(inner: D) -> Self {
        Self::new(inner, RetryPolicy::default())
    }

    /// Execute an operation with retry logic.
    async fn execute_with_retry<T, F, Fut>(
        &self,
        cancel: &CancellationToken,
        resource: &Resource,
        operation_name: &str,
        mut operation: F,
    ) -> HlsResult<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = HlsResult<T>>,
    {
        let mut last_error: Option<HlsError> = None;
        let mut delay = self.policy.base_delay;

        for attempt in 0..=self.policy.max_retries {
            if cancel.is_cancelled() {
                return Err(HlsError::Cancelled);
            }

            match operation().await {
                Ok(v) => {
                    if attempt > 0 {
                        debug!(
                            url = resource.url().as_str(),
                            attempts = attempt + 1,
                            operation = operation_name,
                            "download succeeded after retry"
                        );
                    }
                    return Ok(v);
                }
                Err(e) => {
                    debug!(
                        url = resource.url().as_str(),
                        attempt = attempt + 1,
                        max_attempts = self.policy.max_retries + 1,
                        operation = operation_name,
                        "download attempt failed: {}",
                        e
                    );
                    last_error = Some(e);

                    if attempt < self.policy.max_retries {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => return Err(HlsError::Cancelled),
                            _ = tokio::time::sleep(delay) => {},
                        }
                        delay = (delay * 2).min(self.policy.max_delay);
                    }
                }
            }
        }

        debug!(
            url = resource.url().as_str(),
            attempts = self.policy.max_retries + 1,
            operation = operation_name,
            "download giving up after retries"
        );

        Err(last_error.unwrap_or_else(|| HlsError::io("download failed with no error")))
    }
}

#[async_trait::async_trait]
impl<D> Downloader for RetryDownloader<D>
where
    D: Downloader + Send + Sync,
{
    async fn download_with_headers(
        &self,
        resource: &Resource,
        headers: Option<Headers>,
    ) -> HlsResult<bytes::Bytes> {
        let cancel = self.inner.cancel_token();
        self.execute_with_retry(cancel, resource, "download_with_headers", || {
            self.inner.download_with_headers(resource, headers.clone())
        })
        .await
    }

    async fn stream(&self, resource: &Resource) -> HlsResult<ByteStream> {
        // Streaming operations typically shouldn't be retried at this level
        // as they involve long-lived connections. The inner downloader should
        // handle reconnection if needed.
        self.inner.stream(resource).await
    }

    async fn stream_range(
        &self,
        resource: &Resource,
        start: u64,
        end: Option<u64>,
    ) -> HlsResult<ByteStream> {
        // Same as stream - retry logic is handled at a different layer
        self.inner.stream_range(resource, start, end).await
    }

    async fn probe_content_length(&self, resource: &Resource) -> HlsResult<Option<u64>> {
        let cancel = self.inner.cancel_token();
        self.execute_with_retry(cancel, resource, "probe_content_length", || {
            self.inner.probe_content_length(resource)
        })
        .await
    }

    fn cancel_token(&self) -> &CancellationToken {
        self.inner.cancel_token()
    }
}
