use std::future::Future;

use async_trait::async_trait;
use bytes::Bytes;
use kithara_platform::time::{Duration, timeout};
use url::Url;

mod kithara {
    pub(crate) use kithara_test_macros::flash;
}

use crate::{
    ByteStream,
    error::NetError,
    traits::Net,
    types::{Headers, RangeSpec},
};

/// Timeout decorator for Net implementations
pub struct TimeoutNet<N> {
    timeout: Duration,
    inner: N,
}

impl<N: Net> TimeoutNet<N> {
    pub fn new(inner: N, timeout: Duration) -> Self {
        Self { timeout, inner }
    }
}

#[kithara::flash(io)]
async fn timeout_io<T, Fut>(duration: Duration, fut: Fut) -> Result<T, NetError>
where
    Fut: Future<Output = Result<T, NetError>>,
{
    timeout(duration, fut)
        .await
        .map_err(|_| NetError::timeout())?
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<N: Net> Net for TimeoutNet<N> {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        timeout_io(self.timeout, self.inner.get_bytes(url, headers)).await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        timeout_io(self.timeout, self.inner.get_range(url, range, headers)).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        timeout_io(self.timeout, self.inner.head(url, headers)).await
    }

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        timeout_io(self.timeout, self.inner.post_bytes(url, body, headers)).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        timeout_io(self.timeout, self.inner.stream(url, headers)).await
    }
}
