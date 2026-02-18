use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::StreamExt;
use kithara::net::{ByteStream, Headers, Net, NetError, NetExt, RangeSpec, RetryPolicy};
use kithara_net::mock::NetMock;
use rstest::rstest;
use tokio_util::sync::CancellationToken;
use unimock::{MockFn, Unimock, matching};
use url::Url;

fn success_stream() -> ByteStream {
    let stream = futures::stream::iter(vec![Ok::<_, NetError>(Bytes::from_static(b"success"))]);
    Box::pin(stream)
}

fn should_fail(attempts: &Arc<AtomicUsize>, failures_before_success: usize) -> bool {
    attempts.fetch_add(1, Ordering::SeqCst) < failures_before_success
}

fn leaked<F>(f: F) -> &'static F
where
    F: Send + Sync + 'static,
{
    Box::leak(Box::new(f))
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
                    let mut headers = Headers::new();
                    headers.insert("content-length", "7");
                    Ok(headers)
                }
            })),
    ))
    .no_verify_in_drop()
}

async fn test_all_net_methods_with_retry_net(net: &impl Net) {
    let url = Url::parse("http://example.com").unwrap();

    let result = net.get_bytes(url.clone(), None).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));

    let result = net.stream(url.clone(), None).await;
    assert!(result.is_ok());
    let mut stream = result.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, b"success");

    let range = RangeSpec::new(0, None);
    let result = net.get_range(url.clone(), range, None).await;
    assert!(result.is_ok());
    let mut stream = result.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        assert!(chunk.is_ok());
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, b"success");

    let result = net.head(url, None).await;
    assert!(result.is_ok());
    let headers = result.unwrap();
    assert_eq!(headers.get("content-length"), Some("7"));
}

#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[tokio::test]
async fn test_retryable_errors_success_after_retries(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}

#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[tokio::test]
async fn test_non_retryable_errors_no_retry(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("400 Bad Request".to_string());
    let mock_net = make_retry_mock(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_err());
    assert!(matches!(result.err().unwrap(), NetError::Http(_)));
}

#[rstest]
#[case(2, 1)]
#[case(3, 2)]
#[case(4, 3)]
#[tokio::test]
async fn test_retry_exhaustion(#[case] failures_before_success: usize, #[case] max_retries: u32) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        max_retries,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_err());
    assert!(matches!(result.err().unwrap(), NetError::Http(_)));
}

#[rstest]
#[case(1, Duration::from_millis(100), Duration::from_millis(1000))]
#[case(2, Duration::from_millis(100), Duration::from_millis(1000))]
#[case(3, Duration::from_millis(100), Duration::from_millis(1000))]
#[tokio::test]
async fn test_exponential_backoff_with_max_delay(
    #[case] max_retries: u32,
    #[case] base_delay: Duration,
    #[case] max_delay: Duration,
) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock((max_retries + 1) as usize, error_type);
    let retry_policy = RetryPolicy::new(max_retries, base_delay, max_delay);
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_err());
}

#[rstest]
#[case(1)]
#[case(2)]
#[tokio::test]
async fn test_all_net_methods_with_retry(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    test_all_net_methods_with_retry_net(&retry_net).await;
}

#[rstest]
#[case(1)]
#[case(2)]
#[tokio::test]
async fn test_timeout_retry_chaining(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(10),
        Duration::from_secs(5),
    );

    let net = mock_net
        .with_timeout(Duration::from_secs(5))
        .with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}

#[tokio::test]
async fn test_zero_max_retries() {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock(1, error_type);
    let retry_policy = RetryPolicy::new(0, Duration::from_millis(10), Duration::from_secs(5));
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_err());
    assert!(matches!(result.err().unwrap(), NetError::Http(_)));
}

#[rstest]
#[case(1)]
#[tokio::test]
async fn test_zero_base_delay(#[case] failures_before_success: usize) {
    let error_type = NetError::Http("500 Internal Server Error".to_string());
    let mock_net = make_retry_mock(failures_before_success, error_type);
    let retry_policy = RetryPolicy::new(
        failures_before_success as u32,
        Duration::from_millis(0),
        Duration::from_secs(5),
    );
    let retry_net = mock_net.with_retry(retry_policy, CancellationToken::new());

    let url = Url::parse("http://example.com").unwrap();
    let result = retry_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}
