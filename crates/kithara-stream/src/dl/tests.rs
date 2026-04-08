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
