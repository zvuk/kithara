//! Tests for the unified downloader.

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Instant,
};

use futures::Stream;
use kithara_net::NetOptions;
use kithara_platform::{
    Mutex,
    time::Duration,
    tokio::time::{Sleep, sleep as tokio_sleep},
};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{BodyStream, Downloader, DownloaderConfig, FetchCmd, FetchResult, Peer};

const POLL_MS: u64 = 50;
const SETTLE_MS: u64 = 100;
const COMPLETE_MS: u64 = 500;
const CMD_COUNT: usize = 3;

// Test helpers

/// Minimal protocol stream — shared state + waker, same pattern real protocols use.
#[derive(Clone)]
struct TestStream {
    inner: Arc<Mutex<TestInner>>,
}

struct TestInner {
    queue: VecDeque<FetchCmd>,
    waker: Option<Waker>,
    done: bool,
}

impl TestStream {
    fn new() -> (Self, Arc<Mutex<TestInner>>) {
        let inner = Arc::new(Mutex::new(TestInner {
            queue: VecDeque::new(),
            waker: None,
            done: false,
        }));
        (
            Self {
                inner: Arc::clone(&inner),
            },
            inner,
        )
    }
}

impl Stream for TestStream {
    type Item = FetchCmd;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<FetchCmd>> {
        let mut state = self.inner.lock_sync();
        state.waker = Some(cx.waker().clone());

        if let Some(cmd) = state.queue.pop_front() {
            Poll::Ready(Some(cmd))
        } else if state.done {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

fn test_url() -> Url {
    Url::parse("http://127.0.0.1:1/test").expect("valid url")
}

fn counting_cmd(counter: Arc<AtomicU64>) -> FetchCmd {
    FetchCmd {
        method: super::FetchMethod::default(),
        url: test_url(),
        range: None,
        headers: None,
        on_connect: None,
        writer: None,
        throttle: None,
        on_complete: Some(Box::new(move |_result| {
            counter.fetch_add(1, Ordering::Relaxed);
        })),
    }
}

fn sleep(ms: u64) -> Sleep {
    tokio_sleep(Duration::from_millis(ms))
}

// Tests

#[kithara_test_macros::test(tokio)]
async fn empty_stream_completes() {
    let (stream, inner) = TestStream::new();
    inner.lock_sync().done = true;

    let dl = Downloader::new(DownloaderConfig::default());
    let _handle = dl.register_stream(stream);

    sleep(SETTLE_MS).await;
}

#[kithara_test_macros::test(tokio)]
async fn cancel_stops_loop() {
    let cancel = CancellationToken::new();
    let (stream, _inner) = TestStream::new();

    let dl = Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()));
    let _handle = dl.register_stream(stream);

    cancel.cancel();
    sleep(SETTLE_MS).await;
}

#[kithara_test_macros::test(tokio)]
async fn on_complete_fires_for_all_commands() {
    let (stream, inner) = TestStream::new();
    let counter = Arc::new(AtomicU64::new(0));

    {
        let mut state = inner.lock_sync();
        for _ in 0..CMD_COUNT {
            state.queue.push_back(counting_cmd(Arc::clone(&counter)));
        }
        state.done = true;
    }

    let dl = Downloader::new(DownloaderConfig::default());
    let _handle = dl.register_stream(stream);

    sleep(COMPLETE_MS).await;
    assert_eq!(counter.load(Ordering::Relaxed), 3);
}

#[kithara_test_macros::test(tokio)]
async fn waker_push_after_spawn() {
    let (stream, inner) = TestStream::new();
    let counter = Arc::new(AtomicU64::new(0));

    let dl = Downloader::new(DownloaderConfig::default());
    let _handle = dl.register_stream(stream);

    sleep(POLL_MS).await;

    {
        let mut state = inner.lock_sync();
        state.queue.push_back(counting_cmd(Arc::clone(&counter)));
        state.done = true;
        if let Some(w) = state.waker.take() {
            w.wake();
        }
    }

    sleep(COMPLETE_MS).await;
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[kithara_test_macros::test(tokio)]
async fn multiple_streams_polled() {
    let (a, inner_a) = TestStream::new();
    let (b, inner_b) = TestStream::new();
    let counter = Arc::new(AtomicU64::new(0));

    {
        let mut s = inner_a.lock_sync();
        s.queue.push_back(counting_cmd(Arc::clone(&counter)));
        s.done = true;
    }
    {
        let mut s = inner_b.lock_sync();
        s.queue.push_back(counting_cmd(Arc::clone(&counter)));
        s.done = true;
    }

    let dl = Downloader::new(DownloaderConfig::default());
    let _h1 = dl.register_stream(a);
    let _h2 = dl.register_stream(b);

    sleep(COMPLETE_MS).await;
    assert_eq!(counter.load(Ordering::Relaxed), 2);
}

#[kithara_test_macros::test(tokio)]
async fn register_after_first() {
    let counter = Arc::new(AtomicU64::new(0));

    let (first, inner_first) = TestStream::new();
    {
        let mut s = inner_first.lock_sync();
        s.queue.push_back(counting_cmd(Arc::clone(&counter)));
        // Don't set done — keep stream alive for second registration.
    }

    let dl = Downloader::new(DownloaderConfig::default());
    let _h1 = dl.register_stream(first);

    sleep(POLL_MS).await;

    // Register a second stream after the loop is running.
    let (second, inner_second) = TestStream::new();
    {
        let mut s = inner_second.lock_sync();
        s.queue.push_back(counting_cmd(Arc::clone(&counter)));
        s.done = true;
    }
    let _h2 = dl.register_stream(second);

    // Complete the first stream.
    {
        let mut s = inner_first.lock_sync();
        s.done = true;
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    sleep(COMPLETE_MS).await;
    assert_eq!(counter.load(Ordering::Relaxed), 2);
}

// R4: per-track cancel regression tests.

/// Build a `FetchCmd` whose `stream()` will hit a non-routable address and
/// hang in connect (until the track's cancel fires or `chunk_timeout` expires).
fn unreachable_stream_cmd() -> FetchCmd {
    FetchCmd {
        method: super::FetchMethod::Stream,
        // TEST-NET-1 (RFC 5737) — guaranteed not to route anywhere. Port 1
        // with a connect attempt will either hang until kernel timeout or
        // return a connection error; the important thing is that a
        // cooperative cancel should beat any of those outcomes.
        url: Url::parse("http://192.0.2.1:1/").expect("valid url"),
        range: None,
        headers: None,
        on_connect: None,
        writer: None,
        throttle: None,
        on_complete: None,
    }
}

#[kithara_test_macros::test(tokio)]
async fn execute_returns_promptly_when_track_dropped_mid_fetch() {
    // Use a long request timeout so "promptly" can only be achieved via
    // cancel, not by waiting for the fetch itself to naturally fail.
    let net = NetOptions {
        request_timeout: Duration::from_secs(60),
        ..NetOptions::default()
    };
    let dl = Downloader::new(DownloaderConfig::default().with_net(net));

    // Register a dummy stream to get a TrackHandle.
    let (stream, _inner) = TestStream::new();
    let handle = dl.register_stream(stream);

    // Spawn an execute that will hang on the unreachable address.
    let h2 = handle.clone();
    let task = kithara_platform::tokio::task::spawn(async move {
        let start = Instant::now();
        let result = h2.execute(unreachable_stream_cmd()).await;
        (start.elapsed(), result)
    });

    // Let the fetch start.
    sleep(POLL_MS).await;

    // Cancel track A explicitly — this is what `TrackInner::Drop` would do
    // when the last clone of the handle is dropped.
    handle.cancel().cancel();

    // The task should finish within a second — much less than the 60s
    // timeout we configured.
    let (elapsed, result) = kithara_platform::tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("task should complete within 2s after track cancel")
        .expect("task should not panic");

    assert!(
        elapsed < Duration::from_secs(2),
        "execute should return promptly after track cancel, took {elapsed:?}"
    );
    // Result must be an error of some kind — either Cancelled (if the cancel
    // fired before the connection error propagated) or Http (if the
    // unreachable address resolved to a connection failure first). What
    // matters is that we did NOT wait 60s.
    assert!(
        matches!(result, FetchResult::Err(_)),
        "expected Err after track cancel"
    );
}

#[kithara_test_macros::test(tokio)]
async fn execute_cancel_is_scoped_to_one_track() {
    // Two tracks — cancelling one should not affect the other.
    let dl = Downloader::new(DownloaderConfig::default());
    let (s1, _i1) = TestStream::new();
    let (s2, _i2) = TestStream::new();
    let track_a = dl.register_stream(s1);
    let track_b = dl.register_stream(s2);

    // Cancel track A.
    track_a.cancel().cancel();

    // Track B's cancel token must still be alive — dropping it later should
    // NOT have been pre-triggered by A's cancel.
    assert!(
        !track_b.cancel().is_cancelled(),
        "track B cancel should not fire when A cancels"
    );
}

// Peer / PeerHandle / BodyStream tests

struct MockPeer {
    active: bool,
}

impl Peer for MockPeer {
    fn is_active(&self) -> bool {
        self.active
    }
}

fn test_body_stream(chunks: Vec<&'static [u8]>) -> BodyStream {
    let stream =
        futures::stream::iter(chunks.into_iter().map(|c| Ok(bytes::Bytes::from_static(c))));
    BodyStream::from_raw(Box::pin(stream))
}

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

#[kithara_test_macros::test(tokio)]
async fn peer_handle_cancel_scoped_to_peer() {
    let dl = Downloader::new(DownloaderConfig::default());
    let peer_a = dl.register(Arc::new(MockPeer { active: true }));
    let peer_b = dl.register(Arc::new(MockPeer { active: true }));

    peer_a.cancel().cancel();

    assert!(
        !peer_b.cancel().is_cancelled(),
        "peer B cancel should not fire when A cancels"
    );
}

#[kithara_test_macros::test(tokio)]
async fn peer_handle_cancel_fires_on_last_clone_drop() {
    let dl = Downloader::new(DownloaderConfig::default());
    let handle = dl.register(Arc::new(MockPeer { active: true }));
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
    let handle = dl.register(Arc::new(MockPeer { active: true }));

    let h2 = handle.clone();
    let task = kithara_platform::tokio::task::spawn(async move {
        let start = Instant::now();
        let result = h2
            .execute(FetchCmd {
                method: super::FetchMethod::Stream,
                url: Url::parse("http://192.0.2.1:1/").expect("valid url"),
                range: None,
                headers: None,
                on_connect: None,
                writer: None,
                on_complete: None,
                throttle: None,
            })
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
    let handle = dl.register(Arc::new(MockPeer { active: true }));

    // Downloader-level cancel should cascade to peer.
    cancel.cancel();
    assert!(
        handle.cancel().is_cancelled(),
        "peer cancel should fire when downloader cancels"
    );
}
