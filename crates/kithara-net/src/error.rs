use std::num::NonZeroU16;

use thiserror::Error;
use url::Url;

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
    #[error("HTTP transport protocol violated: {0}")]
    Protocol(String),
    #[error("no HTTP transport installed in this process")]
    NoTransport,
    #[error("request refused: {0}")]
    Refused(String),
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

    /// The failure a spent retry budget was retrying: [`Self::RetryExhausted`]
    /// resolves to what it wrapped, every other variant to itself.
    fn cause(&self) -> &Self {
        match self {
            Self::RetryExhausted { source, .. } => source.cause(),
            other => other,
        }
    }

    /// Whether asking again later can answer differently.
    ///
    /// [`Self::retryability`] answers whether *this request* may be retried
    /// now, which is why a spent budget is [`Retryability::Fatal`] there. This
    /// is the other question, asked by callers that hold a resource and can
    /// come back to it: a track waiting to be played, a segment slot a reader
    /// will want again. A spent budget is classified by what it was retrying.
    ///
    /// True when nothing of the resource was delivered and the obstacle was
    /// reachability, which changes on its own: a host that refused with a
    /// transient status, or one that was not there at all.
    ///
    /// False for a timeout. A transfer that established and then stopped
    /// delivering has already been retried and resumed by the resilient body,
    /// so its exhaustion is a verdict about a server that answers without
    /// delivering — and the layers that own giving up (the segment slot, and
    /// through it every blocking read above it) have nothing else to hear it
    /// from. Also false for everything a later ask cannot change: a missing
    /// resource, a body that will not decode, a cancel.
    #[must_use]
    pub fn can_answer_later(&self) -> bool {
        let cause = self.cause();
        match cause {
            Self::Network(_) => true,
            Self::Status { .. } => cause.retryability() == Retryability::Transient,
            _ => false,
        }
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
            | NetError::Unimplemented
            | NetError::Cancelled
            | NetError::InvalidContentType(_)
            | NetError::Protocol(_)
            | NetError::NoTransport
            | NetError::Refused(_) => Self::Fatal,
        }
    }
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
    #[case::protocol(NetError::Protocol("a second response".to_string()), Retryability::Fatal)]
    #[case::no_transport(NetError::NoTransport, Retryability::Fatal)]
    #[case::refused(NetError::Refused("cleartext not permitted".to_string()), Retryability::Fatal)]
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
