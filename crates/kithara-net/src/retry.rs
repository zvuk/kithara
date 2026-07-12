use std::future::Future;

use async_trait::async_trait;
use bytes::Bytes;
use kithara_platform::{
    CancelToken,
    time::{Duration, sleep},
    tokio,
};
use url::Url;

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

use crate::{
    ByteStream,
    error::{NetError, Retryability},
    observe::Observer,
    traits::Net,
    types::{Headers, RangeSpec, RetryPolicy},
};

pub struct DefaultRetryPolicy {
    policy: RetryPolicy,
}

impl DefaultRetryPolicy {
    pub fn new(policy: RetryPolicy) -> Self {
        Self { policy }
    }

    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        self.policy.delay_for_attempt(attempt)
    }

    pub fn should_retry(&self, error: &NetError, attempt: u32) -> bool {
        if attempt >= self.policy.max_retries {
            return false;
        }

        error.retryability() == Retryability::Transient
    }
}

/// Retry decorator for Net implementations
pub struct RetryNet<N, P> {
    cancel: CancelToken,
    inner: N,
    observer: Option<Observer>,
    retry_policy: P,
}

impl<N: Net, P: RetryPolicyTrait> RetryNet<N, P> {
    pub fn new(inner: N, retry_policy: P, cancel: CancelToken, observer: Option<Observer>) -> Self {
        Self {
            cancel,
            inner,
            observer,
            retry_policy,
        }
    }

    async fn retry_loop<F, Fut, T>(&self, mut op: F) -> Result<T, NetError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, NetError>>,
    {
        let max = self.retry_policy.max_attempts();
        for attempt in 0..=max {
            let error = match op().await {
                Ok(value) => return Ok(value),
                Err(error) => error,
            };
            if self.retry_policy.should_retry(&error, attempt) {
                let delay = self.retry_policy.delay_for_attempt(attempt);
                if let Some(observer) = self.observer.as_ref() {
                    observer.0.retrying(attempt + 1, max, &error, delay);
                }
                tokio::select! {
                    biased;
                    () = self.cancel.cancelled() => return Err(NetError::Cancelled),
                    () = sleep(delay) => {}
                }
                continue;
            }
            // Not retrying. A transient error after a NON-ZERO budget was
            // spent is promoted to a terminal `RetryExhausted` (Fatal) so
            // downstream (HLS settle, readers) treats it as a give-up, not a
            // transient retry signal. A genuinely fatal error (4xx, decode,
            // cancel) surfaces as-is; so does a transient error under
            // `max_retries == 0`, where the decorator is a deliberate
            // pass-through (no retries were promised, none were exhausted).
            return Err(
                if max > 0 && error.retryability() == Retryability::Transient {
                    if let Some(observer) = self.observer.as_ref() {
                        observer.0.retry_exhausted(max, 0, &error);
                    }
                    NetError::RetryExhausted {
                        max_retries: max,
                        source: Box::new(error),
                    }
                } else {
                    error
                },
            );
        }
        // The `0..=max` loop always returns on its final iteration; this is an
        // unreachable terminal safety net.
        Err(NetError::RetryExhausted {
            max_retries: max,
            source: Box::new(NetError::Unimplemented),
        })
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<N: Net, P: RetryPolicyTrait> Net for RetryNet<N, P> {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError> {
        self.retry_loop(|| self.inner.get_bytes(url.clone(), headers.clone()))
            .await
    }

    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError> {
        self.retry_loop(|| {
            self.inner
                .get_range(url.clone(), range.clone(), headers.clone())
        })
        .await
    }

    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError> {
        self.retry_loop(|| self.inner.head(url.clone(), headers.clone()))
            .await
    }

    async fn post_bytes(
        &self,
        url: Url,
        body: Bytes,
        headers: Option<Headers>,
    ) -> Result<Bytes, NetError> {
        self.retry_loop(|| {
            self.inner
                .post_bytes(url.clone(), body.clone(), headers.clone())
        })
        .await
    }

    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError> {
        self.retry_loop(|| self.inner.stream(url.clone(), headers.clone()))
            .await
    }
}

#[kithara::mock(api = RetryPolicyMock)]
pub trait RetryPolicyTrait: Send + Sync {
    fn delay_for_attempt(&self, attempt: u32) -> Duration;
    fn max_attempts(&self) -> u32;
    fn should_retry(&self, error: &NetError, attempt: u32) -> bool;
}

impl RetryPolicyTrait for DefaultRetryPolicy {
    fn delay_for_attempt(&self, attempt: u32) -> Duration {
        self.delay_for_attempt(attempt)
    }

    fn max_attempts(&self) -> u32 {
        self.policy.max_retries
    }

    fn should_retry(&self, error: &NetError, attempt: u32) -> bool {
        self.should_retry(error, attempt)
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{num::NonZeroU16, task::Poll};

    use futures::{pin_mut, poll, stream};
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::traits::NetMock;

    fn test_url() -> Url {
        Url::parse("http://test.com").expect("BUG: hard-coded test URL is valid")
    }

    fn status_404() -> NetError {
        NetError::Status {
            status: NonZeroU16::new(404).expect("BUG: 404 is non-zero"),
            url: None,
            body: None,
        }
    }

    fn empty_stream() -> ByteStream {
        ByteStream::new(Headers::new(), Box::pin(stream::empty()))
    }

    fn fast_retry_policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_secs(1),
        }
    }

    fn retry_net(mock: Unimock, policy: RetryPolicy) -> RetryNet<Unimock, DefaultRetryPolicy> {
        RetryNet::new(
            mock,
            DefaultRetryPolicy::new(policy),
            CancelToken::never(),
            None,
        )
    }

    fn retry_net_default(mock: Unimock) -> RetryNet<Unimock, DefaultRetryPolicy> {
        retry_net(mock, RetryPolicy::default())
    }

    #[kithara::test]
    fn test_default_retry_policy_new() {
        let policy = RetryPolicy::default();
        let retry_policy = DefaultRetryPolicy::new(policy);
        assert_eq!(retry_policy.policy.max_retries, 3);
    }

    #[kithara::test]
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

    #[kithara::test]
    fn test_default_retry_policy_should_not_retry_non_retryable() {
        let policy = RetryPolicy::default();
        let retry_policy = DefaultRetryPolicy::new(policy);
        let error = status_404();
        assert!(!retry_policy.should_retry(&error, 0));
    }

    #[kithara::test]
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

    #[kithara::test(tokio)]
    async fn test_retry_net_get_bytes_success_first_try() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Ok(Bytes::from("success"))),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
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
        let retry_net = retry_net(mock, fast_retry_policy(3));

        let url = test_url();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_get_bytes_max_retries_exhausted() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .each_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
        );
        let retry_net = retry_net(mock, fast_retry_policy(2));

        let url = test_url();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_err());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_get_bytes_non_retryable_error() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .some_call(matching!(_, _))
                .returns(Err(status_404())),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let result = retry_net.get_bytes(url, None).await;

        assert!(result.is_err());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_post_bytes_success_first_try() {
        let mock = Unimock::new(
            NetMock::post_bytes
                .some_call(matching!(_, _, _))
                .returns(Ok(Bytes::from("created"))),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let result = retry_net
            .post_bytes(url, Bytes::from("payload"), None)
            .await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_post_bytes_retry_then_success() {
        let mock = Unimock::new((
            NetMock::post_bytes
                .next_call(matching!(_, _, _))
                .returns(Err(NetError::Timeout)),
            NetMock::post_bytes
                .next_call(matching!(_, _, _))
                .returns(Err(NetError::Timeout)),
            NetMock::post_bytes
                .next_call(matching!(_, _, _))
                .returns(Ok(Bytes::from("created"))),
        ));
        let retry_net = retry_net(mock, fast_retry_policy(3));

        let url = test_url();
        let result = retry_net
            .post_bytes(url, Bytes::from("payload"), None)
            .await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_post_bytes_non_retryable_error() {
        let mock = Unimock::new(
            NetMock::post_bytes
                .some_call(matching!(_, _, _))
                .returns(Err(status_404())),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let result = retry_net
            .post_bytes(url, Bytes::from("payload"), None)
            .await;

        assert!(result.is_err());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_stream_success() {
        let mock = Unimock::new(
            NetMock::stream
                .some_call(matching!(_, _))
                .answers(&|_, _, _| Ok(empty_stream())),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let result = retry_net.stream(url, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
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
                .answers(&|_, _, _| Ok(empty_stream())),
        ));
        let retry_net = retry_net(mock, fast_retry_policy(3));

        let url = test_url();
        let result = retry_net.stream(url, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_get_range_success() {
        let mock = Unimock::new(
            NetMock::get_range
                .some_call(matching!(_, _, _))
                .answers(&|_, _, _, _| Ok(empty_stream())),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let range = RangeSpec::new(0, None);
        let result = retry_net.get_range(url, range, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
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
                .answers(&|_, _, _, _| Ok(empty_stream())),
        ));
        let retry_net = retry_net(mock, fast_retry_policy(3));

        let url = test_url();
        let range = RangeSpec::new(0, None);
        let result = retry_net.get_range(url, range, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
    async fn test_retry_net_head_success() {
        let mock = Unimock::new(
            NetMock::head
                .some_call(matching!(_, _))
                .returns(Ok(Headers::new())),
        );
        let retry_net = retry_net_default(mock);

        let url = test_url();
        let result = retry_net.head(url, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test(tokio)]
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
        let retry_net = retry_net(mock, fast_retry_policy(3));

        let url = test_url();
        let result = retry_net.head(url, None).await;

        assert!(result.is_ok());
    }

    #[kithara::test]
    fn test_retry_policy_trait_max_attempts() {
        let policy = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            max_retries: 5,
        };
        let retry_policy = DefaultRetryPolicy::new(policy);
        assert_eq!(retry_policy.max_attempts(), 5);
    }

    #[kithara::test]
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

    #[kithara::test(tokio)]
    async fn test_retry_net_cancel_interrupts_sleep() {
        let mock = Unimock::new(
            NetMock::get_bytes
                .each_call(matching!(_, _))
                .returns(Err(NetError::Timeout)),
        );
        let policy = RetryPolicy {
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(10),
            max_retries: 3,
        };
        let cancel = CancelToken::never();
        let retry_net = RetryNet::new(mock, DefaultRetryPolicy::new(policy), cancel.clone(), None);

        let url = test_url();
        let result = retry_net.get_bytes(url, None);
        pin_mut!(result);

        assert!(
            matches!(poll!(result.as_mut()), Poll::Pending),
            "retry must park in the retry-delay branch before cancellation"
        );
        cancel.cancel();

        let result = result.await;

        assert!(matches!(result, Err(NetError::Cancelled)));
    }
}
