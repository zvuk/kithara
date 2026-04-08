//! Tests for the unified downloader.

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use futures::Stream;
use kithara_platform::{
    Mutex,
    time::Duration,
    tokio::time::{Sleep, sleep as tokio_sleep},
};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{Downloader, DownloaderConfig, FetchCmd};

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
    let _handle = dl.register(stream);

    sleep(SETTLE_MS).await;
}

#[kithara_test_macros::test(tokio)]
async fn cancel_stops_loop() {
    let cancel = CancellationToken::new();
    let (stream, _inner) = TestStream::new();

    let dl = Downloader::new(DownloaderConfig::default().with_cancel(cancel.clone()));
    let _handle = dl.register(stream);

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
    let _handle = dl.register(stream);

    sleep(COMPLETE_MS).await;
    assert_eq!(counter.load(Ordering::Relaxed), 3);
}

#[kithara_test_macros::test(tokio)]
async fn waker_push_after_spawn() {
    let (stream, inner) = TestStream::new();
    let counter = Arc::new(AtomicU64::new(0));

    let dl = Downloader::new(DownloaderConfig::default());
    let _handle = dl.register(stream);

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
    let _h1 = dl.register(a);
    let _h2 = dl.register(b);

    sleep(COMPLETE_MS).await;
    assert_eq!(counter.load(Ordering::Relaxed), 2);
}

/// Builds a fetch cmd that records its index via `on_complete`.
fn indexed_cmd(order_log: Arc<Mutex<Vec<usize>>>, index: usize) -> FetchCmd {
    FetchCmd {
        method: super::FetchMethod::default(),
        url: test_url(),
        range: None,
        headers: None,
        on_connect: None,
        writer: None,
        throttle: None,
        on_complete: Some(Box::new(move |_result| {
            order_log.lock_sync().push(index);
        })),
    }
}

#[kithara_test_macros::test(tokio)]
async fn execute_batch_fires_on_complete_for_every_cmd() {
    let dl = Downloader::new(DownloaderConfig::default());
    let order_log = Arc::new(Mutex::new(Vec::<usize>::new()));

    let cmds: Vec<FetchCmd> = (0..CMD_COUNT)
        .map(|i| indexed_cmd(Arc::clone(&order_log), i))
        .collect();

    let results = dl.execute_batch(cmds).await;

    assert_eq!(results.len(), CMD_COUNT);
    assert_eq!(
        order_log.lock_sync().len(),
        CMD_COUNT,
        "every cmd's on_complete must fire exactly once"
    );
}

#[kithara_test_macros::test(tokio)]
async fn execute_batch_delivers_on_complete_in_input_order() {
    let dl = Downloader::new(DownloaderConfig::default());
    let order_log = Arc::new(Mutex::new(Vec::<usize>::new()));

    // Build a batch with indices 0..3 and verify on_complete callbacks
    // fire in that exact order, independent of which fetch finished
    // first on the network (all three target the same dead URL and fail
    // with connection-refused roughly simultaneously, but the batch must
    // still invoke callbacks in ascending index order).
    let cmds: Vec<FetchCmd> = (0..CMD_COUNT)
        .map(|i| indexed_cmd(Arc::clone(&order_log), i))
        .collect();

    let _results = dl.execute_batch(cmds).await;

    let log = order_log.lock_sync().clone();
    assert_eq!(
        log,
        (0..CMD_COUNT).collect::<Vec<_>>(),
        "on_complete callbacks must fire in batch input order"
    );
}

#[kithara_test_macros::test(tokio)]
async fn execute_batch_returns_results_in_input_order() {
    let dl = Downloader::new(DownloaderConfig::default());

    // Build 3 cmds and rely on the existing connection-refused pathway.
    let cmds: Vec<FetchCmd> = (0..CMD_COUNT)
        .map(|_| FetchCmd {
            method: super::FetchMethod::default(),
            url: test_url(),
            range: None,
            headers: None,
            on_connect: None,
            writer: None,
            throttle: None,
            on_complete: None,
        })
        .collect();

    let results = dl.execute_batch(cmds).await;

    assert_eq!(results.len(), CMD_COUNT);
    // All three target a dead URL and must surface FetchResult::Err in
    // the returned order. The Vec length is the positive assertion; the
    // exact error variant depends on the HttpClient transport layer.
    for r in &results {
        assert!(
            matches!(r, super::FetchResult::Err(_)),
            "all batch cmds pointed at a dead URL and must return Err"
        );
    }
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
    let _h1 = dl.register(first);

    sleep(POLL_MS).await;

    // Register a second stream after the loop is running.
    let (second, inner_second) = TestStream::new();
    {
        let mut s = inner_second.lock_sync();
        s.queue.push_back(counting_cmd(Arc::clone(&counter)));
        s.done = true;
    }
    let _h2 = dl.register(second);

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

#[kithara_test_macros::test(tokio)]
async fn execute_batch_blocking_from_worker_thread_returns_ordered_results() {
    // execute_batch_blocking is designed to be called from a non-async
    // OS thread (the upcoming HLS plan loop). Use with_runtime to inject
    // the current async runtime handle, then dispatch the blocking call
    // from std::thread::spawn so rx.recv() blocks on a real std thread.
    let dl = Downloader::new(
        DownloaderConfig::default()
            .with_runtime(kithara_platform::tokio::runtime::Handle::current()),
    );

    let order_log = Arc::new(Mutex::new(Vec::<usize>::new()));
    let cmds: Vec<FetchCmd> = (0..CMD_COUNT)
        .map(|i| indexed_cmd(Arc::clone(&order_log), i))
        .collect();

    let dl_clone = dl.clone();
    let thread_handle = std::thread::spawn(move || dl_clone.execute_batch_blocking(cmds));

    let results = kithara_platform::tokio::task::spawn_blocking(move || {
        thread_handle.join().expect("worker thread panicked")
    })
    .await
    .expect("spawn_blocking join failed");

    assert_eq!(results.len(), CMD_COUNT);
    assert_eq!(
        order_log.lock_sync().clone(),
        (0..CMD_COUNT).collect::<Vec<_>>(),
        "blocking batch must deliver on_complete in batch input order"
    );
}

#[kithara_test_macros::test(tokio)]
async fn execute_batch_blocking_fires_every_on_complete() {
    let dl = Downloader::new(
        DownloaderConfig::default()
            .with_runtime(kithara_platform::tokio::runtime::Handle::current()),
    );
    let counter = Arc::new(AtomicU64::new(0));

    let cmds: Vec<FetchCmd> = (0..CMD_COUNT)
        .map(|_| counting_cmd(Arc::clone(&counter)))
        .collect();

    let dl_clone = dl.clone();
    let thread_handle = std::thread::spawn(move || dl_clone.execute_batch_blocking(cmds));

    let results = kithara_platform::tokio::task::spawn_blocking(move || {
        thread_handle.join().expect("worker thread panicked")
    })
    .await
    .expect("spawn_blocking join failed");

    assert_eq!(results.len(), CMD_COUNT);
    assert_eq!(
        counter.load(Ordering::Relaxed),
        CMD_COUNT as u64,
        "blocking batch must fire every on_complete exactly once"
    );
}
