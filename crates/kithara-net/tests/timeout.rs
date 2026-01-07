use std::time::Duration;

use futures::StreamExt;
use kithara_net::{Headers, Net, NetError, RangeSpec, TimeoutNet};
use rstest::*;
use url::Url;

// Mock Net implementation for testing timeout logic
#[derive(Clone)]
struct MockNet {
    delay: Duration,
    should_succeed: bool,
}

impl MockNet {
    fn new(delay: Duration, should_succeed: bool) -> Self {
        Self {
            delay,
            should_succeed,
        }
    }
}

#[async_trait::async_trait]
impl kithara_net::Net for MockNet {
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
    ) -> Result<kithara_net::ByteStream, kithara_net::NetError> {
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
    ) -> Result<kithara_net::ByteStream, kithara_net::NetError> {
        tokio::time::sleep(self.delay).await;
        if self.should_succeed {
            let stream =
                futures::stream::iter(vec![Ok::<_, NetError>(bytes::Bytes::from("success"))]);
            Ok(Box::pin(stream))
        } else {
            Err(NetError::Http("mock error".to_string()))
        }
    }

    async fn head(
        &self,
        _url: Url,
        _headers: Option<Headers>,
    ) -> Result<Headers, kithara_net::NetError> {
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

// Test data for timeout scenarios
#[fixture]
fn timeout_scenarios() -> Vec<(Duration, Duration, bool)> {
    vec![
        (Duration::from_millis(100), Duration::from_millis(200), true), // Should succeed (delay < timeout)
        (
            Duration::from_millis(200),
            Duration::from_millis(100),
            false,
        ), // Should timeout (delay > timeout)
        (Duration::from_millis(50), Duration::from_millis(100), true),  // Should succeed
        (
            Duration::from_millis(150),
            Duration::from_millis(100),
            false,
        ), // Should timeout
    ]
}

// Test data for different methods
#[fixture]
fn net_methods() -> Vec<&'static str> {
    vec!["get_bytes", "stream", "get_range", "head"]
}

// Basic timeout tests - parameterized by scenario
#[rstest]
#[case::get_bytes("get_bytes")]
#[case::stream("stream")]
#[case::get_range("get_range")]
#[case::head("head")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_success_when_fast_enough(#[case] method: &str) {
    let mock_net = MockNet::new(Duration::from_millis(50), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_millis(100));
    let url = Url::parse("http://example.com").unwrap();

    match method {
        "get_bytes" => {
            let result: Result<bytes::Bytes, kithara_net::NetError> =
                timeout_net.get_bytes(url.clone(), None).await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
        }
        "stream" => {
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.stream(url.clone(), None).await;
            assert!(result.is_ok());
            let mut stream = result.unwrap();
            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(chunk, bytes::Bytes::from("success"));
        }
        "get_range" => {
            let range = RangeSpec::new(0, Some(10));
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.get_range(url.clone(), range, None).await;
            assert!(result.is_ok());
            let mut stream = result.unwrap();
            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(chunk, bytes::Bytes::from("success"));
        }
        "head" => {
            let result: Result<kithara_net::Headers, kithara_net::NetError> =
                timeout_net.head(url.clone(), None).await;
            assert!(result.is_ok());
            let headers = result.unwrap();
            assert_eq!(headers.get("content-length"), Some("7"));
        }
        _ => unreachable!(),
    }
}

// Timeout failure tests - parameterized by scenario
#[rstest]
#[case::get_bytes("get_bytes")]
#[case::stream("stream")]
#[case::get_range("get_range")]
#[case::head("head")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_failure_when_too_slow(#[case] method: &str) {
    let mock_net = MockNet::new(Duration::from_millis(200), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_millis(100));
    let url = Url::parse("http://example.com").unwrap();

    match method {
        "get_bytes" => {
            let result: Result<bytes::Bytes, kithara_net::NetError> =
                timeout_net.get_bytes(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        "stream" => {
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.stream(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        "get_range" => {
            let range = RangeSpec::new(0, Some(10));
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.get_range(url.clone(), range, None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        "head" => {
            let result: Result<kithara_net::Headers, kithara_net::NetError> =
                timeout_net.head(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        _ => unreachable!(),
    }
}

// Test that timeout doesn't affect error propagation
#[rstest]
#[case::get_bytes("get_bytes")]
#[case::stream("stream")]
#[case::get_range("get_range")]
#[case::head("head")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_preserves_original_errors(#[case] method: &str) {
    let mock_net = MockNet::new(Duration::from_millis(50), false);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_millis(100));
    let url = Url::parse("http://example.com").unwrap();

    match method {
        "get_bytes" => {
            let result: Result<bytes::Bytes, kithara_net::NetError> =
                timeout_net.get_bytes(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            match error {
                NetError::Http(msg) if msg == "mock error" => (),
                _ => panic!("Expected Http error, got {:?}", error),
            }
        }
        "stream" => {
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.stream(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            match error {
                NetError::Http(msg) if msg == "mock error" => (),
                _ => panic!("Expected Http error, got {:?}", error),
            }
        }
        "get_range" => {
            let range = RangeSpec::new(0, Some(10));
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.get_range(url.clone(), range, None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            match error {
                NetError::Http(msg) if msg == "mock error" => (),
                _ => panic!("Expected Http error, got {:?}", error),
            }
        }
        "head" => {
            let result: Result<kithara_net::Headers, kithara_net::NetError> =
                timeout_net.head(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            match error {
                NetError::Http(msg) if msg == "mock error" => (),
                _ => panic!("Expected Http error, got {:?}", error),
            }
        }
        _ => unreachable!(),
    }
}

// Test timeout with zero duration (immediate timeout)
#[rstest]
#[case::get_bytes("get_bytes")]
#[case::stream("stream")]
#[case::get_range("get_range")]
#[case::head("head")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_zero_timeout(#[case] method: &str) {
    let mock_net = MockNet::new(Duration::from_millis(100), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::ZERO);
    let url = Url::parse("http://example.com").unwrap();

    match method {
        "get_bytes" => {
            let result: Result<bytes::Bytes, kithara_net::NetError> =
                timeout_net.get_bytes(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        "stream" => {
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.stream(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        "get_range" => {
            let range = RangeSpec::new(0, Some(10));
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.get_range(url.clone(), range, None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        "head" => {
            let result: Result<kithara_net::Headers, kithara_net::NetError> =
                timeout_net.head(url.clone(), None).await;
            assert!(result.is_err());
            let error = result.err().unwrap();
            assert!(matches!(error, NetError::Timeout));
        }
        _ => unreachable!(),
    }
}

// Test timeout with very large duration (effectively no timeout)
#[rstest]
#[case::get_bytes("get_bytes")]
#[case::stream("stream")]
#[case::get_range("get_range")]
#[case::head("head")]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_large_timeout(#[case] method: &str) {
    let mock_net = MockNet::new(Duration::from_millis(50), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_secs(10));
    let url = Url::parse("http://example.com").unwrap();

    match method {
        "get_bytes" => {
            let result: Result<bytes::Bytes, kithara_net::NetError> =
                timeout_net.get_bytes(url.clone(), None).await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
        }
        "stream" => {
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.stream(url.clone(), None).await;
            assert!(result.is_ok());
            let mut stream = result.unwrap();
            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(chunk, bytes::Bytes::from("success"));
        }
        "get_range" => {
            let range = RangeSpec::new(0, Some(10));
            let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
                timeout_net.get_range(url.clone(), range, None).await;
            assert!(result.is_ok());
            let mut stream = result.unwrap();
            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(chunk, bytes::Bytes::from("success"));
        }
        "head" => {
            let result: Result<kithara_net::Headers, kithara_net::NetError> =
                timeout_net.head(url.clone(), None).await;
            assert!(result.is_ok());
            let headers = result.unwrap();
            assert_eq!(headers.get("content-length"), Some("7"));
        }
        _ => unreachable!(),
    }
}

// Test TimeoutNet constructor
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_net_constructor() {
    let mock_net = MockNet::new(Duration::from_millis(50), true);
    let timeout = Duration::from_millis(100);

    let timeout_net = TimeoutNet::new(mock_net, timeout);

    // Verify the timeout net works correctly
    let url = Url::parse("http://example.com").unwrap();
    let result: Result<bytes::Bytes, kithara_net::NetError> =
        timeout_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
}

// Test timeout error is retryable
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_error_is_retryable() {
    let timeout_error = kithara_net::NetError::Timeout;
    assert!(timeout_error.is_retryable());
    assert!(timeout_error.is_timeout());
}

// Test timeout with headers
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_with_headers() {
    let mock_net = MockNet::new(Duration::from_millis(50), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_millis(100));
    let url = Url::parse("http://example.com").unwrap();

    let mut headers = Headers::new();
    headers.insert("X-Test", "value");

    let result: Result<bytes::Bytes, kithara_net::NetError> =
        timeout_net.get_bytes(url, Some(headers)).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
}

// Test timeout with range request
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_with_range() {
    let mock_net = MockNet::new(Duration::from_millis(50), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_millis(100));
    let url = Url::parse("http://example.com").unwrap();

    let range = RangeSpec::new(10, Some(20));

    let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
        timeout_net.get_range(url, range, None).await;

    assert!(result.is_ok());
    let mut stream = result.unwrap();
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk, bytes::Bytes::from("success"));
}

// Test that timeout doesn't affect stream consumption after creation
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_timeout_only_applies_to_stream_creation() {
    let mock_net = MockNet::new(Duration::from_millis(50), true);
    let timeout_net = TimeoutNet::new(mock_net, Duration::from_millis(100));
    let url = Url::parse("http://example.com").unwrap();

    // Create stream (should timeout if too slow)
    let result: Result<kithara_net::ByteStream, kithara_net::NetError> =
        timeout_net.stream(url, None).await;
    assert!(result.is_ok());

    // Consuming the stream should not be affected by timeout
    let mut stream = result.unwrap();
    let chunk = stream.next().await;
    assert!(chunk.is_some());
    assert!(chunk.unwrap().is_ok());
}
