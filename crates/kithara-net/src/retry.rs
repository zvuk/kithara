use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::time::sleep;
use url::Url;

use crate::{
    ByteStream,
    error::NetError,
    traits::Net,
    types::{Headers, RangeSpec, RetryPolicy},
};

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
        error.is_retryable()
    }
}

pub struct DefaultRetryPolicy {
    classifier: DefaultRetryClassifier,
    policy: RetryPolicy,
}

impl DefaultRetryPolicy {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            classifier: DefaultRetryClassifier,
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
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        // Updated for HttpClient compatibility: no major changes needed as it decorates Net
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_attempts() {
            match self.inner.get_bytes(url.clone(), headers.clone()).await {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    if !self.retry_policy.should_retry(&error, attempt) {
                        return Err(error);
                    }
                    last_error = Some(error.clone());

                    if attempt < self.retry_policy.max_attempts() {
                        let delay = self.retry_policy.delay_for_attempt(attempt);
                        sleep(delay).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| NetError::RetryExhausted {
            max_retries: self.retry_policy.max_attempts(),
            source: Box::new(NetError::Unimplemented),
        }))
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        // Ensured short names are used
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
                        sleep(delay).await;
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
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        // Compatible with HttpClient via Net trait
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
                        sleep(delay).await;
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
