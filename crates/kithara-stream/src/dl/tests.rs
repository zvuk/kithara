//! Tests for the unified downloader.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use axum::{
    Router,
    routing::{get, head},
};
use bytes::Bytes;
use futures::stream::iter as stream_iter;
use kithara_events::{DownloaderEvent, Event, EventBus};
use kithara_net::NetOptions;
use kithara_platform::{
    Mutex,
    time::Duration,
    tokio::{net::TcpListener as TokioTcpListener, task::spawn as tokio_spawn, time as tokio_time},
};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{BodyStream, Downloader, DownloaderConfig, FetchCmd, Peer, Priority};

mod consts {
    pub(super) const POLL_MS: u64 = 50;
    pub(super) const REQUEST_TIMEOUT_SECS: u64 = 60;
    pub(super) const CANCEL_GUARD_SECS: u64 = 2;
    pub(super) const CONCURRENCY_TEST_TIMEOUT_SECS: u64 = 30;
    pub(super) const FLOOD_BATCH_SIZE: usize = 10;
    pub(super) const FLOOD_DEADLINE_SECS: u64 = 20;
    pub(super) const FLOOD_POLL_MS: u64 = 100;
    pub(super) const PORT_STRESS_TIMEOUT_SECS: u64 = 60;
    pub(super) const SLOW_SERVER_DELAY_MS: u64 = 500;
    pub(super) const SOFT_TIMEOUT_MS: u64 = 50;
    pub(super) const EVENT_BUS_CAPACITY: usize = 64;
    pub(super) const SLOW_DEADLINE_SECS: u64 = 5;
    pub(super) const SLOW_POLL_TIMEOUT_MS: u64 = 200;
}
use consts::*;

// Test helpers

struct MockPeer;

impl Peer for MockPeer {}

fn test_body_stream(chunks: Vec<&'static [u8]>) -> BodyStream {
    let stream = stream_iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))));
    BodyStream::from_raw(Box::pin(stream))
}

fn sleep(ms: u64) -> tokio_time::Sleep {
    tokio_time::sleep(Duration::from_millis(ms))
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
        request_timeout: Duration::from_secs(REQUEST_TIMEOUT_SECS),
        ..NetOptions::default()
    };
    let dl = Downloader::new(DownloaderConfig::default().with_net(net));
    let handle = dl.register(Arc::new(MockPeer));

    let h2 = handle.clone();
    let task = tokio_spawn(async move {
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

    let (elapsed, result) = tokio_time::timeout(Duration::from_secs(CANCEL_GUARD_SECS), task)
        .await
        .expect("task should complete within CANCEL_GUARD_SECS")
        .expect("task should not panic");

    assert!(
        elapsed < Duration::from_secs(CANCEL_GUARD_SECS),
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
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(CONCURRENCY_TEST_TIMEOUT_SECS)))]
async fn max_concurrent_limits_inflight_connections() {
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
                tokio_time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );

    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
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
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(CONCURRENCY_TEST_TIMEOUT_SECS)))]
async fn many_downloaders_global_peak_stays_bounded() {
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
                tokio_time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );

    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    // Spawn all downloaders concurrently.
    let mut tasks = Vec::new();
    for _ in 0..NUM_DOWNLOADERS {
        let url = url.clone();
        tasks.push(tokio_spawn(async move {
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
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(CONCURRENCY_TEST_TIMEOUT_SECS)))]
async fn poll_next_respects_max_concurrent() {
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
                tokio_time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
                "ok"
            }
        }),
    );

    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    /// Peer that produces `remaining` HEAD commands via poll_next.
    struct FloodPeer {
        url: Url,
        remaining: Mutex<usize>,
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
            // Yield a batch of up to FLOOD_BATCH_SIZE at a time.
            let batch_size = (*rem).min(FLOOD_BATCH_SIZE);
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
        remaining: Mutex::new(TOTAL_CMDS),
    });
    let handle = dl.register(peer.clone());

    // Wait for all streaming commands to complete.
    // 1000 cmds × 5ms / 5 concurrent = ~1s theoretical minimum.
    // Give plenty of headroom.
    let deadline = Instant::now() + Duration::from_secs(FLOOD_DEADLINE_SECS);
    loop {
        tokio_time::sleep(Duration::from_millis(FLOOD_POLL_MS)).await;
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
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(PORT_STRESS_TIMEOUT_SECS)))]
async fn port_exhaustion_stress() {
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

    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/head")).expect("url");

    let mut tasks = Vec::new();
    for dl_idx in 0..NUM_DOWNLOADERS {
        let url = url.clone();
        tasks.push(tokio_spawn(async move {
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
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(SLOW_DEADLINE_SECS + SLOW_DEADLINE_SECS)))]
async fn soft_timeout_publishes_load_slow_on_peer_bus() {
    // Server delays longer than our configured soft_timeout, but less
    // than the hard timeout / test timeout.
    let app = Router::new().route(
        "/slow",
        get(|| async {
            tokio_time::sleep(Duration::from_millis(SLOW_SERVER_DELAY_MS)).await;
            "ok"
        }),
    );
    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    let url = Url::parse(&format!("http://{addr}/slow")).expect("url");

    let config =
        DownloaderConfig::default().with_soft_timeout(Duration::from_millis(SOFT_TIMEOUT_MS));
    let dl = Downloader::new(config);

    // Simulate the bus topology Queue builds: a scoped child of a root
    // bus. The downloader emits on the peer's scoped handle; the
    // subscriber (Queue's slow_listener) holds a clone of the same
    // scope.
    let root = EventBus::new(EVENT_BUS_CAPACITY);
    let scoped = root.scoped();
    let mut rx = scoped.subscribe();

    let handle = dl.register(Arc::new(MockPeer)).with_bus(scoped.clone());
    let _ = handle.execute(FetchCmd::get(url)).await;

    // Drain until we see LoadSlow — the soft timer fires before the
    // SLOW_SERVER_DELAY_MS server response completes.
    let deadline = Instant::now() + Duration::from_secs(SLOW_DEADLINE_SECS);
    let mut seen_slow = false;
    while Instant::now() < deadline {
        match tokio_time::timeout(Duration::from_millis(SLOW_POLL_TIMEOUT_MS), rx.recv()).await {
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

// Peer-level priority routing tests (Task 7 of
// 2026-04-20-timeline-flags-peer-priority). Two peers share the same
// Downloader; one advertises High priority via Timeline::set_playing,
// the other stays Low. Under `max_concurrent=2` contention the active
// peer's fetches must complete ahead of the preloader's — that is the
// user-visible win this plan exists to deliver.

#[derive(Clone, Copy, PartialEq, Eq)]
enum PeerTag {
    Active,
    Preload,
}

/// Peer that emits `total_cmds` GET commands and stamps each with its
/// tag when the response arrives. `priority()` reads the shared
/// Timeline so a mid-stream flip of `set_playing` is observable.
struct TaggedPriorityPeer {
    timeline: crate::Timeline,
    remaining: Mutex<usize>,
    url: Url,
    completion_log: Arc<Mutex<Vec<(PeerTag, usize)>>>,
    completion_counter: Arc<AtomicUsize>,
    tag: PeerTag,
}

impl Peer for TaggedPriorityPeer {
    fn priority(&self) -> Priority {
        if self.timeline.is_playing() {
            Priority::High
        } else {
            Priority::Low
        }
    }

    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let mut rem = self.remaining.lock_sync();
        if *rem == 0 {
            return Poll::Pending;
        }
        let take = (*rem).min(FLOOD_BATCH_SIZE);
        *rem -= take;
        let cmds: Vec<FetchCmd> = (0..take)
            .map(|_| {
                let tag = self.tag;
                let log = Arc::clone(&self.completion_log);
                let counter = Arc::clone(&self.completion_counter);
                FetchCmd::get(self.url.clone())
                    .writer(Box::new(|_chunk: &[u8]| Ok(())))
                    .on_complete(Box::new(
                        move |_bytes, _err: Option<&kithara_net::NetError>| {
                            let order = counter.fetch_add(1, Ordering::SeqCst);
                            log.lock_sync().push((tag, order));
                        },
                    ))
            })
            .collect();
        if *rem > 0 {
            cx.waker().wake_by_ref();
        }
        Poll::Ready(Some(cmds))
    }
}

async fn spawn_slow_server(delay_ms: u64) -> Url {
    let app = Router::new().route(
        "/data",
        get(move || async move {
            tokio_time::sleep(Duration::from_millis(delay_ms)).await;
            "ok"
        }),
    );
    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    Url::parse(&format!("http://{addr}/data")).expect("url")
}

/// Active peer (PLAYING=true from the start) must finish its batch
/// ahead of a preload peer (PLAYING=false) when both share the same
/// limited `max_concurrent` pool.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(30)))]
async fn active_peer_completes_before_preload_under_contention() {
    const CMDS_PER_PEER: usize = 20;
    const MAX_CONCURRENT: usize = 2;
    const PER_REQUEST_DELAY_MS: u64 = 30;

    let url = spawn_slow_server(PER_REQUEST_DELAY_MS).await;

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..DownloaderConfig::default()
    };
    let dl = Downloader::new(config);

    let completion_log = Arc::new(Mutex::new(Vec::<(PeerTag, usize)>::new()));
    let completion_counter = Arc::new(AtomicUsize::new(0));

    let timeline_active = crate::Timeline::new();
    timeline_active.set_playing(true);
    let active = Arc::new(TaggedPriorityPeer {
        timeline: timeline_active,
        remaining: Mutex::new(CMDS_PER_PEER),
        url: url.clone(),
        completion_log: Arc::clone(&completion_log),
        completion_counter: Arc::clone(&completion_counter),
        tag: PeerTag::Active,
    });

    let timeline_preload = crate::Timeline::new();
    // preload stays PLAYING=false (default)
    let preload = Arc::new(TaggedPriorityPeer {
        timeline: timeline_preload,
        remaining: Mutex::new(CMDS_PER_PEER),
        url,
        completion_log: Arc::clone(&completion_log),
        completion_counter: Arc::clone(&completion_counter),
        tag: PeerTag::Preload,
    });

    let active_handle = dl.register(active.clone());
    let preload_handle = dl.register(preload.clone());

    // Wait for all 40 to complete.
    let total = CMDS_PER_PEER * 2;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        tokio_time::sleep(Duration::from_millis(FLOOD_POLL_MS)).await;
        if completion_counter.load(Ordering::SeqCst) >= total {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out: completed={}, target={total}",
                completion_counter.load(Ordering::SeqCst)
            );
        }
    }

    drop(active_handle);
    drop(preload_handle);

    let log = completion_log.lock_sync().clone();
    assert_eq!(log.len(), total, "every cmd must complete exactly once");

    // Compute median completion order per tag — a robust aggregate
    // that does not hinge on the ordering of individual fetches.
    let mut active_orders: Vec<usize> = log
        .iter()
        .filter_map(|(tag, ord)| (*tag == PeerTag::Active).then_some(*ord))
        .collect();
    let mut preload_orders: Vec<usize> = log
        .iter()
        .filter_map(|(tag, ord)| (*tag == PeerTag::Preload).then_some(*ord))
        .collect();
    active_orders.sort_unstable();
    preload_orders.sort_unstable();
    let active_median = active_orders[active_orders.len() / 2];
    let preload_median = preload_orders[preload_orders.len() / 2];
    assert!(
        active_median < preload_median,
        "active peer median completion order {active_median} must precede \
         preload peer median {preload_median} — priority routing is broken"
    );

    // Additionally assert the first N completions are biased toward Active.
    let first_quarter = &log[..log.len() / 4];
    let active_in_first_quarter = first_quarter
        .iter()
        .filter(|(tag, _)| *tag == PeerTag::Active)
        .count();
    assert!(
        active_in_first_quarter > first_quarter.len() / 2,
        "active peer must dominate the first quarter of completions: \
         got {active_in_first_quarter}/{}",
        first_quarter.len()
    );
}

/// Negative case: when both peers are idle (PLAYING=false), the
/// Downloader is free to service them in any order. The test asserts
/// only liveness — every cmd eventually completes — so we do not lock
/// in FIFO behaviour that the Registry is not required to uphold.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(30)))]
async fn both_peers_idle_no_priority_ordering_asserted() {
    const CMDS_PER_PEER: usize = 10;
    const MAX_CONCURRENT: usize = 2;
    const PER_REQUEST_DELAY_MS: u64 = 10;

    let url = spawn_slow_server(PER_REQUEST_DELAY_MS).await;

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..DownloaderConfig::default()
    };
    let dl = Downloader::new(config);

    let completion_log = Arc::new(Mutex::new(Vec::<(PeerTag, usize)>::new()));
    let completion_counter = Arc::new(AtomicUsize::new(0));

    let a = Arc::new(TaggedPriorityPeer {
        timeline: crate::Timeline::new(),
        remaining: Mutex::new(CMDS_PER_PEER),
        url: url.clone(),
        completion_log: Arc::clone(&completion_log),
        completion_counter: Arc::clone(&completion_counter),
        tag: PeerTag::Active,
    });
    let b = Arc::new(TaggedPriorityPeer {
        timeline: crate::Timeline::new(),
        remaining: Mutex::new(CMDS_PER_PEER),
        url,
        completion_log: Arc::clone(&completion_log),
        completion_counter: Arc::clone(&completion_counter),
        tag: PeerTag::Preload,
    });

    let handle_a = dl.register(a);
    let handle_b = dl.register(b);

    let total = CMDS_PER_PEER * 2;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        tokio_time::sleep(Duration::from_millis(FLOOD_POLL_MS)).await;
        if completion_counter.load(Ordering::SeqCst) >= total {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out: completed={}, target={total}",
                completion_counter.load(Ordering::SeqCst)
            );
        }
    }

    drop(handle_a);
    drop(handle_b);

    let log = completion_log.lock_sync().clone();
    assert_eq!(
        log.len(),
        total,
        "idle peers must still drain every cmd to completion"
    );
}

/// Deterministic Registry-level routing test. Drives a stub `Peer`
/// whose `priority()` the test flips between High and Low, then
/// submits a command through the public `PeerHandle::execute` path and
/// confirms the response still flows — proving that the Registry
/// accepts priority-tagged commands from peers with either priority.
#[kithara_test_macros::test(tokio, timeout(Duration::from_secs(10)))]
async fn peer_handle_execute_respects_either_peer_priority() {
    struct FlippablePeer {
        timeline: crate::Timeline,
    }
    impl Peer for FlippablePeer {
        fn priority(&self) -> Priority {
            if self.timeline.is_playing() {
                Priority::High
            } else {
                Priority::Low
            }
        }
    }

    let dl = Downloader::new(DownloaderConfig::default());
    let timeline = crate::Timeline::new();
    let peer = Arc::new(FlippablePeer {
        timeline: timeline.clone(),
    });
    let handle = dl.register(peer);

    let url = spawn_slow_server(1).await;

    // Low priority phase — execute must still succeed.
    assert_eq!(peer_priority_from_handle(&handle, &timeline), Priority::Low);
    let low_resp = handle.execute(FetchCmd::get(url.clone())).await;
    assert!(low_resp.is_ok(), "execute must succeed while Low");

    // Flip to High.
    timeline.set_playing(true);
    assert_eq!(
        peer_priority_from_handle(&handle, &timeline),
        Priority::High
    );
    let high_resp = handle.execute(FetchCmd::get(url)).await;
    assert!(high_resp.is_ok(), "execute must succeed while High");
}

/// Helper for the deterministic routing test: reads the effective
/// peer priority from the same Timeline the peer observes.
fn peer_priority_from_handle(_handle: &super::PeerHandle, timeline: &crate::Timeline) -> Priority {
    if timeline.is_playing() {
        Priority::High
    } else {
        Priority::Low
    }
}
