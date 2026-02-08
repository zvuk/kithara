use std::time::Duration;

use futures::StreamExt;
use kithara_net::{Headers, Net, NetError, NetExt, RangeSpec, RetryPolicy};
use rstest::*;
use url::Url;

// Mock Net implementation for testing retry logic

#[derive(Clone)]
struct RetryMockNet {
    failures_before_success: usize,
    current_attempt: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    error_type: NetError,
}

impl RetryMockNet {
    fn new(failures_before_success: usize, error_type: NetError) -> Self {
        Self {
            failures_before_success,
            current_attempt: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error_type,
        }
    }

    fn attempt(&self) -> usize {
        self.current_attempt
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Net for RetryMockNet {
    async fn get_bytes(
        &self,
        _url: Url,
        _headers: Option<Headers>,
    ) -> Result<bytes::Bytes, NetError> {
        let attempt = self.attempt();
        if attempt < self.failures_before_success {
            Err(self.error_type.clone())
        } else {
            Ok(bytes::Bytes::from("success"))
        }
    }

    async fn stream(
        &self,
        _url: Url,
        _headers: Option<Headers>,
    ) -> Result<kithara_net::ByteStream, NetError> {
        let attempt = self.attempt();
        if attempt < self.failures_before_success {
            Err(self.error_type.clone())
        } else {
            let stream =
                futures::stream::iter(vec![Ok::<_, NetError>(bytes::Bytes::from("success"))]);
            Ok(Box::pin(stream))
        }
    }

    async fn get_range(
        &self,
        _url: Url,
        _range: RangeSpec,
        _headers: Option<Headers>,
    ) -> Result<kithara_net::ByteStream, NetError> {
        let attempt = self.attempt();
        if attempt < self.failures_before_success {
            Err(self.error_type.clone())
        } else {
            let stream =
                futures::stream::iter(vec![Ok::<_, NetError>(bytes::Bytes::from("success"))]);
            Ok(Box::pin(stream))
        }
    }

    async fn head(&self, _url: Url, _headers: Option<Headers>) -> Result<Headers, NetError> {
        let attempt = self.attempt();
        if attempt < self.failures_before_success {
            Err(self.error_type.clone())
        } else {
            let mut headers = Headers::new();
            headers.insert("content-length", "7");
            Ok(headers)
        }
    }
}

// Helper functions

async fn test_all_net_methods_with_retry_net(net: &impl Net) {
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

// Test retryable errors - should succeed after retries
#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[tokio::test]
async fn test_retryable_errors_success_after_retries(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
}

// Test non-retryable errors - should fail immediately
#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[tokio::test]
async fn test_non_retryable_errors_no_retry(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("400 Bad Request".to_string());
    let mock_net = RetryMockNet::new(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_err());
    assert!(matches!(result.err().unwrap(), NetError::Http(_)));
}

// Test retry exhaustion - should fail after max retries
#[rstest]
#[case(2, 1)] // 2 failures, max_retries=1
#[case(3, 2)] // 3 failures, max_retries=2
#[case(4, 3)] // 4 failures, max_retries=3
#[tokio::test]
async fn test_retry_exhaustion(#[case] failures_before_success: usize, #[case] max_retries: u32) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        max_retries,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    // Should fail because failures_before_success > max_retries
    assert!(result.is_err());
    assert!(matches!(result.err().unwrap(), NetError::Http(_)));
}

// Test exponential backoff with max delay
#[rstest]
#[case(1, Duration::from_millis(100), Duration::from_millis(1000))]
#[case(2, Duration::from_millis(100), Duration::from_millis(1000))]
#[case(3, Duration::from_millis(100), Duration::from_millis(1000))]
#[tokio::test]
async fn test_exponential_backoff_with_max_delay(
    #[case] max_retries: u32,
    #[case] base_delay: Duration,
    #[case] max_delay: Duration,
) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new((max_retries + 1) as usize, error_type);
    let retry_policy = RetryPolicy::new(max_retries, base_delay, max_delay);
    let retry_net = mock_net.with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_err());
}

// Test all Net methods with retry
#[rstest]
#[case(1)]
#[case(2)]
#[tokio::test]
async fn test_all_net_methods_with_retry(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy);

    // Test all methods using the helper
    test_all_net_methods_with_retry_net(&retry_net).await;
}

// Test method chaining: timeout + retry
#[rstest]
#[case(1)]
#[case(2)]
#[tokio::test]
async fn test_timeout_retry_chaining(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );

    // Chain timeout and retry
    let net = mock_net
        .with_timeout(Duration::from_secs(5))
        .with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
}

// Test retry with zero max_retries (should behave like no retry)
#[tokio::test]
async fn test_zero_max_retries() {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new(1, error_type);
    let retry_policy = RetryPolicy::new(0, Duration::from_millis(10), Duration::from_secs(5));
    let retry_net = mock_net.with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    // Should fail immediately with zero retries
    assert!(result.is_err());
    assert!(matches!(result.err().unwrap(), NetError::Http(_)));
}

// Test retry with zero base_delay (should still work)
#[rstest]
#[case(1)]
#[tokio::test]
async fn test_zero_base_delay(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = RetryMockNet::new(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(0),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy);

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), bytes::Bytes::from("success"));
}
