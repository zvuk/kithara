use reqwest::{Error as ReqwestError, Url};
use thiserror::Error;

pub type NetResult<T> = Result<T, NetError>;

/// Centralized error type for kithara-net
#[derive(Debug, Error, Clone)]
pub enum NetError {
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("Invalid range header: {0}")]
    InvalidRange(String),
    #[error("Timeout")]
    Timeout,
    #[error("Request failed after {max_retries} retries: {source}")]
    RetryExhausted {
        max_retries: u32,
        source: Box<NetError>,
    },
    #[error("HTTP {status}: {body:?} for URL: {url:?}")]
    HttpError {
        status: u16,
        url: Url,
        body: Option<String>,
    },
    #[error("not implemented")]
    Unimplemented,
}

impl NetError {
    /// Creates a timeout error
    pub fn timeout() -> Self {
        Self::Timeout
    }

    /// Creates an HTTP error from a generic string
    pub fn http<S: Into<String>>(msg: S) -> Self {
        Self::Http(msg.into())
    }

    /// Checks if this error is considered retryable
    pub fn is_retryable(&self) -> bool {
        match self {
            NetError::Http(http_err_str) => {
                // Check for retryable HTTP status codes in error strings
                http_err_str.contains("500") ||  // Server errors
                http_err_str.contains("502") ||  // Bad Gateway
                http_err_str.contains("503") ||  // Service Unavailable
                http_err_str.contains("504") ||  // Gateway Timeout
                http_err_str.contains("429") ||  // Too Many Requests
                http_err_str.contains("408") ||  // Request Timeout
                // Check for common network error patterns
                http_err_str.contains("timeout") ||
                http_err_str.contains("connection") ||
                http_err_str.contains("network") ||
                http_err_str.contains("decoding") ||  // Body decode errors
                http_err_str.contains("body") // Body read errors
            }
            NetError::Timeout => true,
            NetError::RetryExhausted { .. } => false,
            NetError::HttpError { status, .. } => {
                // Retry on 5xx server errors and 429 Too Many Requests
                *status >= 500 || *status == 429 || *status == 408
            }
            NetError::InvalidRange(_) | NetError::Unimplemented => false,
        }
    }
}

impl From<ReqwestError> for NetError {
    fn from(e: ReqwestError) -> Self {
        NetError::Http(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    // Test error creation methods
    #[rstest]
    #[case::timeout_error(NetError::timeout(), NetError::Timeout)]
    #[case::http_from_string(NetError::http("test error"), NetError::Http("test error".to_string()))]
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
    #[case::http_500(NetError::HttpError { status: 500, url: Url::parse("http://example.com").unwrap(), body: None }, true)]
    #[case::http_429(NetError::HttpError { status: 429, url: Url::parse("http://example.com").unwrap(), body: None }, true)]
    #[case::http_404(NetError::HttpError { status: 404, url: Url::parse("http://example.com").unwrap(), body: None }, false)]
    #[case::invalid_range(NetError::InvalidRange("test".to_string()), false)]
    #[case::unimplemented(NetError::Unimplemented, false)]
    #[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) }, false)]
    #[tokio::test]
    async fn test_is_retryable(#[case] error: NetError, #[case] expected_retryable: bool) {
        assert_eq!(error.is_retryable(), expected_retryable);
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
        NetError::HttpError { status: 404, url: Url::parse("http://example.com/test").unwrap(), body: Some("Not found".to_string()) },
        "HTTP 404: Some(\"Not found\") for URL: Url { scheme: \"http\", cannot_be_a_base: false, username: \"\", password: None, host: Some(Domain(\"example.com\")), port: None, path: \"/test\", query: None, fragment: None }"
    )]
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

    // Test error cloning
    #[rstest]
    #[case::timeout(NetError::Timeout)]
    #[case::http_error(NetError::HttpError { status: 500, url: Url::parse("http://example.com").unwrap(), body: None })]
    #[case::invalid_range(NetError::InvalidRange("test".to_string()))]
    #[case::unimplemented(NetError::Unimplemented)]
    #[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) })]
    #[tokio::test]
    async fn test_error_cloning(#[case] error: NetError) {
        let cloned = error.clone();

        // Compare string representations since some variants don't implement PartialEq
        assert_eq!(error.to_string(), cloned.to_string());

        // Verify is_retryable is preserved
        assert_eq!(error.is_retryable(), cloned.is_retryable());
    }

    // Test error debug formatting
    #[rstest]
    #[case::timeout(NetError::Timeout)]
    #[case::http_error(NetError::HttpError { status: 404, url: Url::parse("http://example.com").unwrap(), body: None })]
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
    #[tokio::test]
    async fn test_net_result_type() {
        // Test Ok variant
        let ok_result: NetResult<i32> = Ok(42);
        assert!(ok_result.is_ok());
        assert!(matches!(ok_result, Ok(42)));

        // Test Err variant
        let err_result: NetResult<i32> = Err(NetError::Timeout);
        assert!(err_result.is_err());

        match err_result {
            Err(NetError::Timeout) => (),
            _ => panic!("Expected Timeout error"),
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
}
