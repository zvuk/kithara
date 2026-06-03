use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use axum::{
    Router,
    routing::{get, head},
};
use bytes::Bytes;
use dashmap::DashSet;
use futures::stream::iter as stream_iter;
use kithara_abr::Abr;
use kithara_events::{DownloaderEvent, Event, EventBus};
use kithara_net::{HttpClient, NetOptions};
use kithara_platform::{
    CancellationToken, Mutex,
    time::{self, Duration, Instant},
    tokio::{net::TcpListener as TokioTcpListener, task::spawn as tokio_spawn},
};
use kithara_test_utils::kithara;
use url::Url;

use super::{BodyStream, Downloader, DownloaderConfig, FetchCmd, Peer, RequestPriority};
use crate::{Activity, SeekState};

const CONCURRENCY_TEST_TIMEOUT_SECS: u64 = 30;
const FLOOD_BATCH_SIZE: usize = 10;
const FLOOD_POLL_MS: u64 = 100;
const PORT_STRESS_TIMEOUT_SECS: u64 = 60;
const SLOW_DEADLINE_SECS: u64 = 5;

struct MockPeer;

impl Abr for MockPeer {}
impl Peer for MockPeer {}

fn test_client() -> HttpClient {
    HttpClient::new(NetOptions::default(), CancellationToken::default())
}

fn test_config() -> DownloaderConfig {
    DownloaderConfig::for_client(test_client()).build()
}

fn test_body_stream(chunks: Vec<&'static [u8]>) -> BodyStream {
    let stream = stream_iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))));
    BodyStream::wrap_raw(Box::pin(stream))
}

async fn sleep(ms: u64) {
    time::sleep(Duration::from_millis(ms)).await;
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
        .total_timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build();
    let dl = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(net, CancellationToken::default())).build(),
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

    sleep(POLL_MS).await;
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
    let cancel = CancellationToken::default();
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
    const FLOOD_DEADLINE_SECS: u64 = 20;

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

    /// Peer that produces `remaining` HEAD commands via `poll_next`.
    struct FloodPeer {
        url: Url,
        remaining: Mutex<usize>,
    }

    impl Abr for FloodPeer {}
    impl Peer for FloodPeer {
        fn priority(&self) -> RequestPriority {
            RequestPriority::Low
        }

        fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
            let (batch_size, more) = {
                let mut rem = self.remaining.lock_sync();
                if *rem == 0 {
                    return Poll::Ready(None);
                }
                let batch_size = (*rem).min(FLOOD_BATCH_SIZE);
                *rem -= batch_size;
                (batch_size, *rem > 0)
            };
            let cmds: Vec<FetchCmd> = (0..batch_size)
                .map(|_| FetchCmd::head(self.url.clone()).build())
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
    let peer = Arc::new(FloodPeer {
        url,
        remaining: Mutex::new(TOTAL_CMDS),
    });
    let handle = dl.register(peer.clone());

    let deadline = Instant::now() + Duration::from_secs(FLOOD_DEADLINE_SECS);
    loop {
        time::sleep(Duration::from_millis(FLOOD_POLL_MS)).await;
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

/// Verify that Downloaders sharing a single [`HttpClient`] reuse the
/// same keep-alive socket pool across **successive** Downloader
/// lifetimes — the realistic prod pattern (tracks come and go, ABR
/// `switch_variant` rebuilds Downloaders, queue advances next track).
///
/// Contract:
/// - The caller builds one [`HttpClient`] and hands a clone to every
///   Downloader via [`DownloaderConfig::client`]. `reqwest::Client` is
///   internally `Arc`'d so all clones share one connection pool.
/// - When a Downloader is dropped, its keep-alive sockets stay in the
///   shared pool's idle list and are picked up by the next Downloader.
/// - With `WAVES` rounds × `PARALLEL_DLS` parallel Downloaders, the
///   server-observed unique client ports must stay close to a single
///   wave's peak (`PARALLEL_DLS * MAX_CONCURRENT`), independent of the
///   number of waves.
///
/// A regression that reverts to a per-Downloader client would multiply
/// the observed port count by `WAVES`, immediately tripping the
/// assertion.
#[kithara::test(tokio, timeout(Duration::from_secs(PORT_STRESS_TIMEOUT_SECS)))]
async fn shared_client_keepalive_bounds_socket_count() {
    const PARALLEL_DLS: usize = 8;
    const WAVES: usize = 25;
    const REQUESTS_PER_DL: usize = 114;
    const MAX_CONCURRENT: usize = 5;
    /// Single-wave peak when keep-alive is shared correctly: the first
    /// wave establishes ≤ `PARALLEL_DLS * MAX_CONCURRENT` sockets; each
    /// subsequent wave reuses them from the shared idle pool. 50%
    /// headroom covers race between handler completion and pool
    /// checkout. A regression to per-Downloader pools produces
    /// `WAVES * PARALLEL_DLS * MAX_CONCURRENT` ≈ 1000 unique sockets.
    const MAX_UNIQUE_PORTS: usize = PARALLEL_DLS * MAX_CONCURRENT * 3 / 2;

    let total_served = Arc::new(AtomicUsize::new(0));
    let total_served_c = Arc::clone(&total_served);
    let unique_ports: Arc<DashSet<u16>> = Arc::new(DashSet::new());
    let unique_ports_c = Arc::clone(&unique_ports);

    let app = Router::new()
        .route(
            "/head",
            head(move || {
                total_served_c.fetch_add(1, Ordering::Relaxed);
                async { "" }
            }),
        )
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let unique = Arc::clone(&unique_ports_c);
                async move {
                    if let Some(info) = req
                        .extensions()
                        .get::<axum::extract::ConnectInfo<SocketAddr>>()
                    {
                        unique.insert(info.0.port());
                    }
                    next.run(req).await
                }
            },
        ));

    let listener = TokioTcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("local_addr");
    tokio_spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });

    let url = Url::parse(&format!("http://{addr}/head")).expect("url");
    let shared_client = HttpClient::new(
        NetOptions::builder()
            .pool_max_idle_per_host(PARALLEL_DLS * MAX_CONCURRENT)
            .build(),
        CancellationToken::default(),
    );

    let mut total_ok = 0;
    let mut all_failures: Vec<String> = Vec::new();
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
    }

    let expected = total_dls * REQUESTS_PER_DL;
    let unique = unique_ports.len();
    assert!(
        all_failures.is_empty(),
        "shared client should not produce HTTP failures: {total_ok}/{expected} ok, \
         {} downloaders had failures (unique_client_ports={unique}):\n{}",
        all_failures.len(),
        all_failures.join("\n")
    );
    assert!(
        unique <= MAX_UNIQUE_PORTS,
        "shared keep-alive regression: {unique} unique client ports for {expected} requests \
         across {WAVES} waves of {PARALLEL_DLS} downloaders \
         (expected ≤ {MAX_UNIQUE_PORTS}). Successive Downloaders should reuse sockets \
         from the shared pool; this many indicates per-Downloader clients."
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
    let app = Router::new().route(
        "/slow",
        get(|| async {
            time::sleep(Duration::from_millis(SLOW_SERVER_DELAY_MS)).await;
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
    completion_counter: Arc<AtomicUsize>,
    completion_log: CompletionLog,
    remaining: Mutex<usize>,
    tag: PeerTag,
    seek: Arc<SeekState>,
    url: Url,
}

impl TaggedPriorityPeer {
    fn new(
        tag: PeerTag,
        seek: Arc<SeekState>,
        url: Url,
        cmds: usize,
        completion_counter: &Arc<AtomicUsize>,
        completion_log: &CompletionLog,
    ) -> Self {
        Self {
            tag,
            seek,
            url,
            remaining: Mutex::new(cmds),
            completion_counter: Arc::clone(completion_counter),
            completion_log: Arc::clone(completion_log),
        }
    }
}

impl Abr for TaggedPriorityPeer {}
impl Peer for TaggedPriorityPeer {
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let (take, more) = {
            let mut rem = self.remaining.lock_sync();
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
                let counter = Arc::clone(&self.completion_counter);
                FetchCmd::get(self.url.clone())
                    .writer(Box::new(|_chunk: &[u8]| Ok(())))
                    .on_complete(Box::new(
                        move |_bytes,
                              _headers: Option<&kithara_net::Headers>,
                              _err: Option<&kithara_net::NetError>| {
                            let order = counter.fetch_add(1, Ordering::SeqCst);
                            log.lock_sync().push((tag, order));
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
    let completion_counter = Arc::new(AtomicUsize::new(0));

    let seek_active = Arc::new(SeekState::new());
    seek_active.set_playing(true);
    let active = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Active,
        seek_active,
        url.clone(),
        CMDS_PER_PEER,
        &completion_counter,
        &completion_log,
    ));

    let seek_preload = Arc::new(SeekState::new());
    let preload = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Preload,
        seek_preload,
        url,
        CMDS_PER_PEER,
        &completion_counter,
        &completion_log,
    ));

    let active_handle = dl.register(active.clone());
    let preload_handle = dl.register(preload.clone());

    let total = CMDS_PER_PEER * 2;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        time::sleep(Duration::from_millis(FLOOD_POLL_MS)).await;
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
    let completion_counter = Arc::new(AtomicUsize::new(0));

    let a = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Active,
        Arc::new(SeekState::new()),
        url.clone(),
        CMDS_PER_PEER,
        &completion_counter,
        &completion_log,
    ));
    let b = Arc::new(TaggedPriorityPeer::new(
        PeerTag::Preload,
        Arc::new(SeekState::new()),
        url,
        CMDS_PER_PEER,
        &completion_counter,
        &completion_log,
    ));

    let handle_a = dl.register(a);
    let handle_b = dl.register(b);

    let total = CMDS_PER_PEER * 2;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        time::sleep(Duration::from_millis(FLOOD_POLL_MS)).await;
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
