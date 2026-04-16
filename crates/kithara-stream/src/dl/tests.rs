//! Tests for the unified downloader.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

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

/// Verify that the Downloader never exceeds `max_concurrent` in-flight
/// HTTP connections, even when many commands are submitted at once.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(30)))]
async fn max_concurrent_limits_inflight_connections() {
    use std::net::SocketAddr;

    use axum::{Router, routing::get};

    const MAX_CONCURRENT: usize = 5;
    const TOTAL_REQUESTS: usize = 1000;
    const HANDLER_DELAY_MS: u64 = 5;

    let concurrent = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let concurrent_c = Arc::clone(&concurrent);
    let peak_c = Arc::clone(&peak);

    let app = Router::new().route(
        "/slow",
        get(move || {
            let concurrent = Arc::clone(&concurrent_c);
            let peak = Arc::clone(&peak_c);
            async move {
                let current = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                // Update peak.
                loop {
                    let old = peak.load(Ordering::SeqCst);
                    if current <= old
                        || peak
                            .compare_exchange(old, current, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        break;
                    }
                }
                kithara_platform::tokio::time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );

    let listener = kithara_platform::tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    kithara_platform::tokio::task::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..DownloaderConfig::default()
    };
    let dl = Downloader::new(config);
    let handle = dl.register(Arc::new(MockPeer));

    let cmds: Vec<FetchCmd> = (0..TOTAL_REQUESTS)
        .map(|_| FetchCmd::head(url.clone()))
        .collect();
    let results = handle.batch(cmds).await;

    assert_eq!(results.len(), TOTAL_REQUESTS, "all requests must complete");
    let ok_count = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok_count, TOTAL_REQUESTS, "all requests must succeed");

    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak <= MAX_CONCURRENT,
        "peak concurrent {observed_peak} exceeded max_concurrent {MAX_CONCURRENT}"
    );
    assert!(observed_peak > 0, "peak must be at least 1 (sanity check)");
}

/// Simulate many concurrent Downloaders (like parallel test execution).
/// Each submits a batch of HEAD requests. Global peak must stay bounded.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(30)))]
async fn many_downloaders_global_peak_stays_bounded() {
    use std::net::SocketAddr;

    use axum::{Router, routing::get};

    const NUM_DOWNLOADERS: usize = 20;
    const REQUESTS_PER_DL: usize = 30;
    const MAX_CONCURRENT_PER_DL: usize = 3;
    const HANDLER_DELAY_MS: u64 = 20;
    // With 20 downloaders × 3 max_concurrent, theoretical max = 60.
    // We want it well below NUM_DOWNLOADERS × REQUESTS_PER_DL = 600.
    const GLOBAL_PEAK_LIMIT: usize = NUM_DOWNLOADERS * MAX_CONCURRENT_PER_DL;

    let concurrent = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let concurrent_c = Arc::clone(&concurrent);
    let peak_c = Arc::clone(&peak);

    let app = Router::new().route(
        "/slow",
        get(move || {
            let concurrent = Arc::clone(&concurrent_c);
            let peak = Arc::clone(&peak_c);
            async move {
                let current = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                loop {
                    let old = peak.load(Ordering::SeqCst);
                    if current <= old
                        || peak
                            .compare_exchange(old, current, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        break;
                    }
                }
                kithara_platform::tokio::time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );

    let listener = kithara_platform::tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    kithara_platform::tokio::task::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    // Spawn all downloaders concurrently.
    let mut tasks = Vec::new();
    for _ in 0..NUM_DOWNLOADERS {
        let url = url.clone();
        tasks.push(kithara_platform::tokio::task::spawn(async move {
            let config = DownloaderConfig {
                max_concurrent: MAX_CONCURRENT_PER_DL,
                ..DownloaderConfig::default()
            };
            let dl = Downloader::new(config);
            let handle = dl.register(Arc::new(MockPeer));
            let cmds: Vec<FetchCmd> = (0..REQUESTS_PER_DL)
                .map(|_| FetchCmd::head(url.clone()))
                .collect();
            let results = handle.batch(cmds).await;
            results.into_iter().filter(|r| r.is_ok()).count()
        }));
    }

    let mut total_ok = 0;
    for task in tasks {
        total_ok += task.await.expect("task should not panic");
    }

    assert_eq!(
        total_ok,
        NUM_DOWNLOADERS * REQUESTS_PER_DL,
        "all requests must succeed"
    );

    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak <= GLOBAL_PEAK_LIMIT,
        "global peak {observed_peak} exceeded limit {GLOBAL_PEAK_LIMIT} \
         ({NUM_DOWNLOADERS} downloaders × {MAX_CONCURRENT_PER_DL} max_concurrent)"
    );
}

/// Verify that poll_next (streaming path) also respects max_concurrent.
/// A Peer produces 1000 HEAD commands via poll_next. Peak must stay ≤ max_concurrent.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(30)))]
async fn poll_next_respects_max_concurrent() {
    use std::{
        net::SocketAddr,
        task::{Context, Poll},
    };

    use axum::{Router, routing::get};

    use super::{FetchCmd, Peer, Priority};

    const MAX_CONCURRENT: usize = 5;
    const TOTAL_CMDS: usize = 1000;
    const HANDLER_DELAY_MS: u64 = 5;

    let concurrent = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let concurrent_c = Arc::clone(&concurrent);
    let peak_c = Arc::clone(&peak);

    let app = Router::new().route(
        "/slow",
        get(move || {
            let concurrent = Arc::clone(&concurrent_c);
            let peak = Arc::clone(&peak_c);
            async move {
                let current = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                loop {
                    let old = peak.load(Ordering::SeqCst);
                    if current <= old
                        || peak
                            .compare_exchange(old, current, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        break;
                    }
                }
                kithara_platform::tokio::time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );

    let listener = kithara_platform::tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    kithara_platform::tokio::task::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    /// Peer that produces `remaining` HEAD commands via poll_next.
    struct FloodPeer {
        url: Url,
        remaining: kithara_platform::Mutex<usize>,
    }

    impl Peer for FloodPeer {
        fn priority(&self) -> Priority {
            Priority::Low
        }

        fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
            let mut rem = self.remaining.lock_sync();
            if *rem == 0 {
                return Poll::Ready(None);
            }
            // Yield a batch of up to 10 at a time.
            let batch_size = (*rem).min(10);
            *rem -= batch_size;
            let cmds: Vec<FetchCmd> = (0..batch_size)
                .map(|_| FetchCmd::head(self.url.clone()))
                .collect();
            // Re-wake so we get polled again for remaining.
            if *rem > 0 {
                cx.waker().wake_by_ref();
            }
            Poll::Ready(Some(cmds))
        }
    }

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..DownloaderConfig::default()
    };
    let dl = Downloader::new(config);
    let peer = Arc::new(FloodPeer {
        url,
        remaining: kithara_platform::Mutex::new(TOTAL_CMDS),
    });
    let handle = dl.register(peer.clone());

    // Wait for all streaming commands to complete.
    // 1000 cmds × 5ms / 5 concurrent = ~1s theoretical minimum.
    // Give plenty of headroom.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        kithara_platform::tokio::time::sleep(Duration::from_millis(100)).await;
        if *peer.remaining.lock_sync() == 0 && concurrent.load(Ordering::SeqCst) == 0 {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out: remaining={}, concurrent={}",
                *peer.remaining.lock_sync(),
                concurrent.load(Ordering::SeqCst),
            );
        }
    }

    drop(handle);

    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak <= MAX_CONCURRENT,
        "poll_next peak concurrent {observed_peak} exceeded max_concurrent {MAX_CONCURRENT}"
    );
    assert!(observed_peak > 0, "sanity: at least one request ran");
}

/// Reproduce port exhaustion: many concurrent downloaders each doing
/// ~100 HEAD requests (simulates prefetch_metadata × parallel tests).
/// All requests must succeed — no "Can't assign requested address".
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(60)))]
async fn port_exhaustion_stress() {
    use std::net::SocketAddr;

    use axum::{Router, routing::head};

    const NUM_DOWNLOADERS: usize = 200;
    const REQUESTS_PER_DL: usize = 114; // 3 variants × 37 segments + inits
    const MAX_CONCURRENT: usize = 5;
    // 200 × 114 = 22_800 total HEAD requests through one port.

    let total_served = Arc::new(AtomicUsize::new(0));
    let total_served_c = Arc::clone(&total_served);

    let app = Router::new().route(
        "/head",
        head(move || {
            total_served_c.fetch_add(1, Ordering::Relaxed);
            async { "" }
        }),
    );

    let listener = kithara_platform::tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    kithara_platform::tokio::task::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/head")).expect("url");

    let mut tasks = Vec::new();
    for dl_idx in 0..NUM_DOWNLOADERS {
        let url = url.clone();
        tasks.push(kithara_platform::tokio::task::spawn(async move {
            let config = DownloaderConfig {
                max_concurrent: MAX_CONCURRENT,
                ..DownloaderConfig::default()
            };
            let dl = Downloader::new(config);
            let handle = dl.register(Arc::new(MockPeer));
            let cmds: Vec<FetchCmd> = (0..REQUESTS_PER_DL)
                .map(|_| FetchCmd::head(url.clone()))
                .collect();
            let results = handle.batch(cmds).await;
            let failures: Vec<String> = results
                .iter()
                .filter_map(|r| r.as_ref().err().map(|e| format!("{e}")))
                .collect();
            (dl_idx, results.len(), failures)
        }));
    }

    let mut total_ok = 0;
    let mut all_failures: Vec<String> = Vec::new();
    for task in tasks {
        let (dl_idx, count, failures) = task.await.expect("task should not panic");
        total_ok += count - failures.len();
        if !failures.is_empty() {
            all_failures.push(format!(
                "dl[{dl_idx}]: {}/{count} failed, first: {}",
                failures.len(),
                failures[0]
            ));
        }
    }

    let expected = NUM_DOWNLOADERS * REQUESTS_PER_DL;
    assert!(
        all_failures.is_empty(),
        "port exhaustion: {total_ok}/{expected} ok, {} downloaders had failures:\n{}",
        all_failures.len(),
        all_failures.join("\n")
    );
}

/// End-to-end: a slow HTTP response fires `DownloaderEvent::LoadSlow`
/// on the peer's bus, and a subscriber on that bus (as
/// `kithara_queue::Loader` would set up) receives it.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(10)))]
async fn soft_timeout_publishes_load_slow_on_peer_bus() {
    use std::net::SocketAddr;

    use axum::{Router, routing::get};
    use kithara_events::{DownloaderEvent, Event, EventBus};

    // Server delays longer than our configured soft_timeout, but less
    // than the hard timeout / test timeout.
    let app = Router::new().route(
        "/slow",
        get(|| async {
            kithara_platform::tokio::time::sleep(Duration::from_millis(500)).await;
            "ok"
        }),
    );
    let listener = kithara_platform::tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    kithara_platform::tokio::task::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    let config = DownloaderConfig::default().with_soft_timeout(Duration::from_millis(50));
    let dl = Downloader::new(config);

    // Simulate the bus topology Queue builds: a scoped child of a root
    // bus. The downloader emits on the peer's scoped handle; the
    // subscriber (Queue's slow_listener) holds a clone of the same
    // scope.
    let root = EventBus::new(64);
    let scoped = root.scoped();
    let mut rx = scoped.subscribe();

    let handle = dl.register(Arc::new(MockPeer)).with_bus(scoped.clone());
    let _ = handle.execute(FetchCmd::get(url)).await;

    // Drain until we see LoadSlow — the soft timer fires before the
    // 500 ms server response completes.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen_slow = false;
    while Instant::now() < deadline {
        match kithara_platform::tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(Event::Downloader(DownloaderEvent::LoadSlow))) => {
                seen_slow = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert!(
        seen_slow,
        "peer bus subscriber must receive DownloaderEvent::LoadSlow"
    );
}
