use async_trait::async_trait;
use std::time::Duration;

use crate::traits::Net;
use crate::types::{Headers, RangeSpec, RetryPolicy};
use crate::{ByteStream, NetError};

pub trait RetryClassifier {
    fn should_retry(&self, error: &NetError) -> bool;
}

pub struct DefaultRetryClassifier;

impl DefaultRetryClassifier {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DefaultRetryClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl RetryClassifier for DefaultRetryClassifier {
    fn should_retry(&self, error: &NetError) -> bool {
        match error {
            NetError::Http(http_err_str) => {
                // For string-based HTTP errors, we check if the error string contains
                // indicators of retryable HTTP status codes
                // This is a simplified approach - a production implementation might
                // parse status codes more precisely
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
            NetError::HttpStatus { status, .. } => {
                // Retry on 5xx server errors and 429 Too Many Requests
                *status >= 500 || *status == 429 || *status == 408
            }
            NetError::InvalidRange(_) | NetError::Unimplemented => false,
        }
    }
}

pub struct DefaultRetryPolicy {
    classifier: DefaultRetryClassifier,
    policy: RetryPolicy,
}

impl DefaultRetryPolicy {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            classifier: DefaultRetryClassifier::default(),
            policy,
        }
    }

    pub fn should_retry(&self, error: &NetError, attempt: u32) -> bool {
        if attempt >= self.policy.max_retries {
            return false;
        }

        self.classifier.should_retry(error)
    }

    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        self.policy.delay_for_attempt(attempt)
    }
}

/// Retry decorator for Net implementations
pub struct RetryNet<N, P> {
    inner: N,
    retry_policy: P,
}

impl<N: Net, P: RetryPolicyTrait> RetryNet<N, P> {
    pub fn new(inner: N, retry_policy: P) -> Self {
        Self {
            inner,
            retry_policy,
        }
    }
}

#[async_trait]
impl<N: Net, P: RetryPolicyTrait> Net for RetryNet<N, P> {
    async fn get_bytes(&self, url: url::Url) -> Result<bytes::Bytes, NetError> {
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_attempts() {
            match self.inner.get_bytes(url.clone()).await {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    if !self.retry_policy.should_retry(&error, attempt) {
                        return Err(error);
                    }
                    last_error = Some(error.clone());

                    if attempt < self.retry_policy.max_attempts() {
                        let delay = self.retry_policy.delay_for_attempt(attempt);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| NetError::RetryExhausted {
            max_retries: self.retry_policy.max_attempts(),
            source: Box::new(NetError::Unimplemented),
        }))
    }

    async fn stream(
        &self,
        url: url::Url,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_attempts() {
            match self.inner.stream(url.clone(), headers.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    if !self.retry_policy.should_retry(&error, attempt) {
                        return Err(error);
                    }
                    last_error = Some(error.clone());

                    if attempt < self.retry_policy.max_attempts() {
                        let delay = self.retry_policy.delay_for_attempt(attempt);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| NetError::RetryExhausted {
            max_retries: self.retry_policy.max_attempts(),
            source: Box::new(NetError::Unimplemented),
        }))
    }

    async fn get_range(
        &self,
        url: url::Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_attempts() {
            match self
                .inner
                .get_range(url.clone(), range.clone(), headers.clone())
                .await
            {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    if !self.retry_policy.should_retry(&error, attempt) {
                        return Err(error);
                    }
                    last_error = Some(error.clone());

                    if attempt < self.retry_policy.max_attempts() {
                        let delay = self.retry_policy.delay_for_attempt(attempt);
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| NetError::RetryExhausted {
            max_retries: self.retry_policy.max_attempts(),
            source: Box::new(NetError::Unimplemented),
        }))
    }
}

pub trait RetryPolicyTrait: Send + Sync {
    fn should_retry(&self, error: &NetError, attempt: u32) -> bool;
    fn delay_for_attempt(&self, attempt: u32) -> Duration;
    fn max_attempts(&self) -> u32;
}

impl RetryPolicyTrait for DefaultRetryPolicy {
    fn should_retry(&self, error: &NetError, attempt: u32) -> bool {
        self.should_retry(error, attempt)
    }

    fn delay_for_attempt(&self, attempt: u32) -> Duration {
        self.delay_for_attempt(attempt)
    }

    fn max_attempts(&self) -> u32 {
        self.policy.max_retries
    }
}
