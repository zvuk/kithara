//! A process that never installed a transport. The binary is its own
//! process under every runner, so no other test can install one first.
#![cfg(feature = "client-host")]

use std::sync::atomic::{AtomicU32, Ordering};

use kithara_net::{HttpClient, NetError, NetObserver, NetOptions, Observer, RetryPolicy};
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_test_utils::{bufpool::pools, kithara};
use url::Url;

#[derive(Default)]
struct Retries(AtomicU32);

impl NetObserver for Retries {
    fn retrying(&self, _attempt: u32, _max_retries: u32, _error: &NetError, _backoff: Duration) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[kithara::test(tokio, timeout(Duration::from_secs(2)))]
async fn a_request_without_a_transport_fails_at_once_without_retrying() {
    let retries = Arc::new(Retries::default());
    let options = NetOptions::builder()
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(3)
                .base_delay(Duration::from_secs(60))
                .max_delay(Duration::from_secs(60))
                .build(),
        )
        .observer(Observer(Arc::clone(&retries) as Arc<dyn NetObserver>))
        .build();
    let client = HttpClient::new(options, pools(), CancelToken::never());
    let url = Url::parse("http://127.0.0.1/probe").expect("BUG: hard-coded test URL is valid");

    let error = client
        .get_bytes(url, None)
        .await
        .expect_err("no transport, no request");

    assert!(matches!(error, NetError::NoTransport), "got {error:?}");
    assert_eq!(retries.0.load(Ordering::SeqCst), 0);
}
