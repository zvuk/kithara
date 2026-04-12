//! Tests for the unified downloader.

use std::{sync::Arc, time::Instant};

use kithara_net::NetOptions;
use kithara_platform::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{BodyStream, Downloader, DownloaderConfig, FetchCmd, Peer};

const POLL_MS: u64 = 50;

// Test helpers

struct MockPeer;

impl Peer for MockPeer {}

fn test_body_stream(chunks: Vec<&'static [u8]>) -> BodyStream {
    let stream =
        futures::stream::iter(chunks.into_iter().map(|c| Ok(bytes::Bytes::from_static(c))));
    BodyStream::from_raw(Box::pin(stream))
}

fn sleep(ms: u64) -> kithara_platform::tokio::time::Sleep {
    kithara_platform::tokio::time::sleep(Duration::from_millis(ms))
}

// BodyStream tests

#[kithara_test_macros::test(tokio)]
async fn body_stream_collect_accumulates_bytes() {
    let body = test_body_stream(vec![b"hello", b" ", b"world"]);
    let result = body.collect().await.expect("collect should succeed");
    assert_eq!(result.as_ref(), b"hello world");
}

#[kithara_test_macros::test(tokio)]
async fn body_stream_write_all_delegates_to_consumer() {
    let body = test_body_stream(vec![b"abc", b"def"]);
    let mut buf = Vec::new();
    let total = body
        .write_all(|chunk| {
            buf.extend_from_slice(chunk);
            Ok(())
        })
        .await
        .expect("write_all should succeed");

    assert_eq!(total, 6);
    assert_eq!(buf, b"abcdef");
}

#[kithara_test_macros::test(tokio)]
async fn body_stream_empty_collects_to_empty() {
    let body = test_body_stream(vec![]);
    let result = body.collect().await.expect("collect empty should succeed");
    assert!(result.is_empty());
}

// PeerHandle tests

#[kithara_test_macros::test(tokio)]
async fn peer_handle_cancel_scoped_to_peer() {
    let dl = Downloader::new(DownloaderConfig::default());
    let peer_a = dl.register(Arc::new(MockPeer));
    let peer_b = dl.register(Arc::new(MockPeer));

    peer_a.cancel().cancel();

    assert!(
        !peer_b.cancel().is_cancelled(),
        "peer B cancel should not fire when A cancels"
    );
}

#[kithara_test_macros::test(tokio)]
async fn peer_handle_cancel_fires_on_last_clone_drop() {
    let dl = Downloader::new(DownloaderConfig::default());
    let handle = dl.register(Arc::new(MockPeer));
    let cancel = handle.cancel();
    let clone = handle.clone();

    drop(handle);
    assert!(
        !cancel.is_cancelled(),
        "cancel should NOT fire while a clone is alive"
    );

    drop(clone);
    assert!(
        cancel.is_cancelled(),
        "cancel should fire when the last clone is dropped"
    );
}

#[kithara_test_macros::test(tokio)]
async fn peer_handle_execute_returns_error_on_unreachable() {
    let net = NetOptions {
        request_timeout: Duration::from_secs(60),
        ..NetOptions::default()
    };
    let dl = Downloader::new(DownloaderConfig::default().with_net(net));
    let handle = dl.register(Arc::new(MockPeer));

    let h2 = handle.clone();
    let task = kithara_platform::tokio::task::spawn(async move {
        let start = Instant::now();
        let result = h2
            .execute(FetchCmd::get(
                Url::parse("http://192.0.2.1:1/").expect("valid url"),
            ))
            .await;
        (start.elapsed(), result)
    });

    sleep(POLL_MS).await;
    handle.cancel().cancel();

    let (elapsed, result) = kithara_platform::tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("task should complete within 2s")
        .expect("task should not panic");

    assert!(
        elapsed < Duration::from_secs(2),
        "execute should return promptly after cancel, took {elapsed:?}"
    );
    assert!(result.is_err(), "expected Err after peer cancel");
}

#[kithara_test_macros::test(tokio)]
async fn peer_handle_downloader_cancel_cascades() {
    let cancel = CancellationToken::new();
    let dl = Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()));
    let handle = dl.register(Arc::new(MockPeer));

    cancel.cancel();
    assert!(
        handle.cancel().is_cancelled(),
        "peer cancel should fire when downloader cancels"
    );
}
