use std::{pin::Pin, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use url::Url;

use crate::{
    error::NetError,
    retry::{DefaultRetryPolicy, RetryNet},
    timeout::TimeoutNet,
    types::{Headers, RangeSpec, RetryPolicy},
};

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;

#[async_trait]
pub trait Net: Send + Sync {
    /// Get all bytes from a URL
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError>;

    /// Stream bytes from a URL
    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError>;

    /// Get a range of bytes from a URL
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError>;

    /// Perform a HEAD request.
    ///
    /// This is intended for lightweight metadata probes (e.g. `Content-Length`,
    /// `Accept-Ranges`, `Content-Type`). Implementations should return response headers.
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError>;
}

pub trait NetExt: Net + Sized {
    /// Add timeout layer
    fn with_timeout(self, timeout: Duration) -> TimeoutNet<Self> {
        TimeoutNet::new(self, timeout)
    }

    /// Add retry layer
    fn with_retry(self, policy: RetryPolicy) -> RetryNet<Self, DefaultRetryPolicy> {
        RetryNet::new(self, DefaultRetryPolicy::new(policy))
    }
}

impl<T: Net> NetExt for T {}
