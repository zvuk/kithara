#![cfg(not(target_arch = "wasm32"))]

use bytes::Bytes;
use futures::{StreamExt, stream};
use kithara::net::{ByteStream, Headers, Net, NetError, RangeSpec};
use kithara_platform::time::{Duration, sleep};
use url::Url;

pub struct DelayedNet<N> {
    delay: Duration,
    inner: N,
}

impl<N: Net> DelayedNet<N> {
    pub fn new(inner: N, delay: Duration) -> Self {
        Self { delay, inner }
    }
}

#[async_trait::async_trait]
impl<N: Net> Net for DelayedNet<N> {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        sleep(self.delay).await;
        self.inner.get_bytes(url, headers).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        sleep(self.delay).await;
        self.inner.stream(url, headers).await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        sleep(self.delay).await;
        self.inner.get_range(url, range, headers).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        sleep(self.delay).await;
        self.inner.head(url, headers).await
    }
}

pub fn success_stream() -> ByteStream {
    let stream = stream::iter(vec![Ok::<_, NetError>(Bytes::from_static(b"success"))]);
    ByteStream::without_headers(Box::pin(stream))
}

pub fn leaked<F>(f: F) -> &'static F
where
    F: Send + Sync + 'static,
{
    Box::leak(Box::new(f))
}

pub fn ok_headers() -> Headers {
    let mut headers = Headers::new();
    headers.insert("content-length", "7");
    headers
}

pub fn test_url() -> Url {
    Url::parse("http://example.com").expect("valid test URL")
}

pub async fn assert_success_all_net_methods(net: &impl Net) {
    let url = test_url();

    let result = net.get_bytes(url.clone(), None).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));

    let result = net.stream(url.clone(), None).await;
    assert!(result.is_ok());
    let mut stream = result.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, b"success");

    let range = RangeSpec::new(0, None);
    let result = net.get_range(url.clone(), range, None).await;
    assert!(result.is_ok());
    let mut stream = result.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, b"success");

    let result = net.head(url, None).await;
    assert!(result.is_ok());
    let headers = result.unwrap();
    assert_eq!(headers.get("content-length"), Some("7"));
}
