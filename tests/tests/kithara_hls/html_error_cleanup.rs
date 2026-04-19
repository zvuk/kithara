#![forbid(unsafe_code)]

//! Failed-fetch cleanup invariants.
//!
//! When a CDN returns `text/html` for a playlist or a DRM key, the HLS
//! engine must NOT leave orphan files or a retry-storm behind. These tests
//! pin two invariants:
//!
//! 1. **No empty cache files** — `atomic_fetch::fetch_atomic_body` pre-allocates
//!    a `DEFAULT_INITIAL_SIZE` mmap file via `backend.acquire_resource(key)`
//!    before the network fetch. On `reject_html_response` failure the file
//!    must be removed, not parked in the cache directory until process exit.
//!
//! 2. **Bounded retry count** — a dead endpoint must not keep producing
//!    network requests. The HLS engine either backs off, drops the track,
//!    or demotes its priority so the Downloader is not saturated.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_platform::{
    time::{Duration, Instant},
    tokio::time::sleep,
};
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio::{net::TcpListener, task};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone)]
struct ServerState {
    master_hits: Arc<AtomicUsize>,
    media_hits: Arc<AtomicUsize>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            master_hits: Arc::new(AtomicUsize::new(0)),
            media_hits: Arc::new(AtomicUsize::new(0)),
        }
    }
}

async fn master_html_handler(State(state): State<ServerState>) -> Response {
    state.master_hits.fetch_add(1, Ordering::Relaxed);
    let html = "<html><body>503 Service Unavailable</body></html>";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/html; charset=utf-8".parse().unwrap(),
    );
    (StatusCode::OK, headers, html.to_string()).into_response()
}

async fn valid_master_handler(State(state): State<ServerState>) -> Response {
    state.master_hits.fetch_add(1, Ordering::Relaxed);
    let body = "#EXTM3U\n\
                #EXT-X-VERSION:3\n\
                #EXT-X-STREAM-INF:BANDWIDTH=128000\n\
                v0.m3u8\n";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.apple.mpegurl".parse().unwrap(),
    );
    (StatusCode::OK, headers, body.to_string()).into_response()
}

async fn media_html_handler(State(state): State<ServerState>) -> Response {
    state.media_hits.fetch_add(1, Ordering::Relaxed);
    let html = "<html><body>503 Backend Error</body></html>";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/html; charset=utf-8".parse().unwrap(),
    );
    (StatusCode::OK, headers, html.to_string()).into_response()
}

async fn start_server(state: ServerState) -> SocketAddr {
    let app = Router::new()
        .route("/master.m3u8", get(master_html_handler))
        .fallback(get(master_html_handler))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    task::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

async fn start_partial_server(state: ServerState) -> SocketAddr {
    let app = Router::new()
        .route("/master.m3u8", get(valid_master_handler))
        .route("/v0.m3u8", get(media_html_handler))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    task::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

/// Walk `root` recursively and collect every file that is not inside the
/// `_index` directory (which holds `pins.bin` / `lru.bin` / `availability.bin`).
fn collect_cache_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|s| s.to_str()) == Some("_index") {
                    continue;
                }
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// After `Stream::new` fails with a `text/html` master playlist, no orphan
/// cache files may remain under the cache directory. Current behaviour
/// parks a pre-allocated `DEFAULT_INITIAL_SIZE` (64 KB) mmap file keyed by
/// the HTML response URL — confirmed by this test.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn html_master_playlist_leaves_no_orphan_cache_files(temp_dir: TestTempDir) {
    let state = ServerState::new();
    let addr = start_server(state.clone()).await;
    let url = Url::parse(&format!("http://{addr}/master.m3u8")).unwrap();

    let cancel = CancellationToken::new();
    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone());

    let result = Stream::<Hls>::new(config).await;
    assert!(
        result.is_err(),
        "HTML master playlist must fail Stream::new",
    );

    // Give LeaseResource::Drop + cache cleanup a chance to run. In practice
    // Drop is synchronous, so this is conservatively generous.
    sleep(Duration::from_millis(200)).await;

    let leftover = collect_cache_files(temp_dir.path());
    assert!(
        leftover.is_empty(),
        "failed atomic fetch must not leak cache files, found: {leftover:?}",
    );
}

/// A dead HTML endpoint must not produce a retry storm on the shared
/// Downloader. The current behaviour issues a single `Stream::new` attempt
/// which fails immediately; if future code wires auto-retry into the
/// transport, the hit count must still be bounded.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn html_master_playlist_does_not_retry_storm(temp_dir: TestTempDir) {
    let state = ServerState::new();
    let addr = start_server(state.clone()).await;
    let url = Url::parse(&format!("http://{addr}/master.m3u8")).unwrap();

    let cancel = CancellationToken::new();
    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone());

    let _ = Stream::<Hls>::new(config).await;

    // Hold the stream wrapper (even though it failed) and watch hit counts
    // over a few seconds. Any retry storm would show up as a monotonically
    // growing counter.
    let start_hits = state.master_hits.load(Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        sleep(Duration::from_millis(100)).await;
    }
    let end_hits = state.master_hits.load(Ordering::Relaxed);

    assert!(
        end_hits - start_hits <= 1,
        "retry storm detected: {start_hits} → {end_hits} over 3 s",
    );

    // Upper bound on the total count as a sanity check — an unbounded retry
    // loop would blow past this.
    assert!(
        end_hits <= 10,
        "excessive master_hits={end_hits} from a single Stream::new attempt",
    );
}

/// Master playlist succeeds but the media playlist for variant 0 returns
/// `text/html`. `Stream::new` must fail without leaving the pre-allocated
/// 64 KB mmap for the media playlist URL parked in the cache.
///
/// Exercises the exact `atomic_fetch.rs:67` path the user flagged: a second
/// `acquire_resource` after the first succeeded, followed by an
/// `InvalidContent` failure on the network fetch.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn html_media_playlist_after_successful_master_leaves_no_orphan_files(temp_dir: TestTempDir) {
    let state = ServerState::new();
    let addr = start_partial_server(state.clone()).await;
    let url = Url::parse(&format!("http://{addr}/master.m3u8")).unwrap();

    let cancel = CancellationToken::new();
    let config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone());

    let result = Stream::<Hls>::new(config).await;
    assert!(
        result.is_err(),
        "HTML on the media playlist must fail Stream::new",
    );

    // Wait long enough for any pending Drop + cache cleanup to land.
    sleep(Duration::from_millis(200)).await;

    // Master playlist is allowed to remain cached (it succeeded) but the
    // failing media playlist must NOT leave an orphan pre-allocated mmap.
    let leftover = collect_cache_files(temp_dir.path());
    let suspicious: Vec<_> = leftover
        .iter()
        .filter(|p| {
            let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            n.starts_with("v0") // media playlist basename
        })
        .collect();

    assert!(
        suspicious.is_empty(),
        "failed media-playlist fetch leaked cache file(s): {suspicious:?}\n\
         full cache contents: {leftover:?}",
    );
}
