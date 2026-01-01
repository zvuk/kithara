use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;

use crate::error::NetError;
use crate::types::{Headers, RangeSpec};

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;

#[async_trait]
pub trait Net: Send + Sync {
    /// Get all bytes from a URL
    async fn get_bytes(&self, url: url::Url) -> Result<Bytes, NetError>;

    /// Stream bytes from a URL
    async fn stream(&self, url: url::Url, headers: Option<Headers>)
    -> Result<ByteStream, NetError>;

    /// Get a range of bytes from a URL
    async fn get_range(
        &self,
        url: url::Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError>;
}

pub trait NetExt: Net + Sized {
    /// Add timeout layer
    fn with_timeout(self, timeout: std::time::Duration) -> crate::timeout::TimeoutNet<Self>;

    /// Add retry layer
    fn with_retry(
        self,
        policy: crate::types::RetryPolicy,
    ) -> crate::retry::RetryNet<Self, crate::retry::DefaultRetryPolicy>;
}

impl<T: Net> NetExt for T {
    fn with_timeout(self, timeout: std::time::Duration) -> crate::timeout::TimeoutNet<Self> {
        crate::timeout::TimeoutNet::new(self, timeout)
    }

    fn with_retry(
        self,
        policy: crate::types::RetryPolicy,
    ) -> crate::retry::RetryNet<Self, crate::retry::DefaultRetryPolicy> {
        crate::retry::RetryNet::new(self, crate::retry::DefaultRetryPolicy::new(policy))
    }
}
