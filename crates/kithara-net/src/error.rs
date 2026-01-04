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

    /// Creates an HTTP error from a reqwest error
    pub fn from_reqwest(error: reqwest::Error) -> Self {
        Self::Http(error.to_string())
    }

    /// Creates a structured HTTP error with status and (optional) response body
    pub fn http_error(status: u16, url: Url, body: Option<String>) -> Self {
        Self::HttpError { status, url, body }
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
                http_err_str.contains("network")
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

    /// Checks if this error indicates a timeout
    pub fn is_timeout(&self) -> bool {
        matches!(self, NetError::Timeout)
    }

    /// Gets the HTTP status code if this is an HTTP status error
    pub fn status_code(&self) -> Option<u16> {
        match self {
            NetError::HttpError { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl From<ReqwestError> for NetError {
    fn from(e: ReqwestError) -> Self {
        NetError::Http(e.to_string())
    }
}
