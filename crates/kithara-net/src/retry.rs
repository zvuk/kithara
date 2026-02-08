use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::time::sleep;
#[cfg(test)]
use unimock::unimock;
use url::Url;

use crate::{
    ByteStream,
    error::NetError,
    traits::Net,
    types::{Headers, RangeSpec, RetryPolicy},
};

#[cfg_attr(test, unimock(api = RetryClassifierMock))]
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

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        let mut last_error = None;

        for attempt in 0..=self.retry_policy.max_attempts() {
            match self.inner.head(url.clone(), headers.clone()).await {
                Ok(out) => return Ok(out),
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

#[cfg_attr(test, unimock(api = RetryPolicyMock))]
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

#[cfg(test)]
mod tests {
    use rstest::*;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::traits::NetMock;

    // DefaultRetryClassifier Tests

    #[rstest]
    fn test_default_retry_classifier_new() {
        let classifier = DefaultRetryClassifier::new();
        let _ = classifier;
    }

    #[rstest]
    fn test_default_retry_classifier_default() {
        let classifier = DefaultRetryClassifier::default();
        let _ = classifier;
    }

    #[rstest]
    #[case(NetError::Timeout, true, "timeout should retry")]
    #[case(NetError::Http("status: 500".to_string()), true, "500 should retry")]
    #[case(NetError::Http("status: 503".to_string()), true, "503 should retry")]
    #[case(NetError::Http("timeout occurred".to_string()), true, "timeout in message should retry")]
    #[case(NetError::Http("connection error".to_string()), true, "connection error should retry")]
    #[case(NetError::Http("status: 404".to_string()), false, "404 should not retry")]
    #[case(NetError::Http("status: 400".to_string()), false, "400 should not retry")]
    fn test_default_retry_classifier_should_retry(
        #[case] error: NetError,
        #[case] expected: bool,
        #[case] _desc: &str,
    ) {
        let classifier = DefaultRetryClassifier::new();
        assert_eq!(classifier.should_retry(&error), expected);
    }

    // DefaultRetryPolicy Tests

    #[rstest]
    fn test_default_retry_policy_new() {
        let policy = RetryPolicy::default();
        let retry_policy = DefaultRetryPolicy::new(policy);
        assert_eq!(retry_policy.policy.max_retries, 3);
    }

    #[rstest]
    #[case(0, true, "first attempt should retry")]
    #[case(1, true, "second attempt should retry")]
    #[case(2, true, "third attempt should retry")]
    #[case(3, false, "fourth attempt should not retry (max=3)")]
    #[case(4, false, "fifth attempt should not retry")]
    fn test_default_retry_policy_should_retry_max_retries(
        #[case] attempt: u32,
        #[case] expected: bool,
        #[case] _desc: &str,
    ) {
        let policy = RetryPolicy::default();
        let retry_policy = DefaultRetryPolicy::new(policy);
        let error = NetError::Timeout;
        assert_eq!(retry_policy.should_retry(&error, attempt), expected);
    }

    #[rstest]
    fn test_default_retry_policy_should_not_retry_non_retryable() {
        let policy = RetryPolicy::default();
        let retry_policy = DefaultRetryPolicy::new(policy);
        let error = NetError::Http("status: 404".to_string());
        assert!(!retry_policy.should_retry(&error, 0));
    }

    #[rstest]
    #[case(0, Duration::ZERO, "first attempt no delay")]
    #[case(1, Duration::from_millis(100), "second attempt base delay")]
    #[case(2, Duration::from_millis(200), "third attempt 2x delay")]
    #[case(3, Duration::from_millis(400), "fourth attempt 4x delay")]
    fn test_default_retry_policy_delay_for_attempt(
        #[case] attempt: u32,
        #[case] expected: Duration,
        #[case] _desc: &str,
    ) {
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            max_retries: 5,
        };
        let retry_policy = DefaultRetryPolicy::new(policy);
        assert_eq!(retry_policy.delay_for_attempt(attempt), expected);
    }

    // RetryNet Tests - get_bytes

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_get_bytes_success_first_try() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Ok(Bytes::from("success"))),
        );
        let policy = RetryPolicy::default();
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_ok());
    }

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_get_bytes_retry_then_success() {
        let mock = Unimock::new((
            NetMock::get_bytes
                .next_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
            NetMock::get_bytes
                .next_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
            NetMock::get_bytes
                .next_call(matching!(_, _))
                .returns(Ok(Bytes::from("success"))),
        ));
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            max_retries: 3,
        };
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_ok());
    }

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_get_bytes_max_retries_exhausted() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .each_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
        );
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            max_retries: 2,
        };
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_err());
    }

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_get_bytes_non_retryable_error() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Err(NetError::Http("status: 404".to_string()))),
        );
        let policy = RetryPolicy::default();
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_err());
    }

    // RetryNet Tests - stream

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_stream_success() {
        let mock = Unimock::new(
            NetMock::stream
                .some_call(matching!(_, _))
                .answers(&|_, _, _| {
                    use futures::stream;
                    Ok(Box::pin(stream::empty()) as ByteStream)
                }),
        );
        let policy = RetryPolicy::default();
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.stream(url, None).await;

        assert!(result.is_ok());
    }

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_stream_retry_then_success() {
        let mock = Unimock::new((
            NetMock::stream
                .next_call(matching!(_, _))
                .answers(&|_, _, _| Err(NetError::Timeout)),
            NetMock::stream
                .next_call(matching!(_, _))
                .answers(&|_, _, _| Err(NetError::Timeout)),
            NetMock::stream
                .next_call(matching!(_, _))
                .answers(&|_, _, _| {
                    use futures::stream;
                    Ok(Box::pin(stream::empty()) as ByteStream)
                }),
        ));
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            max_retries: 3,
        };
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.stream(url, None).await;

        assert!(result.is_ok());
    }

    // RetryNet Tests - get_range

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_get_range_success() {
        let mock = Unimock::new(NetMock::get_range.some_call(matching!(_, _, _)).answers(
            &|_, _, _, _| {
                use futures::stream;
                Ok(Box::pin(stream::empty()) as ByteStream)
            },
        ));
        let policy = RetryPolicy::default();
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let range = RangeSpec::from_start(0);
        let result = retry_net.get_range(url, range, None).await;

        assert!(result.is_ok());
    }

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_get_range_retry_then_success() {
        let mock = Unimock::new((
            NetMock::get_range
                .next_call(matching!(_, _, _))
                .answers(&|_, _, _, _| Err(NetError::Timeout)),
            NetMock::get_range
                .next_call(matching!(_, _, _))
                .answers(&|_, _, _, _| Err(NetError::Timeout)),
            NetMock::get_range
                .next_call(matching!(_, _, _))
                .answers(&|_, _, _, _| {
                    use futures::stream;
                    Ok(Box::pin(stream::empty()) as ByteStream)
                }),
        ));
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            max_retries: 3,
        };
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let range = RangeSpec::from_start(0);
        let result = retry_net.get_range(url, range, None).await;

        assert!(result.is_ok());
    }

    // RetryNet Tests - head

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_head_success() {
        let mock = Unimock::new(
            NetMock::head
                .some_call(matching!(_, _))
                .returns(Ok(Headers::new())),
        );
        let policy = RetryPolicy::default();
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.head(url, None).await;

        assert!(result.is_ok());
    }

    #[rstest]
    #[tokio::test]
    async fn test_retry_net_head_retry_then_success() {
        let mock = Unimock::new((
            NetMock::head
                .next_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
            NetMock::head
                .next_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
            NetMock::head
                .next_call(matching!(_, _))
                .returns(Ok(Headers::new())),
        ));
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
            max_retries: 3,
        };
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy));

        let url = Url::parse("http://test.com").unwrap();
        let result = retry_net.head(url, None).await;

        assert!(result.is_ok());
    }

    // RetryPolicyTrait Tests

    #[rstest]
    fn test_retry_policy_trait_max_attempts() {
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            max_retries: 5,
        };
        let retry_policy = DefaultRetryPolicy::new(policy);
        assert_eq!(retry_policy.max_attempts(), 5);
    }

    #[rstest]
    fn test_retry_policy_trait_delay() {
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(10),
            max_retries: 3,
        };
        let retry_policy = DefaultRetryPolicy::new(policy);
        assert_eq!(retry_policy.delay_for_attempt(0), Duration::ZERO);
        assert_eq!(retry_policy.delay_for_attempt(1), Duration::from_millis(50));
    }
}
