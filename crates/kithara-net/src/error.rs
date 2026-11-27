use std::{fmt::Write, num::NonZeroU16};

use thiserror::Error;
use url::Url;

pub type NetResult<T> = Result<T, NetError>;

/// Keep at most 200 Unicode characters of an HTTP error body and append
/// its original length with the backend's diagnostic ellipsis.
pub(crate) fn truncate_error_body(mut body: String, ellipsis: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 200;

    let total = body.chars().count();
    if total <= MAX_ERROR_BODY_CHARS {
        return body;
    }
    let cut_at = body
        .char_indices()
        .nth(MAX_ERROR_BODY_CHARS)
        .map_or(body.len(), |(index, _)| index);
    body.truncate(cut_at);
    let _ = write!(body, "{ellipsis}(truncated, {total} chars total)");
    body
}

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
    #[error("Cancelled")]
    Cancelled,
    #[error("Invalid content-type: {0}")]
    InvalidContentType(String),
    #[error("HTTP transport protocol violated: {0}")]
    Protocol(String),
    #[error("no HTTP transport installed in this process")]
    NoTransport,
    #[error("request refused: {0}")]
    Refused(String),
    /// The response declared or delivered more body bytes than the request
    /// allows; the transfer ended there. `bytes` is the declared length, or
    /// what had arrived when the transfer ended.
    #[error("response body of {bytes} bytes, past the {limit} the request allows")]
    TooLarge { bytes: u64, limit: u64 },
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

    /// Whether a later resource request can answer differently, distinct from retrying this request now.
    /// Classifies spent budgets by their underlying cause: transient status/reachability with no delivered
    /// resource may recover. Timeout means the resumed transfer stalled and is terminal for blocking reads;
    /// missing, undecodable or cancelled resources cannot answer differently.
    #[must_use]
    pub fn can_answer_later(&self) -> bool {
        let cause = self.cause();
        match cause {
            Self::Network(_) => true,
            Self::Status { .. } => cause.retryability() == Retryability::Transient,
            _ => false,
        }
    }

    /// The failure a spent retry budget was retrying: [`Self::RetryExhausted`]
    /// resolves to what it wrapped, every other variant to itself.
    fn cause(&self) -> &Self {
        match self {
            Self::RetryExhausted { source, .. } => source.cause(),
            other => other,
        }
    }

    /// Classifies the error for retry decisioning via its typed variant.
    #[must_use]
    pub fn retryability(&self) -> Retryability {
        self.into()
    }

    /// Creates a timeout error.
    #[must_use]
    pub const fn timeout() -> Self {
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
            | NetError::Cancelled
            | NetError::InvalidContentType(_)
            | NetError::Protocol(_)
            | NetError::NoTransport
            | NetError::Refused(_)
            | NetError::TooLarge { .. } => Self::Fatal,
        }
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test]
    #[case::empty(String::new(), "...", String::new())]
    #[case::short("x".repeat(199), "...", "x".repeat(199))]
    #[case::at_limit("x".repeat(200), "...", "x".repeat(200))]
    #[case::ascii(
        "x".repeat(201),
        "...",
        format!("{}...(truncated, 201 chars total)", "x".repeat(200))
    )]
    #[case::unicode_at_limit("\u{e9}".repeat(200), "\u{2026}", "\u{e9}".repeat(200))]
    #[case::unicode(
        "\u{e9}".repeat(201),
        "\u{2026}",
        format!("{}\u{2026}(truncated, 201 chars total)", "\u{e9}".repeat(200))
    )]
    fn error_body_preserves_character_limit_and_backend_suffix(
        #[case] body: String,
        #[case] ellipsis: &str,
        #[case] expected: String,
    ) {
        assert_eq!(truncate_error_body(body, ellipsis), expected);
    }

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
    #[case::retry_exhausted(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) }, Retryability::Fatal)]
    #[case::invalid_content_type(NetError::InvalidContentType("text/html".to_string()), Retryability::Fatal)]
    #[case::protocol(NetError::Protocol("a second response".to_string()), Retryability::Fatal)]
    #[case::no_transport(NetError::NoTransport, Retryability::Fatal)]
    #[case::refused(NetError::Refused("cleartext not permitted".to_string()), Retryability::Fatal)]
    #[case::too_large(NetError::TooLarge { bytes: 9, limit: 8 }, Retryability::Fatal)]
    async fn test_retryability(#[case] error: NetError, #[case] expected: Retryability) {
        assert_eq!(error.retryability(), expected);
    }

    /// A spent budget says nothing about the resource, so the question is put to
    /// what the budget was retrying — however many times the promotion was
    /// applied. A refusal and an absent host can answer differently later; a
    /// stalled transfer, a missing resource and a cancel cannot.
    #[kithara::test(tokio)]
    #[case::refused(NetError::Status { status: nz(503), url: None, body: None }, true)]
    #[case::exhausted_on_refusal(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Status { status: nz(503), url: None, body: None }) }, true)]
    #[case::exhausted_on_throttle(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Status { status: nz(429), url: None, body: None }) }, true)]
    #[case::exhausted_on_transport(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Network("connection closed".to_string())) }, true)]
    #[case::exhausted_twice(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Network("connection closed".to_string())) }) }, true)]
    #[case::stalled_transfer(NetError::Timeout, false)]
    #[case::exhausted_on_stall(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Timeout) }, false)]
    #[case::missing(NetError::RetryExhausted { max_retries: 3, source: Box::new(NetError::Status { status: nz(404), url: None, body: None }) }, false)]
    #[case::undecodable(NetError::Decode("bad box".to_string()), false)]
    #[case::cancelled(NetError::Cancelled, false)]
    async fn can_answer_later_outlives_the_budget(#[case] error: NetError, #[case] expected: bool) {
        assert_eq!(error.can_answer_later(), expected);
    }

    #[kithara::test(tokio)]
    #[case::timeout(NetError::Timeout, "Timeout")]
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
