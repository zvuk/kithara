use std::time::Duration;

use futures::StreamExt;
use kithara::net::{Headers, Net, NetError, NetExt, RangeSpec};
use rstest::*;
use url::Url;

// Mock Net implementation for testing timeout logic

#[derive(Clone)]
struct TimeoutMockNet {
    delay: Duration,
    should_succeed: bool,
}

impl TimeoutMockNet {
    fn new(delay: Duration, should_succeed: bool) -> Self {
        Self {
            delay,
            should_succeed,
        }
    }
}

#[async_trait::async_trait]
impl Net for TimeoutMockNet {
    async fn get_bytes(
        &self,
        _url: Url,
        _headers: Option<Headers>,
    ) -> Result<bytes::Bytes, NetError> {
        tokio::time::sleep(self.delay).await;
        if self.should_succeed {
            Ok(bytes::Bytes::from("success"))
        } else {
            Err(NetError::Http("mock error".to_string()))
        }
    }

    async fn stream(
        &self,
        _url: Url,
        _headers: Option<Headers>,
    ) -> Result<kithara::net::ByteStream, NetError> {
        tokio::time::sleep(self.delay).await;
        if self.should_succeed {
            let stream =
                futures::stream::iter(vec![Ok::<_, NetError>(bytes::Bytes::from("success"))]);
            Ok(Box::pin(stream))
        } else {
            Err(NetError::Http("mock error".to_string()))
        }
    }

    async fn get_range(
        &self,
        _url: Url,
        _range: RangeSpec,
        _headers: Option<Headers>,
    ) -> Result<kithara::net::ByteStream, NetError> {
        tokio::time::sleep(self.delay).await;
        if self.should_succeed {
            let stream =
                futures::stream::iter(vec![Ok::<_, NetError>(bytes::Bytes::from("success"))]);
            Ok(Box::pin(stream))
        } else {
            Err(NetError::Http("mock error".to_string()))
        }
    }

    async fn head(&self, _url: Url, _headers: Option<Headers>) -> Result<Headers, NetError> {
        tokio::time::sleep(self.delay).await;
        if self.should_succeed {
            let mut headers = Headers::new();
            headers.insert("content-length", "7");
            Ok(headers)
        } else {
            Err(NetError::Http("mock error".to_string()))
        }
    }
}

// Helper functions

async fn test_all_net_methods_with_timeout_net(net: &impl Net) {
    let url = Url::parse("http://example.com").unwrap();

    // Test get_bytes
    let result = net.get_bytes(url.clone(), None).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), bytes::Bytes::from("success"));

    // Test stream
    let result = net.stream(url.clone(), None).await;
    assert!(result.is_ok());
    let mut stream = result.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, b"success");

    // Test get_range
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

    // Test head
    let result = net.head(url, None).await;
    assert!(result.is_ok());
    let headers = result.unwrap();
    assert_eq!(headers.get("content-length"), Some("7"));
}

// Tests

// Test timeout scenarios
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
    let mock_net = TimeoutMockNet::new(delay, true);
    let timeout_net = mock_net.with_timeout(timeout);

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
    } else {
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

// Test timeout with error (not timeout) - should preserve original error when timeout > delay
#[rstest]
#[case(Duration::from_millis(100), Duration::from_millis(200))] // Error before timeout
#[tokio::test]
async fn test_timeout_with_error(#[case] delay: Duration, #[case] timeout: Duration) {
    let mock_net = TimeoutMockNet::new(delay, false);
    let timeout_net = mock_net.with_timeout(timeout);

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    // When delay < timeout, should get original Http error
    // When delay > timeout, should get Timeout error
    if delay < timeout {
        assert!(matches!(error, NetError::Http(_)));
    } else {
        assert!(matches!(error, NetError::Timeout));
    }
}

// Test all Net methods with timeout
#[rstest]
#[case(Duration::from_millis(100), Duration::from_millis(200), true)]
#[case(Duration::from_millis(200), Duration::from_millis(100), false)]
#[tokio::test]
async fn test_all_net_methods_with_timeout(
    #[case] delay: Duration,
    #[case] timeout: Duration,
    #[case] should_succeed: bool,
) {
    let mock_net = TimeoutMockNet::new(delay, true);
    let timeout_net = mock_net.with_timeout(timeout);

    if should_succeed {
        test_all_net_methods_with_timeout_net(&timeout_net).await;
    } else {
        // For timeout case, just test get_bytes as representative
        let url = Url::parse("http://example.com").unwrap();
        let result = timeout_net.get_bytes(url, None).await;
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

// Test zero timeout (should timeout immediately for any non-zero delay)
#[rstest]
#[case(Duration::from_millis(0), true)]
#[case(Duration::from_millis(1), false)]
#[case(Duration::from_millis(100), false)]
#[tokio::test]
async fn test_zero_timeout(#[case] delay: Duration, #[case] should_succeed: bool) {
    let mock_net = TimeoutMockNet::new(delay, true);
    let timeout_net = mock_net.with_timeout(Duration::from_millis(0));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
    } else {
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

// Test large timeout (should succeed for reasonable delays)
#[rstest]
#[case(Duration::from_millis(0), true)]
#[case(Duration::from_millis(100), true)]
#[case(Duration::from_millis(1000), true)]
#[case(Duration::from_millis(5000), true)]
#[tokio::test]
async fn test_large_timeout(#[case] delay: Duration, #[case] should_succeed: bool) {
    let mock_net = TimeoutMockNet::new(delay, true);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(10));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
    } else {
        assert!(result.is_err());
        // Should not timeout with 10 second timeout
        assert!(matches!(result.err().unwrap(), NetError::Http(_)));
    }
}

// Test timeout preserves original error when operation fails before timeout
#[rstest]
#[case(Duration::from_millis(50))]
#[case(Duration::from_millis(100))]
#[case(Duration::from_millis(200))]
#[tokio::test]
async fn test_timeout_preserves_error(#[case] delay: Duration) {
    let mock_net = TimeoutMockNet::new(delay, false);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(1));

    let url = Url::parse("http://example.com").unwrap();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    assert!(matches!(error, NetError::Http(_)));
    assert!(error.to_string().contains("mock error"));
}

// Test timeout with representative scenarios
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
    let mock_net = TimeoutMockNet::new(Duration::from_millis(delay_ms), true);
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
        assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
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
