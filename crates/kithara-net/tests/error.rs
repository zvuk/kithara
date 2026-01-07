use std::time::Duration;

use kithara_net::{NetError, NetResult};
use rstest::*;
use url::Url;

// Test data for retryable errors
#[fixture]
fn retryable_errors() -> Vec<NetError> {
    vec![
        NetError::Http("timeout".to_string()),
        NetError::Http("connection failed".to_string()),
        NetError::Http("network error".to_string()),
        NetError::Http("500 Internal Server Error".to_string()),
        NetError::Http("502 Bad Gateway".to_string()),
        NetError::Http("503 Service Unavailable".to_string()),
        NetError::Http("504 Gateway Timeout".to_string()),
        NetError::Http("429 Too Many Requests".to_string()),
        NetError::Http("408 Request Timeout".to_string()),
        NetError::Timeout,
        NetError::http_error(500, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(502, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(503, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(504, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(429, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(408, Url::parse("http://example.com").unwrap(), None),
    ]
}

// Test data for non-retryable errors
#[fixture]
fn non_retryable_errors() -> Vec<NetError> {
    vec![
        NetError::InvalidRange("invalid range".to_string()),
        NetError::Unimplemented,
        NetError::http_error(404, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(400, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(403, Url::parse("http://example.com").unwrap(), None),
        NetError::http_error(401, Url::parse("http://example.com").unwrap(), None),
    ]
}

// Test error creation methods
#[rstest]
#[case::timeout_error(NetError::timeout(), NetError::Timeout)]
#[case::http_from_string(NetError::http("test error"), NetError::Http("test error".to_string()))]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_error_creation_methods(
    #[case] created_error: NetError,
    #[case] expected_error: NetError,
) {
    match (created_error, expected_error) {
        (NetError::Timeout, NetError::Timeout) => (),
        (NetError::Http(a), NetError::Http(b)) => assert_eq!(a, b),
        _ => panic!("Errors don't match"),
    }
}

// Test is_retryable method - parameterized
#[rstest]
#[case::timeout(NetError::Timeout, true)]
#[case::http_500(NetError::http_error(500, Url::parse("http://example.com").unwrap(), None), true)]
#[case::http_429(NetError::http_error(429, Url::parse("http://example.com").unwrap(), None), true)]
#[case::http_404(NetError::http_error(404, Url::parse("http://example.com").unwrap(), None), false)]
#[case::invalid_range(NetError::InvalidRange("test".to_string()), false)]
#[case::unimplemented(NetError::Unimplemented, false)]
#[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) }, false)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_is_retryable(#[case] error: NetError, #[case] expected_retryable: bool) {
    assert_eq!(error.is_retryable(), expected_retryable);
}

// Test is_timeout method
#[rstest]
#[case::timeout(NetError::Timeout, true)]
#[case::http_timeout_in_string(NetError::Http("timeout".to_string()), false)] // Only NetError::Timeout variant
#[case::http_error(NetError::http_error(408, Url::parse("http://example.com").unwrap(), None), false)]
#[case::other_error(NetError::InvalidRange("test".to_string()), false)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_is_timeout(#[case] error: NetError, #[case] expected_is_timeout: bool) {
    assert_eq!(error.is_timeout(), expected_is_timeout);
}

// Test status_code method
#[rstest]
#[case::http_error_with_status(NetError::http_error(404, Url::parse("http://example.com").unwrap(), None), Some(404))]
#[case::http_error_500(NetError::http_error(500, Url::parse("http://example.com").unwrap(), None), Some(500))]
#[case::timeout_error(NetError::Timeout, None)]
#[case::http_string_error(NetError::Http("test".to_string()), None)]
#[case::invalid_range_error(NetError::InvalidRange("test".to_string()), None)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_status_code(#[case] error: NetError, #[case] expected_status: Option<u16>) {
    assert_eq!(error.status_code(), expected_status);
}

// Test error display formatting
#[rstest]
#[case::http_error(
    NetError::Http("connection failed".to_string()),
    "HTTP request failed: connection failed"
)]
#[case::invalid_range(
    NetError::InvalidRange("invalid byte range".to_string()),
    "Invalid range header: invalid byte range"
)]
#[case::timeout(NetError::Timeout, "Timeout")]
#[case::unimplemented(NetError::Unimplemented, "not implemented")]
#[case::http_error_with_details(
    NetError::http_error(404, Url::parse("http://example.com/test").unwrap(), Some("Not found".to_string())),
    "HTTP 404: Some(\"Not found\") for URL: Url { scheme: \"http\", cannot_be_a_base: false, username: \"\", password: None, host: Some(Domain(\"example.com\")), port: None, path: \"/test\", query: None, fragment: None }"
)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_error_display(#[case] error: NetError, #[case] expected_prefix: &str) {
    let display = error.to_string();
    assert!(
        display.starts_with(expected_prefix),
        "Expected display to start with '{}', got '{}'",
        expected_prefix,
        display
    );
}

// Test RetryExhausted error display
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_retry_exhausted_display() {
    let source = Box::new(NetError::Timeout);
    let error = NetError::RetryExhausted {
        max_retries: 3,
        source,
    };

    let display = error.to_string();
    assert!(display.contains("Request failed after 3 retries: Timeout"));
}

// Test from_reqwest_error conversion
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_from_reqwest_error() {
    // Note: We can't easily create a real reqwest::Error without making actual HTTP requests
    // This test verifies the conversion exists and compiles

    // The From<ReqwestError> implementation should exist
    // We'll test this by verifying the trait bound exists
    // This is a compile-time test
    // Create a dummy closure to verify the conversion compiles
    let _converter = |e: reqwest::Error| -> NetError { e.into() };

    // This is mostly a compile-time test
    assert!(true);
}

// Test error cloning
#[rstest]
#[case::timeout(NetError::Timeout)]
#[case::http_error(NetError::http_error(500, Url::parse("http://example.com").unwrap(), None))]
#[case::invalid_range(NetError::InvalidRange("test".to_string()))]
#[case::unimplemented(NetError::Unimplemented)]
#[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) })]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_error_cloning(#[case] error: NetError) {
    let cloned = error.clone();

    // Compare string representations since some variants don't implement PartialEq
    assert_eq!(error.to_string(), cloned.to_string());

    // Verify is_retryable is preserved
    assert_eq!(error.is_retryable(), cloned.is_retryable());

    // Verify is_timeout is preserved
    assert_eq!(error.is_timeout(), cloned.is_timeout());

    // Verify status_code is preserved
    assert_eq!(error.status_code(), cloned.status_code());
}

// Test error debug formatting
#[rstest]
#[case::timeout(NetError::Timeout)]
#[case::http_error(NetError::http_error(404, Url::parse("http://example.com").unwrap(), None))]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_error_debug(#[case] error: NetError) {
    let debug_output = format!("{:?}", error);

    // Debug output should contain the variant name
    match error {
        NetError::Timeout => assert!(debug_output.contains("Timeout")),
        NetError::HttpError { .. } => assert!(debug_output.contains("HttpError")),
        _ => (),
    }
}

// Test NetResult type alias
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_net_result_type() {
    // Test Ok variant
    let ok_result: NetResult<i32> = Ok(42);
    assert!(ok_result.is_ok());
    assert_eq!(ok_result.unwrap(), 42);

    // Test Err variant
    let err_result: NetResult<i32> = Err(NetError::Timeout);
    assert!(err_result.is_err());

    match err_result {
        Err(NetError::Timeout) => (),
        _ => panic!("Expected Timeout error"),
    }
}

// Test http_error factory method with different body types
#[rstest]
#[case::with_string_body(Some("Error body".to_string()))]
#[case::with_none_body(None)]
#[case::with_empty_string_body(Some("".to_string()))]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_http_error_factory(#[case] body: Option<String>) {
    let url = Url::parse("http://example.com/test").unwrap();
    let status = 404;

    let error = NetError::http_error(status, url.clone(), body.clone());

    match error {
        NetError::HttpError {
            status: actual_status,
            url: actual_url,
            body: actual_body,
        } => {
            assert_eq!(actual_status, status);
            assert_eq!(actual_url, url);
            assert_eq!(actual_body, body);
        }
        _ => panic!("Expected HttpError variant"),
    }
}

// Test that HTTP error strings are parsed for retryability
#[rstest]
#[case("500 Internal Server Error", true)]
#[case("502 Bad Gateway", true)]
#[case("503 Service Unavailable", true)]
#[case("504 Gateway Timeout", true)]
#[case("429 Too Many Requests", true)]
#[case("408 Request Timeout", true)]
#[case("timeout while connecting", true)]
#[case("network error", true)]
#[case("connection reset", true)]
#[case("404 Not Found", false)]
#[case("400 Bad Request", false)]
#[case("403 Forbidden", false)]
#[case("401 Unauthorized", false)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_http_error_string_parsing(
    #[case] error_string: &str,
    #[case] expected_retryable: bool,
) {
    let error = NetError::Http(error_string.to_string());
    assert_eq!(error.is_retryable(), expected_retryable);
}

// Test error equality (where applicable)
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn test_error_equality() {
    // Timeout errors should be equal
    let timeout1 = NetError::Timeout;
    let timeout2 = NetError::Timeout;
    // Note: NetError doesn't implement PartialEq, so we compare string representations
    assert_eq!(timeout1.to_string(), timeout2.to_string());

    // Same HTTP error strings should have same display
    let http1 = NetError::Http("test".to_string());
    let http2 = NetError::Http("test".to_string());
    assert_eq!(http1.to_string(), http2.to_string());

    // Different HTTP error strings should have different display
    let http3 = NetError::Http("error1".to_string());
    let http4 = NetError::Http("error2".to_string());
    assert_ne!(http3.to_string(), http4.to_string());
}
