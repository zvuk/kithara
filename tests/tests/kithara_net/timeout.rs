use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use kithara::net::{ByteStream, Headers, Net, NetError, NetExt, RangeSpec};
use kithara_net::mock::NetMock;
use rstest::rstest;
use unimock::{MockFn, Unimock, matching};
use url::Url;

struct DelayedNet<N> {
    delay: Duration,
    inner: N,
}

impl<N: Net> DelayedNet<N> {
    fn new(inner: N, delay: Duration) -> Self {
        Self { delay, inner }
    }
}

#[async_trait::async_trait]
impl<N: Net> Net for DelayedNet<N> {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        tokio::time::sleep(self.delay).await;
        self.inner.get_bytes(url, headers).await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        tokio::time::sleep(self.delay).await;
        self.inner.stream(url, headers).await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        tokio::time::sleep(self.delay).await;
        self.inner.get_range(url, range, headers).await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        tokio::time::sleep(self.delay).await;
        self.inner.head(url, headers).await
    }
}

fn success_stream() -> ByteStream {
    let stream = futures::stream::iter(vec![Ok::<_, NetError>(Bytes::from_static(b"success"))]);
    Box::pin(stream)
}

fn mock_error() -> NetError {
    NetError::Http("mock error".to_string())
}

fn leaked<F>(f: F) -> &'static F
where
    F: Send + Sync + 'static,
{
    Box::leak(Box::new(f))
}

fn make_timeout_mock(should_succeed: bool) -> Unimock {
    Unimock::new((
        NetMock::get_bytes
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_succeed {
                    Ok(Bytes::from_static(b"success"))
                } else {
                    Err(mock_error())
                }
            })),
        NetMock::stream
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_succeed {
                    Ok(success_stream())
                } else {
                    Err(mock_error())
                }
            })),
        NetMock::get_range
            .some_call(matching!(_, _, _))
            .answers(leaked(move |_, _url, _range, _headers| {
                if should_succeed {
                    Ok(success_stream())
                } else {
                    Err(mock_error())
                }
            })),
        NetMock::head
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_succeed {
                    let mut headers = Headers::new();
                    headers.insert("content-length", "7");
                    Ok(headers)
                } else {
                    Err(mock_error())
                }
            })),
    ))
    .no_verify_in_drop()
}

async fn test_all_net_methods_with_timeout_net(net: &impl Net) {
    let url = Url::parse("http://example.com").unwrap();

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

#[rstest]
#[case::success_before_timeout(Duration::from_millis(100), Duration::from_millis(200), true)]
#[case::timeout_before_success(Duration::from_millis(200), Duration::from_millis(100), false)]
#[case::zero_delay(Duration::from_millis(0), Duration::from_millis(100), true)]
#[case::large_timeout(Duration::from_millis(1000), Duration::from_millis(10), false)]
#[tokio::test]
async fn test_timeout_scenarios(
    #[case] delay: Duration,
    #[case] timeout: Duration,
    #[case] should_succeed: bool,
) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(timeout);

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
    } else {
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

#[rstest]
#[case(Duration::from_millis(100), Duration::from_millis(200))]
#[tokio::test]
async fn test_timeout_with_error(#[case] delay: Duration, #[case] timeout: Duration) {
    let mock_net = DelayedNet::new(make_timeout_mock(false), delay);
    let timeout_net = mock_net.with_timeout(timeout);

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    if delay < timeout {
        assert!(matches!(error, NetError::Http(_)));
    } else {
        assert!(matches!(error, NetError::Timeout));
    }
}

#[rstest]
#[case(Duration::from_millis(100), Duration::from_millis(200), true)]
#[case(Duration::from_millis(200), Duration::from_millis(100), false)]
#[tokio::test]
async fn test_all_net_methods_with_timeout(
    #[case] delay: Duration,
    #[case] timeout: Duration,
    #[case] should_succeed: bool,
) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(timeout);

    if should_succeed {
        test_all_net_methods_with_timeout_net(&timeout_net).await;
    } else {
        let url = Url::parse("http://example.com").unwrap();
        let result = timeout_net.get_bytes(url, None).await;
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

#[rstest]
#[case(Duration::from_millis(0), true)]
#[case(Duration::from_millis(1), false)]
#[case(Duration::from_millis(100), false)]
#[tokio::test]
async fn test_zero_timeout(#[case] delay: Duration, #[case] should_succeed: bool) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_millis(0));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
    } else {
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

#[rstest]
#[case(Duration::from_millis(0), true)]
#[case(Duration::from_millis(100), true)]
#[case(Duration::from_millis(1000), true)]
#[case(Duration::from_millis(5000), true)]
#[tokio::test]
async fn test_large_timeout(#[case] delay: Duration, #[case] should_succeed: bool) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(10));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
    } else {
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Http(_)));
    }
}

#[rstest]
#[case(Duration::from_millis(50))]
#[case(Duration::from_millis(100))]
#[case(Duration::from_millis(200))]
#[tokio::test]
async fn test_timeout_preserves_error(#[case] delay: Duration) {
    let mock_net = DelayedNet::new(make_timeout_mock(false), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(1));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    assert!(matches!(error, NetError::Http(_)));
    assert!(error.to_string().contains("mock error"));
}

#[rstest]
#[case::fast_delay(100, 200, true)]
#[case::slow_delay(200, 100, false)]
#[case::quick_success(50, 100, true)]
#[case::moderate_timeout(150, 100, false)]
#[case::zero_delay(0, 100, true)]
#[case::large_delay(1000, 10, false)]
#[tokio::test]
async fn test_timeout_representative_scenarios(
    #[case] delay_ms: u64,
    #[case] timeout_ms: u64,
    #[case] should_succeed: bool,
) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), Duration::from_millis(delay_ms));
    let timeout_net = mock_net.with_timeout(Duration::from_millis(timeout_ms));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(
            result.is_ok(),
            "Should succeed with delay={}ms, timeout={}ms",
            delay_ms,
            timeout_ms
        );
        assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
    } else {
        assert!(
            result.is_err(),
            "Should timeout with delay={}ms, timeout={}ms",
            delay_ms,
            timeout_ms
        );
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}
