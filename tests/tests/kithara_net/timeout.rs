use std::time::Duration;

use bytes::Bytes;
use kithara::net::{Net, NetError, NetExt};
use kithara_net::mock::NetMock;
use rstest::rstest;
use unimock::{MockFn, Unimock, matching};

use super::fixture::{
    DelayedNet, assert_success_all_net_methods, leaked, ok_headers, success_stream, test_url,
};

fn mock_error() -> NetError {
    NetError::Http("mock error".to_string())
}

fn assert_bytes_or_timeout(result: Result<Bytes, NetError>, should_succeed: bool) {
    if should_succeed {
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
    } else {
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

fn make_timeout_mock(should_succeed: bool) -> Unimock {
    Unimock::new((
        NetMock::get_bytes
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_succeed {
                    Ok(Bytes::from_static(b"success"))
                } else {
                    Err(mock_error())
                }
            })),
        NetMock::stream
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_succeed {
                    Ok(success_stream())
                } else {
                    Err(mock_error())
                }
            })),
        NetMock::get_range
            .some_call(matching!(_, _, _))
            .answers(leaked(move |_, _url, _range, _headers| {
                if should_succeed {
                    Ok(success_stream())
                } else {
                    Err(mock_error())
                }
            })),
        NetMock::head
            .some_call(matching!(_, _))
            .answers(leaked(move |_, _url, _headers| {
                if should_succeed {
                    Ok(ok_headers())
                } else {
                    Err(mock_error())
                }
            })),
    ))
    .no_verify_in_drop()
}

#[rstest]
#[case::success_before_timeout(Duration::from_millis(100), Duration::from_millis(200), true)]
#[case::timeout_before_success(Duration::from_millis(200), Duration::from_millis(100), false)]
#[case::zero_delay(Duration::from_millis(0), Duration::from_millis(100), true)]
#[case::large_timeout(Duration::from_millis(1000), Duration::from_millis(10), false)]
#[tokio::test]
async fn test_timeout_scenarios(
    #[case] delay: Duration,
    #[case] timeout: Duration,
    #[case] should_succeed: bool,
) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(timeout);

    let url = test_url();
    let result = timeout_net.get_bytes(url, None).await;

    assert_bytes_or_timeout(result, should_succeed);
}

#[tokio::test]
async fn test_timeout_with_error() {
    let delay = Duration::from_millis(100);
    let timeout = Duration::from_millis(200);
    let mock_net = DelayedNet::new(make_timeout_mock(false), delay);
    let timeout_net = mock_net.with_timeout(timeout);

    let url = test_url();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    if delay < timeout {
        assert!(matches!(error, NetError::Http(_)));
    } else {
        assert!(matches!(error, NetError::Timeout));
    }
}

#[rstest]
#[case(Duration::from_millis(100), Duration::from_millis(200), true)]
#[case(Duration::from_millis(200), Duration::from_millis(100), false)]
#[tokio::test]
async fn test_all_net_methods_with_timeout(
    #[case] delay: Duration,
    #[case] timeout: Duration,
    #[case] should_succeed: bool,
) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(timeout);

    if should_succeed {
        assert_success_all_net_methods(&timeout_net).await;
    } else {
        let url = test_url();
        let result = timeout_net.get_bytes(url, None).await;
        assert!(result.is_err());
        assert!(matches!(result.err().unwrap(), NetError::Timeout));
    }
}

#[rstest]
#[case(Duration::from_millis(0), true)]
#[case(Duration::from_millis(1), false)]
#[case(Duration::from_millis(100), false)]
#[tokio::test]
async fn test_zero_timeout(#[case] delay: Duration, #[case] should_succeed: bool) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_millis(0));

    let url = test_url();
    let result = timeout_net.get_bytes(url, None).await;

    assert_bytes_or_timeout(result, should_succeed);
}

#[rstest]
#[case(Duration::from_millis(0))]
#[case(Duration::from_millis(100))]
#[case(Duration::from_millis(1000))]
#[case(Duration::from_millis(5000))]
#[tokio::test]
async fn test_large_timeout(#[case] delay: Duration) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(10));

    let url = test_url();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Bytes::from_static(b"success"));
}

#[rstest]
#[case(Duration::from_millis(50))]
#[case(Duration::from_millis(100))]
#[case(Duration::from_millis(200))]
#[tokio::test]
async fn test_timeout_preserves_error(#[case] delay: Duration) {
    let mock_net = DelayedNet::new(make_timeout_mock(false), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(1));

    let url = test_url();
    let result = timeout_net.get_bytes(url, None).await;

    assert!(result.is_err());
    let error = result.err().unwrap();

    assert!(matches!(error, NetError::Http(_)));
    assert!(error.to_string().contains("mock error"));
}

#[rstest]
#[case::fast_delay(100, 200, true)]
#[case::slow_delay(200, 100, false)]
#[case::quick_success(50, 100, true)]
#[case::moderate_timeout(150, 100, false)]
#[case::zero_delay(0, 100, true)]
#[case::large_delay(1000, 10, false)]
#[tokio::test]
async fn test_timeout_representative_scenarios(
    #[case] delay_ms: u64,
    #[case] timeout_ms: u64,
    #[case] should_succeed: bool,
) {
    let mock_net = DelayedNet::new(make_timeout_mock(true), Duration::from_millis(delay_ms));
    let timeout_net = mock_net.with_timeout(Duration::from_millis(timeout_ms));

    let url = test_url();
    let result = timeout_net.get_bytes(url, None).await;

    assert_bytes_or_timeout(result, should_succeed);
}
