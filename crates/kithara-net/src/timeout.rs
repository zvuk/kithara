use async_trait::async_trait;
use std::time::Duration;

use crate::ByteStream;
use crate::error::NetError;
use crate::traits::Net;
use crate::types::{Headers, RangeSpec};

/// Timeout decorator for Net implementations
pub struct TimeoutNet<N> {
    inner: N,
    timeout: Duration,
}

impl<N: Net> TimeoutNet<N> {
    pub fn new(inner: N, timeout: Duration) -> Self {
        Self { inner, timeout }
    }
}

#[async_trait]
impl<N: Net> Net for TimeoutNet<N> {
    async fn get_bytes(&self, url: url::Url) -> Result<bytes::Bytes, NetError> {
        tokio::time::timeout(self.timeout, self.inner.get_bytes(url))
            .await
            .map_err(|_| NetError::timeout())?
    }

    async fn stream(
        &self,
        url: url::Url,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        // For streaming, we only timeout request/response phase, not entire stream
        tokio::time::timeout(self.timeout, self.inner.stream(url, headers))
            .await
            .map_err(|_| NetError::timeout())?
    }

    async fn get_range(
        &self,
        url: url::Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        // For range requests, we only timeout request/response phase, not entire stream
        tokio::time::timeout(self.timeout, self.inner.get_range(url, range, headers))
            .await
            .map_err(|_| NetError::timeout())?
    }
}
