#![forbid(unsafe_code)]
//! `kithara-file` html-response cleanup invariants.
//!
//! Repro path for the user-reported leak: a remote track URL behind a
//! corporate firewall returns `text/html` (captive-portal / VPN error page).
//! The `Stream<File>` stays alive in the app (held by the player queue),
//! so the pre-allocated 64 KB mmap created by
//! `FileStreamState::create → AssetStore::acquire_resource` survives inside
//! `FileInner.res` until the app shuts down.
//!
//! Invariant under test: after the download task fails with an
//! `InvalidContent` (html) error, the cache file for the failing URL must
//! NOT remain on disk — even while `Stream<File>` is still live. Expected
//! to be RED on main: `run_full_download`'s err path currently only calls
//! `state.res.fail(...)`, which marks the resource Failed but leaves the
//! mmap file intact.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
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
    events::{Event, EventBus, FileEvent},
    file::{File, FileConfig},
    stream::Stream,
};
use kithara_platform::{
    time::Instant,
    tokio::time::{sleep, timeout},
};
use kithara_test_utils::{TestTempDir, temp_dir};
use tokio::{net::TcpListener, task};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone)]
struct ServerState {
    track_hits: Arc<AtomicUsize>,
}

impl ServerState {
    fn new() -> Self {
        Self {
            track_hits: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// Mirrors the corporate-firewall captive-portal response the user reported:
/// the backend replies `200 OK text/html` with an error body instead of the
/// expected audio payload.
async fn captive_portal_handler(State(state): State<ServerState>) -> Response {
    state.track_hits.fetch_add(1, Ordering::Relaxed);
    let html = "<html><body>VPN required to access this resource</body></html>";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/html; charset=utf-8".parse().unwrap(),
    );
    (StatusCode::OK, headers, html.to_string()).into_response()
}

async fn start_captive_portal_server(state: ServerState) -> SocketAddr {
    // Catch-all route — whatever path the test uses, the server returns html.
    let app = Router::new()
        .fallback(get(captive_portal_handler))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    task::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

/// Walk `root` recursively and collect every file that is not inside the
/// `_index` directory.
fn collect_cache_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
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

/// After `Stream<File>::new` returns, the async download task races; wait on
/// the event bus for a terminal `DownloadError` (or `DownloadComplete`)
/// before inspecting the cache.
async fn wait_for_download_terminal(bus: &EventBus, within: Duration) -> bool {
    let mut rx = bus.subscribe();
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let recv = timeout(remaining, rx.recv());
        match recv.await {
            Ok(Ok(Event::File(FileEvent::DownloadError { .. }))) => return true,
            Ok(Ok(Event::File(FileEvent::DownloadComplete { .. }))) => return true,
            Ok(Ok(_)) => {} // unrelated event — keep waiting
            Ok(Err(_)) | Err(_) => return false,
        }
    }
}

/// Stream stays alive throughout the test. The captive-portal html response
/// must not leave a 64 KB orphan mmap parked in the cache directory.
///
/// Without the fix this test is RED: `FileInner.res` still holds a clone of
/// the pre-allocated `AssetResource`, so `LeaseResource::Drop` doesn't fire
/// until `Stream<File>` itself is dropped at app shutdown.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn remote_file_html_response_does_not_leak_cache_file_while_stream_alive(
    temp_dir: TestTempDir,
) {
    let server_state = ServerState::new();
    let addr = start_captive_portal_server(server_state.clone()).await;
    // Mirrors the user-reported URL shape: path segment + query string such
    // that `ResourceKey::from_url` produces a `{segment}_{query}` basename.
    let url = Url::parse(&format!("http://{addr}/track/streamhq?id=27390231")).unwrap();

    let bus = EventBus::new(64);
    let cancel = CancellationToken::new();
    let config = FileConfig::new(url.into())
        .with_events(bus.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone());

    let stream = Stream::<File>::new(config).await.unwrap();

    // Download task is async. Block on the bus until it emits a terminal
    // event (DownloadError on html) so we inspect the cache at the right
    // moment — not racing the fetch.
    let saw_terminal = wait_for_download_terminal(&bus, Duration::from_secs(5)).await;
    assert!(
        saw_terminal,
        "expected DownloadError on html response within 5 s",
    );

    // Give the err-path a moment to perform any pending cleanup hops.
    sleep(Duration::from_millis(200)).await;

    // CRITICAL: assert while `stream` is still held. On main the pre-allocated
    // 64 KB mmap persists here because `FileInner.res` keeps the LeaseResource
    // clone alive, blocking LeaseResource::Drop from calling remove_resource.
    let leftover = collect_cache_files(temp_dir.path());
    assert!(
        leftover.is_empty(),
        "captive-portal html response left orphan cache file(s) while Stream<File> \
         was still live: {leftover:?}\n\
         expected: no files under the cache root (Drop-style cleanup must happen \
         eagerly on download failure, not at Stream-drop time)",
    );

    // Explicit drop is intentional — placed here so rust-analyzer doesn't
    // shorten `stream`'s lifetime above the assertion. Without this the
    // borrow-checker is free to drop `stream` before the cache walk,
    // defeating the whole point of the test.
    drop(stream);
}

/// A captive-portal endpoint must not trigger a retry storm on the
/// Downloader. `run_full_download` is a single attempt; asserting this
/// explicitly locks the invariant against future accidental retry loops.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn remote_file_html_response_does_not_retry_storm(temp_dir: TestTempDir) {
    let server_state = ServerState::new();
    let addr = start_captive_portal_server(server_state.clone()).await;
    let url = Url::parse(&format!("http://{addr}/track/streamhq?id=27390231")).unwrap();

    let bus = EventBus::new(64);
    let cancel = CancellationToken::new();
    let config = FileConfig::new(url.into())
        .with_events(bus.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone());

    let stream = Stream::<File>::new(config).await.unwrap();
    let _ = wait_for_download_terminal(&bus, Duration::from_secs(5)).await;

    // Hold the Stream alive and watch for new hits.
    let baseline = server_state.track_hits.load(Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        sleep(Duration::from_millis(100)).await;
    }
    let after = server_state.track_hits.load(Ordering::Relaxed);

    assert!(
        after - baseline <= 1,
        "retry storm detected while Stream<File> held a failed resource: \
         {baseline} → {after} hits over 3 s",
    );

    drop(stream);
}
