use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

use axum::{
    Router,
    body::Body,
    routing::{get, head},
};
use bytes::Bytes;
use futures::stream::iter as stream_iter;
use kithara_abr::Abr;
use kithara_events::{DownloaderEvent, Event, EventBus};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, Notify},
    time::{self, Duration},
    tokio::{net::TcpListener as TokioTcpListener, task::spawn as tokio_spawn},
};
use kithara_test_utils::kithara;
use url::Url;

use super::{BodyStream, Downloader, DownloaderConfig, FetchCmd, Peer, RequestPriority};
use crate::{Activity, SeekState};

const CONCURRENCY_TEST_TIMEOUT_SECS: u64 = 30;
const FLOOD_BATCH_SIZE: usize = 10;
const PORT_STRESS_TIMEOUT_SECS: u64 = 60;
const SLOW_DEADLINE_SECS: u64 = 5;

struct MockPeer;

impl Abr for MockPeer {}
impl Peer for MockPeer {}

fn test_client() -> HttpClient {
    HttpClient::new(NetOptions::default(), CancelToken::never())
}

fn test_config() -> DownloaderConfig {
    DownloaderConfig::for_client(test_client()).build()
}

fn test_body_stream(chunks: Vec<&'static [u8]>) -> BodyStream {
    let stream = stream_iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))));
    BodyStream::wrap_raw(Box::pin(stream))
}

/// Event-driven completion barrier. Each finished fetch calls
/// [`complete`](Self::complete); the `target`-th completion signals the
/// waiter parked in [`wait`](Self::wait).
///
/// This replaces the `sleep(poll)` + wall-clock-deadline busy loops these
/// tests used to drive: the waiter parks on the completion *event*, not on
/// a virtual-clock deadline. Under flash a virtual deadline would race the
/// clock past in-flight real socket bytes and false-trip; parking on the
/// event lets the clock advance through the (virtual) server delays while
/// the REAL fetch round-trips drive the count. The harness `timeout(...)`
/// (REAL wall time) is the only backstop.
struct CompletionGate {
    done: AtomicUsize,
    ready: Notify,
    target: usize,
}

impl CompletionGate {
    fn new(target: usize) -> Arc<Self> {
        Arc::new(Self {
            target,
            done: AtomicUsize::new(0),
            ready: Notify::default(),
        })
    }

    /// Record one completion and signal the waiter on the `target`-th.
    /// Returns the pre-increment count (the completion order).
    fn complete(&self) -> usize {
        let order = self.done.fetch_add(1, Ordering::SeqCst);
        if order + 1 == self.target {
            self.ready.notify_one();
        }
        order
    }

    /// Park until `target` completions are recorded. No wall-clock deadline:
    /// the per-test harness `timeout(...)` is the real-time backstop.
    async fn wait(&self) {
        loop {
            let notified = self.ready.notified();
            if self.done.load(Ordering::SeqCst) >= self.target {
                return;
            }
            notified.await;
        }
    }
}

#[kithara::test(tokio)]
async fn body_stream_collect_accumulates_bytes() {
    let body = test_body_stream(vec![b"hello", b" ", b"world"]);
    let result = body.collect().await.expect("collect should succeed");
    assert_eq!(result.as_ref(), b"hello world");
}

#[kithara::test(tokio)]
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

#[kithara::test(tokio)]
async fn body_stream_empty_collects_to_empty() {
    let body = test_body_stream(vec![]);
    let result = body.collect().await.expect("collect empty should succeed");
    assert!(result.is_empty());
}

#[kithara::test(tokio)]
async fn peer_handle_cancel_scoped_to_peer() {
    let dl = Downloader::new(test_config());
    let peer_a = dl.register(Arc::new(MockPeer));
    let peer_b = dl.register(Arc::new(MockPeer));

    peer_a.cancel().cancel();

    assert!(
        !peer_b.cancel().is_cancelled(),
        "peer B cancel should not fire when A cancels"
    );
}

#[kithara::test(tokio)]
async fn peer_handle_cancel_fires_on_last_clone_drop() {
    let dl = Downloader::new(test_config());
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

#[kithara::test(tokio)]
async fn peer_handle_execute_returns_error_on_unreachable() {
    const POLL_MS: u64 = 50;
    const REQUEST_TIMEOUT_SECS: u64 = 60;
    const CANCEL_GUARD_SECS: u64 = 2;
    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build();
    let dl = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, CancelToken::never())).build(),
    );
    let handle = dl.register(Arc::new(MockPeer));

    let h2 = handle.clone();
    let task = tokio_spawn(async move {
        let start = Instant::now();
        let result = h2
            .execute(FetchCmd::get(Url::parse("http://192.0.2.1:1/").expect("valid url")).build())
            .await;
        (start.elapsed(), result)
    });

    time::sleep(Duration::from_millis(POLL_MS)).await;
    handle.cancel().cancel();

    let (elapsed, result) = time::timeout(Duration::from_secs(CANCEL_GUARD_SECS), task)
        .await
        .expect("task should complete within CANCEL_GUARD_SECS")
        .expect("task should not panic");

    assert!(
        elapsed < Duration::from_secs(CANCEL_GUARD_SECS),
        "execute should return promptly after cancel, took {elapsed:?}"
    );
    assert!(result.is_err(), "expected Err after peer cancel");
}

#[kithara::test(tokio)]
async fn peer_handle_downloader_cancel_cascades() {
    let cancel = CancelToken::never();
    let dl = Downloader::new(
        DownloaderConfig::for_client(test_client())
            .cancel(cancel.clone())
            .build(),
    );
    let handle = dl.register(Arc::new(MockPeer));

    cancel.cancel();
    assert!(
        handle.cancel().is_cancelled(),
        "peer cancel should fire when downloader cancels"
    );
}

/// Verify that the Downloader never exceeds `max_concurrent` in-flight
/// HTTP connections, even when many commands are submitted at once.
#[kithara::test(tokio, timeout(Duration::from_secs(CONCURRENCY_TEST_TIMEOUT_SECS)))]
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
                time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
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
        ..test_config()
    };
    let dl = Downloader::new(config);
    let handle = dl.register(Arc::new(MockPeer));

    let cmds: Vec<FetchCmd> = (0..TOTAL_REQUESTS)
        .map(|_| FetchCmd::head(url.clone()).build())
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
#[kithara::test(tokio, timeout(Duration::from_secs(CONCURRENCY_TEST_TIMEOUT_SECS)))]
async fn many_downloaders_global_peak_stays_bounded() {
    const NUM_DOWNLOADERS: usize = 20;
    const REQUESTS_PER_DL: usize = 30;
    const MAX_CONCURRENT_PER_DL: usize = 3;
    const HANDLER_DELAY_MS: u64 = 20;
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
                time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
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

    let mut tasks = Vec::new();
    for _ in 0..NUM_DOWNLOADERS {
        let url = url.clone();
        tasks.push(tokio_spawn(async move {
            let config = DownloaderConfig {
                max_concurrent: MAX_CONCURRENT_PER_DL,
                ..test_config()
            };
            let dl = Downloader::new(config);
            let handle = dl.register(Arc::new(MockPeer));
            let cmds: Vec<FetchCmd> = (0..REQUESTS_PER_DL)
                .map(|_| FetchCmd::head(url.clone()).build())
                .collect();
            let results = handle.batch(cmds).await;
            results.into_iter().filter(Result::is_ok).count()
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

/// Verify that `poll_next` (streaming path) also respects `max_concurrent`.
/// A Peer produces 1000 HEAD commands via `poll_next`. Peak must stay ≤ `max_concurrent`.
#[kithara::test(tokio, timeout(Duration::from_secs(CONCURRENCY_TEST_TIMEOUT_SECS)))]
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
                time::sleep(Duration::from_millis(HANDLER_DELAY_MS)).await;
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

    /// Peer that produces `remaining` HEAD commands via `poll_next`,
    /// counting each completion into a shared [`CompletionGate`].
    struct FloodPeer {
        url: Url,
        remaining: Mutex<usize>,
        gate: Arc<CompletionGate>,
    }

    impl Abr for FloodPeer {}
    impl Peer for FloodPeer {
        fn priority(&self) -> RequestPriority {
            RequestPriority::Low
        }

        fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
            let (batch_size, more) = {
                let mut rem = self.remaining.lock();
                if *rem == 0 {
                    return Poll::Ready(None);
                }
                let batch_size = (*rem).min(FLOOD_BATCH_SIZE);
                *rem -= batch_size;
                (batch_size, *rem > 0)
            };
            let cmds: Vec<FetchCmd> = (0..batch_size)
                .map(|_| {
                    let gate = Arc::clone(&self.gate);
                    // No-op writer: a HEAD carries no body, but the streaming
                    // deliver path only invokes `on_complete` when a writer is
                    // present, so we supply one to receive the completion signal.
                    FetchCmd::head(self.url.clone())
                        .writer(Box::new(|_chunk: &[u8]| Ok(())))
                        .on_complete(Box::new(
                            move |_bytes,
                                  _headers: Option<&kithara_net::Headers>,
                                  _err: Option<&kithara_net::NetError>| {
                                gate.complete();
                            },
                        ))
                        .build()
                })
                .collect();
            if more {
                cx.waker().wake_by_ref();
            }
            Poll::Ready(Some(cmds))
        }
    }

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..test_config()
    };
    let dl = Downloader::new(config);
    let gate = CompletionGate::new(TOTAL_CMDS);
    let handle = dl.register(Arc::new(FloodPeer {
        url,
        remaining: Mutex::new(TOTAL_CMDS),
        gate: Arc::clone(&gate),
    }));

    // Wait on the completion event: every HEAD has replied. The server
    // decrements `concurrent` before sending each reply, so once the last
    // completion fires `concurrent` is already 0 (asserted below).
    gate.wait().await;

    drop(handle);

    assert_eq!(
        concurrent.load(Ordering::SeqCst),
        0,
        "all in-flight HEADs must have drained once every completion fired"
    );

    let observed_peak = peak.load(Ordering::SeqCst);
    assert!(
        observed_peak <= MAX_CONCURRENT,
        "poll_next peak concurrent {observed_peak} exceeded max_concurrent {MAX_CONCURRENT}"
    );
    assert!(observed_peak > 0, "sanity: at least one request ran");
}

/// Verify that Downloaders sharing a single [`HttpClient`] reuse the same
/// keep-alive pool across **successive** Downloader lifetimes: tracks come and
/// go, ABR `switch_variant` rebuilds Downloaders, and the queue advances.
///
/// Contract:
/// - The caller builds one [`HttpClient`] and hands a clone to every
///   Downloader via [`DownloaderConfig::client`]. `reqwest::Client` is
///   internally `Arc`'d so all clones share one connection pool.
/// - When a Downloader is dropped, its keep-alive connections stay in the
///   shared pool's idle list and are picked up by the next Downloader.
/// - With `WAVES` rounds x `PARALLEL_DLS` parallel Downloaders, the
///   client-observed opened connection count stays close to a small number of
///   waves, independent of total request count.
///
/// A regression that reverts to a per-Downloader client multiplies the opened
/// connection count by `WAVES`, immediately tripping the assertion.
#[kithara::test(tokio, timeout(Duration::from_secs(PORT_STRESS_TIMEOUT_SECS)))]
async fn shared_client_keepalive_bounds_connection_count() {
    const PARALLEL_DLS: usize = 8;
    const WAVES: usize = 25;
    const REQUESTS_PER_DL: usize = 114;
    const MAX_CONCURRENT: usize = 5;
    /// Client-observed opened connections are a churn sentinel. Correct
    /// shared-pool behavior stays close to a few waves; per-Downloader pools
    /// produce `WAVES * PARALLEL_DLS * MAX_CONCURRENT` ≈ 1000 connections.
    const MAX_CLIENT_CONNECTIONS: usize = PARALLEL_DLS * MAX_CONCURRENT * 5;

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
    let addr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/head")).expect("url");
    let shared_client = HttpClient::new(
        NetOptions::builder()
            .pool_max_idle_per_host(PARALLEL_DLS * MAX_CONCURRENT)
            .build(),
        CancelToken::never(),
    );

    let mut total_ok = 0;
    let mut all_failures: Vec<String> = Vec::new();
    let mut connection_history: Vec<String> = Vec::with_capacity(WAVES);
    let total_dls = WAVES * PARALLEL_DLS;
    for wave in 0..WAVES {
        let mut tasks = Vec::with_capacity(PARALLEL_DLS);
        for slot in 0..PARALLEL_DLS {
            let url = url.clone();
            let client = shared_client.clone();
            let dl_idx = wave * PARALLEL_DLS + slot;
            tasks.push(tokio_spawn(async move {
                let config = DownloaderConfig {
                    client,
                    max_concurrent: MAX_CONCURRENT,
                    ..test_config()
                };
                let dl = Downloader::new(config);
                let handle = dl.register(Arc::new(MockPeer));
                let cmds: Vec<FetchCmd> = (0..REQUESTS_PER_DL)
                    .map(|_| FetchCmd::head(url.clone()).build())
                    .collect();
                let results = handle.batch(cmds).await;
                let failures: Vec<String> = results
                    .iter()
                    .filter_map(|r| r.as_ref().err().map(|e| format!("{e}")))
                    .collect();
                (dl_idx, results.len(), failures)
            }));
        }
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
        connection_history.push(format!(
            "wave {wave}: client_connections={}, served={}, ok={total_ok}",
            shared_client.connection_count(),
            total_served.load(Ordering::SeqCst)
        ));
    }

    let expected = total_dls * REQUESTS_PER_DL;
    let connection_count = shared_client.connection_count();
    assert!(
        all_failures.is_empty(),
        "shared client should not produce HTTP failures: {total_ok}/{expected} ok, \
         {} downloaders had failures (client_connections={connection_count}):\n{}",
        all_failures.len(),
        all_failures.join("\n")
    );
    assert!(
        connection_count <= MAX_CLIENT_CONNECTIONS,
        "shared keep-alive regression: client opened {connection_count} connections \
         for {expected} requests \
         across {WAVES} waves of {PARALLEL_DLS} downloaders \
         (expected ≤ {MAX_CLIENT_CONNECTIONS}). Successive Downloaders should reuse sockets \
         from the shared pool; this many indicates per-Downloader clients or severe churn. \
         Per-wave opened-connection history:\n{}",
        connection_history.join("\n")
    );
}

/// End-to-end: a slow HTTP response fires `DownloaderEvent::LoadSlow`
/// on the peer's bus, and a subscriber on that bus (as
/// `kithara_queue::Loader` would set up) receives it.
#[kithara::test(tokio, timeout(Duration::from_secs(SLOW_DEADLINE_SECS + SLOW_DEADLINE_SECS)))]
async fn soft_timeout_publishes_load_slow_on_peer_bus() {
    const SLOW_SERVER_DELAY_MS: u64 = 500;
    const SOFT_TIMEOUT_MS: u64 = 50;
    const EVENT_BUS_CAPACITY: usize = 64;
    const SLOW_POLL_TIMEOUT_MS: u64 = 200;
    // Real server delay via the free fn (NOT flash-rewritten), so the slow
    // response outlasts the REAL soft_timeout. An inline handler would have
    // its `time::sleep` rewritten to a virtual sleep that collapses to ~0 wall
    // time under flash — the real soft_timeout would then never fire and no
    // LoadSlow would be published.
    let url = spawn_slow_server(SLOW_SERVER_DELAY_MS).await;

    let config = DownloaderConfig::for_client(test_client())
        .soft_timeout(Duration::from_millis(SOFT_TIMEOUT_MS))
        .build();
    let dl = Downloader::new(config);

    let root = EventBus::new(EVENT_BUS_CAPACITY);
    let scoped = root.scoped();
    let mut rx = scoped.subscribe();

    let handle = dl.register(Arc::new(MockPeer)).with_bus(scoped.clone());
    let _ = handle.execute(FetchCmd::get(url).build()).await;

    let deadline = Instant::now() + Duration::from_secs(SLOW_DEADLINE_SECS);
    let mut seen_slow = false;
    while Instant::now() < deadline {
        match time::timeout(Duration::from_millis(SLOW_POLL_TIMEOUT_MS), rx.recv()).await {
            Ok(Ok(Event::Downloader(DownloaderEvent::LoadSlow { .. }))) => {
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

#[kithara::test(tokio, timeout(Duration::from_secs(SLOW_DEADLINE_SECS + SLOW_DEADLINE_SECS)))]
async fn soft_timeout_covers_response_body() {
    const SLOW_BODY_DELAY_MS: u64 = 500;
    const SOFT_TIMEOUT_MS: u64 = 50;
    const EVENT_BUS_CAPACITY: usize = 64;
    const SLOW_POLL_TIMEOUT_MS: u64 = 200;

    let url = spawn_slow_body_server(SLOW_BODY_DELAY_MS).await;
    let config = DownloaderConfig::for_client(test_client())
        .soft_timeout(Duration::from_millis(SOFT_TIMEOUT_MS))
        .build();
    let dl = Downloader::new(config);
    let root = EventBus::new(EVENT_BUS_CAPACITY);
    let scoped = root.scoped();
    let mut rx = scoped.subscribe();

    let handle = dl.register(Arc::new(MockPeer)).with_bus(scoped);
    let response = handle
        .execute(FetchCmd::get(url).build())
        .await
        .expect("fetch slow body");
    let body = response.body.collect().await.expect("collect slow body");
    assert_eq!(body.as_ref(), b"ok");

    let deadline = Instant::now() + Duration::from_secs(SLOW_DEADLINE_SECS);
    let mut seen_slow = false;
    while Instant::now() < deadline {
        match time::timeout(Duration::from_millis(SLOW_POLL_TIMEOUT_MS), rx.recv()).await {
            Ok(Ok(Event::Downloader(DownloaderEvent::LoadSlow { .. }))) => {
                seen_slow = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert!(seen_slow, "body-only stall must publish LoadSlow");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PeerTag {
    Active,
    Preload,
}

type CompletionLog = Arc<Mutex<Vec<(PeerTag, usize)>>>;

/// Peer that emits `total_cmds` GET commands and stamps each with its
/// tag when the response arrives. `priority()` reads the shared
/// `SeekState` activity so a mid-stream flip of `set_playing` is observable.
struct TaggedPriorityPeer {
    gate: Arc<CompletionGate>,
    seek: Arc<SeekState>,
    completion_log: CompletionLog,
    remaining: Mutex<usize>,
    tag: PeerTag,
    url: Url,
}

impl TaggedPriorityPeer {
    fn new(
        tag: PeerTag,
        seek: Arc<SeekState>,
        url: Url,
        cmds: usize,
        gate: &Arc<CompletionGate>,
        completion_log: &CompletionLog,
    ) -> Self {
        Self {
            tag,
            seek,
            url,
            remaining: Mutex::new(cmds),
            gate: Arc::clone(gate),
            completion_log: Arc::clone(completion_log),
        }
    }
}

impl Abr for TaggedPriorityPeer {}
impl Peer for TaggedPriorityPeer {
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let (take, more) = {
            let mut rem = self.remaining.lock();
            if *rem == 0 {
                return Poll::Pending;
            }
            let take = (*rem).min(FLOOD_BATCH_SIZE);
            *rem -= take;
            (take, *rem > 0)
        };
        let cmds: Vec<FetchCmd> = (0..take)
            .map(|_| {
                let tag = self.tag;
                let log = Arc::clone(&self.completion_log);
                let gate = Arc::clone(&self.gate);
                FetchCmd::get(self.url.clone())
                    .writer(Box::new(|_chunk: &[u8]| Ok(())))
                    .on_complete(Box::new(
                        move |_bytes,
                              _headers: Option<&kithara_net::Headers>,
                              _err: Option<&kithara_net::NetError>| {
                            let order = gate.complete();
                            log.lock().push((tag, order));
                        },
                    ))
                    .build()
            })
            .collect();
        if more {
            cx.waker().wake_by_ref();
        }
        Poll::Ready(Some(cmds))
    }

    fn priority(&self) -> RequestPriority {
        if self.seek.is_playing() {
            RequestPriority::High
        } else {
            RequestPriority::Low
        }
    }
}

async fn spawn_slow_server(delay_ms: u64) -> Url {
    let app = Router::new().route(
        "/data",
        get(move || async move {
            time::sleep(Duration::from_millis(delay_ms)).await;
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

async fn spawn_slow_body_server(delay_ms: u64) -> Url {
    let app = Router::new().route(
        "/data",
        get(move || async move {
            Body::from_stream(futures::stream::once(async move {
                time::sleep(Duration::from_millis(delay_ms)).await;
                Ok::<_, Infallible>(Bytes::from_static(b"ok"))
            }))
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
#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn active_peer_completes_before_preload_under_contention() {
    const CMDS_PER_PEER: usize = 20;
    const MAX_CONCURRENT: usize = 2;
    const PER_REQUEST_DELAY_MS: u64 = 30;

    let url = spawn_slow_server(PER_REQUEST_DELAY_MS).await;

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..test_config()
    };
    let dl = Downloader::new(config);

    let completion_log = Arc::new(Mutex::new(Vec::<(PeerTag, usize)>::new()));
    let total = CMDS_PER_PEER * 2;
    let gate = CompletionGate::new(total);

    let seek_active = Arc::new(SeekState::new());
    seek_active.set_playing(true);
    let active = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Active,
        seek_active,
        url.clone(),
        CMDS_PER_PEER,
        &gate,
        &completion_log,
    ));

    let seek_preload = Arc::new(SeekState::new());
    let preload = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Preload,
        seek_preload,
        url,
        CMDS_PER_PEER,
        &gate,
        &completion_log,
    ));

    let active_handle = dl.register(active.clone());
    let preload_handle = dl.register(preload.clone());

    // Park until every cmd from both peers has completed (event-driven; the
    // harness timeout(30s) is the real-time backstop).
    gate.wait().await;

    drop(active_handle);
    drop(preload_handle);

    let log = completion_log.lock().clone();
    assert_eq!(log.len(), total, "every cmd must complete exactly once");

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
#[kithara::test(tokio, timeout(Duration::from_secs(30)))]
async fn both_peers_idle_no_priority_ordering_asserted() {
    const CMDS_PER_PEER: usize = 10;
    const MAX_CONCURRENT: usize = 2;
    const PER_REQUEST_DELAY_MS: u64 = 10;

    let url = spawn_slow_server(PER_REQUEST_DELAY_MS).await;

    let config = DownloaderConfig {
        max_concurrent: MAX_CONCURRENT,
        ..test_config()
    };
    let dl = Downloader::new(config);

    let completion_log = Arc::new(Mutex::new(Vec::<(PeerTag, usize)>::new()));
    let total = CMDS_PER_PEER * 2;
    let gate = CompletionGate::new(total);

    let a = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Active,
        Arc::new(SeekState::new()),
        url.clone(),
        CMDS_PER_PEER,
        &gate,
        &completion_log,
    ));
    let b = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Preload,
        Arc::new(SeekState::new()),
        url,
        CMDS_PER_PEER,
        &gate,
        &completion_log,
    ));

    let handle_a = dl.register(a);
    let handle_b = dl.register(b);

    // Park until every cmd from both idle peers has drained (event-driven;
    // the harness timeout(30s) is the real-time backstop).
    gate.wait().await;

    drop(handle_a);
    drop(handle_b);

    let log = completion_log.lock().clone();
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
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn peer_handle_execute_respects_either_peer_priority() {
    struct FlippablePeer {
        seek: Arc<SeekState>,
    }
    impl Abr for FlippablePeer {}
    impl Peer for FlippablePeer {
        fn priority(&self) -> RequestPriority {
            if self.seek.is_playing() {
                RequestPriority::High
            } else {
                RequestPriority::Low
            }
        }
    }

    let dl = Downloader::new(test_config());
    let seek = Arc::new(SeekState::new());
    let peer = Arc::new(FlippablePeer {
        seek: Arc::clone(&seek),
    });
    let handle = dl.register(peer);

    let url = spawn_slow_server(1).await;

    assert_eq!(
        peer_priority_from_handle(&handle, &seek),
        RequestPriority::Low
    );
    let low_resp = handle.execute(FetchCmd::get(url.clone()).build()).await;
    assert!(low_resp.is_ok(), "execute must succeed while Low");

    seek.set_playing(true);
    assert_eq!(
        peer_priority_from_handle(&handle, &seek),
        RequestPriority::High
    );
    let high_resp = handle.execute(FetchCmd::get(url).build()).await;
    assert!(high_resp.is_ok(), "execute must succeed while High");
}

/// Helper for the deterministic routing test: reads the effective
/// peer priority from the same `SeekState` activity the peer observes.
fn peer_priority_from_handle(_handle: &super::PeerHandle, seek: &SeekState) -> RequestPriority {
    if seek.is_playing() {
        RequestPriority::High
    } else {
        RequestPriority::Low
    }
}
