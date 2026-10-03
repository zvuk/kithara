#![cfg(not(target_arch = "wasm32"))]

use bytes::Bytes;
use kithara_net::{Net, NetError, NetExt, mock::NetMock};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use unimock::{MockFn, Unimock, matching};

use crate::support::{
    DelayedNet, assert_success_all_net_methods, leaked, ok_headers, success_stream, test_url,
};

fn mock_error() -> NetError {
    NetError::Network("mock error".to_string())
}

/// Every branch names the value it rejected: a bare `is_ok`/`matches!` reports
/// only that the shape was wrong, and the error the decorator produced is the
/// one fact that says why.
fn assert_bytes_or_timeout(result: Result<Bytes, NetError>, should_succeed: bool) {
    match (should_succeed, result) {
        (true, Ok(bytes)) => assert_eq!(bytes, Bytes::from_static(b"success")),
        (true, Err(error)) => {
            panic!("the guarded call was expected to succeed, it failed: {error}")
        }
        (false, Err(NetError::Timeout)) => {}
        (false, Err(error)) => {
            panic!("the deadline was expected to fire, the call failed on: {error}")
        }
        (false, Ok(bytes)) => {
            panic!("the deadline was expected to fire, the call returned {bytes:?}")
        }
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
        NetMock::post_bytes
            .some_call(matching!(_, _, _))
            .answers(leaked(move |_, _url, _body, _headers| {
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

#[kithara::test(tokio)]
#[case::success_before_timeout(Duration::from_millis(100), Duration::from_millis(200), true)]
#[case::timeout_before_success(Duration::from_millis(200), Duration::from_millis(100), false)]
#[case::zero_delay(Duration::from_millis(0), Duration::from_millis(100), true)]
#[case::large_timeout(Duration::from_millis(1000), Duration::from_millis(10), false)]
#[case::quick_success(Duration::from_millis(50), Duration::from_millis(100), true)]
#[case::moderate_timeout(Duration::from_millis(150), Duration::from_millis(100), false)]
#[case::zero_timeout_immediate(Duration::ZERO, Duration::ZERO, true)]
#[case::zero_timeout_delayed_50(Duration::from_millis(50), Duration::ZERO, false)]
#[case::zero_timeout_delayed_100(Duration::from_millis(100), Duration::ZERO, false)]
#[case::large_timeout_immediate(Duration::ZERO, Duration::from_secs(10), true)]
#[case::large_timeout_100(Duration::from_millis(100), Duration::from_secs(10), true)]
#[case::large_timeout_1000(Duration::from_millis(1000), Duration::from_secs(10), true)]
#[case::large_timeout_5000(Duration::from_millis(5000), Duration::from_secs(10), true)]
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

/// The delay is below the deadline, so the inner failure is what has to travel:
/// the decorator must not restate it as its own timeout.
#[kithara::test(tokio)]
async fn test_timeout_with_error() {
    let mock_net = DelayedNet::new(make_timeout_mock(false), Duration::from_millis(100));
    let timeout_net = mock_net.with_timeout(Duration::from_millis(200));

    let url = test_url();
    let error = timeout_net
        .get_bytes(url, None)
        .await
        .expect_err("the mock answers every call with an error");

    assert!(
        matches!(error, NetError::Network(_)),
        "the decorator replaced the failure the call produced: {error}"
    );
}

#[kithara::test(tokio)]
async fn test_all_net_methods_with_timeout() {
    let mock_net = DelayedNet::new(make_timeout_mock(true), Duration::from_millis(100));
    let timeout_net = mock_net.with_timeout(Duration::from_millis(200));
    assert_success_all_net_methods(&timeout_net).await;
}

#[kithara::test(tokio)]
#[case(Duration::from_millis(50))]
#[case(Duration::from_millis(100))]
#[case(Duration::from_millis(200))]
async fn test_timeout_preserves_error(#[case] delay: Duration) {
    let mock_net = DelayedNet::new(make_timeout_mock(false), delay);
    let timeout_net = mock_net.with_timeout(Duration::from_secs(1));

    let url = test_url();
    let error = timeout_net
        .get_bytes(url, None)
        .await
        .expect_err("the mock answers every call with an error");

    assert!(
        matches!(error, NetError::Network(_)),
        "the decorator replaced the failure the call produced: {error}"
    );
    assert!(
        error.to_string().contains("mock error"),
        "the message the mock wrote did not survive the decorator: {error}"
    );
}
