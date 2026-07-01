#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
use std::fmt::Write;
use std::num::NonZeroU16;

use thiserror::Error;
use url::Url;

#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
use crate::backend::BackendError as ReqwestError;

pub type NetResult<T> = Result<T, NetError>;

/// Centralized error type for kithara-net.
#[non_exhaustive]
#[derive(Debug, Error, Clone)]
pub enum NetError {
    #[error("HTTP {status}: {body:?} for URL: {url:?}")]
    Status {
        status: NonZeroU16,
        url: Option<Url>,
        body: Option<String>,
    },
    #[error("Timeout")]
    Timeout,
    #[error("Network error: {0}")]
    Network(String),
    #[error("Decode error: {0}")]
    Decode(String),
    #[error("Request failed after {max_retries} retries: {source}")]
    RetryExhausted { max_retries: u32, source: Box<Self> },
    #[error("not implemented")]
    Unimplemented,
    #[error("Cancelled")]
    Cancelled,
    #[error("Invalid content-type: {0}")]
    InvalidContentType(String),
}

/// Whether a failed request is worth retrying. Decided from the typed
/// [`NetError`] discriminant, never from substring matching on the message.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    Transient,
    Fatal,
}

impl NetError {
    /// HTTP 408 Request Timeout.
    const HTTP_REQUEST_TIMEOUT: u16 = 408;

    /// Minimum HTTP status code for server errors (5xx).
    const HTTP_SERVER_ERROR_MIN: u16 = 500;

    /// HTTP 429 Too Many Requests.
    const HTTP_TOO_MANY_REQUESTS: u16 = 429;

    /// Classifies the error for retry decisioning via its typed variant.
    #[must_use]
    pub fn retryability(&self) -> Retryability {
        self.into()
    }

    /// Creates a timeout error.
    #[must_use]
    pub fn timeout() -> Self {
        Self::Timeout
    }
}

impl From<&NetError> for Retryability {
    fn from(error: &NetError) -> Self {
        match error {
            NetError::Status { status, .. } => {
                let code = status.get();
                if code >= NetError::HTTP_SERVER_ERROR_MIN
                    || code == NetError::HTTP_TOO_MANY_REQUESTS
                    || code == NetError::HTTP_REQUEST_TIMEOUT
                {
                    Self::Transient
                } else {
                    Self::Fatal
                }
            }
            NetError::Timeout | NetError::Network(_) => Self::Transient,
            NetError::Decode(_)
            | NetError::RetryExhausted { .. }
            | NetError::Unimplemented
            | NetError::Cancelled
            | NetError::InvalidContentType(_) => Self::Fatal,
        }
    }
}

#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
impl From<ReqwestError> for NetError {
    fn from(e: ReqwestError) -> Self {
        if e.is_timeout() {
            return Self::Timeout;
        }
        // WHY: non-status reqwest errors (connect, body-EOF, decode) stay retryable so an early stream close can resume; fatal Decode is for a local sink write (dl/response.rs).
        e.status()
            .and_then(|s| NonZeroU16::new(s.as_u16()))
            .map_or_else(
                || Self::Network(error_chain(&e)),
                |status| Self::Status {
                    status,
                    url: e.url().cloned(),
                    body: None,
                },
            )
    }
}

#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
fn error_chain(e: &ReqwestError) -> String {
    let mut msg = e.to_string();
    let mut current: &dyn std::error::Error = e;
    while let Some(source) = current.source() {
        let _ = write!(msg, ": {source}");
        current = source;
    }
    msg
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    fn test_url(raw: &str) -> Url {
        Url::parse(raw).expect("BUG: hard-coded test URL is valid")
    }

    fn nz(status: u16) -> NonZeroU16 {
        NonZeroU16::new(status).expect("BUG: hard-coded test status is non-zero")
    }

    #[kithara::test(tokio)]
    #[case::timeout_error(NetError::timeout(), NetError::Timeout)]
    async fn test_error_creation_methods(
        #[case] created_error: NetError,
        #[case] expected_error: NetError,
    ) {
        match (created_error, expected_error) {
            (NetError::Timeout, NetError::Timeout) => (),
            _ => panic!("Errors don't match"),
        }
    }

    #[kithara::test(tokio)]
    #[case::timeout(NetError::Timeout, Retryability::Transient)]
    #[case::network(NetError::Network("connection reset".to_string()), Retryability::Transient)]
    #[case::status_500(NetError::Status { status: nz(500), url: Some(test_url("http://example.com")), body: None }, Retryability::Transient)]
    #[case::status_429(NetError::Status { status: nz(429), url: Some(test_url("http://example.com")), body: None }, Retryability::Transient)]
    #[case::status_408(NetError::Status { status: nz(408), url: Some(test_url("http://example.com")), body: None }, Retryability::Transient)]
    #[case::status_404(NetError::Status { status: nz(404), url: Some(test_url("http://example.com")), body: None }, Retryability::Fatal)]
    #[case::decode(NetError::Decode("invalid body".to_string()), Retryability::Fatal)]
    #[case::unimplemented(NetError::Unimplemented, Retryability::Fatal)]
    #[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) }, Retryability::Fatal)]
    #[case::invalid_content_type(NetError::InvalidContentType("text/html".to_string()), Retryability::Fatal)]
    async fn test_retryability(#[case] error: NetError, #[case] expected: Retryability) {
        assert_eq!(error.retryability(), expected);
    }

    #[kithara::test(tokio)]
    #[case::timeout(NetError::Timeout, "Timeout")]
    #[case::unimplemented(NetError::Unimplemented, "not implemented")]
    #[case::network(NetError::Network("dns failure".to_string()), "Network error: dns failure")]
    #[case::status_with_details(
        NetError::Status { status: nz(404), url: Some(test_url("http://example.com/test")), body: Some("Not found".to_string()) },
        "HTTP 404: Some(\"Not found\") for URL: Some("
    )]
    async fn test_error_display(#[case] error: NetError, #[case] expected_prefix: &str) {
        let display = error.to_string();
        assert!(
            display.starts_with(expected_prefix),
            "Expected display to start with '{}', got '{}'",
            expected_prefix,
            display
        );
    }

    #[kithara::test(tokio)]
    async fn test_retry_exhausted_display() {
        let source = Box::new(NetError::Timeout);
        let error = NetError::RetryExhausted {
            source,
            max_retries: 3,
        };

        let display = error.to_string();
        assert!(display.contains("Request failed after 3 retries: Timeout"));
    }

    #[kithara::test(tokio)]
    #[case::timeout(NetError::Timeout)]
    #[case::status(NetError::Status { status: nz(500), url: Some(test_url("http://example.com")), body: None })]
    #[case::network(NetError::Network("reset".to_string()))]
    #[case::unimplemented(NetError::Unimplemented)]
    #[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) })]
    async fn test_error_cloning(#[case] error: NetError) {
        let cloned = error.clone();

        assert_eq!(error.to_string(), cloned.to_string());

        assert_eq!(error.retryability(), cloned.retryability());
    }

    #[kithara::test(tokio)]
    #[case::timeout(NetError::Timeout)]
    #[case::status(NetError::Status { status: nz(404), url: Some(test_url("http://example.com")), body: None })]
    async fn test_error_debug(#[case] error: NetError) {
        let debug_output = format!("{:?}", error);

        match error {
            NetError::Timeout => assert!(debug_output.contains("Timeout")),
            NetError::Status { .. } => assert!(debug_output.contains("Status")),
            _ => (),
        }
    }

    #[kithara::test(tokio)]
    async fn test_net_result_type() {
        let ok_result: NetResult<i32> = Ok(42);
        assert!(ok_result.is_ok());
        assert!(matches!(ok_result, Ok(42)));

        let err_result: NetResult<i32> = Err(NetError::Timeout);
        assert!(err_result.is_err());

        match err_result {
            Err(NetError::Timeout) => (),
            _ => panic!("Expected Timeout error"),
        }
    }
}
