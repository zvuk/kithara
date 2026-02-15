use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use url::Url;

use crate::{
    ByteStream,
    error::NetError,
    traits::Net,
    types::{Headers, RangeSpec},
};

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

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl<N: Net> Net for TimeoutNet<N> {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        tokio::time::timeout(self.timeout, self.inner.get_bytes(url, headers))
            .await
            .map_err(|_| NetError::timeout())?
    }

    async fn stream(
        &self,
        url: url::Url,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        tokio::time::timeout(self.timeout, self.inner.stream(url, headers))
            .await
            .map_err(|_| NetError::timeout())?
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        tokio::time::timeout(self.timeout, self.inner.get_range(url, range, headers))
            .await
            .map_err(|_| NetError::timeout())?
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        tokio::time::timeout(self.timeout, self.inner.head(url, headers))
            .await
            .map_err(|_| NetError::timeout())?
    }
}

/// On wasm32, pass through to inner without timeout wrapping.
/// Browser fetch has its own timeout mechanisms.
#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl<N: Net> Net for TimeoutNet<N> {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.inner.get_bytes(url, headers).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.inner.stream(url, headers).await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        self.inner.get_range(url, range, headers).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        self.inner.head(url, headers).await
    }
}
