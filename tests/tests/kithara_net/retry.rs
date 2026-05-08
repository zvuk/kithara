#![cfg(not(target_arch = "wasm32"))]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use kithara::net::{Net, NetError, NetExt, RetryPolicy};
use kithara_integration_tests::net_fixture::{
    assert_success_all_net_methods, leaked, ok_headers, success_stream, test_url,
};
use kithara_net::mock::NetMock;
use tokio_util::sync::CancellationToken;
use unimock::{MockFn, Unimock, matching};

fn should_fail(attempts: &Arc<AtomicUsize>, failures_before_success: usize) -> bool {
    attempts.fetch_add(1, Ordering::SeqCst) < failures_before_success
}

fn make_retry_mock(failures_before_success: usize, error_type: NetError) -> Unimock {
    let attempts = Arc::new(AtomicUsize::new(0));
    let error = Arc::new(error_type);

    let bytes_attempts = Arc::clone(&attempts);
    let bytes_error = Arc::clone(&error);

    let stream_attempts = Arc::clone(&attempts);
    let stream_error = Arc::clone(&error);

    let range_attempts = Arc::clone(&attempts);
    let range_error = Arc::clone(&error);

    let head_attempts = Arc::clone(&attempts);
    let head_error = Arc::clone(&error);

    Unimock::new((
        NetMock::get_bytes
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_fail(&bytes_attempts, failures_before_success) {
                    Err(bytes_error.as_ref().clone())
                } else {
                    Ok(Bytes::from_static(b"success"))
                }
            })),
        NetMock::stream
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_fail(&stream_attempts, failures_before_success) {
                    Err(stream_error.as_ref().clone())
                } else {
                    Ok(success_stream())
                }
            })),
        NetMock::get_range
            .some_call(matching!(_, _, _))
            .answers(leaked(move |_, _url, _range, _headers| {
                if should_fail(&range_attempts, failures_before_success) {
                    Err(range_error.as_ref().clone())
                } else {
                    Ok(success_stream())
                }
            })),
        NetMock::head
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_fail(&head_attempts, failures_before_success) {
                    Err(head_error.as_ref().clone())
                } else {
                    Ok(ok_headers())
                }
            })),
    ))
    .no_verify_in_drop()
}

/// Shared builder: wrap the mock with the retry adapter so each test can
/// focus on the assertion. `max_retries == failures` is the common case
/// (success path); tests that diverge (e.g. `test_retry_exhaustion`) pass
/// their own value.
async fn try_with_retry(
    failures_before_success: usize,
    error: NetError,
    max_retries: u32,
    base_delay: Duration,
) -> Result<Bytes, NetError> {
    let mock_net = make_retry_mock(failures_before_success, error);
    let retry_policy = RetryPolicy::new(max_retries, base_delay, Duration::from_secs(5));
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());
    retry_net.get_bytes(test_url(), None).await
}

fn http_500() -> NetError {
    NetError::Http("500 Internal Server Error".to_string())
}

#[kithara::test(tokio)]
#[case(1)]
#[case(2)]
#[case(3)]
async fn test_retryable_errors_success_after_retries(#[case] failures_before_success: usize) {
    let result = try_with_retry(
        failures_before_success,
        http_500(),
        failures_before_success as u32,
        Duration::from_millis(10),
    )
    .await;
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}

#[kithara::test(tokio)]
#[case(1)]
#[case(2)]
#[case(3)]
async fn test_non_retryable_errors_no_retry(#[case] failures_before_success: usize) {
    let result = try_with_retry(
        failures_before_success,
        NetError::Http("400 Bad Request".to_string()),
        failures_before_success as u32,
        Duration::from_millis(10),
    )
    .await;
    assert!(matches!(result, Err(NetError::Http(_))));
}

#[kithara::test(tokio)]
#[case(2, 1)]
#[case(3, 2)]
#[case(4, 3)]
async fn test_retry_exhaustion(#[case] failures_before_success: usize, #[case] max_retries: u32) {
    let result = try_with_retry(
        failures_before_success,
        http_500(),
        max_retries,
        Duration::from_millis(10),
    )
    .await;
    assert!(matches!(result, Err(NetError::Http(_))));
}

#[kithara::test(tokio)]
#[case(1, Duration::from_millis(100), Duration::from_millis(1000))]
#[case(2, Duration::from_millis(100), Duration::from_millis(1000))]
#[case(3, Duration::from_millis(100), Duration::from_millis(1000))]
async fn test_exponential_backoff_with_max_delay(
    #[case] max_retries: u32,
    #[case] base_delay: Duration,
    #[case] max_delay: Duration,
) {
    let mock_net = make_retry_mock((max_retries + 1) as usize, http_500());
    let retry_policy = RetryPolicy::new(max_retries, base_delay, max_delay);
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());
    let result = retry_net.get_bytes(test_url(), None).await;
    assert!(result.is_err());
}

#[kithara::test(tokio)]
#[case(1)]
#[case(2)]
async fn test_all_net_methods_with_retry(#[case] failures_before_success: usize) {
    let mock_net = make_retry_mock(failures_before_success, http_500());
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());
    assert_success_all_net_methods(&retry_net).await;
}

#[kithara::test(tokio)]
#[case(1)]
#[case(2)]
async fn test_timeout_retry_chaining(#[case] failures_before_success: usize) {
    let mock_net = make_retry_mock(failures_before_success, http_500());
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let net = mock_net
        .with_timeout(Duration::from_secs(5))
        .with_retry(retry_policy, CancellationToken::new());
    let result = net.get_bytes(test_url(), None).await;
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}

#[kithara::test(tokio)]
async fn test_zero_max_retries() {
    let result = try_with_retry(1, http_500(), 0, Duration::from_millis(10)).await;
    assert!(matches!(result, Err(NetError::Http(_))));
}

#[kithara::test(tokio)]
#[case(1)]
async fn test_zero_base_delay(#[case] failures_before_success: usize) {
    let result = try_with_retry(
        failures_before_success,
        http_500(),
        failures_before_success as u32,
        Duration::from_millis(0),
    )
    .await;
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}
